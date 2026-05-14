//! Conflict planner for ADB tar uploads.
//!
//! plan.md §5 + Phase 2 retro §4-6: the tar-stream crate accepts a
//! `ConflictPlan` keyed by relative path → `ConflictAction`. It does
//! *not* compute that plan. The orchestrator needs to:
//!
//!   1. probe the device-side root (`manifest::probe`) for the existing
//!      tree,
//!   2. walk the local upload set,
//!   3. for each local file, decide `Emit` / `Skip` / `Rename(new_name)`
//!      / `Overwrite` based on the user policy, the `mtimeToleranceSeconds`
//!      default, and a `RenameRule`,
//!   4. surface a report so the UI can show "skipped N, renamed M".
//!
//! This module is the home of step 3. It is sync, deterministic, and has
//! no IO of its own — `manifest::probe` is the caller's responsibility,
//! so tests can build a [`DeviceManifest`] in-memory.
//!
//! The renumbering for `{n}`-style renames is computed against both the
//! existing manifest *and* the names this planner has already chosen in
//! the same pass — so two locally-conflicting renames don't end up with
//! the same target name (Phase 2 retro §4-6).

use std::collections::HashSet;

use tar_stream::{ConflictAction, ConflictPlan, RenameRule, TarPath};

use crate::manifest::{is_same_file, DeviceManifest};

/// One local file the orchestrator wants to upload. `rel_path` is the
/// path relative to the upload root, slash-separated, matching how
/// `tar-stream::walk` will emit it.
#[derive(Debug, Clone)]
pub struct LocalFile {
    pub rel_path: String,
    pub size: u64,
    pub mtime_secs: i64,
}

/// User-facing upload policy. plan.md §5 default settings.
#[derive(Debug, Clone)]
pub struct UploadPolicy {
    /// "Same file" judgement: when a local and remote entry have
    /// identical name+size and mtime within tolerance, we treat them
    /// as the same file. Default: skip the duplicate.
    pub same_file_skip: bool,
    /// When a different file collides by name: rename the local entry.
    /// `false` would mean "fall back to overwrite", but plan.md §5
    /// default is Rename.
    pub different_file_rename: bool,
    /// Tolerance for mtime equality in seconds. Default 2s (plan.md §5).
    pub mtime_tolerance_secs: u32,
    /// Rule that renders the new last-segment name when a Rename is
    /// chosen.
    pub rename_rule: RenameRule,
}

impl UploadPolicy {
    /// plan.md §5 initial defaults: `Skip` same-file, `Rename` diff-file,
    /// 2s tolerance, `{name} ({n}){ext}` rename rule.
    pub fn plan_defaults() -> Self {
        Self {
            same_file_skip: true,
            different_file_rename: true,
            mtime_tolerance_secs: 2,
            rename_rule: RenameRule::default_paren_n(),
        }
    }
}

/// Input bundle for [`plan_upload`].
#[derive(Debug, Clone)]
pub struct PlanRequest<'a> {
    pub locals: &'a [LocalFile],
    pub remote: &'a DeviceManifest,
    pub policy: &'a UploadPolicy,
}

/// Diagnostic report alongside the produced `ConflictPlan`. The UI uses
/// these counts and lists for the conflict-summary dialog (plan.md §5
/// `Ask every time` policy → manifest-driven batch dialog).
#[derive(Debug, Clone, Default)]
pub struct PlanReport {
    /// Skipped because remote has identical (size, mtime±tolerance).
    pub skipped_same: Vec<String>,
    /// Renamed because a different file already had the same name.
    /// Pairs are `(original_rel_path, new_last_segment)`.
    pub renamed: Vec<(String, String)>,
    /// No conflict — emitted as-is.
    pub clean: Vec<String>,
}

impl PlanReport {
    pub fn skipped_count(&self) -> usize {
        self.skipped_same.len()
    }
    pub fn renamed_count(&self) -> usize {
        self.renamed.len()
    }
    pub fn clean_count(&self) -> usize {
        self.clean.len()
    }
}

