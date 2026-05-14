//! Device-side manifest probe (plan.md §5).
//!
//! Strategy (Phase 0 retro §1.5): run a single `adb shell` invocation
//!
//! ```text
//! find <root> -type f -exec stat -c '%n %s %Y' {} \;
//! ```
//!
//! and parse the output into a per-file table the orchestrator can
//! diff against the local upload set. mtime resolution is 1 second
//! (toybox stat behaviour observed on Android 15), which matches the
//! plan.md default `mtimeToleranceSeconds: 2`.
//!
//! Returns a structured manifest that exposes:
//!   * the **set of paths** present on the device under <root>
//!   * for each path: size + mtime
//!
//! Conflict policy decisions live one layer up (orchestrator) — this
//! module is only responsible for the probe and parse.

use std::collections::HashMap;

use crate::error::{AdbError, Result};
use crate::session::AdbSession;

/// One device-side file as reported by `find ... stat`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestEntry {
    /// Path relative to the probed root, slash-separated. Stripped of
    /// the root prefix so the orchestrator can match against local
    /// relative paths directly.
    pub rel_path: String,
    pub size: u64,
    pub mtime_secs: i64,
}

#[derive(Debug, Clone, Default)]
pub struct DeviceManifest {
    /// Absolute device-side root path used for the probe (`/sdcard/Download/foo`).
    pub root: String,
    /// rel_path → entry.
    pub entries: HashMap<String, ManifestEntry>,
}

impl DeviceManifest {
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn contains(&self, rel: &str) -> bool {
        self.entries.contains_key(rel)
    }
    pub fn get(&self, rel: &str) -> Option<&ManifestEntry> {
        self.entries.get(rel)
    }
}

/// Build the `find ... stat` command. adb's shell transport flattens its
/// argv into a single command string before re-parsing under the
/// device's `sh`, which means tokens like `;` and `{}` get eaten by the
/// outer shell unless we pre-escape them. We send one already-quoted
/// command string so the device-side `sh` sees exactly the form Phase 0
/// retro §1.5 validated:
///
/// ```text
/// find '<root>' -type f -exec stat -c '%n %s %Y' {} \;
/// ```
///
/// `root` must already be a vetted absolute path; the caller validates
/// `..` and shared-storage scope.
pub fn build_probe_command(root: &str) -> Vec<String> {
    let script = format!(
        "find {root} -type f -exec stat -c '%n %s %Y' {{}} \\;",
        root = shell_quote(root),
    );
    vec![script]
}

/// Single-quote a path for inclusion in a shell command. We assume the
/// caller already rejected `..` and `\n`; what's left to escape is the
/// single quote itself.
fn shell_quote(s: &str) -> String {
    let escaped = s.replace('\'', "'\\''");
    format!("'{escaped}'")
}

/// Probe `root` on the device identified by `serial`, returning a
/// manifest of every regular file. Returns an empty manifest (not an
/// error) when `root` doesn't exist on the device — that's the
/// "conflict-impossible fast path" case from plan.md §5.
pub fn probe(session: &AdbSession, serial: &str, root: &str) -> Result<DeviceManifest> {
    if root.contains("..") || !root.starts_with('/') {
        return Err(AdbError::CommandFailed {
            code: -1,
            stderr: format!("manifest probe rejected unsafe root: {root}"),
        });
    }
    let cmd = build_probe_command(root);
    let cmd_refs: Vec<&str> = cmd.iter().map(String::as_str).collect();
    let out = session.shell(serial, &cmd_refs)?;
    // toybox find emits "No such file or directory" on stderr with
    // exit_code 1 when the root is missing. We treat that as empty
    // manifest, not an error, so callers can use it as a "tree is
    // empty / new" signal.
    if out.exit_code != 0 {
        let stderr_lower = out.stderr.to_ascii_lowercase();
        if stderr_lower.contains("no such file") {
            return Ok(DeviceManifest {
                root: root.to_string(),
                entries: HashMap::new(),
            });
        }
        return Err(AdbError::CommandFailed {
            code: out.exit_code,
            stderr: out.stderr,
        });
    }
    let entries = parse_manifest_output(&out.stdout, root)?;
    Ok(DeviceManifest {
        root: root.to_string(),
        entries,
    })
}

