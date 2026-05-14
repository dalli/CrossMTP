//! `TarStreamBuilder` — pipeline driver that walks a root, applies the
//! conflict plan, and writes a USTAR byte stream to a sink Writer.
//!
//! plan.md §4.5 pipeline stages (Scanner → TarWriter → ADB Writer) are
//! collapsed here into a single sequential builder for Phase 2; the
//! adb-session layer's `AdbProcess::take_stdin()` is the sink. Bounded
//! queueing arrives in Phase 3 when the orchestrator runs this in a
//! worker thread.

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use crate::conflict::{ConflictAction, ConflictPlan};
use crate::error::{Result, TarError};
use crate::header::{dir_header, end_of_archive_marker, file_header, pad_to_block, BLOCK_SIZE};
use crate::path::TarPath;
use crate::progress::ProgressCounter;
use crate::traversal::{walk, Entry, EntryKind};

const COPY_BUF: usize = 1024 * 1024; // 1 MiB — plan.md §4.5

pub struct TarStreamBuilder {
    root: PathBuf,
    plan: ConflictPlan,
    progress: ProgressCounter,
    /// When true, encountering an `Unsupported` entry kind aborts the
    /// stream. Default false — the orchestrator decides per-policy.
    fail_on_unsupported: bool,
}

impl TarStreamBuilder {
    pub fn new<P: Into<PathBuf>>(root: P) -> Self {
        Self {
            root: root.into(),
            plan: ConflictPlan::new(),
            progress: ProgressCounter::new(),
            fail_on_unsupported: false,
        }
    }

    pub fn with_conflict_plan(mut self, plan: ConflictPlan) -> Self {
        self.plan = plan;
        self
    }

    pub fn fail_on_unsupported(mut self, fail: bool) -> Self {
        self.fail_on_unsupported = fail;
        self
    }

    pub fn progress(&self) -> &ProgressCounter {
        &self.progress
    }

    /// Walk `root` and write the full tar stream to `sink`. Returns the
    /// final progress snapshot. The stream is terminated with two
    /// 512-byte zero blocks per POSIX.
    pub fn write_to<W: Write>(mut self, sink: &mut W) -> Result<ProgressCounter> {
        let entries = walk(&self.root)?;
        for e in entries {
            self.write_entry(&e, sink)?;
        }
        sink.write_all(&end_of_archive_marker())?;
        Ok(self.progress)
    }

    fn write_entry<W: Write>(&mut self, e: &Entry, sink: &mut W) -> Result<()> {
        let rel = e.relative.join("/");
        self.progress.begin_entry(&rel);

        let tar_path = TarPath::from_components(e.relative.clone())?;

        match &e.kind {
            EntryKind::Directory => {
                let h = dir_header(&tar_path, e.mtime_secs)?;
                sink.write_all(&h)?;
                self.progress.add_bytes(h.len() as u64);
                Ok(())
            }
            EntryKind::Unsupported(kind) => {
                if self.fail_on_unsupported {
                    Err(TarError::UnsupportedEntry {
                        path: rel,
                        kind: kind.clone(),
                    })
                } else {
                    self.progress.record_skipped();
                    Ok(())
                }
            }
            EntryKind::File => {
                let action = self.plan.action_for(&tar_path).clone();
                match action {
                    ConflictAction::Skip => {
                        self.progress.record_skipped();
                        Ok(())
                    }
                    ConflictAction::Rename(new_name) => {
                        let new_path = tar_path.with_file_name(&new_name)?;
                        self.write_file(&e.source, &new_path, e.size, e.mtime_secs, sink)
                    }
                    ConflictAction::Emit | ConflictAction::Overwrite => {
                        self.write_file(&e.source, &tar_path, e.size, e.mtime_secs, sink)
                    }
                }
            }
        }
    }

    fn write_file<W: Write>(
        &mut self,
        source: &Path,
        tar_path: &TarPath,
        size: u64,
        mtime: i64,
        sink: &mut W,
    ) -> Result<()> {
        let h = file_header(tar_path, size, mtime, 0o644)?;
        sink.write_all(&h)?;
        self.progress.add_bytes(h.len() as u64);

        let mut f = File::open(source).map_err(|e| TarError::Io {
            path: source.display().to_string(),
            source: e,
        })?;
        let mut copied: u64 = 0;
        let mut buf = vec![0u8; COPY_BUF];
        loop {
            let n = f.read(&mut buf).map_err(|e| TarError::Io {
                path: source.display().to_string(),
                source: e,
            })?;
            if n == 0 {
                break;
            }
            sink.write_all(&buf[..n])?;
            copied += n as u64;
            self.progress.add_bytes(n as u64);
        }
        if copied != size {
            // File changed under us mid-stream — emit zero padding so
            // the tar block grid stays valid, then surface the error
            // because the device-side extractor will see a truncated
            // file.
            return Err(TarError::Io {
                path: source.display().to_string(),
                source: std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    format!("file size changed mid-stream: expected {size}, read {copied}"),
                ),
            });
        }
        let pad = pad_to_block(size) as usize;
        if pad > 0 {
            sink.write_all(&vec![0u8; pad])?;
            self.progress.add_bytes(pad as u64);
        }
        self.progress.record_emitted(size);
        let _ = BLOCK_SIZE; // silence unused import in release
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{create_dir_all, File};
    use std::io::{Cursor, Write as _};

