//! Per-platform / per-backend capability advertisement.
//!
//! AGENTS.md and the dev plan both say: do not pretend every platform can
//! do everything. Instead, the Session Layer publishes a `Capabilities`
//! struct and the orchestrator + UI feature-detect against it. New
//! capabilities are added as Phase 1+ scope grows.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities {
    pub can_list: bool,
    pub can_download: bool,
    pub can_upload: bool,
    pub can_rename: bool,
    pub can_delete: bool,
    pub can_create_folder: bool,
    pub supports_progress_callback: bool,
    pub supports_cancel: bool,
    pub supports_background_reconnect: bool,
}

impl Capabilities {
    /// What the macOS libmtp backend honestly supports today.
    /// Phase 1 deliberately keeps the modify-flags `false` — the MVP plan
    /// excludes rename/delete/create.
    pub const fn macos_libmtp_default() -> Self {
        Self {
            can_list: true,
            can_download: true,
            can_upload: true,
            can_rename: false,
            can_delete: false,
            // Phase 5 확장: 재귀 폴더 업로드를 위해 내부적으로 사용.
            // 사용자용 "새 폴더" UI는 여전히 MVP 외.
            can_create_folder: true,
            // Phase 2: progress and cancel are now wired through
            // `Device::{download,upload}_file_with_progress`.
            supports_progress_callback: true,
            supports_cancel: true,
            supports_background_reconnect: false,
        }
    }
}
