//! Steam library discovery helpers for Linux.
//!
//! Probes every known Steam installation root (native, the `~/.steam/root`
//! and `~/.steam/steam` symlinks, Flatpak, and Snap), parsing each root's
//! `steamapps/libraryfolders.vdf` to find the Steam library that contains a
//! given app ID. This is required because Steam allows users to install
//! games on any configured library disk — the default
//! `~/.local/share/Steam` path is not guaranteed to contain a given title,
//! and not every install uses that default root at all.
//!
//! The VDF format used by `libraryfolders.vdf` is a subset of Valve's
//! `KeyValues` text format (VDF v1).  We use a hand-rolled line scanner rather
//! than a full parser because the file is small (<100 lines), the structure
//! is well-known, and adding a VDF crate dependency is unnecessary overhead.
//!
//! ## VDF structure (Steam client 2024+)
//!
//! ```text
//! "libraryfolders"
//! {
//!     "0"
//!     {
//!         "path"    "/home/user/.local/share/Steam"
//!         "apps"
//!         {
//!             "2141910"    "12345678"
//!             "730"        "87654321"
//!         }
//!     }
//!     "1"
//!     {
//!         "path"    "/mnt/games/SteamLibrary"
//!         "apps"
//!         {
//!             "2141910"    "12345678"
//!         }
//!     }
//! }
//! ```

use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// MTGA's Steam App ID.
pub(crate) const MTGA_APP_ID: u32 = 2_141_910;

/// Locate every Steam library that contains `appid`, probing all known
/// Steam installation roots.
///
/// Reads `$HOME` and delegates to [`steam_libraries_for_appid_in`]. Returns
/// an empty `Vec` if `$HOME` is unset.
pub(crate) fn steam_libraries_for_appid(appid: u32) -> Vec<PathBuf> {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return Vec::new();
    };
    steam_libraries_for_appid_in(&home, appid)
}

/// Locate every Steam library that contains `appid`, given a home
/// directory.
///
/// Probes, in order: `~/.steam/root`, `~/.steam/steam` (the two common
/// Steam symlinks), `~/.local/share/Steam` (native default), the Flatpak
/// install path, and the Snap install path. Roots that don't exist and
/// roots that canonicalize to a real directory already probed via another
/// alias are skipped. For each remaining root, parses
/// `steamapps/libraryfolders.vdf` for a library containing `appid`, falling
/// back to the root itself if `steamapps/compatdata/<appid>` exists there
/// directly.
///
/// Takes `home` as a parameter (rather than reading `$HOME` directly) so it
/// can be exercised with tempdir fixtures — VDF parsing and compatdata
/// fallback included — without mutating the process environment.
pub(crate) fn steam_libraries_for_appid_in(home: &Path, appid: u32) -> Vec<PathBuf> {
    let mut seen_roots: HashSet<PathBuf> = HashSet::new();
    let mut libraries = Vec::new();

    for root in candidate_steam_roots(home) {
        let Ok(canonical_root) = std::fs::canonicalize(&root) else {
            continue; // doesn't exist, or a broken symlink
        };
        if !seen_roots.insert(canonical_root.clone()) {
            continue; // already probed this real directory via another root alias
        }
        if let Some(library) = library_for_root(&canonical_root, appid) {
            libraries.push(library);
        }
    }

    libraries
}

/// Ordered set of Steam installation roots to probe, before existence
/// checking and canonical-path deduplication.
fn candidate_steam_roots(home: &Path) -> Vec<PathBuf> {
    vec![
        home.join(".steam").join("root"),
        home.join(".steam").join("steam"),
        home.join(".local").join("share").join("Steam"),
        home.join(".var")
            .join("app")
            .join("com.valvesoftware.Steam")
            .join(".local")
            .join("share")
            .join("Steam"),
        home.join("snap")
            .join("steam")
            .join("common")
            .join(".local")
            .join("share")
            .join("Steam"),
    ]
}

/// Checks a single, already-canonicalized Steam root for a library
/// containing `appid`: first via `libraryfolders.vdf`, then via the
/// `compatdata` fallback (the root itself, if present).
fn library_for_root(root: &Path, appid: u32) -> Option<PathBuf> {
    let vdf_path = root.join("steamapps").join("libraryfolders.vdf");
    if vdf_path.exists() {
        if let Ok(contents) = std::fs::read_to_string(&vdf_path) {
            if let Some(library) = parse_library_for_appid(&contents, appid) {
                return Some(library);
            }
        }
    }

    // Fallback: check this root directly.
    if default_compat_path(root, appid).exists() {
        return Some(root.to_path_buf());
    }

    None
}

