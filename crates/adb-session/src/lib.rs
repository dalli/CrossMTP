//! CrossMTP ADB Session Layer.
//!
//! Phase 1+2 of the ADB+tar plan ([docs/plan.md](../../docs/plan.md) §8).
//! Owns:
//!
//!   * `adb` executable discovery (CROSSMTP_ADB → SDK → PATH → brew)
//!   * `adb devices -l` parsing
//!   * device state normalisation (`unauthorized` / `offline` /
//!     `no permissions` / `device not found` → typed errors)
//!   * shell command execution
//!   * long-running child process lifecycle (`AdbProcess`)
//!   * capability advertisement (`AdbCapabilities`)
//!   * **per-device** capability probe (`DeviceCapabilities`) — Phase 2
//!   * manifest probe (`manifest::probe`) — Phase 2 §5
//!   * end-to-end tar upload integration (`tar_upload::upload_tar`) —
//!     Phase 2 §6.1 cancel sequence included
//!
//! Intentionally **not** in scope yet: orchestrator integration (Phase 3),
//! UI opt-in (Phase 4), real-device throughput measurement (Phase 5).

pub mod capability;
pub mod device_caps;
pub mod devices;
pub mod discovery;
pub mod error;
pub mod manifest;
pub mod process;
pub mod session;
pub mod tar_upload;

pub use capability::AdbCapabilities;
pub use device_caps::{probe_device, DeviceCapabilities};
pub use devices::{AdbDevice, DeviceState};
pub use discovery::{discover_adb, AdbLocation, AdbSource};
pub use error::{AdbError, Result};
pub use manifest::{
    build_probe_command, is_same_file, parse_manifest_output, probe as probe_manifest,
    DeviceManifest, ManifestEntry,
};
pub use process::{AdbOutput, AdbProcess, AdbRunner, CommandRunner};
pub use session::AdbSession;
pub use tar_upload::{is_safe_dest_path, upload_tar, CancelHandle, UploadOutcome};
