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
/// Tries candidates in order (Steam/Proton first, then Lutris) and returns
/// the `.../LocalLow` base dir for the first candidate whose `Player.log`
/// exists.  The Linux arm is existence-aware because the base directory
/// must be discovered dynamically (Steam library can be on any disk).
///
/// Returns [`DiscoveryError::LogFileMissing`] (not [`DiscoveryError::UnsupportedPlatform`])
/// when no candidate contains a `Player.log`, matching the absent-file
/// contract of the Windows/macOS arms.
#[cfg(target_os = "linux")]
fn resolve_base_dir() -> Result<PathBuf, DiscoveryError> {
    use crate::log::steam::{steam_library_for_appid, MTGA_APP_ID};

    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or(DiscoveryError::BaseDirNotFound)?;

    let steam_lib = steam_library_for_appid(MTGA_APP_ID);
    let candidates = linux_candidate_base_dirs(&home, steam_lib.as_deref());

    let mtga_tail = Path::new("Wizards Of The Coast")
        .join("MTGA")
        .join("Player.log");

    for candidate in &candidates {
        if candidate.join(&mtga_tail).exists() {
            ::log::info!("Linux: discovered MTGA base dir: {}", candidate.display());
            return Ok(candidate.clone());
        }
    }

    // Nothing found: report the Lutris path (last candidate) as the missing
    // path so callers can surface a useful "file not found" message.
    // The Lutris candidate is always present in the list (linux_candidate_base_dirs
    // always appends it), so last() is always Some; fall back to building the
    // Lutris path directly if the vector were somehow empty.
    let lutris_locallow = home
        .join("Games")
        .join("magic-the-gathering-arena")
        .join("drive_c")
        .join("users")
        .join("steamuser")
        .join("AppData")
        .join("LocalLow");
    let missing_path = candidates.into_iter().last().map_or_else(
        || lutris_locallow.join(&mtga_tail),
        |base| base.join(&mtga_tail),
    );

    ::log::warn!(
        "Linux: no MTGA Player.log found; last checked: {}",
        missing_path.display()
    );
    Err(DiscoveryError::LogFileMissing { path: missing_path })
}

