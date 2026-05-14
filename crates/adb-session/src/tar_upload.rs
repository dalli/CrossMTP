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
use std::time::{Duration, Instant};

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

/// Byte-level progress callback fired roughly every 100ms while the tar
/// stream is writing. Receives a running snapshot of bytes emitted on
/// the wire. Phase 4 introduction (plan.md §8 Phase 4 + Phase 3 retro
/// §6-3): the orchestrator wraps this to emit `Event::Progress` so the
/// UI can show live byte counters during ADB uploads instead of jumping
/// from 0 → total at the end.
///
/// Owned `Box` (not `&mut dyn`) so the upload helper doesn't have to
/// thread an extra lifetime through every call site.
pub type ProgressCallback = Box<dyn FnMut(u64) + Send>;

/// Stream `source_root` (local file or directory) into `dest_path` on
/// `serial` using `adb shell tar -x -C <dest_path>`. Blocks the calling
/// thread until the device-side tar exits or `cancel` fires.
///
/// Thin wrapper around [`upload_tar_with_progress`] for callers that
/// don't need byte-level progress events.
pub fn upload_tar(
    session: &AdbSession,
    serial: &str,
    source_root: &Path,
    dest_path: &str,
    plan: ConflictPlan,
    cancel: CancelHandle,
) -> Result<UploadOutcome> {
    upload_tar_with_progress(session, serial, source_root, dest_path, plan, cancel, None)
}

/// Same as [`upload_tar`] but invokes `on_progress(bytes_written_so_far)`
/// at most once per 100ms while streaming. The callback runs on the same
/// thread as the writer, so it must not block — the orchestrator wraps
/// it as a non-blocking channel send.
pub fn upload_tar_with_progress(
    session: &AdbSession,
    serial: &str,
    source_root: &Path,
    dest_path: &str,
    plan: ConflictPlan,
    cancel: CancelHandle,
    on_progress: Option<ProgressCallback>,
) -> Result<UploadOutcome> {
    if !is_safe_dest_path(dest_path) {
        return Err(AdbError::CommandFailed {
            code: -1,
            stderr: format!("tar-upload rejected unsafe dest path: {dest_path}"),
        });
    }
    cancel.set_dest(dest_path);

    // `adb shell` rejoins argv with spaces and feeds it to the device's
    // /system/bin/sh -c "...", so any shell metachar in the dest path
    // (parentheses, spaces, &, ;, $) gets re-interpreted unless we
    // single-quote the whole thing. plan.md §4.2.
    let dest_quoted = sh_single_quote(dest_path);

    // `mkdir -p` the destination first. Cheap, idempotent.
    let mkdir_arg = format!("mkdir -p {}", dest_quoted);
    let mkdir = session.shell(serial, &[mkdir_arg.as_str()])?;
    if mkdir.exit_code != 0 {
        return Err(AdbError::CommandFailed {
            code: mkdir.exit_code,
            stderr: mkdir.stderr,
        });
    }

    let tar_arg = format!("tar -x -C {}", dest_quoted);
    let mut child: AdbProcess =
        session.spawn(serial, &["shell", tar_arg.as_str()], "tar-x")?;

    let mut stdin = child.take_stdin().ok_or_else(|| AdbError::CommandFailed {
        code: -1,
        stderr: "adb child did not expose stdin".into(),
    })?;

    let progress = build_and_stream(
        TarStreamBuilder::new(PathBuf::from(source_root)).with_conflict_plan(plan),
        &mut stdin,
        &cancel,
        on_progress,
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
    on_progress: Option<ProgressCallback>,
) -> std::result::Result<ProgressSnapshot, tar_stream::TarError> {
    let mut wrapped = CancelAwareSink {
        inner: sink,
        cancel,
        bytes_written: 0,
        on_progress,
        last_emit: Instant::now(),
    };
    let counter = builder.write_to(&mut wrapped)?;
    // Final emit so the UI sees the totals even when streaming finishes
    // inside the 100ms throttle window.
    if let Some(cb) = wrapped.on_progress.as_mut() {
        (cb)(wrapped.bytes_written);
    }
    Ok(counter.snapshot())
}

struct CancelAwareSink<'a, W: Write> {
    inner: &'a mut W,
    cancel: &'a CancelHandle,
    bytes_written: u64,
    on_progress: Option<ProgressCallback>,
    last_emit: Instant,
}

