//! High-level "stream a tar into `adb shell tar -x -C <dest>`" path.
//!
//! Phase 2 entrypoint that ties together:
//!   * `tar-stream::TarStreamBuilder` for the wire bytes
//!   * `AdbSession::spawn` for the child process
//!   * §6.1 cancel sequence including device-side `tar` PID cleanup
//!
//! Used by `mtp-cli adb tar-upload` and (Phase 3) the orchestrator.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use tar_stream::{ConflictPlan, ProgressSnapshot, TarStreamBuilder};

use crate::error::{AdbError, Result};
use crate::process::AdbProcess;
use crate::session::AdbSession;

/// Outcome of a tar-upload run.
#[derive(Debug, Clone)]
pub struct UploadOutcome {
    pub progress: ProgressSnapshot,
    pub host_exit_code: Option<i32>,
    pub stderr_tail: String,
}

/// Cancellation handle the orchestrator can hold while the upload runs.
/// Calling `cancel()` triggers the §6.1 5-step sequence:
///
///   1. flag the stream loop to stop emitting new entries (not relevant
///      yet because Phase 2 runs sequentially; flag is reserved for
///      Phase 3 when traversal and write are on separate threads).
///   2. SIGTERM the host adb child, grace, SIGKILL.
///   3. **kill the device-side tar** via `pkill -f 'tar -x -C <dest>'`.
///   4. confirm both ended.
///   5. orchestrator transitions to `cancelled`.
#[derive(Clone)]
pub struct CancelHandle {
    inner: Arc<Mutex<CancelState>>,
}

struct CancelState {
    requested: bool,
    /// Device-side `tar -x` cleanup command, parameterised on dest path.
    /// Captured at spawn time so cancel can pkill the exact pattern.
    dest_path: Option<String>,
}

impl CancelHandle {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(CancelState {
                requested: false,
                dest_path: None,
            })),
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.inner.lock().unwrap().requested
    }

    pub fn cancel(&self) {
        self.inner.lock().unwrap().requested = true;
    }

    fn set_dest(&self, dest: &str) {
        self.inner.lock().unwrap().dest_path = Some(dest.to_string());
    }

    #[allow(dead_code)]
    fn dest(&self) -> Option<String> {
        self.inner.lock().unwrap().dest_path.clone()
    }
}

impl Default for CancelHandle {
    fn default() -> Self {
        Self::new()
    }
}

/// Stream `source_root` (local file or directory) into `dest_path` on
/// `serial` using `adb shell tar -x -C <dest_path>`. Blocks the calling
/// thread until the device-side tar exits or `cancel` fires.
pub fn upload_tar(
    session: &AdbSession,
    serial: &str,
    source_root: &Path,
    dest_path: &str,
    plan: ConflictPlan,
    cancel: CancelHandle,
) -> Result<UploadOutcome> {
    if !is_safe_dest_path(dest_path) {
        return Err(AdbError::CommandFailed {
            code: -1,
            stderr: format!("tar-upload rejected unsafe dest path: {dest_path}"),
        });
    }
    cancel.set_dest(dest_path);

    // `mkdir -p` the destination first. Cheap, idempotent.
    let mkdir = session.shell(serial, &["mkdir", "-p", dest_path])?;
    if mkdir.exit_code != 0 {
        return Err(AdbError::CommandFailed {
            code: mkdir.exit_code,
            stderr: mkdir.stderr,
        });
    }

    let mut child: AdbProcess =
        session.spawn(serial, &["shell", "tar", "-x", "-C", dest_path], "tar-x")?;

    let mut stdin = child.take_stdin().ok_or_else(|| AdbError::CommandFailed {
        code: -1,
        stderr: "adb child did not expose stdin".into(),
    })?;

    let progress = build_and_stream(
        TarStreamBuilder::new(PathBuf::from(source_root)).with_conflict_plan(plan),
        &mut stdin,
        &cancel,
    );
    // Drop stdin to signal EOF to device-side tar.
    drop(stdin);

    // If the stream errored, terminate the child and surface the error.
    let progress = match progress {
        Ok(p) => p,
        Err(stream_err) => {
            let _ = best_effort_pkill(session, serial, dest_path);
            let _ = child.terminate(Duration::from_secs(1));
            return Err(AdbError::CommandFailed {
                code: -1,
                stderr: format!("tar stream failed: {stream_err}"),
            });
        }
    };

    // If cancel was requested mid-stream, follow the §6.1 5-step sequence.
    if cancel.is_cancelled() {
        let _ = best_effort_pkill(session, serial, dest_path);
        let _ = child.terminate(Duration::from_secs(1));
        return Ok(UploadOutcome {
            progress,
            host_exit_code: None,
            stderr_tail: "cancelled".into(),
        });
    }

    // Normal path: wait for the device-side tar to drain and exit on
    // its own (we already closed stdin above). No signals here — that
    // path is reserved for the cancel branch.
    let waited = child.wait_capture()?;
    Ok(UploadOutcome {
        progress,
        host_exit_code: Some(waited.exit_code),
        stderr_tail: waited.stderr.chars().take(2048).collect(),
    })
}