/// Parse `libraryfolders.vdf` content and return the library `path` whose
/// `apps` block contains `appid`.
///
/// This is a pure function (no filesystem I/O) for testability.
pub(crate) fn parse_library_for_appid(vdf_content: &str, appid: u32) -> Option<PathBuf> {
    let appid_str = appid.to_string();

    let mut current_path: Option<String> = None;
    let mut in_apps_block = false;
    let mut brace_depth: u32 = 0;
    // Depth at which the `apps` block opened (so we know when to exit it)
    let mut apps_depth: u32 = 0;

    for raw_line in vdf_content.lines() {
        let line = raw_line.trim();

        if line == "{" {
            brace_depth += 1;
            continue;
        }

        if line == "}" {
            if in_apps_block && brace_depth == apps_depth {
                // Exiting the apps block without finding the appid.
                in_apps_block = false;
                current_path = None;
            }
            brace_depth = brace_depth.saturating_sub(1);
            continue;
        }

        if in_apps_block {
            // Lines inside `apps { }` look like: `"<appid>"    "<bytes>"`
            // Use full-string equality to avoid prefix collisions (e.g. "21419100").
            if let Some(found_id) = extract_first_quoted(line) {
                if found_id == appid_str {
                    return current_path.map(PathBuf::from);
                }
            }
            continue;
        }

        // Outside apps block: look for `"path"` and `"apps"` keys.
        //
        // VDF has two line shapes:
        //   key-value: `"key"    "value"` — handled by extract_key_value
        //   section header: `"apps"` (no value) — extract_key_value returns None
        //     for these because there is no second quoted token; handle them
        //     separately so that `in_apps_block` is set correctly.
        if extract_first_quoted(line) == Some("apps") && extract_key_value(line).is_none() {
            // The `{` for the apps block appears on the *next* line.
            in_apps_block = true;
            apps_depth = brace_depth + 1;
        } else if let Some((key, value)) = extract_key_value(line) {
            if key == "path" {
                current_path = Some(value.to_string());
            }
        }
    }

    None
}

/// Extract `("key", "value")` from a VDF line like `"key"    "value"`.
///
/// Returns `None` if the line doesn't match the two-token pattern.
fn extract_key_value(line: &str) -> Option<(&str, &str)> {
    let (key, rest) = extract_quoted_and_rest(line)?;
    let value = extract_first_quoted(rest.trim_start())?;
    Some((key, value))
}

/// Extract the content of the first `"..."` quoted token in `s`.
///
/// Returns the content *without* surrounding quotes, or `None`.
fn extract_first_quoted(s: &str) -> Option<&str> {
    let start = s.find('"')? + 1;
    let end = s[start..].find('"')? + start;
    Some(&s[start..end])
}

/// Extract the first quoted token and the remainder of the string after it.
fn extract_quoted_and_rest(s: &str) -> Option<(&str, &str)> {
    let open = s.find('"')? + 1;
    let close = s[open..].find('"')? + open;
    Some((&s[open..close], &s[close + 1..]))
}

