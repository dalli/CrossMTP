//! Child-process lifecycle abstraction.
//!
//! Phase 1 of plan.md only requires that the layer be **testable** in
//! terms of process lifecycle — it does not yet run a live `tar -x`
//! stream. We therefore split:
//!
//! * `AdbRunner` — trait that "runs an adb command and reports its
//!   exit". Real impl shells out via `std::process::Command`; tests
//!   inject a stub.
//! * `AdbProcess` — RAII wrapper around a spawned long-running child
//!   used by Phase 2. We surface `terminate()` + `kill()` here so the
//!   §6.1 cancel sequence has a concrete API to call against.
//!
//! `AdbProcess` itself is **not** exercised by unit tests because that
//! would require a real `adb` binary; Phase 0's `scripts/adb-phase0-*`
//! harnesses cover the real-process path.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::devices::{parse_devices_output, AdbDevice};
use crate::error::{AdbError, Result};

/// Capture of a finished one-shot adb command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdbOutput {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl AdbOutput {
    pub fn into_stdout_if_ok(self) -> Result<String> {
        if self.exit_code == 0 {
            Ok(self.stdout)
        } else {
            Err(AdbError::CommandFailed {
                code: self.exit_code,
                stderr: self.stderr,
            })
        }
    }
}

/// Pluggable runner so the higher-level `AdbSession` is unit-testable
/// without a real `adb` binary on the host.
pub trait AdbRunner: Send + Sync {
    /// Run `adb <args...>` to completion and capture stdout/stderr.
    fn run(&self, args: &[&str]) -> Result<AdbOutput>;
}

/// Real runner that invokes the resolved `adb` executable. Honours
/// neither device selection nor `-s <serial>` — callers prepend those
/// arguments themselves so the runner stays dumb and the session layer
/// is the only thing that knows about adb semantics.
pub struct CommandRunner {
    pub adb_path: PathBuf,
}

impl CommandRunner {
    pub fn new(adb_path: impl Into<PathBuf>) -> Self {
        Self {
            adb_path: adb_path.into(),
        }
    }
}