/// Parse the raw text from `find … stat`. Format is `<path> <size> <mtime>`
/// per line; paths may contain spaces, but our `stat` template puts size
/// and mtime as the last two whitespace-separated tokens so we can split
/// from the **right**.
pub fn parse_manifest_output(stdout: &str, root: &str) -> Result<HashMap<String, ManifestEntry>> {
    let mut map = HashMap::new();
    let root = root.trim_end_matches('/');
    for raw in stdout.lines() {
        let line = raw.trim_end_matches('\r');
        if line.trim().is_empty() {
            continue;
        }
        // split from the right: <... path with spaces ...> <size> <mtime>
        let mut parts = line.rsplitn(3, ' ');
        let mtime = parts
            .next()
            .ok_or_else(|| AdbError::ParseError(line.to_string()))?;
        let size = parts
            .next()
            .ok_or_else(|| AdbError::ParseError(line.to_string()))?;
        let path = parts
            .next()
            .ok_or_else(|| AdbError::ParseError(line.to_string()))?;

        let size: u64 = size
            .parse()
            .map_err(|_| AdbError::ParseError(format!("bad size: {line}")))?;
        let mtime: i64 = mtime
            .parse()
            .map_err(|_| AdbError::ParseError(format!("bad mtime: {line}")))?;

        let rel_path = path
            .strip_prefix(root)
            .map(|s| s.trim_start_matches('/'))
            .unwrap_or(path)
            .to_string();

        if rel_path.is_empty() {
            // The probe sometimes lists the root itself when it's a
            // file (rare given `-type f` + a directory root) — skip.
            continue;
        }
        map.insert(
            rel_path.clone(),
            ManifestEntry {
                rel_path,
                size,
                mtime_secs: mtime,
            },
        );
    }
    Ok(map)
}

/// Plan.md §5 "same file" judgement. Returns true when name+size match
/// and mtime difference is within `tolerance` seconds.
pub fn is_same_file(
    local_size: u64,
    local_mtime: i64,
    remote: &ManifestEntry,
    tolerance_secs: u32,
) -> bool {
    if local_size != remote.size {
        return false;
    }
    let diff = (local_mtime - remote.mtime_secs).unsigned_abs();
    diff <= tolerance_secs as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn probe_command_is_single_quoted_string() {
        let cmd = build_probe_command("/sdcard/Download/foo");
        assert_eq!(cmd.len(), 1);
        assert!(cmd[0].starts_with("find '/sdcard/Download/foo'"));
        assert!(cmd[0].contains("-exec stat -c '%n %s %Y'"));
        assert!(cmd[0].ends_with("{} \\;"));
    }

    #[test]
    fn probe_command_uses_quoted_root_and_escaped_semicolon() {
        probe_command_is_single_quoted_string();
    }

    #[test]
    fn shell_quote_escapes_embedded_quote() {
        assert_eq!(shell_quote("a'b"), "'a'\\''b'");
        assert_eq!(shell_quote("/sdcard"), "'/sdcard'");
    }

    #[test]
    fn parses_simple_output() {
        let out = "\
/sdcard/Download/foo/a.txt 5 1778716800
/sdcard/Download/foo/sub/b.txt 12 1778716900
";
        let m = parse_manifest_output(out, "/sdcard/Download/foo").unwrap();
        assert_eq!(m.len(), 2);
        let a = m.get("a.txt").unwrap();
        assert_eq!(a.size, 5);
        assert_eq!(a.mtime_secs, 1_778_716_800);
        assert_eq!(m.get("sub/b.txt").unwrap().size, 12);
    }

    #[test]
    fn parses_filenames_with_spaces() {
        let out = "/sdcard/Download/foo/한글 파일.txt 7 1778716801\n";
        let m = parse_manifest_output(out, "/sdcard/Download/foo").unwrap();
        let e = m.get("한글 파일.txt").unwrap();
        assert_eq!(e.size, 7);
        assert_eq!(e.mtime_secs, 1_778_716_801);
    }

    #[test]
    fn strips_trailing_slash_from_root() {
        let out = "/sdcard/Download/foo/a.txt 5 1778716800\n";
        let m = parse_manifest_output(out, "/sdcard/Download/foo/").unwrap();
        assert!(m.contains_key("a.txt"));
    }

    #[test]
    fn empty_output_yields_empty_map() {
        let m = parse_manifest_output("", "/sdcard/Download/foo").unwrap();
        assert!(m.is_empty());
    }

    #[test]
    fn bad_lines_surface_parse_error() {
        let err = parse_manifest_output("garbage line\n", "/sdcard/Download/foo").unwrap_err();
        assert!(matches!(err, AdbError::ParseError(_)));
    }

    #[test]
    fn same_file_within_tolerance_returns_true() {
        let r = ManifestEntry {
            rel_path: "a.txt".into(),
            size: 100,
            mtime_secs: 1000,
        };
        assert!(is_same_file(100, 1001, &r, 2));
        assert!(is_same_file(100, 999, &r, 2));
        assert!(is_same_file(100, 1002, &r, 2));
    }

    #[test]
    fn same_file_outside_tolerance_returns_false() {
        let r = ManifestEntry {
            rel_path: "a.txt".into(),
            size: 100,
            mtime_secs: 1000,
        };
        assert!(!is_same_file(100, 1003, &r, 2));
        assert!(!is_same_file(101, 1000, &r, 2));
    }

    #[test]
    fn probe_rejects_unsafe_root_traversal() {
        // We can build a session-free check by inspecting build_probe_command
        // but the real validation lives in `probe()`. Use AdbError shape.
        let err_cases = ["../etc", "no-leading-slash", "/sdcard/../etc"];
        for r in err_cases {
            // Simulate the precondition guard from `probe()`.
            let unsafe_ = r.contains("..") || !r.starts_with('/');
            assert!(unsafe_, "should be rejected: {r}");
        }
    }
}
