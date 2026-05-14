//! Lazy directory traversal yielding entries in deterministic order.
//!
//! plan.md §4.3: symbolic links, sockets, fifos, device files are
//! "skip or fail" by policy. Default = **skip with a UnsupportedEntry
//! note**; the orchestrator picks one (fail or warn) based on user
//! config. Symlinks are NOT followed by default — following them is the
//! main vector for path traversal in archive tools.

use std::fs::{self, Metadata};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use crate::error::{Result, TarError};
use crate::exclude::is_macos_metadata;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Directory,
    /// A symlink, special device, socket, fifo, or anything else not
    /// representable as a plain USTAR file. Builder decides per-policy
    /// whether to skip or fail.
    Unsupported(String),
}

#[derive(Debug, Clone)]
pub struct Entry {
    /// Local absolute path on disk (what we'll Open / stat).
    pub source: PathBuf,
    /// Relative path under the traversal root, with components ordered
    /// root-to-leaf. This is what becomes the tar entry name once
    /// validated through `TarPath`.
    pub relative: Vec<String>,
    pub kind: EntryKind,
    /// File size in bytes for `EntryKind::File`; 0 for directories.
    pub size: u64,
    /// Unix epoch seconds — used both for tar header mtime and for the
    /// conflict planner's tolerance check.
    pub mtime_secs: i64,
}

/// Walk `root` and yield entries in stable, recursion-pre-order. Hidden
/// macOS metadata is dropped at the source so it never enters the tar.
///
/// `root` must point at an existing directory or single file:
///   * if file: yields exactly one [`Entry::File`] with empty `relative`
///     prefix — caller pairs it with a tar entry name explicitly.
///   * if directory: yields the directory's *contents* (not the dir
///     itself as a wrapper entry), so the caller chooses how to root
///     the tar layout.
pub fn walk(root: &Path) -> Result<Vec<Entry>> {
    let meta = fs::symlink_metadata(root).map_err(|e| TarError::Io {
        path: root.display().to_string(),
        source: e,
    })?;
    if meta.file_type().is_symlink() {
        return Err(TarError::UnsupportedEntry {
            path: root.display().to_string(),
            kind: "symlink at traversal root".into(),
        });
    }
    let mut out = Vec::new();
    if meta.is_file() {
        let file_name = root
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(|| TarError::InvalidEntryName {
                reason: format!("root file has no name: {}", root.display()),
            })?;
        if is_macos_metadata(file_name) {
            return Ok(out);
        }
        out.push(Entry {
            source: root.to_path_buf(),
            relative: vec![file_name.to_string()],
            kind: EntryKind::File,
            size: meta.len(),
            mtime_secs: mtime_of(&meta),
        });
        return Ok(out);
    }
    if !meta.is_dir() {
        return Err(TarError::UnsupportedEntry {
            path: root.display().to_string(),
            kind: format!("{:?}", meta.file_type()),
        });
    }
    walk_dir(root, &[], &mut out)?;
    Ok(out)
}

fn walk_dir(dir: &Path, prefix: &[String], out: &mut Vec<Entry>) -> Result<()> {
    let mut children: Vec<_> = fs::read_dir(dir)
        .map_err(|e| TarError::Io {
            path: dir.display().to_string(),
            source: e,
        })?
        .collect::<std::io::Result<Vec<_>>>()
        .map_err(|e| TarError::Io {
            path: dir.display().to_string(),
            source: e,
        })?;
    // Deterministic order — important for tests and for reproducible
    // conflict planning.
    children.sort_by_key(|e| e.file_name());

    for entry in children {
        let name = entry.file_name();
        let Some(name_str) = name.to_str() else {
            // Reject non-UTF8 names rather than silently transcoding.
            return Err(TarError::InvalidEntryName {
                reason: format!(
                    "non-UTF8 filename under {}: {:?}",
                    dir.display(),
                    name
                ),
            });
        };
        if is_macos_metadata(name_str) {
            continue;
        }
        let path = entry.path();
        let meta = fs::symlink_metadata(&path).map_err(|e| TarError::Io {
            path: path.display().to_string(),
            source: e,
        })?;
        let ft = meta.file_type();
        let mut next_prefix = prefix.to_vec();
        next_prefix.push(name_str.to_string());

        if ft.is_symlink() {
            out.push(Entry {
                source: path.clone(),
                relative: next_prefix.clone(),
                kind: EntryKind::Unsupported("symlink".into()),
                size: 0,
                mtime_secs: mtime_of(&meta),
            });
            continue;
        }
        if ft.is_file() {
            out.push(Entry {
                source: path.clone(),
                relative: next_prefix.clone(),
                kind: EntryKind::File,
                size: meta.len(),
                mtime_secs: mtime_of(&meta),
            });
            continue;
        }
        if ft.is_dir() {
            out.push(Entry {
                source: path.clone(),
                relative: next_prefix.clone(),
                kind: EntryKind::Directory,
                size: 0,
                mtime_secs: mtime_of(&meta),
            });
            walk_dir(&path, &next_prefix, out)?;
            continue;
        }
        // socket / fifo / device — annotate but don't fail traversal.
        out.push(Entry {
            source: path,
            relative: next_prefix,
            kind: EntryKind::Unsupported(format!("{ft:?}")),
            size: 0,
            mtime_secs: mtime_of(&meta),
        });
    }
    Ok(())
}