    fn tempdir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "tar-stream-builder-{}-{}",
            std::process::id(),
            rand_suffix()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        create_dir_all(&dir).unwrap();
        dir
    }
    fn rand_suffix() -> u64 {
        use std::sync::atomic::{AtomicU64, Ordering};
        static C: AtomicU64 = AtomicU64::new(0);
        C.fetch_add(1, Ordering::Relaxed)
    }
    fn write(p: &Path, contents: &[u8]) {
        if let Some(parent) = p.parent() {
            create_dir_all(parent).unwrap();
        }
        File::create(p).unwrap().write_all(contents).unwrap();
    }

    /// Parse only file payloads out of our stream (header → size → body
    /// → pad). Returns Vec<(name, body_bytes)>. Skips dir entries and
    /// the trailing two zero blocks.
    fn parse_tar(bytes: &[u8]) -> Vec<(String, Vec<u8>)> {
        let mut out = Vec::new();
        let mut i = 0;
        while i + BLOCK_SIZE <= bytes.len() {
            let block = &bytes[i..i + BLOCK_SIZE];
            if block.iter().all(|b| *b == 0) {
                break;
            }
            let typeflag = block[156];
            let name = std::str::from_utf8(&block[0..100])
                .unwrap()
                .trim_matches('\0')
                .to_string();
            let prefix = std::str::from_utf8(&block[345..500])
                .unwrap()
                .trim_matches('\0')
                .to_string();
            let full = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            };
            let size_str = std::str::from_utf8(&block[124..136])
                .unwrap()
                .trim_matches('\0')
                .trim();
            let size = u64::from_str_radix(size_str, 8).unwrap_or(0) as usize;
            i += BLOCK_SIZE;
            if typeflag == b'0' || typeflag == 0 {
                let body = bytes[i..i + size].to_vec();
                out.push((full, body));
                let pad = pad_to_block(size as u64) as usize;
                i += size + pad;
            } else if typeflag == b'L' {
                // LongLink — skip its payload, the next header is the real one
                i += size + pad_to_block(size as u64) as usize;
            } else {
                // dir or other — no payload
            }
        }
        out
    }

    #[test]
    fn emits_simple_file_with_payload_and_padding() {
        let dir = tempdir();
        write(&dir.join("a.txt"), b"hello");
        let mut buf = Cursor::new(Vec::new());
        let b = TarStreamBuilder::new(&dir);
        let p = b.write_to(&mut buf).unwrap();
        assert_eq!(p.snapshot().files_emitted, 1);
        let parsed = parse_tar(&buf.into_inner());
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].0, "a.txt");
        assert_eq!(parsed[0].1, b"hello");
    }

    #[test]
    fn nested_layout_preserves_relative_paths() {
        let dir = tempdir();
        write(&dir.join("a.txt"), b"A");
        write(&dir.join("sub/b.txt"), b"B");
        write(&dir.join("sub/deeper/c.txt"), b"C");
        let mut buf = Cursor::new(Vec::new());
        TarStreamBuilder::new(&dir).write_to(&mut buf).unwrap();
        let parsed = parse_tar(&buf.into_inner());
        let names: Vec<String> = parsed.iter().map(|p| p.0.clone()).collect();
        assert!(names.contains(&"a.txt".into()));
        assert!(names.contains(&"sub/b.txt".into()));
        assert!(names.contains(&"sub/deeper/c.txt".into()));
    }

    #[test]
    fn macos_metadata_is_dropped_from_stream() {
        let dir = tempdir();
        write(&dir.join("._a.txt"), b"x");
        write(&dir.join(".DS_Store"), b"x");
        write(&dir.join("real.txt"), b"r");
        let mut buf = Cursor::new(Vec::new());
        TarStreamBuilder::new(&dir).write_to(&mut buf).unwrap();
        let parsed = parse_tar(&buf.into_inner());
        let names: Vec<String> = parsed.iter().map(|p| p.0.clone()).collect();
        assert_eq!(names, vec!["real.txt".to_string()]);
    }

    #[test]
    fn skip_action_drops_specific_entry() {
        let dir = tempdir();
        write(&dir.join("a.txt"), b"A");
        write(&dir.join("b.txt"), b"B");
        let mut plan = ConflictPlan::new();
        plan.insert(&TarPath::new("a.txt").unwrap(), ConflictAction::Skip);
        let mut buf = Cursor::new(Vec::new());
        let p = TarStreamBuilder::new(&dir)
            .with_conflict_plan(plan)
            .write_to(&mut buf)
            .unwrap();
        let parsed = parse_tar(&buf.into_inner());
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].0, "b.txt");
        assert_eq!(p.snapshot().files_skipped, 1);
        assert_eq!(p.snapshot().files_emitted, 1);
    }

    #[test]
    fn rename_action_rewrites_last_segment_only() {
        let dir = tempdir();
        write(&dir.join("sub/a.txt"), b"A");
        let mut plan = ConflictPlan::new();
        plan.insert(
            &TarPath::new("sub/a.txt").unwrap(),
            ConflictAction::Rename("a (1).txt".into()),
        );
        let mut buf = Cursor::new(Vec::new());
        TarStreamBuilder::new(&dir)
            .with_conflict_plan(plan)
            .write_to(&mut buf)
            .unwrap();
        let parsed = parse_tar(&buf.into_inner());
        let names: Vec<String> = parsed.iter().map(|p| p.0.clone()).collect();
        assert!(names.contains(&"sub/a (1).txt".to_string()));
        assert!(!names.contains(&"sub/a.txt".to_string()));
    }

    #[test]
    fn overwrite_action_keeps_path_unchanged() {
        let dir = tempdir();
        write(&dir.join("a.txt"), b"A");
        let mut plan = ConflictPlan::new();
        plan.insert(
            &TarPath::new("a.txt").unwrap(),
            ConflictAction::Overwrite,
        );
        let mut buf = Cursor::new(Vec::new());
        TarStreamBuilder::new(&dir)
            .with_conflict_plan(plan)
            .write_to(&mut buf)
            .unwrap();
        let parsed = parse_tar(&buf.into_inner());
        assert_eq!(parsed[0].0, "a.txt");
        assert_eq!(parsed[0].1, b"A");
    }

    #[test]
    fn stream_ends_with_two_zero_blocks() {
        let dir = tempdir();
        write(&dir.join("a.txt"), b"a");
        let mut buf = Cursor::new(Vec::new());
        TarStreamBuilder::new(&dir).write_to(&mut buf).unwrap();
        let v = buf.into_inner();
        assert!(v.len() >= BLOCK_SIZE * 2);
        let tail = &v[v.len() - BLOCK_SIZE * 2..];
        assert!(tail.iter().all(|b| *b == 0));
    }

    #[test]
    fn korean_filenames_round_trip() {
        let dir = tempdir();
        write(&dir.join("한글.txt"), b"K");
        let mut buf = Cursor::new(Vec::new());
        TarStreamBuilder::new(&dir).write_to(&mut buf).unwrap();
        let parsed = parse_tar(&buf.into_inner());
        let names: Vec<String> = parsed.iter().map(|p| p.0.clone()).collect();
        assert!(names.iter().any(|n| n.ends_with("한글.txt")));
    }

    #[test]
    fn unsupported_entry_skips_by_default() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let dir = tempdir();
            write(&dir.join("real.txt"), b"r");
            let target = dir.join("real.txt");
            let link = dir.join("link.txt");
            symlink(&target, &link).unwrap();
            let mut buf = Cursor::new(Vec::new());
            let p = TarStreamBuilder::new(&dir).write_to(&mut buf).unwrap();
            let parsed = parse_tar(&buf.into_inner());
            let names: Vec<String> = parsed.iter().map(|p| p.0.clone()).collect();
            assert_eq!(names, vec!["real.txt".to_string()]);
            assert!(p.snapshot().files_skipped >= 1);
        }
    }

    #[test]
    fn unsupported_entry_fails_when_strict() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let dir = tempdir();
            write(&dir.join("real.txt"), b"r");
            let target = dir.join("real.txt");
            let link = dir.join("link.txt");
            symlink(&target, &link).unwrap();
            let mut buf = Cursor::new(Vec::new());
            let err = TarStreamBuilder::new(&dir)
                .fail_on_unsupported(true)
                .write_to(&mut buf)
                .unwrap_err();
            assert!(matches!(err, TarError::UnsupportedEntry { .. }));
        }
    }

    #[test]
    fn nested_rename_does_not_overwrite_neighbouring_file() {
        // plan.md §9.1: nested-folder conflict must not produce
        // overwrite. The builder only does what the plan tells it, so
        // we assert that two siblings with similar names stay distinct.
        let dir = tempdir();
        write(&dir.join("sub/a.txt"), b"A");
        write(&dir.join("sub/a (1).txt"), b"AA");
        let mut plan = ConflictPlan::new();
        plan.insert(
            &TarPath::new("sub/a.txt").unwrap(),
            ConflictAction::Rename("a (2).txt".into()),
        );
        let mut buf = Cursor::new(Vec::new());
        TarStreamBuilder::new(&dir)
            .with_conflict_plan(plan)
            .write_to(&mut buf)
            .unwrap();
        let parsed = parse_tar(&buf.into_inner());
        let names: Vec<String> = parsed.iter().map(|p| p.0.clone()).collect();
        assert!(names.contains(&"sub/a (1).txt".to_string()));
        assert!(names.contains(&"sub/a (2).txt".to_string()));
        assert!(!names.contains(&"sub/a.txt".to_string()));
    }
}