impl AdbRunner for CommandRunner {
    fn run(&self, args: &[&str]) -> Result<AdbOutput> {
        let output = Command::new(&self.adb_path)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()?;

        let exit_code = output.status.code().ok_or(AdbError::CommandTerminated)?;
        Ok(AdbOutput {
            exit_code,
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

/// Convenience: run `adb devices -l` against a runner and parse the
/// result. Lives next to the runner so tests can drive it without the
/// full `AdbSession`.
pub fn list_devices_via(runner: &dyn AdbRunner) -> Result<Vec<AdbDevice>> {
    let out = runner.run(&["devices", "-l"])?;
    let text = out.into_stdout_if_ok()?;
    parse_devices_output(&text)
}

// ---------- long-running child wrapper ----------

/// RAII handle for an `adb` child process started with piped stdio.
/// Created by `AdbSession::spawn_shell` for callers that need to drive
/// a long-running shell (Phase 2 tar streaming). Drop sends SIGKILL as
/// a last-resort backstop; the §6.1 cancellation sequence should
/// `terminate()` + `wait_with_timeout()` + `kill()` explicitly.
pub struct AdbProcess {
    child: Option<Child>,
    label: String,
}

impl AdbProcess {
    pub(crate) fn new(child: Child, label: impl Into<String>) -> Self {
        Self {
            child: Some(child),
            label: label.into(),
        }
    }

    /// Process id on Unix, where the orchestrator will need it for the
    /// `pkill -f` device-side cleanup path (Phase 0 retro §2.2).
    pub fn pid(&self) -> Option<u32> {
        self.child.as_ref().map(Child::id)
    }

    pub fn label(&self) -> &str {
        &self.label
    }

    /// Take stdin so the caller can drive a tar stream (Phase 2).
    pub fn take_stdin(&mut self) -> Option<std::process::ChildStdin> {
        self.child.as_mut().and_then(|c| c.stdin.take())
    }
    pub fn take_stdout(&mut self) -> Option<std::process::ChildStdout> {
        self.child.as_mut().and_then(|c| c.stdout.take())
    }
    pub fn take_stderr(&mut self) -> Option<std::process::ChildStderr> {
        self.child.as_mut().and_then(|c| c.stderr.take())
    }

    /// Send SIGTERM and wait up to `grace` for the child to exit. If it
    /// hasn't exited, SIGKILL it and wait again. Mirrors plan.md §6.1
    /// step 3 (host-side termination only — device-side `tar` PID
    /// cleanup is the orchestrator's responsibility in Phase 2 step 4).
    pub fn terminate(&mut self, grace: Duration) -> Result<AdbOutput> {
        let Some(mut child) = self.child.take() else {
            return Err(AdbError::CommandTerminated);
        };

        #[cfg(unix)]
        unsafe {
            // SIGTERM = 15.
            libc_kill(child.id() as i32, 15);
        }

        let deadline = Instant::now() + grace;
        loop {
            match child.try_wait()? {
                Some(_) => break,
                None => {
                    if Instant::now() >= deadline {
                        let _ = child.kill();
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(25));
                }
            }
        }
        let _ = child.wait()?;
        let mut stdout = String::new();
        let mut stderr = String::new();
        if let Some(mut s) = child.stdout.take() {
            let _ = s.read_to_string(&mut stdout);
        }
        if let Some(mut s) = child.stderr.take() {
            let _ = s.read_to_string(&mut stderr);
        }
        let code = child.wait()?.code().unwrap_or(-1);
        Ok(AdbOutput {
            exit_code: code,
            stdout,
            stderr,
        })
    }

    /// Wait for the child to exit on its own (no signals sent). Use
    /// this for the **normal-completion** path after the caller has
    /// already drained / dropped stdin so the child will see EOF and
    /// exit. Returns captured stdout+stderr and the exit code.
    ///
    /// Calling this on an already-disposed handle returns
    /// `AdbError::CommandTerminated`.
    pub fn wait_capture(&mut self) -> Result<AdbOutput> {
        let Some(mut child) = self.child.take() else {
            return Err(AdbError::CommandTerminated);
        };
        // Drain stdout/stderr BEFORE wait — wait can deadlock if the
        // pipes are full, but with our typical tar -x volume that's
        // fine since we already closed stdin.
        let mut stdout = String::new();
        let mut stderr = String::new();
        if let Some(mut s) = child.stdout.take() {
            let _ = s.read_to_string(&mut stdout);
        }
        if let Some(mut s) = child.stderr.take() {
            let _ = s.read_to_string(&mut stderr);
        }
        let status = child.wait()?;
        let code = status.code().unwrap_or(-1);
        Ok(AdbOutput {
            exit_code: code,
            stdout,
            stderr,
        })
    }

    /// Force SIGKILL — used by tests and by the Drop backstop.
    pub fn kill(&mut self) -> Result<()> {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        Ok(())
    }
}

impl Drop for AdbProcess {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            // Best-effort: don't let test or library bugs leak adb
            // children behind us. The orchestrator's structured
            // cancellation path should normally have called terminate()
            // long before this runs.
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Tiny manual binding so we don't pull `nix` or `libc` as a dep.
#[cfg(unix)]
#[allow(non_snake_case)]
unsafe fn libc_kill(pid: i32, sig: i32) {
    extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    // SAFETY: extern "C" declaration matches POSIX `kill(2)`.
    unsafe {
        let _ = kill(pid, sig);
    }
}

/// Spawn `adb <args>` with piped stdio. Crate-private; callers go
/// through `AdbSession::spawn`.
pub(crate) fn spawn_piped(
    adb_path: &Path,
    args: &[&str],
    label: impl Into<String>,
) -> Result<AdbProcess> {
    let child = Command::new(adb_path)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    Ok(AdbProcess::new(child, label))
}

// Test-only stub runner. Lives outside `#[cfg(test)] mod tests` so other
// modules in this crate can use it via `pub(crate) use`; `cfg(test)`
// gates compilation so it never reaches release builds.
#[cfg(test)]
pub(crate) use stub::StubRunner;

#[cfg(test)]
mod stub {
    use super::*;
    use std::sync::Mutex;

    /// Stub runner that records the args it was called with and replays
    /// scripted outputs. Used by `devices.rs` integration via
    /// `list_devices_via` and by `session.rs` tests for state probing.
    pub(crate) struct StubRunner {
        pub calls: Mutex<Vec<Vec<String>>>,
        pub responses: Mutex<Vec<Result<AdbOutput>>>,
    }

    impl StubRunner {
        pub fn new(responses: Vec<Result<AdbOutput>>) -> Self {
            Self {
                calls: Mutex::new(vec![]),
                responses: Mutex::new(responses),
            }
        }
    }

    impl AdbRunner for StubRunner {
        fn run(&self, args: &[&str]) -> Result<AdbOutput> {
            self.calls
                .lock()
                .unwrap()
                .push(args.iter().map(|s| s.to_string()).collect());
            let mut r = self.responses.lock().unwrap();
            if r.is_empty() {
                panic!("StubRunner ran out of scripted responses");
            }
            r.remove(0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_devices_via_runs_devices_dash_l_and_parses() {
        let stub = StubRunner::new(vec![Ok(AdbOutput {
            exit_code: 0,
            stdout: "List of devices attached\nSER device transport_id:1\n".into(),
            stderr: String::new(),
        })]);
        let devs = list_devices_via(&stub).unwrap();
        assert_eq!(devs.len(), 1);
        assert_eq!(devs[0].serial, "SER");
        let calls = stub.calls.lock().unwrap();
        assert_eq!(calls[0], vec!["devices".to_string(), "-l".into()]);
    }

    #[test]
    fn nonzero_exit_surfaces_command_failed() {
        let stub = StubRunner::new(vec![Ok(AdbOutput {
            exit_code: 1,
            stdout: String::new(),
            stderr: "adb: protocol fault".into(),
        })]);
        let err = list_devices_via(&stub).unwrap_err();
        assert!(matches!(err, AdbError::CommandFailed { code: 1, .. }));
    }
}
