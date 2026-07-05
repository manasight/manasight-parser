//! Platform-specific log file path resolution.
//!
//! Resolves the default location of MTG Arena's `Player.log` on each
//! supported platform (Windows via `known-folders`, macOS via `~/Library/Logs/`,
//! Linux via Steam/Proton `libraryfolders.vdf` or Lutris fallback).
//!
//! # Usage
//!
//! ```rust,no_run
//! use manasight_parser::log::discovery;
//!
//! // Resolve and verify the log file exists:
//! match discovery::discover_log_file() {
//!     Ok(paths) => println!("Found: {}", paths.player_log().display()),
//!     Err(e) => eprintln!("Discovery failed: {e}"),
//! }
//! ```
//!
//! When [`discover_log_file`] returns [`DiscoveryError::LogFileMissing`],
//! callers should notify the user (e.g., "MTG Arena not found" or "Enable
//! Detailed Logging") and poll periodically until the file appears.

use std::path::{Path, PathBuf};
#[cfg(target_os = "linux")]
use std::time::SystemTime;

// ---------------------------------------------------------------------------
// LogPaths
// ---------------------------------------------------------------------------

/// Resolved paths to MTG Arena log files.
///
/// Both files reside in the same directory. `player_prev_log` contains the
/// previous session's log and is used for catch-up parsing on startup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogPaths {
    /// Path to the active `Player.log`.
    player_log: PathBuf,
    /// Path to the previous session's `Player-prev.log`.
    player_prev_log: PathBuf,
}

impl LogPaths {
    /// Returns the path to `Player.log`.
    pub fn player_log(&self) -> &Path {
        &self.player_log
    }

    /// Returns the path to `Player-prev.log`.
    pub fn player_prev_log(&self) -> &Path {
        &self.player_prev_log
    }
}

// ---------------------------------------------------------------------------
// DiscoveryError
// ---------------------------------------------------------------------------

/// Errors that can occur during log file discovery.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DiscoveryError {
    /// The platform-specific base directory could not be determined.
    ///
    /// On Windows this means `KnownFolder::LocalAppDataLow` failed to
    /// resolve. On macOS this means the `HOME` environment variable is
    /// not set.
    #[error("could not resolve platform log directory")]
    BaseDirNotFound,

    /// The resolved log file path does not exist on disk.
    ///
    /// Callers should notify the user (e.g., "MTG Arena not found") and
    /// poll periodically until the file appears.
    #[error("log file not found at {path}", path = path.display())]
    LogFileMissing {
        /// The expected path that was checked.
        path: PathBuf,
    },

    /// The current operating system is not supported.
    ///
    /// Only Windows, macOS, and Linux (Steam/Proton via `libraryfolders.vdf`,
    /// Lutris fallback) are supported targets.
    #[error("unsupported platform for log file discovery")]
    UnsupportedPlatform,
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Subdirectory components appended to the platform base directory.
const MTGA_LOG_DIR: &[&str] = &["Wizards Of The Coast", "MTGA"];

/// Name of the active log file.
const PLAYER_LOG: &str = "Player.log";

/// Name of the previous session's log file.
const PLAYER_PREV_LOG: &str = "Player-prev.log";

// ---------------------------------------------------------------------------
// Platform-specific base directory resolution
// ---------------------------------------------------------------------------

/// Resolves the platform base directory for MTGA logs on Windows.
///
/// Uses `KnownFolder::LocalAppDataLow` via the `known-folders` crate.
#[cfg(target_os = "windows")]
fn resolve_base_dir() -> Result<PathBuf, DiscoveryError> {
    known_folders::get_known_folder_path(known_folders::KnownFolder::LocalAppDataLow)
        .ok_or(DiscoveryError::BaseDirNotFound)
}

/// Resolves the platform base directory for MTGA logs on macOS.
///
/// Reads the `HOME` environment variable and appends `Library/Logs`.
#[cfg(target_os = "macos")]
fn resolve_base_dir() -> Result<PathBuf, DiscoveryError> {
    std::env::var("HOME")
        .ok()
        .map(|home| PathBuf::from(home).join("Library").join("Logs"))
        .ok_or(DiscoveryError::BaseDirNotFound)
}

