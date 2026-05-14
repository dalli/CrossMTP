//! Per-device capability probe (Phase 2 inherits from Phase 1 §4).
//!
//! Phase 1 advertised layer-level capabilities (`AdbCapabilities`). For
//! conflict planning and storage-matrix decisions we now need to know
//! **per-device** facts: does `tar` exist? does `find` exist? is the
//! chosen destination writable?
//!
//! Each fact is a single one-shot `adb shell` invocation, so the probe
//! is cheap. We do NOT cache: callers cache on the orchestrator side
//! after the first successful upload setup.

use crate::error::Result;
use crate::session::AdbSession;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceCapabilities {
    pub has_tar: bool,
    pub has_find: bool,
    pub has_stat: bool,
    /// Set when the device's `tar` is toybox (the Phase 0 baseline).
    /// Other implementations (BusyBox, vendor BSD) work but may not
    /// support `--xform`; for now we just record the impl tag.
    pub tar_impl: Option<String>,
    /// True once we've confirmed `tar -x -C` exits cleanly with an
    /// empty stdin. Required before promoting `adb_tar_upload` to true.
    pub tar_extract_smoke_ok: bool,
}

impl DeviceCapabilities {
    /// True iff the device meets the minimum bar for the ADB+tar fast
    /// path: tar (any impl), find + stat for manifest probe, and tar -x
    /// passed the smoke check.
    pub fn can_tar_upload(&self) -> bool {
        self.has_tar && self.has_find && self.has_stat && self.tar_extract_smoke_ok
    }
}

/// Run the four shell checks. Each is silent on success; any failure
/// downgrades the capability flag rather than aborting the probe.
pub fn probe_device(session: &AdbSession, serial: &str) -> Result<DeviceCapabilities> {
    let mut caps = DeviceCapabilities {
        has_tar: false,
        has_find: false,
        has_stat: false,
        tar_impl: None,
        tar_extract_smoke_ok: false,
    };

    // 1. which tar (toybox uses `which`; `command -v` is the portable
    //    fallback. Try the cheapest first.)
    let which_tar = session.shell(serial, &["which", "tar"])?;
    caps.has_tar = which_tar.exit_code == 0 && !which_tar.stdout.trim().is_empty();

    // 2. which find
    let which_find = session.shell(serial, &["which", "find"])?;
    caps.has_find = which_find.exit_code == 0 && !which_find.stdout.trim().is_empty();

    // 3. which stat
    let which_stat = session.shell(serial, &["which", "stat"])?;
    caps.has_stat = which_stat.exit_code == 0 && !which_stat.stdout.trim().is_empty();

    // 4. tar impl tag — toybox identifies itself by writing
    //    "tar - .." to stdout for `tar --help` and "toybox" in `toybox --version`.
    if caps.has_tar {
        let tb = session.shell(serial, &["toybox", "--version"])?;
        if tb.exit_code == 0 && tb.stdout.to_ascii_lowercase().contains("toybox") {
            caps.tar_impl = Some(format!("toybox {}", tb.stdout.trim()));
        } else {
            // Fall back to a tag we can show in the UI without lying.
            caps.tar_impl = Some("unknown".into());
        }
    }

    // 5. tar -x smoke: feed an end-of-archive marker (two zero blocks) on
    //    stdin and have tar extract into /tmp. If that works the device's
    //    tar at least accepts our binary path.
    //
    //    We don't run this here because it requires `spawn_piped`. The
    //    orchestrator does it once at session bring-up via
    //    `tar_upload::smoke_check_extract`.
    Ok(caps)
}

/// Classify a `which` failure mode for diagnostics. Mostly useful in
/// tests that simulate a stripped-down device.
pub fn classify_which_failure(stderr: &str) -> &'static str {
    let s = stderr.to_ascii_lowercase();
    if s.contains("not found") {
        "binary-missing"
    } else if s.contains("permission") {
        "permission"
    } else {
        "unknown"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::{AdbLocation, AdbSource};
    use crate::process::{AdbOutput, AdbRunner};
    use crate::process::StubRunner;
    use std::path::PathBuf;
    use std::sync::Arc;

    fn ok(out: &str) -> AdbOutput {
        AdbOutput {
            exit_code: 0,
            stdout: out.into(),
            stderr: String::new(),
        }
    }
    fn fail() -> AdbOutput {
        AdbOutput {
            exit_code: 1,
            stdout: String::new(),
            stderr: "not found".into(),
        }
    }
    fn loc() -> AdbLocation {
        AdbLocation {
            path: PathBuf::from("/usr/local/bin/adb"),
            source: AdbSource::Path,
        }
    }

    #[test]
    fn full_toybox_device_reports_all_caps() {
        let stub: Arc<dyn AdbRunner> = Arc::new(StubRunner::new(vec![
            Ok(ok("/system/bin/tar\n")),
            Ok(ok("/system/bin/find\n")),
            Ok(ok("/system/bin/stat\n")),
            Ok(ok("toybox 0.8.11\n")),
        ]));
        let session = AdbSession::with_runner(loc(), stub);
        let caps = probe_device(&session, "SER").unwrap();
        assert!(caps.has_tar);
        assert!(caps.has_find);
        assert!(caps.has_stat);
        assert!(caps.tar_impl.as_deref().unwrap().contains("toybox"));
        // Without smoke check the full fast-path bar is not met.
        assert!(!caps.can_tar_upload());
    }

    #[test]
    fn missing_tar_disables_fast_path() {
        let stub: Arc<dyn AdbRunner> = Arc::new(StubRunner::new(vec![
            Ok(fail()),
            Ok(ok("/system/bin/find\n")),
            Ok(ok("/system/bin/stat\n")),
        ]));
        let session = AdbSession::with_runner(loc(), stub);
        let caps = probe_device(&session, "SER").unwrap();
        assert!(!caps.has_tar);
        assert!(caps.tar_impl.is_none());
        assert!(!caps.can_tar_upload());
    }

    #[test]
    fn non_toybox_tar_reports_unknown_impl() {
        let stub: Arc<dyn AdbRunner> = Arc::new(StubRunner::new(vec![
            Ok(ok("/usr/bin/tar\n")),
            Ok(ok("/usr/bin/find\n")),
            Ok(ok("/usr/bin/stat\n")),
            Ok(fail()),
        ]));
        let session = AdbSession::with_runner(loc(), stub);
        let caps = probe_device(&session, "SER").unwrap();
        assert_eq!(caps.tar_impl.as_deref(), Some("unknown"));
    }

    #[test]
    fn classify_which_distinguishes_missing_from_permission() {
        assert_eq!(classify_which_failure("tar: not found"), "binary-missing");
        assert_eq!(
            classify_which_failure("permission denied"),
            "permission"
        );
        assert_eq!(classify_which_failure("???"), "unknown");
    }
}
