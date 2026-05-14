//! Conflict planning consumed by the tar stream builder.
//!
//! plan.md §5: this crate does NOT decide policies — that's the
//! orchestrator's job working off the manifest probe in `adb-session`.
//! What we accept is a per-entry [`ConflictAction`] keyed by source path
//! (the local relative path) telling us:
//!
//!   * `Skip`      — drop this entry from the stream entirely
//!   * `Rename(s)` — emit it under a new last-segment name `s`
//!   * `Overwrite` — emit as-is; device-side tar handles the overwrite
//!   * `Emit`      — same as Overwrite but used when no conflict exists,
//!     kept distinct so audit logs can differentiate them
//!
//! The builder treats unknown entries as `Emit` (no conflict found).

use std::collections::HashMap;

use crate::error::{Result, TarError};
use crate::path::TarPath;
use crate::sanitize::{sanitize_rename_pattern, sanitize_tar_path};

/// What the stream builder should do with a particular entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConflictAction {
    /// Default: there was no conflict, emit unchanged.
    Emit,
    /// Drop from the stream.
    Skip,
    /// Emit but with the **last segment** rewritten to this name.
    /// Parent components are unchanged.
    Rename(String),
    /// Emit unchanged; device tar will replace the existing file.
    Overwrite,
}

/// Plan keyed by local relative path (the same string the builder uses
/// as the tar entry name before any rename). Used by the stream builder
/// to look up a per-entry action in O(1).
#[derive(Debug, Clone, Default)]
pub struct ConflictPlan {
    map: HashMap<String, ConflictAction>,
}

impl ConflictPlan {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, path: &TarPath, action: ConflictAction) {
        self.map.insert(path.as_str(), action);
    }

    pub fn action_for(&self, path: &TarPath) -> &ConflictAction {
        self.map.get(&path.as_str()).unwrap_or(&ConflictAction::Emit)
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

/// Rename rule the orchestrator can apply to compute the new last-segment
/// name when materialising a `Rename` action. Kept here (not in
/// orchestrator) so the sanitisation guarantees stay close to the tar
/// output path validation.
#[derive(Debug, Clone)]
pub struct RenameRule {
    pattern: String,
}

impl RenameRule {
    /// Build from a user pattern. Sanitisation runs once at construction;
    /// callers can build a rule per upload queue and reuse it.
    pub fn new(pattern: &str) -> std::result::Result<Self, String> {
        let cleaned = sanitize_rename_pattern(pattern)?;
        Ok(Self { pattern: cleaned })
    }

    /// Default `{name} ({n}){ext}` rule (plan.md §5 initial default).
    pub fn default_paren_n() -> Self {
        Self {
            pattern: "{name} ({n}){ext}".into(),
        }
    }

    pub fn pattern(&self) -> &str {
        &self.pattern
    }

    /// Render a new filename for a given source path + numbering + optional
    /// timestamp. The returned string is sanitised and safe to drop into
    /// `ConflictAction::Rename(_)`.
    pub fn render(&self, source: &TarPath, n: u32, timestamp_secs: i64) -> Result<String> {
        let (stem, ext) = source.split_stem_ext();
        let ts = crate::sanitize::sanitize_timestamp(timestamp_secs);
        let mut out = self.pattern.clone();
        out = out.replace("{name}", &stem);
        out = out.replace("{ext}", &ext);
        out = out.replace("{n}", &n.to_string());
        out = out.replace("{timestamp}", &ts);
        let cleaned = sanitize_tar_path(&out);
        if cleaned.is_empty() {
            return Err(TarError::InvalidEntryName {
                reason: "rendered rename was empty".into(),
            });
        }
        Ok(cleaned)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tp(s: &str) -> TarPath {
        TarPath::new(s).unwrap()
    }

    #[test]
    fn empty_plan_returns_emit_for_any_path() {
        let plan = ConflictPlan::new();
        assert_eq!(plan.action_for(&tp("a/b.txt")), &ConflictAction::Emit);
    }

    #[test]
    fn lookup_matches_exact_path_string() {
        let mut plan = ConflictPlan::new();
        plan.insert(&tp("a/b.txt"), ConflictAction::Skip);
        plan.insert(&tp("a/c.txt"), ConflictAction::Overwrite);
        assert_eq!(plan.action_for(&tp("a/b.txt")), &ConflictAction::Skip);
        assert_eq!(plan.action_for(&tp("a/c.txt")), &ConflictAction::Overwrite);
        assert_eq!(plan.action_for(&tp("a/d.txt")), &ConflictAction::Emit);
    }

    #[test]
    fn rename_rule_default_renders_paren_n() {
        let rule = RenameRule::default_paren_n();
        let new = rule.render(&tp("photo.tar.gz"), 1, 0).unwrap();
        assert_eq!(new, "photo.tar (1).gz");
    }

    #[test]
    fn rename_rule_timestamp_is_filesystem_safe() {
        let rule = RenameRule::new("{name} - {timestamp}{ext}").unwrap();
        let new = rule.render(&tp("a.txt"), 0, 1_778_716_800).unwrap();
        assert!(!new.contains(':'));
        assert!(new.contains("20260514-000000"));
        assert!(new.ends_with(".txt"));
    }

    #[test]
    fn rename_rule_handles_dotfile_no_ext() {
        let rule = RenameRule::default_paren_n();
        let new = rule.render(&tp(".gitignore"), 2, 0).unwrap();
        // stem is `.gitignore`, ext is empty → `.gitignore (2)`
        assert_eq!(new, ".gitignore (2)");
    }

    #[test]
    fn rename_rule_rejects_unknown_var_at_construction() {
        assert!(RenameRule::new("{name}-{user}").is_err());
    }
}
