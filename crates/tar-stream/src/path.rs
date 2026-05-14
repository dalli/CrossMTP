//! Relative tar entry path with traversal protection.
//!
//! Plan §4.3: tar paths must be **relative only**. No `..`, no absolute
//! paths, no NUL bytes. We normalise here once at construction so the
//! header writer and conflict planner can trust the result.

use std::path::{Component, Path, PathBuf};

use crate::error::{Result, TarError};

/// A validated, slash-separated, relative path destined for a tar entry.
///
/// Internally stored as `Vec<String>` of components so we can re-render
/// with `/` regardless of host OS. `as_str()` always uses `/`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TarPath {
    components: Vec<String>,
}

impl TarPath {
    /// Validate and normalise a candidate path. Rejects:
    ///   * absolute paths
    ///   * any `..` (even if it would resolve under root)
    ///   * empty components
    ///   * NUL bytes
    ///   * Windows-style drive prefixes
    pub fn new<P: AsRef<Path>>(p: P) -> Result<Self> {
        let p = p.as_ref();
        let raw = p.to_string_lossy().into_owned();
        let mut components = Vec::new();
        for c in p.components() {
            match c {
                Component::Normal(s) => {
                    let s = s.to_string_lossy().into_owned();
                    if s.is_empty() {
                        return Err(TarError::InvalidEntryName {
                            reason: format!("empty component in {raw}"),
                        });
                    }
                    if s.contains('\0') {
                        return Err(TarError::InvalidEntryName {
                            reason: format!("NUL byte in {raw}"),
                        });
                    }
                    components.push(s);
                }
                Component::CurDir => continue,
                Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                    return Err(TarError::PathTraversal { path: raw });
                }
            }
        }
        if components.is_empty() {
            return Err(TarError::InvalidEntryName {
                reason: format!("path resolved to empty: {raw}"),
            });
        }
        Ok(TarPath { components })
    }

    /// Build a TarPath from already-validated components. Used by the
    /// rename helper when it knows it's mutating only the last segment.
    pub fn from_components(components: Vec<String>) -> Result<Self> {
        if components.is_empty() {
            return Err(TarError::InvalidEntryName {
                reason: "empty components".into(),
            });
        }
        for c in &components {
            if c.is_empty() || c == ".." || c.contains('/') || c.contains('\0') {
                return Err(TarError::InvalidEntryName {
                    reason: format!("bad component: {c}"),
                });
            }
        }
        Ok(TarPath { components })
    }

    pub fn components(&self) -> &[String] {
        &self.components
    }

    /// Slash-separated string suitable for a tar header `name` field.
    pub fn as_str(&self) -> String {
        self.components.join("/")
    }

    /// Filename component (last segment), without extension splitting.
    pub fn file_name(&self) -> &str {
        self.components.last().map(String::as_str).unwrap_or("")
    }

    /// Replace the last component with `new_name` (which must itself be
    /// a single safe component — no `/`, no `..`).
    pub fn with_file_name(&self, new_name: &str) -> Result<Self> {
        if new_name.is_empty()
            || new_name == ".."
            || new_name.contains('/')
            || new_name.contains('\0')
        {
            return Err(TarError::InvalidEntryName {
                reason: format!("bad rename target: {new_name}"),
            });
        }
        let mut c = self.components.clone();
        if let Some(last) = c.last_mut() {
            *last = new_name.to_string();
        }
        Ok(TarPath { components: c })
    }

    /// Split filename into stem + extension. The extension is the
    /// substring after the **last** `.`, or empty if the name has no dot
    /// or starts with one (`.gitignore` → stem `.gitignore`, ext "").
    pub fn split_stem_ext(&self) -> (String, String) {
        let name = self.file_name();
        let bytes = name.as_bytes();
        // find last '.' that is not at index 0
        let mut idx = None;
        for (i, b) in bytes.iter().enumerate().rev() {
            if *b == b'.' && i != 0 {
                idx = Some(i);
                break;
            }
        }
        match idx {
            Some(i) => (name[..i].to_string(), name[i..].to_string()),
            None => (name.to_string(), String::new()),
        }
    }
}

impl std::fmt::Display for TarPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.as_str())
    }
}

/// Convenience: turn a candidate string into a TarPath, applying any
/// trivial normalisation the tar header writer expects (forward slashes,
/// no leading `./`).
pub fn tar_path_from_str(s: &str) -> Result<TarPath> {
    let normalised = PathBuf::from(s);
    TarPath::new(normalised)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_simple_relative_path() {
        let p = TarPath::new("a/b/c.txt").unwrap();
        assert_eq!(p.as_str(), "a/b/c.txt");
        assert_eq!(p.file_name(), "c.txt");
    }

    #[test]
    fn strips_curdir_segments() {
        let p = TarPath::new("./a/./b").unwrap();
        assert_eq!(p.as_str(), "a/b");
    }

    #[test]
    fn rejects_parent_dir() {
        let err = TarPath::new("a/../etc/passwd").unwrap_err();
        assert!(matches!(err, TarError::PathTraversal { .. }));
    }

    #[test]
    fn rejects_absolute_unix_path() {
        let err = TarPath::new("/etc/passwd").unwrap_err();
        assert!(matches!(err, TarError::PathTraversal { .. }));
    }

    #[test]
    fn rejects_empty_after_normalisation() {
        let err = TarPath::new("./").unwrap_err();
        assert!(matches!(err, TarError::InvalidEntryName { .. }));
    }

    #[test]
    fn with_file_name_swaps_last_segment() {
        let p = TarPath::new("a/b/c.txt").unwrap();
        let q = p.with_file_name("c (1).txt").unwrap();
        assert_eq!(q.as_str(), "a/b/c (1).txt");
    }

    #[test]
    fn with_file_name_rejects_slash_in_replacement() {
        let p = TarPath::new("a.txt").unwrap();
        assert!(p.with_file_name("foo/bar").is_err());
        assert!(p.with_file_name("..").is_err());
    }

    #[test]
    fn stem_and_ext_split() {
        let p = TarPath::new("a/b/photo.tar.gz").unwrap();
        assert_eq!(p.split_stem_ext(), ("photo.tar".into(), ".gz".into()));
        let p = TarPath::new(".gitignore").unwrap();
        assert_eq!(p.split_stem_ext(), (".gitignore".into(), String::new()));
        let p = TarPath::new("README").unwrap();
        assert_eq!(p.split_stem_ext(), ("README".into(), String::new()));
    }

    #[test]
    fn from_components_validates_each() {
        assert!(TarPath::from_components(vec!["a".into(), "b".into()]).is_ok());
        assert!(TarPath::from_components(vec!["a/b".into()]).is_err());
        assert!(TarPath::from_components(vec!["..".into()]).is_err());
        assert!(TarPath::from_components(vec![]).is_err());
    }
}