/// Resolves the platform base directory for MTGA logs on Linux.
///
/// Probes every known Steam installation root plus the Lutris fallback, and
/// returns the `.../LocalLow` base dir for the candidate whose `Player.log`
/// has the most recent modification time. The Linux arm is existence- and
/// freshness-aware because the base directory must be discovered
/// dynamically (a Steam library can be on any disk, and a stale leftover
/// log in one candidate must not shadow a live log in another).
///
/// Returns [`DiscoveryError::LogFileMissing`] (not [`DiscoveryError::UnsupportedPlatform`])
/// when no candidate contains a `Player.log`, matching the absent-file
/// contract of the Windows/macOS arms.
#[cfg(target_os = "linux")]
fn resolve_base_dir() -> Result<PathBuf, DiscoveryError> {
    use crate::log::steam::{steam_libraries_for_appid, MTGA_APP_ID};

    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or(DiscoveryError::BaseDirNotFound)?;

    let steam_libraries = steam_libraries_for_appid(MTGA_APP_ID);
    let candidates = linux_candidate_base_dirs(&home, &steam_libraries);

    let mtga_tail = Path::new("Wizards Of The Coast")
        .join("MTGA")
        .join("Player.log");

    if let Some((base_dir, modified)) = select_freshest_candidate(&candidates, &mtga_tail) {
        ::log::info!(
            "Linux: discovered MTGA base dir: {} (Player.log modified {modified:?})",
            base_dir.display(),
        );
        return Ok(base_dir);
    }

    // Nothing found: report the first Steam-derived candidate as the
    // missing path if any Steam library was found (so the error points at
    // a real, if empty, Steam install), otherwise the Lutris fallback
    // (last candidate, always present — linux_candidate_base_dirs always
    // appends it), preserving today's error surface when no Steam install
    // exists at all.
    let lutris_locallow = home
        .join("Games")
        .join("magic-the-gathering-arena")
        .join("drive_c")
        .join("users")
        .join("steamuser")
        .join("AppData")
        .join("LocalLow");
    let missing_base = if steam_libraries.is_empty() {
        candidates.last()
    } else {
        candidates.first()
    };
    let missing_path = missing_base.map_or_else(
        || lutris_locallow.join(&mtga_tail),
        |base| base.join(&mtga_tail),
    );

    ::log::warn!(
        "Linux: no MTGA Player.log found; last checked: {}",
        missing_path.display()
    );
    Err(DiscoveryError::LogFileMissing { path: missing_path })
}

/// Returns the `.../LocalLow` candidate base directories for Linux MTGA
/// discovery: one candidate per discovered Steam library, plus the Lutris
/// fallback prefix.
///
/// This pure helper takes `home` and the already-discovered Steam
/// libraries so it can be unit-tested with temp directories without
/// mutating `$HOME`.
#[cfg(target_os = "linux")]
pub(crate) fn linux_candidate_base_dirs(home: &Path, steam_libraries: &[PathBuf]) -> Vec<PathBuf> {
    use crate::log::steam::MTGA_APP_ID;

    let mut candidates: Vec<PathBuf> = steam_libraries
        .iter()
        .map(|library| {
            library
                .join("steamapps")
                .join("compatdata")
                .join(MTGA_APP_ID.to_string())
                .join("pfx")
                .join("drive_c")
                .join("users")
                .join("steamuser")
                .join("AppData")
                .join("LocalLow")
        })
        .collect();

    // Lutris (fallback, UNVERIFIED)
    let lutris_locallow = home
        .join("Games")
        .join("magic-the-gathering-arena")
        .join("drive_c")
        .join("users")
        .join("steamuser")
        .join("AppData")
        .join("LocalLow");
    candidates.push(lutris_locallow);

    candidates
}

