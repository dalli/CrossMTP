//! CrossMTP on-the-fly USTAR stream builder.
//!
//! Phase 2 of the ADB+tar plan ([docs/plan.md](../../docs/plan.md) §4.3).
//! Owns:
//!
//!   * lazy directory traversal with deterministic ordering
//!   * USTAR header generation (no temp file on disk)
//!   * macOS metadata hard-exclude (`._*`, `.DS_Store`, `.Spotlight-V100`,
//!     `.Trashes`, `.fseventsd`) — default deny, not a policy toggle
//!   * path traversal rejection (no `..`, no absolute paths)
//!   * conflict-policy aware entry skip / rename
//!   * progress counters: total bytes, total files, current path
//!
//! Intentionally **out of scope**: streaming over network/adb (that lives
//! in `adb-session`), conflict policy *decision making* (that lives in
//! `orchestrator` + adb-session manifest probe). This crate only consumes
//! a [`ConflictPlan`] and produces a tar byte stream.

pub mod conflict;
pub mod error;
pub mod exclude;
pub mod header;
pub mod path;
pub mod progress;
pub mod sanitize;
pub mod stream;
pub mod traversal;

pub use conflict::{ConflictAction, ConflictPlan, RenameRule};
pub use error::{Result, TarError};
pub use exclude::{is_macos_metadata, MACOS_METADATA_PATTERNS};
pub use path::TarPath;
pub use progress::{ProgressCounter, ProgressSnapshot};
pub use sanitize::{sanitize_rename_pattern, sanitize_timestamp, sanitize_tar_path};
pub use stream::TarStreamBuilder;
pub use traversal::{walk, Entry, EntryKind};
