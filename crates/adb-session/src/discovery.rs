//! `adb` executable discovery.
//!
//! plan.md §4.2 fixes the lookup order:
//!   1. `CROSSMTP_ADB` env var
//!   2. `~/Library/Android/sdk/platform-tools/adb`
//!   3. `adb` in `PATH`
//!   4. `/opt/homebrew/bin/adb`
//!
//! The function returns the resolved absolute path on success or
//! `AdbError::AdbNotAvailable` so the UI can branch into the
//! platform-tools install guide.

use std::env;
use std::path::PathBuf;

use crate::error::{AdbError, Result};

/// Where did we end up finding `adb`? Useful for diagnostics in the UI
/// and for retrospectives — Phase 0 explicitly wanted this recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdbSource {
    EnvVar,
    AndroidSdk,
    Path,
    Homebrew,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdbLocation {
    pub path: PathBuf,
    pub source: AdbSource,
}

/// Probe order from plan.md §4.2 with the host environment we actually
/// observe at call time. Pure I/O check — no `adb` invocation here so
/// callers can reuse this in tests without a real device.
pub fn discover_adb() -> Result<AdbLocation> {
    discover_adb_with_env(&RealEnv)
}

/// Test seam so unit tests can stub out HOME / PATH / file existence
/// without touching the real filesystem.
pub trait DiscoveryEnv {
    fn env_var(&self, key: &str) -> Option<String>;
    fn home_dir(&self) -> Option<PathBuf>;
    fn path_dirs(&self) -> Vec<PathBuf>;
    fn is_executable(&self, path: &std::path::Path) -> bool;
}

struct RealEnv;

impl DiscoveryEnv for RealEnv {
    fn env_var(&self, key: &str) -> Option<String> {
        env::var(key).ok().filter(|s| !s.is_empty())
    }
    fn home_dir(&self) -> Option<PathBuf> {
        env::var_os("HOME").map(PathBuf::from)
    }
    fn path_dirs(&self) -> Vec<PathBuf> {
        env::var_os("PATH")
            .map(|paths| env::split_paths(&paths).collect())
            .unwrap_or_default()
    }
    fn is_executable(&self, path: &std::path::Path) -> bool {
        is_regular_executable(path)
    }
}

pub fn discover_adb_with_env<E: DiscoveryEnv>(env: &E) -> Result<AdbLocation> {
    // 1. CROSSMTP_ADB env var.
    if let Some(custom) = env.env_var("CROSSMTP_ADB") {
        let p = PathBuf::from(custom);
        if env.is_executable(&p) {
            return Ok(AdbLocation {
                path: p,
                source: AdbSource::EnvVar,
            });
        }
        // If the user explicitly pointed at something but it's not
        // executable, that's still a failure of "adb not available"
        // — fall through to the other candidates so a misconfigured
        // env var doesn't dead-end a working install.
    }

    // 2. Android SDK default location.
    if let Some(home) = env.home_dir() {
        let p = home.join("Library/Android/sdk/platform-tools/adb");
        if env.is_executable(&p) {
            return Ok(AdbLocation {
                path: p,
                source: AdbSource::AndroidSdk,
            });
        }
    }

    // 3. PATH lookup.
    for dir in env.path_dirs() {
        let p = dir.join("adb");
        if env.is_executable(&p) {
            return Ok(AdbLocation {
                path: p,
                source: AdbSource::Path,
            });
        }
    }

    // 4. Homebrew on Apple Silicon.
    let brew = PathBuf::from("/opt/homebrew/bin/adb");
    if env.is_executable(&brew) {
        return Ok(AdbLocation {
            path: brew,
            source: AdbSource::Homebrew,
        });
    }

    Err(AdbError::AdbNotAvailable)
}