/// Selects the candidate whose `Player.log` (at `candidate.join(mtga_tail)`)
/// has the most recent modification time.
///
/// Returns the winning base dir and its `Player.log` modification time, or
/// `None` if no candidate has an existing, readable `Player.log`. Pure with
/// respect to global/env state (depends only on its arguments and
/// filesystem metadata), so it is unit-testable with tempdir fixtures.
#[cfg(target_os = "linux")]
fn select_freshest_candidate(
    candidates: &[PathBuf],
    mtga_tail: &Path,
) -> Option<(PathBuf, SystemTime)> {
    candidates
        .iter()
        .filter_map(|candidate| {
            let modified = std::fs::metadata(candidate.join(mtga_tail))
                .and_then(|m| m.modified())
                .ok()?;
            Some((candidate.clone(), modified))
        })
        .max_by_key(|(_, modified)| *modified)
}

/// Returns [`DiscoveryError::UnsupportedPlatform`] on non-Windows/macOS/Linux targets.
#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
fn resolve_base_dir() -> Result<PathBuf, DiscoveryError> {
    Err(DiscoveryError::UnsupportedPlatform)
}

// ---------------------------------------------------------------------------
// Path construction (platform-independent)
// ---------------------------------------------------------------------------

/// Builds [`LogPaths`] from a platform base directory.
///
/// Appends the MTGA-specific subdirectory components and log file names.
fn build_log_paths(base_dir: PathBuf) -> LogPaths {
    let mut mtga_dir = base_dir;
    for component in MTGA_LOG_DIR {
        mtga_dir.push(component);
    }
    LogPaths {
        player_log: mtga_dir.join(PLAYER_LOG),
        player_prev_log: mtga_dir.join(PLAYER_PREV_LOG),
    }
}

