//! Steam library discovery helpers for Linux.
//!
//! Parses `~/.local/share/Steam/steamapps/libraryfolders.vdf` to find the
//! Steam library that contains a given app ID, then returns the library root
//! path.  This is required because Steam allows users to install games on
//! any configured library disk — the default `~/.local/share/Steam` path is
//! not guaranteed to contain a given title.
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

use std::path::{Path, PathBuf};

/// MTGA's Steam App ID.
pub(crate) const MTGA_APP_ID: u32 = 2_141_910;

/// Locate the Steam library root that contains `appid`.
///
/// Parses `~/.local/share/Steam/steamapps/libraryfolders.vdf` and returns the
/// `path` value of the first library entry whose `apps` block contains the
/// given `appid`.
///
/// Falls back to `~/.local/share/Steam` if:
/// - the VDF file is missing / unreadable, **and**
/// - `<default_steam>/steamapps/compatdata/<appid>` exists.
///
/// Returns `None` if neither the VDF nor the fallback resolve to a library
/// that contains the app's `compatdata` directory.
pub(crate) fn steam_library_for_appid(appid: u32) -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    let default_steam = home.join(".local").join("share").join("Steam");
    let vdf_path = default_steam.join("steamapps").join("libraryfolders.vdf");

    // Try to parse the VDF and find the library that contains appid.
    if vdf_path.exists() {
        if let Ok(contents) = std::fs::read_to_string(&vdf_path) {
            if let Some(library) = parse_library_for_appid(&contents, appid) {
                return Some(library);
            }
        }
    }

    // Fallback: check the default Steam library directly.
    let compat = default_compat_path(&default_steam, appid);
    if compat.exists() {
        return Some(default_steam);
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
}
