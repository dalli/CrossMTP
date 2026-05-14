//! `AdbSession` — top-level entry point for Phase 1.
//!
//! Responsibilities (plan.md §4.2 / §8 Phase 1):
//!   * discover the `adb` executable
//!   * enumerate devices and classify their state
//!   * select a single device by **serial** (Phase 0 retro §2.3)
//!   * run shell commands against the selected device
//!   * spawn long-running children with structured lifecycle so the
//!     orchestrator can cancel them later
//!   * report capabilities so the UI can branch
//!
//! What this layer does NOT do:
//!   * stream tar (Phase 2)
//!   * conflict resolution (Phase 2)
//!   * device-side `tar` PID cleanup (Phase 2 step 4)
//!   * UI integration (Phase 4)

use std::sync::Arc;

use crate::capability::AdbCapabilities;
use crate::devices::AdbDevice;
use crate::discovery::{discover_adb, AdbLocation};
use crate::error::{AdbError, Result};
use crate::process::{list_devices_via, spawn_piped, AdbOutput, AdbProcess, AdbRunner, CommandRunner};

pub struct AdbSession {
    location: AdbLocation,
    runner: Arc<dyn AdbRunner>,
    capabilities: AdbCapabilities,
}

impl std::fmt::Debug for AdbSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AdbSession")
            .field("location", &self.location)
            .field("capabilities", &self.capabilities)
            .finish()
    }
}

impl AdbSession {
    /// Resolve the `adb` executable and build a session that talks to
    /// it via the real `std::process::Command`.
    pub fn open() -> Result<Self> {
        let location = discover_adb()?;
        let runner: Arc<dyn AdbRunner> = Arc::new(CommandRunner::new(location.path.clone()));
        Ok(Self {
            location,
            runner,
            capabilities: AdbCapabilities::phase2_default(),
        })
    }

    /// Build a session with an injected runner. Used by tests and by
    /// future code that wants to drive adb through a different
    /// transport (adb-over-network, mock, etc.).
    pub fn with_runner(location: AdbLocation, runner: Arc<dyn AdbRunner>) -> Self {
        Self {
            location,
            runner,
            capabilities: AdbCapabilities::phase2_default(),
        }
    }

    pub fn location(&self) -> &AdbLocation {
        &self.location
    }

    pub fn capabilities(&self) -> &AdbCapabilities {
        &self.capabilities
    }

    /// Enumerate devices as adb sees them right now. Returns an empty
    /// vec when adb itself is reachable but no device is attached — the
    /// UI distinguishes "no device" from "adb missing" via capabilities.
    pub fn list_devices(&self) -> Result<Vec<AdbDevice>> {
        list_devices_via(self.runner.as_ref())
    }

    /// Find the first device that is in the `device` state. Returns
    /// `AdbError::NoDevice` if no device is plugged in, or the most
    /// specific state error (`Unauthorized` / `Offline` / etc.) if at
    /// least one device is listed but none are ready.
    pub fn pick_ready_device(&self) -> Result<AdbDevice> {
        let devs = self.list_devices()?;
        if devs.is_empty() {
            return Err(AdbError::NoDevice);
        }
        if let Some(d) = devs.iter().find(|d| d.is_ready()) {
            return Ok(d.clone());
        }
        // None ready — surface the first device's classified error so
        // the UI shows "accept the prompt on the phone" instead of a
        // generic "no device".
        devs[0].require_ready()?;
        unreachable!("require_ready on a non-ready device always errors")
    }

    /// Look up a device by serial. Returns `DeviceNotFound` if the
    /// serial isn't in the current `adb devices` list, otherwise the
    /// state-specific error if the device is not ready.
    pub fn require_device(&self, serial: &str) -> Result<AdbDevice> {
        let devs = self.list_devices()?;
        let Some(dev) = devs.into_iter().find(|d| d.serial == serial) else {
            return Err(AdbError::DeviceNotFound {
                serial: serial.to_string(),
            });
        };
        dev.require_ready()?;
        Ok(dev)
    }

    /// Run `adb -s <serial> shell <command...>` to completion. The
    /// command is passed as separate args so the runner can avoid
    /// shell-injection joins; quoting is the caller's responsibility on
    /// the *device* side. We do not stringify-and-split here.
    pub fn shell(&self, serial: &str, command: &[&str]) -> Result<AdbOutput> {
        let mut args = vec!["-s", serial, "shell"];
        args.extend_from_slice(command);
        self.runner.run(&args)
    }

