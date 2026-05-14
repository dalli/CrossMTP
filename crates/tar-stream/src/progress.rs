//! Progress counters for the tar stream.
//!
//! plan.md §4.3: builder must expose current path, completed bytes, and
//! totals. We keep counters cheap (atomic-free, single-thread) because
//! the streamer is sequential anyway; the orchestrator will lift each
//! snapshot into its event channel.

#[derive(Debug, Default, Clone)]
pub struct ProgressCounter {
    files_seen: u64,
    files_emitted: u64,
    files_skipped: u64,
    bytes_emitted: u64,
    current_path: Option<String>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ProgressSnapshot {
    pub files_seen: u64,
    pub files_emitted: u64,
    pub files_skipped: u64,
    pub bytes_emitted: u64,
    pub current_path: Option<String>,
}

impl ProgressCounter {
    pub fn new() -> Self {
        Self::default()
    }

    pub(crate) fn begin_entry(&mut self, path: &str) {
        self.files_seen += 1;
        self.current_path = Some(path.to_string());
    }

    pub(crate) fn record_emitted(&mut self, bytes: u64) {
        self.files_emitted += 1;
        self.bytes_emitted += bytes;
    }

    pub(crate) fn record_skipped(&mut self) {
        self.files_skipped += 1;
    }

    pub(crate) fn add_bytes(&mut self, bytes: u64) {
        self.bytes_emitted += bytes;
    }

    pub fn snapshot(&self) -> ProgressSnapshot {
        ProgressSnapshot {
            files_seen: self.files_seen,
            files_emitted: self.files_emitted,
            files_skipped: self.files_skipped,
            bytes_emitted: self.bytes_emitted,
            current_path: self.current_path.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_accumulate_per_entry() {
        let mut p = ProgressCounter::new();
        p.begin_entry("a.txt");
        p.record_emitted(100);
        p.begin_entry("b.txt");
        p.record_emitted(200);
        let s = p.snapshot();
        assert_eq!(s.files_seen, 2);
        assert_eq!(s.files_emitted, 2);
        assert_eq!(s.bytes_emitted, 300);
        assert_eq!(s.current_path.as_deref(), Some("b.txt"));
    }

    #[test]
    fn skip_increments_skipped_only() {
        let mut p = ProgressCounter::new();
        p.begin_entry("a.txt");
        p.record_skipped();
        let s = p.snapshot();
        assert_eq!(s.files_seen, 1);
        assert_eq!(s.files_emitted, 0);
        assert_eq!(s.files_skipped, 1);
        assert_eq!(s.bytes_emitted, 0);
    }
}
