//! Normalised error model for the ADB Session Layer.
//!
//! plan.md §4.2 lists the cases that must be distinguishable:
//! `unauthorized`, `offline`, `no permissions`, `device not found`.
//! We also separate "adb binary itself was not found" because that has
//! a different UI affordance (link to platform-tools install guide,
//! not a device-state hint).

use std::io;
use thiserror::Error;

pub type Result<T> = std::result::Result<T, AdbError>;

#[derive(Debug, Error)]
pub enum AdbError {
    /// `adb` executable was not discovered via any of the configured
    /// candidates. UI should show the platform-tools install link.
    #[error("adb executable not found (checked CROSSMTP_ADB, ~/Library/Android/sdk/platform-tools/adb, PATH, /opt/homebrew/bin/adb)")]
    AdbNotAvailable,

    /// `adb devices -l` returned no entries at all.
    #[error("no Android device connected")]
    NoDevice,

    /// Device is listed but in `unauthorized` state — the user has not
    /// yet accepted the RSA fingerprint prompt on the phone.
    #[error("device {serial} is unauthorized — accept the USB debugging prompt on the phone")]
    Unauthorized { serial: String },

    /// Device is listed but in `offline` state.
    #[error("device {serial} is offline")]
    Offline { serial: String },

    /// `no permissions` line from adb — usually a udev/permissions issue
    /// on Linux, but we keep the variant for parity since plan.md §4.2
    /// requires it as a distinct error.
    #[error("no permission to access device {serial}")]
    NoPermissions { serial: String },

    /// User asked for a specific serial but it wasn't in the device list.
    #[error("device {serial} not found in adb devices list")]
    DeviceNotFound { serial: String },

    /// `adb` ran but exited non-zero. `stderr` is the raw text from the
    /// child so UI can echo it for diagnostics, but callers should
    /// prefer the more specific variants above when classification is
    /// possible.
    #[error("adb command failed (exit {code}): {stderr}")]
    CommandFailed { code: i32, stderr: String },

    /// The child process was terminated by a signal or otherwise did
    /// not produce an exit code (e.g. SIGTERM during cancellation).
    #[error("adb command terminated without exit code")]
    CommandTerminated,

    /// Could not parse the output of `adb devices -l`. The raw text is
    /// kept so the user can copy it into a bug report.
    #[error("failed to parse adb devices output: {0}")]
    ParseError(String),

    /// IO error spawning or talking to the adb child process.
    #[error("io error: {0}")]
    Io(#[from] io::Error),
}

impl AdbError {
    /// True when the device-side state is the cause and the UI should
    /// prompt the user to do something on the phone (accept dialog,
    /// unlock, replug). Mirrors `MtpError::is_likely_permission_issue`
    /// from the MTP layer so the UI can use one code path.
    pub fn is_likely_user_action_required(&self) -> bool {
        matches!(
            self,
            AdbError::Unauthorized { .. }
                | AdbError::Offline { .. }
                | AdbError::NoPermissions { .. }
        )
    }

    /// True when retrying the command on the same device handle has no
    /// chance of succeeding without intervention (cable replug, install
    /// platform-tools, etc.). Used by capability probes to avoid loops.
    pub fn is_fatal_for_session(&self) -> bool {
        matches!(
            self,
            AdbError::AdbNotAvailable | AdbError::NoDevice | AdbError::DeviceNotFound { .. }
        )
    }
}
