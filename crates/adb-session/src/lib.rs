//! CrossMTP ADB Session Layer.
//!
//! Phase 1 of the ADB+tar plan ([docs/plan.md](../../docs/plan.md) §8).
//! Owns:
//!
//!   * `adb` executable discovery (CROSSMTP_ADB → SDK → PATH → brew)
//!   * `adb devices -l` parsing
//!   * device state normalisation (`unauthorized` / `offline` /
//!     `no permissions` / `device not found` → typed errors)
//!   * shell command execution
//!   * long-running child process lifecycle (`AdbProcess`)
//!   * capability advertisement (`AdbCapabilities`)
//!
//! Intentionally **not** in scope for Phase 1: tar streaming, manifest
//! probing, transfer state machine. Those land in Phase 2 / 3.

pub mod capability;
pub mod devices;
pub mod discovery;
pub mod error;
pub mod process;
pub mod session;

pub use capability::AdbCapabilities;
pub use devices::{AdbDevice, DeviceState};
pub use discovery::{discover_adb, AdbLocation, AdbSource};
pub use error::{AdbError, Result};
pub use process::{AdbOutput, AdbProcess, AdbRunner, CommandRunner};
pub use session::AdbSession;