impl<'a, W: Write> Write for CancelAwareSink<'a, W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.cancel.is_cancelled() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "upload cancelled",
            ));
        }
        let n = self.inner.write(buf)?;
        self.bytes_written += n as u64;
        if let Some(cb) = self.on_progress.as_mut() {
            // Throttle to ~10 emits/sec. The IPC pipeline in the Tauri
            // layer already throttles further but the ADB write loop can
            // call us hundreds of times per second on a fast device, so
            // we cap here too.
            if self.last_emit.elapsed() >= Duration::from_millis(100) {
                (cb)(self.bytes_written);
                self.last_emit = Instant::now();
            }
        }
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// `pkill -f` the device-side tar. We match the exact arg form we used
/// at spawn so we don't kill other tars the user is running. Note: the
/// pattern we kill on is the *unquoted* form because that's what the
/// device's process table actually contains (sh -c expands the quotes
/// before exec).
pub fn best_effort_pkill(session: &AdbSession, serial: &str, dest_path: &str) -> Result<()> {
    // toybox pkill is available on the Phase 0 baseline. If it isn't,
    // the call returns CommandFailed which we swallow.
    let pattern = format!("tar -x -C {dest_path}");
    let cmd = format!("pkill -f {}", sh_single_quote(&pattern));
    let _ = session.shell(serial, &[cmd.as_str()])?;
    Ok(())
}

/// POSIX-safe single-quote wrap for an arbitrary string. Embedded `'`
/// becomes `'\''` (close, escape, reopen). Result can be passed
/// verbatim to `sh -c`.
fn sh_single_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for ch in s.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
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
    // Build a minimal valid tar archive in-memory: one empty regular
    // file (`.smoke`) followed by two zero blocks (POSIX end-of-archive).
    // toybox tar rejects an "all-zeros" stream as "Not tar", so we must
    // include at least one real header for the smoke check to be
    // honest about whether `tar -x -C` actually works on the device.
    let archive = build_tiny_smoke_archive();
    let mut child: AdbProcess = session.spawn(
        serial,
        &["shell", "tar", "-x", "-C", "/data/local/tmp"],
        "tar-x-smoke",
    )?;
    if let Some(mut stdin) = child.take_stdin() {
        let _ = stdin.write_all(&archive);
        // Drop stdin → EOF.
    }
    let waited = child.wait_capture()?;
    Ok(waited.exit_code == 0)
}

/// Returns bytes for a USTAR archive containing a single empty file
/// `.smoke`. 512-byte header + 1024 bytes of zero blocks = 1536 bytes.
fn build_tiny_smoke_archive() -> Vec<u8> {
    let mut header = [0u8; 512];
    let name = b".smoke";
    header[..name.len()].copy_from_slice(name);
    // mode "0000644 \0"
    header[100..108].copy_from_slice(b"0000644\0");
    // uid/gid "0000000 \0"
    header[108..116].copy_from_slice(b"0000000\0");
    header[116..124].copy_from_slice(b"0000000\0");
    // size = 0 → "00000000000\0"
    header[124..136].copy_from_slice(b"00000000000\0");
    // mtime = 0
    header[136..148].copy_from_slice(b"00000000000\0");
    // checksum field initialised to spaces while computing
    for b in &mut header[148..156] {
        *b = b' ';
    }
    // typeflag '0' = regular file
    header[156] = b'0';
    // ustar magic + version
    header[257..263].copy_from_slice(b"ustar\0");
    header[263..265].copy_from_slice(b"00");
    // checksum: sum of all unsigned bytes, written as 6-digit octal +
    // NUL + space.
    let sum: u32 = header.iter().map(|b| *b as u32).sum();
    let cs = format!("{:06o}\0 ", sum);
    header[148..156].copy_from_slice(cs.as_bytes());

    let mut out = Vec::with_capacity(1536);
    out.extend_from_slice(&header);
    out.extend_from_slice(&[0u8; 1024]); // EOF marker
    out
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
            bytes_written: 0,
            on_progress: None,
            last_emit: Instant::now(),
        };
        h.cancel();
        let err = s.write(b"hi").unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::Interrupted);
    }

    #[test]
    fn cancel_aware_sink_tracks_bytes_written() {
        let h = CancelHandle::new();
        let mut buf = Vec::new();
        let mut s = CancelAwareSink {
            inner: &mut buf,
            cancel: &h,
            bytes_written: 0,
            on_progress: None,
            last_emit: Instant::now(),
        };
        s.write_all(b"hello").unwrap();
        s.write_all(b" world").unwrap();
        assert_eq!(s.bytes_written, 11);
    }
}
