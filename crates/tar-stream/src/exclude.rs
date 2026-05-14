//! macOS metadata hard-exclude filter.
//!
//! Phase 0 retro §1.3: AppleDouble (`._*`), `.DS_Store`, `.Spotlight-V100`,
//! `.Trashes`, `.fseventsd` are **default deny**. Not a user-facing toggle.
//! The Tar Stream Builder must drop these regardless of `COPYFILE_DISABLE`.

/// Patterns we never include in a tar entry. Order doesn't matter; we
/// check membership against each component name.
pub const MACOS_METADATA_PATTERNS: &[&str] = &[
    ".DS_Store",
    ".Spotlight-V100",
    ".Trashes",
    ".fseventsd",
];

/// True if `name` is a macOS metadata entry we hard-exclude. Matches:
///   * any name starting with `._` (AppleDouble companion)
///   * exact membership in [`MACOS_METADATA_PATTERNS`]
pub fn is_macos_metadata(name: &str) -> bool {
    if name.starts_with("._") {
        return true;
    }
    MACOS_METADATA_PATTERNS.iter().any(|p| *p == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appledouble_prefix_is_excluded() {
        assert!(is_macos_metadata("._a.txt"));
        assert!(is_macos_metadata("._한글.txt"));
        assert!(is_macos_metadata("._"));
    }

    #[test]
    fn known_metadata_names_excluded() {
        for n in [".DS_Store", ".Spotlight-V100", ".Trashes", ".fseventsd"] {
            assert!(is_macos_metadata(n), "{n} should be excluded");
        }
    }

    #[test]
    fn dotfile_that_is_not_metadata_passes() {
        assert!(!is_macos_metadata(".gitignore"));
        assert!(!is_macos_metadata(".env"));
        assert!(!is_macos_metadata("normal.txt"));
    }

    #[test]
    fn case_sensitive_exact_match_only() {
        // We don't lowercase — APFS is case-insensitive by default but
        // the on-disk name we see is exact, and we mirror that.
        assert!(!is_macos_metadata(".ds_store"));
    }
}