/// Checks whether the primary log file exists on disk.
///
/// Returns the paths on success, or [`DiscoveryError::LogFileMissing`]
/// if `Player.log` is not found.
fn check_existence(paths: LogPaths) -> Result<LogPaths, DiscoveryError> {
    if paths.player_log.exists() {
        ::log::info!("discovered log file: {}", paths.player_log.display());
        Ok(paths)
    } else {
        ::log::warn!("log file not found: {}", paths.player_log.display());
        Err(DiscoveryError::LogFileMissing {
            path: paths.player_log,
        })
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Resolves the expected platform-specific log file paths without checking
/// whether the files exist on disk.
///
/// Useful for displaying the expected path in configuration UI or logs.
/// Use [`discover_log_file`] to also verify the file exists.
///
/// Note: on Linux the base directory is auto-discovered (Steam library can
/// be on any configured disk), so the Linux arm performs existence checks
/// internally as part of resolution.  A successful return on Linux means
/// the file was found; a [`DiscoveryError::LogFileMissing`] return means
/// no candidate location contained a `Player.log`.
///
/// # Errors
///
/// - [`DiscoveryError::UnsupportedPlatform`] on platforms other than
///   Windows, macOS, and Linux.
/// - [`DiscoveryError::BaseDirNotFound`] if the platform base directory
///   cannot be resolved.
pub fn resolve_log_paths() -> Result<LogPaths, DiscoveryError> {
    let base_dir = resolve_base_dir()?;
    Ok(build_log_paths(base_dir))
}

/// Resolves the platform-specific `Player.log` path and verifies the file
/// exists on disk.
///
/// When this returns [`DiscoveryError::LogFileMissing`], callers should
/// notify the user and poll periodically (e.g., every 5 seconds) until
/// the file appears.
///
/// # Errors
///
/// - [`DiscoveryError::UnsupportedPlatform`] on platforms other than
///   Windows, macOS, and Linux.
/// - [`DiscoveryError::BaseDirNotFound`] if the platform base directory
///   cannot be resolved.
/// - [`DiscoveryError::LogFileMissing`] if the resolved path does not
///   exist on disk.
pub fn discover_log_file() -> Result<LogPaths, DiscoveryError> {
    let paths = resolve_log_paths()?;
    check_existence(paths)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    // -- Path construction (platform-independent) --

    #[test]
    fn test_build_log_paths_appends_mtga_components() {
        let base = PathBuf::from("/some/base");
        let paths = build_log_paths(base);
        assert_eq!(
            paths.player_log(),
            Path::new("/some/base/Wizards Of The Coast/MTGA/Player.log")
        );
        assert_eq!(
            paths.player_prev_log(),
            Path::new("/some/base/Wizards Of The Coast/MTGA/Player-prev.log")
        );
    }

    #[test]
    fn test_build_log_paths_windows_style_path() {
        let base = PathBuf::from(r"C:\Users\User\AppData\LocalLow");
        let paths = build_log_paths(base);

        // On all platforms, PathBuf joins with the OS separator, but the
        // path components are correct regardless.
        let log_str = paths.player_log().to_string_lossy();
        assert!(log_str.contains("Wizards Of The Coast"));
        assert!(log_str.contains("MTGA"));
        assert!(log_str.ends_with("Player.log"));
    }

    #[test]
    fn test_build_log_paths_macos_style_path() {
        let base = PathBuf::from("/Users/player/Library/Logs");
        let paths = build_log_paths(base);
        assert_eq!(
            paths.player_log(),
            Path::new("/Users/player/Library/Logs/Wizards Of The Coast/MTGA/Player.log")
        );
    }

    #[test]
    fn test_build_log_paths_both_files_share_directory() {
        let paths = build_log_paths(PathBuf::from("/base"));
        assert_eq!(
            paths.player_log().parent(),
            paths.player_prev_log().parent()
        );
    }

    #[test]
    fn test_build_log_paths_player_prev_log_correct_name() {
        let paths = build_log_paths(PathBuf::from("/base"));
        let filename = paths
            .player_prev_log()
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_default();
        assert_eq!(filename, "Player-prev.log");
    }

    #[test]
    fn test_build_log_paths_player_log_correct_name() {
        let paths = build_log_paths(PathBuf::from("/base"));
        let filename = paths
            .player_log()
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_default();
        assert_eq!(filename, "Player.log");
    }

    // -- LogPaths accessors --

    #[test]
    fn test_log_paths_clone_is_equal() {
        let paths = build_log_paths(PathBuf::from("/base"));
        let cloned = paths.clone();
        assert_eq!(paths, cloned);
    }

    // -- Existence check --

    #[test]
    fn test_check_existence_found_returns_ok() -> TestResult {
        let dir = tempfile::tempdir()?;
        let mtga_dir = dir.path().join("Wizards Of The Coast").join("MTGA");
        fs::create_dir_all(&mtga_dir)?;
        fs::write(mtga_dir.join("Player.log"), "test log data")?;

        let paths = build_log_paths(dir.path().to_path_buf());
        let result = check_existence(paths);
        assert!(result.is_ok());
        Ok(())
    }

    #[test]
    fn test_check_existence_found_returns_correct_paths() -> TestResult {
        let dir = tempfile::tempdir()?;
        let mtga_dir = dir.path().join("Wizards Of The Coast").join("MTGA");
        fs::create_dir_all(&mtga_dir)?;
        fs::write(mtga_dir.join("Player.log"), "data")?;

        let paths = build_log_paths(dir.path().to_path_buf());
        let found = check_existence(paths)?;
        assert_eq!(found.player_log(), mtga_dir.join("Player.log"));
        assert_eq!(found.player_prev_log(), mtga_dir.join("Player-prev.log"));
        Ok(())
    }

    #[test]
    fn test_check_existence_missing_returns_log_file_missing() -> TestResult {
        let dir = tempfile::tempdir()?;
        // Directory exists but Player.log does not.
        let paths = build_log_paths(dir.path().to_path_buf());
        let result = check_existence(paths);
        assert!(matches!(result, Err(DiscoveryError::LogFileMissing { .. })));
        Ok(())
    }

    #[test]
    fn test_check_existence_missing_error_contains_expected_path() -> TestResult {
        let dir = tempfile::tempdir()?;
        let paths = build_log_paths(dir.path().to_path_buf());
        let expected_path = paths.player_log().to_path_buf();

        match check_existence(paths) {
            Err(DiscoveryError::LogFileMissing { path }) => {
                assert_eq!(path, expected_path);
            }
            other => return Err(format!("expected LogFileMissing, got: {other:?}").into()),
        }
        Ok(())
    }

    #[test]
    fn test_check_existence_directory_exists_but_no_file() -> TestResult {
        let dir = tempfile::tempdir()?;
        let mtga_dir = dir.path().join("Wizards Of The Coast").join("MTGA");
        fs::create_dir_all(&mtga_dir)?;
        // Directory exists but Player.log does not.

        let paths = build_log_paths(dir.path().to_path_buf());
        let result = check_existence(paths);
        assert!(matches!(result, Err(DiscoveryError::LogFileMissing { .. })));
        Ok(())
    }

    // -- DiscoveryError display --

    #[test]
    fn test_discovery_error_base_dir_not_found_display() {
        let err = DiscoveryError::BaseDirNotFound;
        assert_eq!(err.to_string(), "could not resolve platform log directory");
    }

    #[test]
    fn test_discovery_error_unsupported_platform_display() {
        let err = DiscoveryError::UnsupportedPlatform;
        assert_eq!(
            err.to_string(),
            "unsupported platform for log file discovery"
        );
    }

    #[test]
    fn test_discovery_error_log_file_missing_display() {
        let err = DiscoveryError::LogFileMissing {
            path: PathBuf::from("/some/path/Player.log"),
        };
        let display = err.to_string();
        assert!(display.contains("/some/path/Player.log"));
        assert!(display.contains("log file not found"));
    }

    // -- DiscoveryError properties --

    #[test]
    fn test_discovery_error_clone_is_equal() {
        let err = DiscoveryError::LogFileMissing {
            path: PathBuf::from("/test"),
        };
        let cloned = err.clone();
        assert_eq!(err, cloned);
    }

    // -- Platform-specific resolution --

    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    #[test]
    fn test_resolve_log_paths_unsupported_platform() {
        let result = resolve_log_paths();
        assert!(matches!(result, Err(DiscoveryError::UnsupportedPlatform)));
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    #[test]
    fn test_discover_log_file_unsupported_platform() {
        let result = discover_log_file();
        assert!(matches!(result, Err(DiscoveryError::UnsupportedPlatform)));
    }

    // -- Linux candidate base dir discovery (pure helper, tempdir-safe) --

    #[cfg(target_os = "linux")]
    mod linux_discovery {
        use super::*;
        use crate::log::discovery::linux_candidate_base_dirs;
        use crate::log::steam::MTGA_APP_ID;
        use std::fs;
        use std::time::Duration;

        type TestResult = Result<(), Box<dyn std::error::Error>>;

        /// Build the full Player.log path under a `LocalLow` base dir.
        fn player_log_path(locallow: &Path) -> PathBuf {
            locallow
                .join("Wizards Of The Coast")
                .join("MTGA")
                .join("Player.log")
        }

        #[test]
        fn test_linux_candidate_base_dirs_one_candidate_per_library_then_lutris_last() {
            let home = PathBuf::from("/home/testuser");
            let libraries = vec![
                PathBuf::from("/mnt/games/SteamLibrary"),
                PathBuf::from("/home/testuser/.local/share/Steam"),
            ];
            let candidates = linux_candidate_base_dirs(&home, &libraries);

            assert_eq!(candidates.len(), 3);
            // One candidate per library, in order, each under its own library.
            assert!(candidates[0].starts_with(&libraries[0]));
            assert!(candidates[0]
                .to_string_lossy()
                .contains("steamapps/compatdata"));
            assert!(candidates[0]
                .to_string_lossy()
                .contains(&MTGA_APP_ID.to_string()));
            assert!(candidates[1].starts_with(&libraries[1]));
            assert!(candidates[1]
                .to_string_lossy()
                .contains(&MTGA_APP_ID.to_string()));
            // Lutris fallback is always last.
            assert!(candidates[2]
                .to_string_lossy()
                .contains("Games/magic-the-gathering-arena"));
        }

        #[test]
        fn test_linux_candidate_base_dirs_no_steam_only_lutris() {
            let home = PathBuf::from("/home/testuser");
            let candidates = linux_candidate_base_dirs(&home, &[]);

            assert_eq!(candidates.len(), 1);
            assert!(candidates[0]
                .to_string_lossy()
                .contains("Games/magic-the-gathering-arena"));
        }

        #[test]
        fn test_select_freshest_candidate_no_existing_logs_returns_none() -> TestResult {
            let tmp = tempfile::tempdir()?;
            let candidates = vec![tmp.path().join("a"), tmp.path().join("b")];
            let mtga_tail = Path::new("Wizards Of The Coast")
                .join("MTGA")
                .join("Player.log");

            assert!(select_freshest_candidate(&candidates, &mtga_tail).is_none());
            Ok(())
        }

        #[test]
        fn test_select_freshest_candidate_single_existing_log_is_selected() -> TestResult {
            let tmp = tempfile::tempdir()?;
            let only = tmp.path().join("only");
            fs::create_dir_all(player_log_path(&only).parent().unwrap_or(&only))?;
            fs::write(player_log_path(&only), "test")?;
            let candidates = vec![only.clone()];
            let mtga_tail = Path::new("Wizards Of The Coast")
                .join("MTGA")
                .join("Player.log");

            match select_freshest_candidate(&candidates, &mtga_tail) {
                Some((base_dir, _)) => assert_eq!(base_dir, only),
                None => {
                    return Err(
                        "expected the only candidate with a Player.log to be selected".into(),
                    )
                }
            }
            Ok(())
        }

        #[test]
        fn test_select_freshest_candidate_prefers_freshest_even_when_listed_first() -> TestResult {
            // Regression coverage for the stale-fallback-shadowing bug: a
            // candidate that is *listed first* but has a *stale* Player.log
            // must lose to a later-listed candidate with a *fresher* one —
            // selection is by mtime, not list order / first-exists.
            let tmp = tempfile::tempdir()?;
            let stale = tmp.path().join("stale-first");
            let fresh = tmp.path().join("fresh-second");
            let mtga_tail = Path::new("Wizards Of The Coast")
                .join("MTGA")
                .join("Player.log");

            for base in [&stale, &fresh] {
                let log_path = base.join(&mtga_tail);
                fs::create_dir_all(log_path.parent().unwrap_or(base))?;
            }
            fs::write(stale.join(&mtga_tail), "old leftover log")?;
            std::thread::sleep(Duration::from_millis(50));
            fs::write(fresh.join(&mtga_tail), "live log")?;

            let candidates = vec![stale.clone(), fresh.clone()];
            match select_freshest_candidate(&candidates, &mtga_tail) {
                Some((base_dir, _)) => assert_eq!(base_dir, fresh),
                None => return Err("expected a candidate to be selected".into()),
            }
            Ok(())
        }

        #[test]
        fn test_linux_player_log_path_helper_appends_correctly() {
            let base = PathBuf::from("/base/LocalLow");
            let log_path = player_log_path(&base);
            assert_eq!(
                log_path,
                PathBuf::from("/base/LocalLow/Wizards Of The Coast/MTGA/Player.log")
            );
        }
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn test_resolve_log_paths_windows_contains_locallow() -> TestResult {
        let paths = resolve_log_paths()?;
        let log_str = paths.player_log().to_string_lossy();
        assert!(
            log_str.contains("LocalLow"),
            "Windows path should contain LocalLow: {log_str}"
        );
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn test_resolve_log_paths_macos_contains_library_logs() -> TestResult {
        let paths = resolve_log_paths()?;
        let log_str = paths.player_log().to_string_lossy();
        assert!(
            log_str.contains("Library/Logs"),
            "macOS path should contain Library/Logs: {log_str}"
        );
        Ok(())
    }

    // -- Integration: discover_log_file --

    #[test]
    fn test_discover_log_file_returns_error_in_ci() {
        // On unsupported platforms, discover_log_file returns
        // UnsupportedPlatform. On supported platforms, it returns
        // LogFileMissing because MTGA is not installed in CI.
        let result = discover_log_file();
        assert!(result.is_err());
    }
}