fn mtime_of(meta: &Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{create_dir_all, File};
    use std::io::Write;

    fn tempdir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "tar-stream-test-{}-{}",
            std::process::id(),
            rand_suffix()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        create_dir_all(&dir).unwrap();
        dir
    }

    fn rand_suffix() -> u64 {
        // Cheap monotonic-ish counter without deps.
        use std::sync::atomic::{AtomicU64, Ordering};
        static C: AtomicU64 = AtomicU64::new(0);
        C.fetch_add(1, Ordering::Relaxed)
    }

    fn write(p: &Path, contents: &str) {
        if let Some(parent) = p.parent() {
            create_dir_all(parent).unwrap();
        }
        File::create(p).unwrap().write_all(contents.as_bytes()).unwrap();
    }

    #[test]
    fn walk_single_file_yields_one_entry() {
        let dir = tempdir();
        let f = dir.join("hello.txt");
        write(&f, "hi");
        let entries = walk(&f).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].relative, vec!["hello.txt"]);
        assert_eq!(entries[0].kind, EntryKind::File);
        assert_eq!(entries[0].size, 2);
    }

    #[test]
    fn walk_dir_yields_children_in_sorted_order() {
        let dir = tempdir();
        write(&dir.join("b.txt"), "b");
        write(&dir.join("a.txt"), "a");
        write(&dir.join("sub/c.txt"), "c");
        let entries = walk(&dir).unwrap();
        let paths: Vec<String> = entries
            .iter()
            .map(|e| e.relative.join("/"))
            .collect();
        assert_eq!(paths, vec!["a.txt", "b.txt", "sub", "sub/c.txt"]);
    }

    #[test]
    fn walk_excludes_macos_metadata_at_every_level() {
        let dir = tempdir();
        write(&dir.join("._root.txt"), "x");
        write(&dir.join(".DS_Store"), "x");
        write(&dir.join("real.txt"), "real");
        write(&dir.join("sub/._한글.txt"), "x");
        write(&dir.join("sub/keep.txt"), "k");
        let entries = walk(&dir).unwrap();
        let paths: Vec<String> = entries
            .iter()
            .map(|e| e.relative.join("/"))
            .collect();
        assert_eq!(paths, vec!["real.txt", "sub", "sub/keep.txt"]);
    }

    #[test]
    fn walk_marks_symlink_as_unsupported_without_following() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let dir = tempdir();
            let target = dir.join("real.txt");
            write(&target, "real");
            let link = dir.join("link.txt");
            symlink(&target, &link).unwrap();
            let entries = walk(&dir).unwrap();
            let link_entry = entries
                .iter()
                .find(|e| e.relative == vec!["link.txt"])
                .expect("symlink listed");
            assert!(matches!(link_entry.kind, EntryKind::Unsupported(_)));
        }
    }

    #[test]
    fn walk_rejects_traversal_root_that_is_symlink() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let dir = tempdir();
            let target = dir.join("real");
            create_dir_all(&target).unwrap();
            let link = dir.join("link");
            symlink(&target, &link).unwrap();
            let err = walk(&link).unwrap_err();
            assert!(matches!(err, TarError::UnsupportedEntry { .. }));
        }
    }

    #[test]
    fn walk_keeps_korean_filenames() {
        let dir = tempdir();
        write(&dir.join("한글.txt"), "k");
        let entries = walk(&dir).unwrap();
        assert_eq!(entries[0].relative, vec!["한글.txt"]);
    }
}