/// Build the default compatdata path for an app under a Steam library root.
fn default_compat_path(steam_root: &Path, appid: u32) -> PathBuf {
    steam_root
        .join("steamapps")
        .join("compatdata")
        .join(appid.to_string())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_VDF_SINGLE: &str = r#"
"libraryfolders"
{
    "0"
    {
        "path"    "/home/user/.local/share/Steam"
        "label"    ""
        "contentid"    "123"
        "totalsize"    "0"
        "update_clean_bytes_tally"    "0"
        "time_last_update_corruption"    "0"
        "apps"
        {
            "228980"    "184139082"
            "2141910"    "12345678"
            "730"    "87654321"
        }
    }
}
"#;

    const SAMPLE_VDF_MULTI: &str = r#"
"libraryfolders"
{
    "0"
    {
        "path"    "/home/user/.local/share/Steam"
        "apps"
        {
            "730"    "87654321"
        }
    }
    "1"
    {
        "path"    "/mnt/games/SteamLibrary"
        "apps"
        {
            "2141910"    "12345678"
            "440"    "99999999"
        }
    }
}
"#;

    const SAMPLE_VDF_NO_MTGA: &str = r#"
"libraryfolders"
{
    "0"
    {
        "path"    "/home/user/.local/share/Steam"
        "apps"
        {
            "730"    "87654321"
        }
    }
}
"#;

    #[test]
    fn test_parse_library_single_library_contains_mtga() {
        let result = parse_library_for_appid(SAMPLE_VDF_SINGLE, 2_141_910);
        assert_eq!(result, Some(PathBuf::from("/home/user/.local/share/Steam")));
    }

    #[test]
    fn test_parse_library_multi_library_mtga_on_second() {
        let result = parse_library_for_appid(SAMPLE_VDF_MULTI, 2_141_910);
        assert_eq!(result, Some(PathBuf::from("/mnt/games/SteamLibrary")));
    }

    #[test]
    fn test_parse_library_other_app_on_first() {
        let result = parse_library_for_appid(SAMPLE_VDF_MULTI, 730);
        assert_eq!(result, Some(PathBuf::from("/home/user/.local/share/Steam")));
    }

    #[test]
    fn test_parse_library_app_not_found_returns_none() {
        let result = parse_library_for_appid(SAMPLE_VDF_NO_MTGA, 2_141_910);
        assert_eq!(result, None);
    }

    #[test]
    fn test_parse_library_empty_vdf_returns_none() {
        let result = parse_library_for_appid("", 2_141_910);
        assert_eq!(result, None);
    }

    #[test]
    fn test_parse_library_malformed_vdf_returns_none() {
        let result = parse_library_for_appid("not valid vdf content !!!", 2_141_910);
        assert_eq!(result, None);
    }

    #[test]
    fn test_parse_library_prefix_collision_does_not_match() {
        // "21419100" must NOT match app id 2141910 — full-string equality check.
        const VDF_WITH_SIMILAR_ID: &str = r#"
"libraryfolders"
{
    "0"
    {
        "path"    "/home/user/.local/share/Steam"
        "apps"
        {
            "21419100"    "12345678"
        }
    }
}
"#;
        let result = parse_library_for_appid(VDF_WITH_SIMILAR_ID, 2_141_910);
        assert_eq!(result, None);
    }

    #[test]
    fn test_extract_first_quoted_normal() {
        assert_eq!(extract_first_quoted(r#""hello""#), Some("hello"));
    }

    #[test]
    fn test_extract_first_quoted_with_whitespace() {
        assert_eq!(extract_first_quoted(r#"  "my value"   "#), Some("my value"));
    }

    #[test]
    fn test_extract_first_quoted_no_quotes_returns_none() {
        assert_eq!(extract_first_quoted("no quotes here"), None);
    }

    #[test]
    fn test_extract_key_value_normal() {
        let result = extract_key_value(r#"        "path"    "/home/user/.local/share/Steam""#);
        assert_eq!(result, Some(("path", "/home/user/.local/share/Steam")));
    }

    #[test]
    fn test_extract_key_value_only_key_no_value_returns_none() {
        // A line with only one quoted token (e.g., `"apps"`) has no value.
        let result = extract_key_value(r#"        "apps""#);
        assert_eq!(result, None);
    }

    // -- Root probing: steam_libraries_for_appid_in (tempdir fixtures) --

    mod root_probing {
        use super::*;
        use std::fs;
        use std::os::unix::fs::symlink;

        type TestResult = Result<(), Box<dyn std::error::Error>>;

        /// Write a minimal single-library `libraryfolders.vdf` under
        /// `root/steamapps/` mapping `appid` to `library_path`.
        fn write_vdf(root: &Path, appid: u32, library_path: &Path) -> std::io::Result<()> {
            let steamapps = root.join("steamapps");
            fs::create_dir_all(&steamapps)?;
            let path = library_path.display();
            let vdf = format!(
                r#"
"libraryfolders"
{{
    "0"
    {{
        "path"    "{path}"
        "apps"
        {{
            "{appid}"    "12345678"
        }}
    }}
}}
"#
            );
            fs::write(steamapps.join("libraryfolders.vdf"), vdf)
        }

        /// Create `root/steamapps/compatdata/<appid>/` so the compatdata
        /// fallback matches `root` directly (no VDF involved).
        fn write_compatdata(root: &Path, appid: u32) -> std::io::Result<()> {
            fs::create_dir_all(
                root.join("steamapps")
                    .join("compatdata")
                    .join(appid.to_string()),
            )
        }

        #[test]
        fn test_steam_libraries_for_appid_in_default_root_vdf_match_returns_library() -> TestResult
        {
            let home = tempfile::tempdir()?;
            let default_root = home.path().join(".local").join("share").join("Steam");
            fs::create_dir_all(&default_root)?;
            let library = home.path().join("ExternalLibrary");
            write_vdf(&default_root, MTGA_APP_ID, &library)?;

            let result = steam_libraries_for_appid_in(home.path(), MTGA_APP_ID);
            assert_eq!(result, vec![library]);
            Ok(())
        }

        #[test]
        fn test_steam_libraries_for_appid_in_flatpak_root_vdf_match_returns_library() -> TestResult
        {
            let home = tempfile::tempdir()?;
            let flatpak_root = home
                .path()
                .join(".var")
                .join("app")
                .join("com.valvesoftware.Steam")
                .join(".local")
                .join("share")
                .join("Steam");
            fs::create_dir_all(&flatpak_root)?;
            let library = home.path().join("FlatpakLibrary");
            write_vdf(&flatpak_root, MTGA_APP_ID, &library)?;

            let result = steam_libraries_for_appid_in(home.path(), MTGA_APP_ID);
            assert_eq!(result, vec![library]);
            Ok(())
        }

        #[test]
        fn test_steam_libraries_for_appid_in_deb_symlink_root_compatdata_fallback_returns_root(
        ) -> TestResult {
            let home = tempfile::tempdir()?;
            // .deb installs live at ~/.steam/debian-installation, referenced
            // via the ~/.steam/steam symlink.
            let real_root = home.path().join(".steam").join("debian-installation");
            fs::create_dir_all(&real_root)?;
            write_compatdata(&real_root, MTGA_APP_ID)?;
            symlink(&real_root, home.path().join(".steam").join("steam"))?;

            let result = steam_libraries_for_appid_in(home.path(), MTGA_APP_ID);
            let expected_root = fs::canonicalize(&real_root)?;
            assert_eq!(result, vec![expected_root]);
            Ok(())
        }

        #[test]
        fn test_steam_libraries_for_appid_in_snap_root_compatdata_fallback_returns_root(
        ) -> TestResult {
            let home = tempfile::tempdir()?;
            let snap_root = home
                .path()
                .join("snap")
                .join("steam")
                .join("common")
                .join(".local")
                .join("share")
                .join("Steam");
            fs::create_dir_all(&snap_root)?;
            write_compatdata(&snap_root, MTGA_APP_ID)?;

            let result = steam_libraries_for_appid_in(home.path(), MTGA_APP_ID);
            let expected_root = fs::canonicalize(&snap_root)?;
            assert_eq!(result, vec![expected_root]);
            Ok(())
        }

        #[test]
        fn test_steam_libraries_for_appid_in_symlinked_roots_dedup_returns_single_library(
        ) -> TestResult {
            let home = tempfile::tempdir()?;
            let real_root = home.path().join("RealSteamInstall");
            fs::create_dir_all(&real_root)?;
            write_compatdata(&real_root, MTGA_APP_ID)?;

            let dot_steam = home.path().join(".steam");
            fs::create_dir_all(&dot_steam)?;
            symlink(&real_root, dot_steam.join("root"))?;
            symlink(&real_root, dot_steam.join("steam"))?;

            let result = steam_libraries_for_appid_in(home.path(), MTGA_APP_ID);
            // `.steam/root` and `.steam/steam` both resolve to the same real
            // directory: probing must collapse them into a single library,
            // not report it twice.
            assert_eq!(result.len(), 1);
            Ok(())
        }

        #[test]
        fn test_steam_libraries_for_appid_in_no_roots_exist_returns_empty() -> TestResult {
            let home = tempfile::tempdir()?;
            let result = steam_libraries_for_appid_in(home.path(), MTGA_APP_ID);
            assert!(result.is_empty());
            Ok(())
        }

        #[test]
        fn test_steam_libraries_for_appid_in_root_without_match_excluded_from_results() -> TestResult
        {
            let home = tempfile::tempdir()?;
            let default_root = home.path().join(".local").join("share").join("Steam");
            fs::create_dir_all(&default_root)?;
            // VDF exists but has no entry for our appid, and no compatdata
            // fallback directory either.
            write_vdf(&default_root, 730, &home.path().join("OtherLibrary"))?;

            let result = steam_libraries_for_appid_in(home.path(), MTGA_APP_ID);
            assert!(result.is_empty());
            Ok(())
        }
    }
}