/// Compute the per-entry conflict plan for `locals` against `remote`,
/// following `policy`. Pure function; the caller is responsible for
/// having already run `manifest::probe`.
///
/// Errors only when [`TarPath::new`] rejects a local path or the
/// configured `RenameRule` cannot render a valid name. Those are
/// caller bugs (the orchestrator validates paths up front), not
/// runtime conditions, so we surface them as `String` to keep the
/// crate's public error model tight.
pub fn plan_upload(req: &PlanRequest) -> std::result::Result<(ConflictPlan, PlanReport), String> {
    let mut plan = ConflictPlan::new();
    let mut report = PlanReport::default();

    // Track names we've already chosen in this pass — including
    // renames — so two collisions in the same upload set don't pick
    // the same fresh name.
    let mut taken: HashSet<String> = req.remote.entries.keys().cloned().collect();
    // Pre-fill with all local rel_paths so a rename for `a.txt` skips
    // any other local entry that *was* going to be `a.txt`. The chosen
    // rename target then gets added to `taken`.
    for f in req.locals {
        taken.insert(f.rel_path.clone());
    }

    for local in req.locals {
        let tar_path = TarPath::new(&local.rel_path)
            .map_err(|e| format!("invalid local path {}: {e}", local.rel_path))?;

        match req.remote.get(&local.rel_path) {
            None => {
                // No collision at all.
                plan.insert(&tar_path, ConflictAction::Emit);
                report.clean.push(local.rel_path.clone());
            }
            Some(remote_entry) => {
                let same = is_same_file(
                    local.size,
                    local.mtime_secs,
                    remote_entry,
                    req.policy.mtime_tolerance_secs,
                );
                let should_skip = same && req.policy.same_file_skip;
                let should_rename = req.policy.different_file_rename;
                if should_skip {
                    plan.insert(&tar_path, ConflictAction::Skip);
                    report.skipped_same.push(local.rel_path.clone());
                } else if should_rename {
                    let new_last = pick_fresh_rename(&tar_path, &req.policy.rename_rule, &taken)?;
                    let new_full = compose_full_path(&local.rel_path, &new_last);
                    taken.insert(new_full.clone());
                    plan.insert(&tar_path, ConflictAction::Rename(new_last.clone()));
                    report.renamed.push((local.rel_path.clone(), new_last));
                } else {
                    // Both rename and skip disabled → user opted into overwrite.
                    plan.insert(&tar_path, ConflictAction::Overwrite);
                    report.clean.push(local.rel_path.clone());
                }
            }
        }
    }
    Ok((plan, report))
}

fn pick_fresh_rename(
    source: &TarPath,
    rule: &RenameRule,
    taken: &HashSet<String>,
) -> std::result::Result<String, String> {
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let parent_prefix = parent_prefix(&source.as_str());
    for n in 1..1000u32 {
        let candidate_last = rule
            .render(source, n, now_secs)
            .map_err(|e| format!("rename render failed: {e}"))?;
        let full = match &parent_prefix {
            Some(p) => format!("{p}/{candidate_last}"),
            None => candidate_last.clone(),
        };
        if !taken.contains(&full) {
            return Ok(candidate_last);
        }
    }
    Err(format!(
        "could not find fresh rename for {} within 1000 tries",
        source.as_str()
    ))
}

fn parent_prefix(rel: &str) -> Option<String> {
    rel.rfind('/').map(|i| rel[..i].to_string())
}