    /// Spawn a long-running `adb -s <serial> <args...>` child with
    /// piped stdio. The caller drives stdin/stdout/stderr and must
    /// dispose of the returned [`AdbProcess`] explicitly (via
    /// `terminate` or `kill`) when done. This is the API Phase 2 uses
    /// for `adb shell tar -x -C <dest>`.
    pub fn spawn(&self, serial: &str, args: &[&str], label: &str) -> Result<AdbProcess> {
        // Spawning is a real fs/process op — it can't go through the
        // mockable `AdbRunner` trait. Tests cover the lifecycle behaviour
        // via `AdbProcess::terminate` on a known-shape child elsewhere.
        let mut full: Vec<&str> = vec!["-s", serial];
        full.extend_from_slice(args);
        spawn_piped(&self.location.path, &full, label)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::AdbSource;
    use crate::process::StubRunner;
    use std::path::PathBuf;

    fn loc() -> AdbLocation {
        AdbLocation {
            path: PathBuf::from("/usr/local/bin/adb"),
            source: AdbSource::Path,
        }
    }

    fn ok(stdout: &str) -> AdbOutput {
        AdbOutput {
            exit_code: 0,
            stdout: stdout.into(),
            stderr: String::new(),
        }
    }

    #[test]
    fn pick_ready_device_returns_first_in_device_state() {
        let stub = Arc::new(StubRunner::new(vec![Ok(ok(
            "List of devices attached\nA unauthorized\nB device transport_id:5\n",
        ))]));
        let session = AdbSession::with_runner(loc(), stub);
        let d = session.pick_ready_device().unwrap();
        assert_eq!(d.serial, "B");
    }

    #[test]
    fn pick_ready_device_surfaces_unauthorized_when_only_choice() {
        let stub = Arc::new(StubRunner::new(vec![Ok(ok(
            "List of devices attached\nA unauthorized\n",
        ))]));
        let session = AdbSession::with_runner(loc(), stub);
        let err = session.pick_ready_device().unwrap_err();
        assert!(matches!(err, AdbError::Unauthorized { .. }));
    }

    #[test]
    fn pick_ready_device_returns_no_device_when_empty() {
        let stub = Arc::new(StubRunner::new(vec![Ok(ok("List of devices attached\n"))]));
        let session = AdbSession::with_runner(loc(), stub);
        let err = session.pick_ready_device().unwrap_err();
        assert!(matches!(err, AdbError::NoDevice));
    }

    #[test]
    fn require_device_returns_not_found_for_unknown_serial() {
        let stub = Arc::new(StubRunner::new(vec![Ok(ok(
            "List of devices attached\nKNOWN device\n",
        ))]));
        let session = AdbSession::with_runner(loc(), stub);
        let err = session.require_device("UNKNOWN").unwrap_err();
        assert!(matches!(err, AdbError::DeviceNotFound { .. }));
    }

    #[test]
    fn require_device_returns_state_error_when_not_ready() {
        let stub = Arc::new(StubRunner::new(vec![Ok(ok(
            "List of devices attached\nKNOWN offline\n",
        ))]));
        let session = AdbSession::with_runner(loc(), stub);
        let err = session.require_device("KNOWN").unwrap_err();
        assert!(matches!(err, AdbError::Offline { .. }));
    }

    #[test]
    fn shell_passes_serial_and_command_args_verbatim() {
        let stub = Arc::new(StubRunner::new(vec![Ok(ok("hello\n"))]));
        let session = AdbSession::with_runner(loc(), stub.clone());
        let out = session.shell("SER", &["echo", "hello"]).unwrap();
        assert_eq!(out.stdout, "hello\n");
        let calls = stub.calls.lock().unwrap();
        assert_eq!(
            calls[0],
            vec![
                "-s".to_string(),
                "SER".into(),
                "shell".into(),
                "echo".into(),
                "hello".into()
            ]
        );
    }

    #[test]
    fn shell_propagates_command_failed_with_stderr() {
        let stub = Arc::new(StubRunner::new(vec![Ok(AdbOutput {
            exit_code: 1,
            stdout: String::new(),
            stderr: "device offline".into(),
        })]));
        let session = AdbSession::with_runner(loc(), stub);
        let out = session.shell("SER", &["ls"]).unwrap();
        let err = out.into_stdout_if_ok().unwrap_err();
        assert!(matches!(err, AdbError::CommandFailed { code: 1, .. }));
    }
}