/// Returns the ordered `.../LocalLow` candidate base directories for Linux
/// MTGA discovery (Steam/Proton first, Lutris second).
///
/// This pure helper takes `home` and an optional `steam_lib` so it can be
/// unit-tested with temp directories without mutating `$HOME`.
#[cfg(target_os = "linux")]
pub(crate) fn linux_candidate_base_dirs(home: &Path, steam_lib: Option<&Path>) -> Vec<PathBuf> {
    let mut candidates = Vec::new();

    // Steam/Proton (primary)
    if let Some(library) = steam_lib {
        let locallow = library
            .join("steamapps")
            .join("compatdata")
            .join("2141910")
            .join("pfx")
            .join("drive_c")
            .join("users")
            .join("steamuser")
            .join("AppData")
            .join("LocalLow");
        candidates.push(locallow);
    }

    // Lutris (fallback, UNVERIFIED — manasight/manasight-docs#772)
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
        use std::fs;

        type TestResult = Result<(), Box<dyn std::error::Error>>;

        /// Build the full Player.log path under a LocalLow base dir.
        fn player_log_path(locallow: &Path) -> PathBuf {
            locallow
                .join("Wizards Of The Coast")
                .join("MTGA")
                .join("Player.log")
        }

        #[test]
        fn test_linux_candidate_base_dirs_steam_first_lutris_second() {
            let home = PathBuf::from("/home/testuser");
            let steam_lib = PathBuf::from("/mnt/games/SteamLibrary");
            let candidates = linux_candidate_base_dirs(&home, Some(&steam_lib));

            assert_eq!(candidates.len(), 2);
            // Steam candidate is first
            assert!(candidates[0]
                .to_string_lossy()
                .contains("steamapps/compatdata"));
            assert!(candidates[0].to_string_lossy().contains("2141910"));
            // Lutris candidate is second
            assert!(candidates[1]
                .to_string_lossy()
                .contains("Games/magic-the-gathering-arena"));
        }

        #[test]
        fn test_linux_candidate_base_dirs_no_steam_only_lutris() {
            let home = PathBuf::from("/home/testuser");
            let candidates = linux_candidate_base_dirs(&home, None);

            assert_eq!(candidates.len(), 1);
            assert!(candidates[0]
                .to_string_lossy()
                .contains("Games/magic-the-gathering-arena"));
        }

        #[test]
        fn test_linux_resolve_base_dir_steam_found() -> TestResult {
            let tmp = tempfile::tempdir()?;
            let steam_lib = tmp.path().join("SteamLibrary");
            let locallow = steam_lib
                .join("steamapps")
                .join("compatdata")
                .join("2141910")
                .join("pfx")
                .join("drive_c")
                .join("users")
                .join("steamuser")
                .join("AppData")
                .join("LocalLow");
            let mtga_dir = locallow.join("Wizards Of The Coast").join("MTGA");
            fs::create_dir_all(&mtga_dir)?;
            fs::write(mtga_dir.join("Player.log"), "test")?;

            let candidates = linux_candidate_base_dirs(tmp.path(), Some(&steam_lib));
            let mtga_tail = Path::new("Wizards Of The Coast")
                .join("MTGA")
                .join("Player.log");

            let found = candidates.iter().find(|c| c.join(&mtga_tail).exists());
            assert!(found.is_some(), "Steam candidate should be found");
            assert_eq!(found, Some(&locallow));
            Ok(())
        }

        #[test]
        fn test_linux_resolve_base_dir_lutris_fallback() -> TestResult {
            let tmp = tempfile::tempdir()?;
            // No steam lib; only Lutris
            let lutris_locallow = tmp
                .path()
                .join("Games")
                .join("magic-the-gathering-arena")
                .join("drive_c")
                .join("users")
                .join("steamuser")
                .join("AppData")
                .join("LocalLow");
            let mtga_dir = lutris_locallow.join("Wizards Of The Coast").join("MTGA");
            fs::create_dir_all(&mtga_dir)?;
            fs::write(mtga_dir.join("Player.log"), "test")?;

            let candidates = linux_candidate_base_dirs(tmp.path(), None);
            let mtga_tail = Path::new("Wizards Of The Coast")
                .join("MTGA")
                .join("Player.log");

            let found = candidates.iter().find(|c| c.join(&mtga_tail).exists());
            assert!(found.is_some(), "Lutris candidate should be found");
            assert_eq!(found, Some(&lutris_locallow));
            Ok(())
        }

        #[test]
        fn test_linux_candidate_base_dirs_nothing_found_no_candidates_match() -> TestResult {
            let tmp = tempfile::tempdir()?;
            // No Player.log anywhere
            let steam_lib = tmp.path().join("SteamLibrary");
            let candidates = linux_candidate_base_dirs(tmp.path(), Some(&steam_lib));
            let mtga_tail = Path::new("Wizards Of The Coast")
                .join("MTGA")
                .join("Player.log");

            let found = candidates.iter().find(|c| c.join(&mtga_tail).exists());
            assert!(
                found.is_none(),
                "No candidate should match when files don't exist"
            );
            Ok(())
        }

        #[test]
        fn test_linux_candidate_steam_path_contains_correct_appid() {
            let home = PathBuf::from("/home/user");
            let steam_lib = PathBuf::from("/home/user/.local/share/Steam");
            let candidates = linux_candidate_base_dirs(&home, Some(&steam_lib));

            assert!(!candidates.is_empty());
            let steam_candidate = &candidates[0];
            assert!(
                steam_candidate.to_string_lossy().contains("2141910"),
                "Steam candidate must use MTGA app id 2141910: {}",
                steam_candidate.display()
            );
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
