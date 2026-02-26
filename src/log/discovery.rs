//! Platform-specific log file path resolution.
//!
//! Resolves the default location of MTG Arena's `Player.log` on each
//! supported platform (Windows via `known-folders`, macOS via `~/Library/Logs/`).
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
    /// Only Windows and macOS are supported targets.
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

/// Returns [`DiscoveryError::UnsupportedPlatform`] on non-Windows/macOS targets.
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
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
/// # Errors
///
/// - [`DiscoveryError::UnsupportedPlatform`] on platforms other than
///   Windows and macOS.
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
/// - [`DiscoveryError::UnsupportedPlatform`] on unsupported platforms.
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

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    #[test]
    fn test_resolve_log_paths_unsupported_platform() {
        let result = resolve_log_paths();
        assert!(matches!(result, Err(DiscoveryError::UnsupportedPlatform)));
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    #[test]
    fn test_discover_log_file_unsupported_platform() {
        let result = discover_log_file();
        assert!(matches!(result, Err(DiscoveryError::UnsupportedPlatform)));
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