#[cfg(unix)]
fn is_regular_executable(path: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    meta.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_regular_executable(path: &std::path::Path) -> bool {
    std::fs::metadata(path).map(|m| m.is_file()).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashSet;

    struct StubEnv {
        vars: std::collections::HashMap<String, String>,
        home: Option<PathBuf>,
        path: Vec<PathBuf>,
        executable: RefCell<HashSet<PathBuf>>,
    }

    impl StubEnv {
        fn new() -> Self {
            Self {
                vars: Default::default(),
                home: None,
                path: vec![],
                executable: RefCell::new(HashSet::new()),
            }
        }
        fn with_executable(self, p: impl Into<PathBuf>) -> Self {
            self.executable.borrow_mut().insert(p.into());
            self
        }
    }

    impl DiscoveryEnv for StubEnv {
        fn env_var(&self, key: &str) -> Option<String> {
            self.vars.get(key).cloned()
        }
        fn home_dir(&self) -> Option<PathBuf> {
            self.home.clone()
        }
        fn path_dirs(&self) -> Vec<PathBuf> {
            self.path.clone()
        }
        fn is_executable(&self, path: &std::path::Path) -> bool {
            self.executable.borrow().contains(path)
        }
    }

    #[test]
    fn env_var_takes_precedence_over_other_candidates() {
        let mut env = StubEnv::new().with_executable("/custom/adb");
        env.vars.insert("CROSSMTP_ADB".into(), "/custom/adb".into());
        env.home = Some(PathBuf::from("/Users/dev"));
        env.executable
            .borrow_mut()
            .insert(PathBuf::from("/Users/dev/Library/Android/sdk/platform-tools/adb"));

        let got = discover_adb_with_env(&env).unwrap();
        assert_eq!(got.source, AdbSource::EnvVar);
        assert_eq!(got.path, PathBuf::from("/custom/adb"));
    }

    #[test]
    fn falls_through_when_env_var_points_at_missing_binary() {
        let mut env = StubEnv::new();
        env.vars.insert("CROSSMTP_ADB".into(), "/nope/adb".into());
        env.home = Some(PathBuf::from("/Users/dev"));
        env.executable
            .borrow_mut()
            .insert(PathBuf::from("/Users/dev/Library/Android/sdk/platform-tools/adb"));

        let got = discover_adb_with_env(&env).unwrap();
        assert_eq!(got.source, AdbSource::AndroidSdk);
    }

    #[test]
    fn android_sdk_beats_path_and_homebrew() {
        let mut env = StubEnv::new();
        env.home = Some(PathBuf::from("/Users/dev"));
        env.path = vec![PathBuf::from("/usr/local/bin")];
        env.executable
            .borrow_mut()
            .insert(PathBuf::from("/Users/dev/Library/Android/sdk/platform-tools/adb"));
        env.executable
            .borrow_mut()
            .insert(PathBuf::from("/usr/local/bin/adb"));
        env.executable
            .borrow_mut()
            .insert(PathBuf::from("/opt/homebrew/bin/adb"));

        let got = discover_adb_with_env(&env).unwrap();
        assert_eq!(got.source, AdbSource::AndroidSdk);
    }

    #[test]
    fn path_beats_homebrew_when_sdk_missing() {
        let mut env = StubEnv::new();
        env.path = vec![PathBuf::from("/usr/local/bin")];
        env.executable
            .borrow_mut()
            .insert(PathBuf::from("/usr/local/bin/adb"));
        env.executable
            .borrow_mut()
            .insert(PathBuf::from("/opt/homebrew/bin/adb"));

        let got = discover_adb_with_env(&env).unwrap();
        assert_eq!(got.source, AdbSource::Path);
    }

    #[test]
    fn homebrew_is_the_last_fallback() {
        let env = StubEnv::new();
        env.executable
            .borrow_mut()
            .insert(PathBuf::from("/opt/homebrew/bin/adb"));
        let got = discover_adb_with_env(&env).unwrap();
        assert_eq!(got.source, AdbSource::Homebrew);
    }

    #[test]
    fn missing_everywhere_surfaces_adb_not_available() {
        let env = StubEnv::new();
        let err = discover_adb_with_env(&env).unwrap_err();
        assert!(matches!(err, AdbError::AdbNotAvailable));
    }
}
