//! ADB-side capability advertisement.
//!
//! Mirrors the MTP `Capabilities` philosophy from `mtp-session`:
//! advertise only what the layer can honestly do today. plan.md §4.1
//! defines the capability names the UI feature-detects against.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdbCapabilities {
    /// `adbAvailabilityProbe` — the layer can answer whether adb itself
    /// is installed and reachable. Always true once the layer is
    /// compiled in; kept as a flag so the UI can mark it explicitly.
    pub adb_availability_probe: bool,

    /// `adbTarUpload` — Phase 1 does NOT implement the streaming tar
    /// path. Stays `false` until Phase 2 lands the Tar Stream Builder.
    pub adb_tar_upload: bool,

    /// Can issue read-only `adb shell` commands (used by Phase 1
    /// capability probes and by Phase 2 manifest probe).
    pub can_run_shell: bool,

    /// Can spawn and track long-running adb child processes (the
    /// `AdbProcess` lifecycle). Required for Phase 2 cancellation
    /// design from §6.1.
    pub can_track_child_processes: bool,
}

impl AdbCapabilities {
    /// What the Phase 1 ADB Session Layer honestly supports.
    pub const fn phase1_default() -> Self {
        Self {
            adb_availability_probe: true,
            adb_tar_upload: false,
            can_run_shell: true,
            can_track_child_processes: true,
        }
    }
}