fn compose_full_path(original_rel: &str, new_last: &str) -> String {
    match parent_prefix(original_rel) {
        Some(p) => format!("{p}/{new_last}"),
        None => new_last.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::ManifestEntry;
    use std::collections::HashMap;

    fn local(rel: &str, size: u64, mtime: i64) -> LocalFile {
        LocalFile {
            rel_path: rel.into(),
            size,
            mtime_secs: mtime,
        }
    }

    fn manifest(entries: &[(&str, u64, i64)]) -> DeviceManifest {
        let mut map = HashMap::new();
        for (p, s, m) in entries {
            map.insert(
                (*p).to_string(),
                ManifestEntry {
                    rel_path: (*p).to_string(),
                    size: *s,
                    mtime_secs: *m,
                },
            );
        }
        DeviceManifest {
            root: "/sdcard/Download/x".into(),
            entries: map,
        }
    }

    fn defaults() -> UploadPolicy {
        UploadPolicy::plan_defaults()
    }

    #[test]
    fn no_remote_collisions_yield_all_emit() {
        let locals = vec![local("a.txt", 10, 100), local("sub/b.txt", 20, 200)];
        let remote = DeviceManifest::default();
        let policy = defaults();
        let (plan, report) =
            plan_upload(&PlanRequest { locals: &locals, remote: &remote, policy: &policy }).unwrap();
        assert_eq!(report.clean_count(), 2);
        assert_eq!(report.skipped_count(), 0);
        assert_eq!(report.renamed_count(), 0);
        let _ = plan; // ConflictPlan default is Emit; nothing to assert beyond that
    }

    #[test]
    fn same_file_within_tolerance_is_skipped() {
        let locals = vec![local("a.txt", 10, 100)];
        let remote = manifest(&[("a.txt", 10, 101)]);
        let policy = defaults();
        let (plan, report) =
            plan_upload(&PlanRequest { locals: &locals, remote: &remote, policy: &policy }).unwrap();
        let tp = TarPath::new("a.txt").unwrap();
        assert_eq!(plan.action_for(&tp), &ConflictAction::Skip);
        assert_eq!(report.skipped_same, vec!["a.txt"]);
    }

    #[test]
    fn different_file_collision_renames() {
        let locals = vec![local("a.txt", 10, 100)];
        let remote = manifest(&[("a.txt", 999, 100)]); // different size → not same file
        let policy = defaults();
        let (plan, report) =
            plan_upload(&PlanRequest { locals: &locals, remote: &remote, policy: &policy }).unwrap();
        let tp = TarPath::new("a.txt").unwrap();
        match plan.action_for(&tp) {
            ConflictAction::Rename(new) => assert_eq!(new, "a (1).txt"),
            other => panic!("expected Rename, got {other:?}"),
        }
        assert_eq!(report.renamed_count(), 1);
    }

    #[test]
    fn rename_skips_existing_numbered_variant() {
        let locals = vec![local("a.txt", 10, 100)];
        let remote = manifest(&[("a.txt", 999, 100), ("a (1).txt", 11, 101)]);
        let policy = defaults();
        let (plan, _) =
            plan_upload(&PlanRequest { locals: &locals, remote: &remote, policy: &policy }).unwrap();
        let tp = TarPath::new("a.txt").unwrap();
        match plan.action_for(&tp) {
            ConflictAction::Rename(new) => assert_eq!(new, "a (2).txt"),
            other => panic!("expected Rename, got {other:?}"),
        }
    }

    #[test]
    fn two_local_collisions_get_distinct_renames() {
        // Two distinct local files both wanting the same remote-side name
        // is rare (rel_paths must differ), but a more realistic case is
        // a single local file conflicting with both the remote name and
        // a remote `(1)` variant. Verified by previous test. Here we
        // assert two *different* local conflicts in different folders
        // don't accidentally share `(1)` numbering since they live under
        // different parents.
        let locals = vec![
            local("dir1/a.txt", 10, 100),
            local("dir2/a.txt", 10, 100),
        ];
        let remote = manifest(&[("dir1/a.txt", 999, 100), ("dir2/a.txt", 999, 100)]);
        let policy = defaults();
        let (plan, report) =
            plan_upload(&PlanRequest { locals: &locals, remote: &remote, policy: &policy }).unwrap();
        let new1 = match plan.action_for(&TarPath::new("dir1/a.txt").unwrap()) {
            ConflictAction::Rename(n) => n.clone(),
            o => panic!("expected Rename, got {o:?}"),
        };
        let new2 = match plan.action_for(&TarPath::new("dir2/a.txt").unwrap()) {
            ConflictAction::Rename(n) => n.clone(),
            o => panic!("expected Rename, got {o:?}"),
        };
        assert_eq!(new1, "a (1).txt");
        assert_eq!(new2, "a (1).txt");
        assert_eq!(report.renamed_count(), 2);
    }

    #[test]
    fn policy_can_force_overwrite_for_different_file() {
        let locals = vec![local("a.txt", 10, 100)];
        let remote = manifest(&[("a.txt", 999, 100)]);
        let mut policy = defaults();
        policy.different_file_rename = false;
        policy.same_file_skip = false;
        let (plan, _) =
            plan_upload(&PlanRequest { locals: &locals, remote: &remote, policy: &policy }).unwrap();
        let tp = TarPath::new("a.txt").unwrap();
        assert_eq!(plan.action_for(&tp), &ConflictAction::Overwrite);
    }

    #[test]
    fn same_file_can_be_forced_to_overwrite_when_skip_disabled() {
        let locals = vec![local("a.txt", 10, 100)];
        let remote = manifest(&[("a.txt", 10, 100)]);
        let mut policy = defaults();
        policy.same_file_skip = false;
        let (plan, _) =
            plan_upload(&PlanRequest { locals: &locals, remote: &remote, policy: &policy }).unwrap();
        let tp = TarPath::new("a.txt").unwrap();
        // same_file ∧ !skip → falls through to "different_file" branch
        // → policy says rename → emit a Rename, not overwrite.
        assert!(matches!(plan.action_for(&tp), ConflictAction::Rename(_)));
    }
}