fn build_and_stream<W: Write>(
    builder: TarStreamBuilder,
    sink: &mut W,
    cancel: &CancelHandle,
) -> std::result::Result<ProgressSnapshot, tar_stream::TarError> {
    let counter = builder.write_to(&mut CancelAwareSink { inner: sink, cancel })?;
    Ok(counter.snapshot())
}

struct CancelAwareSink<'a, W: Write> {
    inner: &'a mut W,
    cancel: &'a CancelHandle,
}

impl<'a, W: Write> Write for CancelAwareSink<'a, W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.cancel.is_cancelled() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "upload cancelled",
            ));
        }
        self.inner.write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// `pkill -f` the device-side tar. We match the exact arg form we used
/// at spawn so we don't kill other tars the user is running.
pub fn best_effort_pkill(session: &AdbSession, serial: &str, dest_path: &str) -> Result<()> {
    // toybox pkill is available on the Phase 0 baseline. If it isn't,
    // the call returns CommandFailed which we swallow.
    let _ = session.shell(
        serial,
        &["pkill", "-f", &format!("tar -x -C {dest_path}")],
    )?;
    Ok(())
}

/// Active smoke-check: spawn `adb shell tar -x -C /data/local/tmp`,
/// write a single end-of-archive marker (two 512-byte zero blocks) to
/// its stdin, close stdin, and assert exit code 0. This proves the
/// device-side tar accepts our wire format end-to-end without writing
/// any real files.
///
/// `/data/local/tmp` is intentionally chosen as the extraction root:
/// the marker contains no entries so nothing is actually created,
/// but the path must exist and be writable by `adb shell`. Phase 0
/// confirmed it's available on the toybox baseline.
///
/// Used by `device_caps::probe_device_with_smoke` and by the
/// orchestrator at ADB session bring-up. The result is cached by
/// `DeviceCapabilities::tar_extract_smoke_ok`.
pub fn smoke_check_extract(session: &AdbSession, serial: &str) -> Result<bool> {
    let mut child: AdbProcess = session.spawn(
        serial,
        &["shell", "tar", "-x", "-C", "/data/local/tmp"],
        "tar-x-smoke",
    )?;
    if let Some(mut stdin) = child.take_stdin() {
        // Two 512-byte zero blocks = POSIX end-of-archive marker.
        let zero = [0u8; 1024];
        let _ = stdin.write_all(&zero);
        // Drop stdin → EOF.
    }
    let waited = child.wait_capture()?;
    Ok(waited.exit_code == 0)
}

/// Refuse paths that traverse outside the shared-storage root or contain
/// shell metacharacters we don't want to forward to the device.
pub fn is_safe_dest_path(p: &str) -> bool {
    if !p.starts_with('/') {
        return false;
    }
    if p.contains("..") || p.contains('\n') || p.contains('`') || p.contains('$') {
        return false;
    }
    // Whitelist common shared-storage roots (Phase 0 retro §1.4).
    p.starts_with("/sdcard/")
        || p == "/sdcard"
        || p.starts_with("/storage/emulated/0/")
        || p == "/storage/emulated/0"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_paths_accepted_by_allow_list() {
        for p in [
            "/sdcard/Download/foo",
            "/sdcard/DCIM",
            "/storage/emulated/0/Download/x",
        ] {
            assert!(is_safe_dest_path(p), "{p} should be safe");
        }
    }

    #[test]
    fn unsafe_paths_rejected() {
        for p in [
            "relative/path",
            "/sdcard/../etc",
            "/sdcard/`whoami`",
            "/sdcard/foo$bar",
            "/etc/passwd",
            "/data/local/tmp/x",
            "/sdcard/with\nnewline",
        ] {
            assert!(!is_safe_dest_path(p), "{p} should be rejected");
        }
    }

    #[test]
    fn cancel_handle_starts_idle_then_flips() {
        let h = CancelHandle::new();
        assert!(!h.is_cancelled());
        h.cancel();
        assert!(h.is_cancelled());
    }

    #[test]
    fn cancel_handle_records_dest_path() {
        let h = CancelHandle::new();
        assert_eq!(h.dest(), None);
        h.set_dest("/sdcard/Download/x");
        assert_eq!(h.dest().as_deref(), Some("/sdcard/Download/x"));
    }

    #[test]
    fn cancel_aware_sink_returns_interrupted_when_flagged() {
        let h = CancelHandle::new();
        let mut buf = Vec::new();
        let mut s = CancelAwareSink {
            inner: &mut buf,
            cancel: &h,
        };
        h.cancel();
        let err = s.write(b"hi").unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::Interrupted);
    }
}
