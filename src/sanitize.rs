//! Privacy scrubber for raw MTGA log text.
//!
//! Strips sensitive data (auth tokens, bearer tokens, OS-specific user paths,
//! session identifiers, display names, deck names, email addresses, IP
//! addresses, and hardware fingerprint lines) from unstructured `Player.log`
//! text. This is a best-effort filter; novel token formats may slip through.
//!
//! Regex patterns are compiled once via [`std::sync::LazyLock`] and reused
//! across all calls.

use std::sync::LazyLock;

use regex::Regex;

/// A compiled regex pattern paired with its replacement string.
struct ScrubPattern {
    regex: Regex,
    replacement: &'static str,
    /// When `true`, this pattern redacts a player display name field
    /// (`screenName` or `playerName`). Used by [`scrub_raw_log_with`] to
    /// conditionally skip name redaction when [`ScrubOptions::keep_player_names`]
    /// is set.
    is_player_name: bool,
    /// When `true`, this pattern pseudonymizes a free-text deck `Name` field
    /// to a deterministic `Deck-<8hex>` label derived from the adjacent
    /// `DeckId`. Used by [`scrub_raw_log_with`] to conditionally skip deck-name
    /// pseudonymization when [`ScrubOptions::keep_deck_names`] is set.
    is_deck_name: bool,
}

/// Options controlling which classes of data are redacted by [`scrub_raw_log_with`].
///
/// All fields default to `false`, which reproduces the same behavior as
/// [`scrub_raw_log`] (maximum redaction).
///
/// # Examples
///
/// ```
/// use manasight_parser::{ScrubOptions, scrub_raw_log_with};
///
/// // Preserve player names while still redacting everything else.
/// let opts = ScrubOptions { keep_player_names: true, keep_deck_names: false };
/// let raw = r#"Token: secret123 and "screenName": "Player#999""#;
/// let clean = scrub_raw_log_with(raw, &opts);
/// assert!(clean.contains("Token: <redacted>"));
/// assert!(clean.contains(r#""Player#999""#));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ScrubOptions {
    /// When `true`, the `screenName` and `playerName` JSON fields are **not**
    /// redacted. All other patterns (tokens, bearer tokens, paths, `clientId`,
    /// `userId`, `sessionId`, email addresses, IP addresses, hardware
    /// fingerprints) still apply.
    ///
    /// Use this when the upload destination should retain both players' handles
    /// for replay or analytics attribution (AC-OPP-1).
    pub keep_player_names: bool,
    /// When `true`, free-text deck `Name` fields are **not** pseudonymized —
    /// they pass through untouched. All other patterns (tokens, paths, player
    /// names, session identifiers, hardware fingerprints) still apply.
    ///
    /// By default (`false`), deck names are pseudonymized to a deterministic
    /// `Deck-<first 8 hex of DeckId>` label: `DeckId`/`Format`/`Attributes`/
    /// card-list structure is preserved, and the same `DeckId` always maps to
    /// the same label, so rename chains and cross-carrier occurrences of one
    /// deck stay correlatable after scrubbing.
    pub keep_deck_names: bool,
}

/// Compiled privacy-scrubbing patterns, initialized once on first use.
///
/// Each entry strips a class of sensitive data from raw log lines:
/// - Auth tokens (`Token: <value>`)
/// - Bearer tokens (`Bearer <value>`, word-boundary guarded to avoid game
///   cosmetic false positives like `Title_StandardBearer`)
/// - `WotC` account IDs in log prefixes (`Match to <id>:`)
/// - JSON `"clientId"` and `"userId"` values
/// - Windows user paths (`C:\Users\<username>\`)
/// - macOS user paths (`/Users/<username>/`)
/// - Linux user paths (`/home/<username>/`)
/// - Session identifiers (JSON `"token"` and `"sessionId"` values)
/// - Display names (JSON `"screenName"` and `"playerName"` values)
/// - Deck names (free-text JSON `"Name"` values adjacent to a `"DeckId"`,
///   pseudonymized to `Deck-<first 8 hex of DeckId>`; also the Unity
///   "Can't find pet for deck ..." console diagnostic)
/// - Hardware fingerprint lines, across three log formats:
///   - Windows `GfxDevice` block (Renderer, Vendor, VRAM, Driver)
///   - macOS Metal `GfxDevice` block (preferred device, Using device, Initializing
///     Metal device caps, enumerated Metal devices, Metal device count)
///   - Unity `SystemInfo` block on both platforms (graphicsDeviceName,
///     graphicsDeviceVendor, graphicsDeviceVersion, deviceModel, operatingSystem,
///     processorType)
/// - Email addresses
/// - IPv4 dotted-quad addresses
/// - IPv6 addresses (compressed, full, `::1`, `fe80::` link-local)
static SCRUB_PATTERNS: LazyLock<Vec<ScrubPattern>> = LazyLock::new(|| {
    // Patterns, replacements, and per-pattern flags.
    // Each regex is compiled exactly once.
    // Order matters: more specific patterns should come before general ones
    // if there is overlap. Currently there is no overlap between categories.
    //
    // Tuple fields: (pattern, replacement, is_player_name, is_deck_name)
    let definitions: &[(&str, &str, bool, bool)] = &[
        // Auth tokens: "Token: <base64-or-hex-value>"
        // Matches "Token:" followed by optional whitespace and a non-whitespace token value.
        (r"Token:\s*\S+", "Token: <redacted>", false, false),
        // Bearer tokens in HTTP Authorization headers.
        // Uses word boundary to avoid matching game cosmetics like
        // "Title_StandardBearer" where "Bearer" appears as a substring
        // of a larger word. The \b anchor matches at the start of the
        // string or after a non-word character, so "Bearer" following
        // a letter (as in "StandardBearer") does not match.
        (r"\bBearer\s+\S+", "Bearer <redacted>", false, false),
        // WotC account IDs in log line prefixes.
        // Arena logs game messages prefixed with the player's account ID:
        //   "Match to CR4QJUQPDBCVVMGCGNZLWGDFJE: AuthenticateResponse"
        (
            r"Match to [A-Z0-9_]+:",
            "Match to <redacted>:",
            false,
            false,
        ),
        // JSON "clientId" values from authenticateResponse blocks.
        (
            r#""[Cc]lient[Ii]d"\s*:\s*"[^"]+""#,
            r#""clientId": "<redacted>""#,
            false,
            false,
        ),
        // JSON "userId" values from matchGameRoomStateChangedEvent blocks.
        (
            r#""[Uu]ser[Ii]d"\s*:\s*"[^"]+""#,
            r#""userId": "<redacted>""#,
            false,
            false,
        ),
        // Windows paths: C:\Users\<username>\ (any drive letter)
        (r"[A-Z]:\\Users\\[^\\]+\\", r"<user-path>\", false, false),
        // macOS paths: /Users/<username>/
        (r"/Users/[^/]+/", "<user-path>/", false, false),
        // Linux paths: /home/<username>/
        (r"/home/[^/]+/", "<user-path>/", false, false),
        // Session identifiers: JSON "token" values from authenticateResponse
        // and similar auth payloads.
        (
            r#""[Tt]oken"\s*:\s*"[^"]+""#,
            r#""token": "<redacted>""#,
            false,
            false,
        ),
        // Session identifiers: JSON "sessionId" values from auth responses.
        (
            r#""[Ss]ession[Ii]d"\s*:\s*"[^"]+""#,
            r#""sessionId": "<redacted>""#,
            false,
            false,
        ),
        // Display names: JSON "screenName" values from authenticateResponse.
        // is_player_name = true so scrub_raw_log_with can skip this when
        // keep_player_names is set.
        (
            r#""[Ss]creen[Nn]ame"\s*:\s*"[^"]+""#,
            r#""screenName": "<redacted>""#,
            true,
            false,
        ),
        // Display names: JSON "playerName" values from match state.
        // Contains BOTH players' display names, meaning opponent PII
        // is leaked without this pattern.
        // is_player_name = true — skipped when keep_player_names is set.
        (
            r#""[Pp]layer[Nn]ame"\s*:\s*"[^"]+""#,
            r#""playerName": "<redacted>""#,
            true,
            false,
        ),
        // Deck names: escaped-context pair (`==>` request payloads, e.g.
        // DeckUpsertDeckV3/V2 and EventSetDeckV2/V3 submissions). The client
        // serializer escapes the JSON-in-string payload, so keys/values are
        // preceded by a literal backslash (`\"DeckId\":\"...\"`). Tolerates a
        // run of intervening simple scalar fields (e.g. `\"Mana\":\"\"`)
        // between `DeckId` and `Name`, since request carriers insert one
        // there — the matcher only handles simple scalars (string/number/
        // bool/null); the env-gated corpus guard test backstops any future
        // non-scalar drift. is_deck_name = true — skipped when
        // keep_deck_names is set.
        (
            r#"(\\"DeckId\\":\s*\\")([0-9a-fA-F]{8})([0-9a-fA-F-]*)(\\",\s*(?:\\"[A-Za-z0-9_]+\\":(?:\\"[^"\\]*\\"|-?[0-9]+|true|false|null),\s*)*\\"Name\\":\s*\\")((?:[^"\\]|\\\\.|\\"[^,}\]"])*)"#,
            r"${1}${2}${3}${4}Deck-${2}",
            false,
            true,
        ),
        // Deck names: plain-context pair (`<==` responses, `StartHook`
        // account bootstrap, precon catalog, course summaries — the largest
        // carriers by occurrence count). Same intervening-scalar tolerance as
        // the escaped-context pattern above, without the backslash prefix.
        // is_deck_name = true — skipped when keep_deck_names is set.
        (
            r#"("DeckId":\s*")([0-9a-fA-F]{8})([0-9a-fA-F-]*)(",\s*(?:"[A-Za-z0-9_]+":(?:"[^"\\]*"|-?[0-9]+|true|false|null),\s*)*"Name":\s*")((?:[^"\\]|\\.)*)"#,
            r"${1}${2}${3}${4}Deck-${2}",
            false,
            true,
        ),
        // Deck names: Unity console pet-diagnostic line (plain text, not
        // JSON). Optional `[N] ` frame prefix appears in unstripped archive
        // logs. is_deck_name = true — skipped when keep_deck_names is set.
        // The trailing `(\r?)$` captures an optional CR before the line
        // terminator and re-emits it in the replacement, so CRLF-terminated
        // lines (standard for Windows-written Player.log files) are scrubbed
        // without their line ending being normalized to LF-only. The `regex`
        // crate has no lookahead, so a bare `\r?` before `$` would consume
        // the `\r` rather than preserve it.
        (
            r"(?m)^(\[\d+\] )?Can't find pet for deck .+ \(([0-9a-fA-F]{8})([0-9a-fA-F-]{28})\)(\r?)$",
            "${1}Can't find pet for deck Deck-${2} (${2}${3})${4}",
            false,
            true,
        ),
        // Hardware fingerprint: GPU renderer line in log header.
        // (?m) enables per-line ^ matching since we scrub the full text buffer.
        // Leading whitespace (^\s+) is required to avoid false positives.
        (
            r"(?m)^\s+Renderer:\s+.+",
            "  Renderer: <redacted>",
            false,
            false,
        ),
        // Hardware fingerprint: GPU vendor.
        (
            r"(?m)^\s+Vendor:\s+.+",
            "  Vendor: <redacted>",
            false,
            false,
        ),
        // Hardware fingerprint: VRAM size in MB.
        (r"(?m)^\s+VRAM:\s+.+", "  VRAM: <redacted>", false, false),
        // Hardware fingerprint: GPU driver version.
        (
            r"(?m)^\s+Driver:\s+.+",
            "  Driver: <redacted>",
            false,
            false,
        ),
        // Hardware fingerprint: macOS Metal GPU — "preferred device" line.
        // Has one leading space in the real log. ^\s* (zero-or-more) is used across
        // all macOS Metal patterns because sibling lines ("Using device",
        // "Initializing Metal device caps", and the enumerated "N: …" line) start at
        // column 0; a one-or-more ^\s+ anchor would miss those column-0 lines.
        // ^\s* matches both the single-space-indented "preferred device:" line and
        // the column-0 lines uniformly.
        (
            r"(?m)^\s*preferred device:\s+.+",
            " preferred device: <redacted>",
            false,
            false,
        ),
        // Hardware fingerprint: macOS Metal GPU — device count line.
        // "Metal devices available: <N>" carries only a count (not a model), but
        // is redacted for symmetry with the adjacent GPU-model lines.
        (
            r"(?m)^\s*Metal devices available:\s+.+",
            "Metal devices available: <redacted>",
            false,
            false,
        ),
        // Hardware fingerprint: macOS Metal GPU — enumerated device line.
        // Format: "N: <gpu-model> (high power)" / "N: <gpu-model> (low power)".
        // Anchored on the Metal power suffix to avoid over-matching other
        // "N: …" log lines (e.g. numbered list items in game output). The
        // trailing `(\r?)$` preserves an optional CR before the line
        // terminator (see the pet-diagnostic pattern above for why capture-
        // and-re-emit is used instead of a bare `\r?`).
        (
            r"(?m)^\s*\d+:\s+.+\((?:high|low) power\)(\r?)$",
            "<N>: <redacted>${1}",
            false,
            false,
        ),
        // Hardware fingerprint: macOS Metal GPU — "Using device" line.
        // Starts at column 0; the Windows ^\s+ anchor does not match it.
        (
            r"(?m)^\s*Using device\s+.+",
            "Using device <redacted>",
            false,
            false,
        ),
        // Hardware fingerprint: macOS Metal GPU — "Initializing Metal device caps" line.
        // Starts at column 0; the Windows ^\s+ anchor does not match it.
        (
            r"(?m)^\s*Initializing Metal device caps:\s+.+",
            "Initializing Metal device caps: <redacted>",
            false,
            false,
        ),
        // Hardware fingerprint: Unity `SystemInfo` block (newer MTGA builds,
        // >= 2026.58, on BOTH macOS and Windows). Emitted at the very start of the
        // log as `key value` lines — no leading whitespace, no colon — so neither
        // the Windows `^\s+Renderer:` patterns nor the macOS Metal patterns above
        // match them. Each identifying key (GPU/vendor/API, device model, OS
        // version, CPU) is redacted; innocuous capability keys (graphicsMemorySize,
        // processorCount, maxTextureSize, graphicsShaderLevel, …) are left intact.
        // `^graphicsDeviceName ` (trailing space) does not match
        // `graphicsDeviceType`/`graphicsDeviceVendor`, and `^operatingSystem ` does
        // not match `operatingSystemFamily` — the space falls at a different offset.
        (
            r"(?m)^graphicsDeviceName .+",
            "graphicsDeviceName <redacted>",
            false,
            false,
        ),
        (
            r"(?m)^graphicsDeviceVendor .+",
            "graphicsDeviceVendor <redacted>",
            false,
            false,
        ),
        (
            r"(?m)^graphicsDeviceVersion .+",
            "graphicsDeviceVersion <redacted>",
            false,
            false,
        ),
        (
            r"(?m)^deviceModel .+",
            "deviceModel <redacted>",
            false,
            false,
        ),
        (
            r"(?m)^operatingSystem .+",
            "operatingSystem <redacted>",
            false,
            false,
        ),
        (
            r"(?m)^processorType .+",
            "processorType <redacted>",
            false,
            false,
        ),
        // Email addresses (defense-in-depth; MTGA logs carry no known third-party
        // emails empirically, but this closes a latent gap for future client changes).
        (
            r"[a-zA-Z0-9._%+\-]+@[a-zA-Z0-9.\-]+\.[a-zA-Z]{2,}",
            "<email-redacted>",
            false,
            false,
        ),
        // IPv6 addresses — matched BEFORE IPv4 to avoid the embedded IPv4 portion
        // of IPv4-mapped IPv6 addresses being double-substituted.
        //
        // Covers: full 8-group addresses, compressed addresses (::), loopback (::1),
        // link-local (fe80::...), and IPv4-mapped (::ffff:a.b.c.d).
        //
        // Three alternations (leftmost wins):
        //   1. `::` optionally followed by hex groups — covers `::1`, `::`, `::ffff:...`
        //   2. One or more hex groups followed by `::` and optional hex tail — covers
        //      `fe80::1`, `2001:db8::1`
        //   3. Three or more colon-separated hex groups without `::` — covers full
        //      8-group addresses like `2001:0db8:85a3:0000:0000:8a2e:0370:7334`
        //
        // Alternation 1 uses no leading \b because `::` starts with a non-word
        // character. Alternations 2 and 3 use \b to avoid partial matches inside
        // larger tokens.
        (
            concat!(
                r"::(?:[0-9a-fA-F]{1,4}(?::[0-9a-fA-F]{1,4})*)?",
                r"|\b[0-9a-fA-F]{1,4}(?::[0-9a-fA-F]{1,4})*::[0-9a-fA-F]{0,4}(?::[0-9a-fA-F]{1,4})*",
                r"|\b(?:[0-9a-fA-F]{1,4}:){3,7}[0-9a-fA-F]{1,4}\b",
            ),
            "<ip-redacted>",
            false,
            false,
        ),
        // IPv4 dotted-quad addresses (defense-in-depth).
        //
        // NOTE: A straightforward dotted-quad regex also matches version strings
        // of the form "N.N.N.N" (e.g. "Version: 1.2.3.4" in the MTGA log header).
        // Because the `regex` crate is DFA-based and does not support lookbehind,
        // there is no way to exclude the version-line context without substantial
        // added complexity. The deliberate tradeoff here is that a 4-segment version
        // string is syntactically indistinguishable from an IPv4 address; redacting
        // it is acceptable as defense-in-depth (AC-PRIV-8). The test fixture
        // `test_scrub_raw_log_hardware_fingerprint_in_full_log_header` has been
        // updated accordingly.
        (
            r"\b(?:(?:25[0-5]|2[0-4]\d|[01]?\d\d?)\.){3}(?:25[0-5]|2[0-4]\d|[01]?\d\d?)\b",
            "<ip-redacted>",
            false,
            false,
        ),
    ];

    definitions
        .iter()
        .filter_map(|(pattern, replacement, is_player_name, is_deck_name)| {
            // These patterns are static string literals validated by tests.
            // A compilation failure here indicates a programmer error in the
            // pattern definitions above, not a runtime data issue.
            match Regex::new(pattern) {
                Ok(regex) => Some(ScrubPattern {
                    regex,
                    replacement,
                    is_player_name: *is_player_name,
                    is_deck_name: *is_deck_name,
                }),
                Err(e) => {
                    ::log::error!("BUG: failed to compile privacy pattern {pattern:?}: {e}");
                    None
                }
            }
        })
        .collect()
});

/// Redact PII and credentials from raw MTGA `Player.log` text.
///
/// Applies each compiled privacy regex pattern to the full input text,
/// replacing all matches with redaction placeholders. Handles empty input,
/// single-line input, and multi-megabyte files without panicking.
///
/// This is equivalent to `scrub_raw_log_with(input, &ScrubOptions::default())`.
///
/// # Examples
///
/// ```
/// use manasight_parser::sanitize::scrub_raw_log;
///
/// let raw = r#"Token: secret123 and "screenName": "Player#999""#;
/// let clean = scrub_raw_log(raw);
/// assert!(clean.contains("Token: <redacted>"));
/// assert!(!clean.contains("secret123"));
/// ```
pub fn scrub_raw_log(input: &str) -> String {
    scrub_raw_log_with(input, &ScrubOptions::default())
}

/// Redact PII and credentials from raw MTGA `Player.log` text with configurable options.
///
/// Like [`scrub_raw_log`], but accepts a [`ScrubOptions`] value to control which
/// data classes are redacted. See [`ScrubOptions`] for available flags.
///
/// # Examples
///
/// ```
/// use manasight_parser::{ScrubOptions, scrub_raw_log_with};
///
/// // Keep player handles for server-side replay attribution.
/// let opts = ScrubOptions { keep_player_names: true, keep_deck_names: false };
/// let raw = r#""screenName": "TimCahill#1234", "token": "secret""#;
/// let clean = scrub_raw_log_with(raw, &opts);
/// assert!(clean.contains("TimCahill#1234"));
/// assert!(clean.contains(r#""token": "<redacted>""#));
/// ```
pub fn scrub_raw_log_with(input: &str, opts: &ScrubOptions) -> String {
    if input.is_empty() {
        return String::new();
    }

    let mut result = input.to_owned();
    for pattern in SCRUB_PATTERNS.iter() {
        if opts.keep_player_names && pattern.is_player_name {
            continue;
        }
        if opts.keep_deck_names && pattern.is_deck_name {
            continue;
        }
        result = pattern
            .regex
            .replace_all(&result, pattern.replacement)
            .into_owned();
    }
    result
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // --- Empty and trivial input ---

    #[test]
    fn test_scrub_raw_log_empty_input_returns_empty() {
        assert_eq!(scrub_raw_log(""), "");
    }

    #[test]
    fn test_scrub_raw_log_single_line_no_sensitive_data_unchanged() {
        let input = "[UnityCrossThreadLogger] Game started";
        assert_eq!(scrub_raw_log(input), input);
    }

    #[test]
    fn test_scrub_raw_log_multiline_no_sensitive_data_unchanged() {
        let input = "Line 1\nLine 2\nLine 3\n";
        assert_eq!(scrub_raw_log(input), input);
    }

    // --- Auth token patterns ---

    #[test]
    fn test_scrub_raw_log_token_value_redacted() {
        let input =
            "Token: eyJhbGciOiJSUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.signature";
        let result = scrub_raw_log(input);
        assert_eq!(result, "Token: <redacted>");
    }

    #[test]
    fn test_scrub_raw_log_token_no_space_after_colon_redacted() {
        let input = "Token:abc123def456";
        let result = scrub_raw_log(input);
        assert_eq!(result, "Token: <redacted>");
    }

    #[test]
    fn test_scrub_raw_log_token_with_surrounding_text() {
        let input = "[Auth] Login response Token: eyJhbGciOiJSUzI1NiJ9.payload.sig -- done";
        let result = scrub_raw_log(input);
        assert_eq!(result, "[Auth] Login response Token: <redacted> -- done");
    }

    #[test]
    fn test_scrub_raw_log_multiple_tokens_on_separate_lines() {
        let input = "Token: first_token\nSome other line\nToken: second_token\n";
        let result = scrub_raw_log(input);
        assert!(result.contains("Token: <redacted>"));
        assert!(!result.contains("first_token"));
        assert!(!result.contains("second_token"));
    }

    // --- Bearer token patterns ---

    #[test]
    fn test_scrub_raw_log_bearer_token_redacted() {
        let input = "Authorization: Bearer eyJhbGciOiJSUzI1NiJ9.payload.signature";
        let result = scrub_raw_log(input);
        assert_eq!(result, "Authorization: Bearer <redacted>");
    }

    #[test]
    fn test_scrub_raw_log_bearer_with_extra_whitespace() {
        let input = "Bearer   some_token_value";
        let result = scrub_raw_log(input);
        assert_eq!(result, "Bearer <redacted>");
    }

    #[test]
    fn test_scrub_raw_log_bearer_false_positive_standard_bearer_not_redacted() {
        let input = r#""Title_StandardBearer""#;
        assert_eq!(scrub_raw_log(input), input);
    }

    #[test]
    fn test_scrub_raw_log_bearer_jwt_still_redacted() {
        let input = "Authorization: Bearer eyJhbGciOiJIUzI1NiJ9.payload.signature";
        let result = scrub_raw_log(input);
        assert_eq!(result, "Authorization: Bearer <redacted>");
        assert!(!result.contains("eyJhbGciOiJIUzI1NiJ9"));
    }

    // --- Windows path patterns ---

    #[test]
    fn test_scrub_raw_log_windows_path_redacted() {
        let input =
            r"Loading from C:\Users\JohnDoe\AppData\LocalLow\Wizards Of The Coast\MTGA\Player.log";
        let result = scrub_raw_log(input);
        assert!(result.contains(r"<user-path>\AppData\LocalLow"));
        assert!(!result.contains("JohnDoe"));
    }

    #[test]
    fn test_scrub_raw_log_windows_path_different_drive_letter() {
        let input = r"D:\Users\Alice\Documents\game.log";
        let result = scrub_raw_log(input);
        assert!(result.contains(r"<user-path>\Documents"));
        assert!(!result.contains("Alice"));
    }

    // --- macOS path patterns ---

    #[test]
    fn test_scrub_raw_log_macos_path_redacted() {
        let input = "/Users/johndoe/Library/Logs/com.wizards.mtga/Player.log";
        let result = scrub_raw_log(input);
        assert!(result.contains("<user-path>/Library/Logs"));
        assert!(!result.contains("johndoe"));
    }

    #[test]
    fn test_scrub_raw_log_macos_path_with_spaces_in_context() {
        let input = "Reading file at /Users/jane_doe/Library/Logs/app.log successfully";
        let result = scrub_raw_log(input);
        assert!(result.contains("<user-path>/Library/Logs"));
        assert!(!result.contains("jane_doe"));
    }

    // --- Linux path patterns ---

    #[test]
    fn test_scrub_raw_log_linux_path_redacted() {
        let input = "/home/gamer/.local/share/Steam/steamapps/common/MTGA/Player.log";
        let result = scrub_raw_log(input);
        assert!(result.contains("<user-path>/.local/share"));
        assert!(!result.contains("gamer"));
    }

    #[test]
    fn test_scrub_raw_log_linux_path_different_username() {
        let input = "Config at /home/mtg_player/.config/manasight/settings.toml";
        let result = scrub_raw_log(input);
        assert!(result.contains("<user-path>/.config/manasight"));
        assert!(!result.contains("mtg_player"));
    }

    // --- Session identifier patterns ---

    #[test]
    fn test_scrub_raw_log_json_token_value_redacted() {
        let input = r#"{"screenName": "Player#1", "token": "abc123secret"}"#;
        let result = scrub_raw_log(input);
        assert!(result.contains(r#""token": "<redacted>""#));
        assert!(!result.contains("abc123secret"));
    }

    #[test]
    fn test_scrub_raw_log_json_token_uppercase_key_redacted() {
        let input = r#"{"Token": "eyJhbGci.payload.sig"}"#;
        let result = scrub_raw_log(input);
        assert!(result.contains(r#""token": "<redacted>""#));
        assert!(!result.contains("eyJhbGci"));
    }

    #[test]
    fn test_scrub_raw_log_json_session_id_redacted() {
        let input = r#"{"sessionId": "sess_abc123def456", "status": "connected"}"#;
        let result = scrub_raw_log(input);
        assert!(result.contains(r#""sessionId": "<redacted>""#));
        assert!(!result.contains("sess_abc123def456"));
    }

    #[test]
    fn test_scrub_raw_log_authenticate_response_block() {
        let input = "[UnityCrossThreadLogger]authenticateResponse\n\
                     {\"screenName\": \"TestPlayer#12345\", \"token\": \"secret_jwt_value\"}";
        let result = scrub_raw_log(input);
        assert!(!result.contains("secret_jwt_value"));
        assert!(result.contains(r#""token": "<redacted>""#));
        assert!(!result.contains("TestPlayer#12345"));
        assert!(result.contains(r#""screenName": "<redacted>""#));
    }

    #[test]
    fn test_scrub_raw_log_session_id_with_spaces_in_json() {
        let input = r#"{ "SessionId" : "long-session-id-value-here" }"#;
        let result = scrub_raw_log(input);
        assert!(result.contains(r#""sessionId": "<redacted>""#));
        assert!(!result.contains("long-session-id-value-here"));
    }

    // --- WotC account ID in log prefix ---

    #[test]
    fn test_scrub_raw_log_match_to_account_id_redacted() {
        let input = "Match to CR4QJUQPDBCVVMGCGNZLWGDFJE: AuthenticateResponse";
        let result = scrub_raw_log(input);
        assert_eq!(result, "Match to <redacted>: AuthenticateResponse");
        assert!(!result.contains("CR4QJUQPDBCVVMGCGNZLWGDFJE"));
    }

    #[test]
    fn test_scrub_raw_log_match_to_with_underscore_in_id() {
        let input = "Match to SOME_ACCOUNT_ID_123: MatchCreated";
        let result = scrub_raw_log(input);
        assert_eq!(result, "Match to <redacted>: MatchCreated");
        assert!(!result.contains("SOME_ACCOUNT_ID_123"));
    }

    #[test]
    fn test_scrub_raw_log_match_to_with_log_timestamp_prefix() {
        let input = "[UnityCrossThreadLogger]3/22/2026 12:00:31 PM: Match to CR4QJUQPDBCVVMGCGNZLWGDFJE: AuthenticateResponse";
        let result = scrub_raw_log(input);
        assert!(result.contains("Match to <redacted>:"));
        assert!(!result.contains("CR4QJUQPDBCVVMGCGNZLWGDFJE"));
    }

    // --- JSON clientId pattern ---

    #[test]
    fn test_scrub_raw_log_json_client_id_redacted() {
        let input = r#""clientId": "CR4QJUQPDBCVVMGCGNZLWGDFJE""#;
        let result = scrub_raw_log(input);
        assert_eq!(result, r#""clientId": "<redacted>""#);
        assert!(!result.contains("CR4QJUQPDBCVVMGCGNZLWGDFJE"));
    }

    #[test]
    fn test_scrub_raw_log_json_client_id_with_spaces() {
        let input = r#"{ "ClientId" : "ABCDEF123456" }"#;
        let result = scrub_raw_log(input);
        assert!(result.contains(r#""clientId": "<redacted>""#));
        assert!(!result.contains("ABCDEF123456"));
    }

    // --- JSON userId pattern ---

    #[test]
    fn test_scrub_raw_log_json_user_id_redacted() {
        let input = r#""userId": "CR4QJUQPDBCVVMGCGNZLWGDFJE""#;
        let result = scrub_raw_log(input);
        assert_eq!(result, r#""userId": "<redacted>""#);
        assert!(!result.contains("CR4QJUQPDBCVVMGCGNZLWGDFJE"));
    }

    #[test]
    fn test_scrub_raw_log_json_user_id_uppercase_key() {
        let input = r#"{"UserId": "OPPONENT_ACCOUNT_ID_XYZ"}"#;
        let result = scrub_raw_log(input);
        assert!(result.contains(r#""userId": "<redacted>""#));
        assert!(!result.contains("OPPONENT_ACCOUNT_ID_XYZ"));
    }

    #[test]
    fn test_scrub_raw_log_json_user_id_in_match_event() {
        let input = r#"{"players": [{"userId": "PLAYER_ABC"}, {"userId": "OPPONENT_XYZ"}]}"#;
        let result = scrub_raw_log(input);
        assert!(!result.contains("PLAYER_ABC"));
        assert!(!result.contains("OPPONENT_XYZ"));
        assert_eq!(result.matches(r#""userId": "<redacted>""#).count(), 2);
    }

    // --- screenName pattern ---

    #[test]
    fn test_scrub_raw_log_screen_name_redacted() {
        let input = r#""screenName": "PlayerDisplayName#12345""#;
        let result = scrub_raw_log(input);
        assert_eq!(result, r#""screenName": "<redacted>""#);
        assert!(!result.contains("PlayerDisplayName"));
    }

    #[test]
    fn test_scrub_raw_log_screen_name_uppercase_key() {
        let input = r#"{"ScreenName": "SomePlayer#99999"}"#;
        let result = scrub_raw_log(input);
        assert!(result.contains(r#""screenName": "<redacted>""#));
        assert!(!result.contains("SomePlayer"));
    }

    #[test]
    fn test_scrub_raw_log_screen_name_no_space_after_colon() {
        let input = r#""screenName":"Truffie#12345""#;
        let result = scrub_raw_log(input);
        assert!(result.contains(r#""screenName": "<redacted>""#));
        assert!(!result.contains("Truffie"));
    }

    // --- playerName pattern ---

    #[test]
    fn test_scrub_raw_log_player_name_redacted() {
        let input = r#""playerName": "OpponentName#67890""#;
        let result = scrub_raw_log(input);
        assert_eq!(result, r#""playerName": "<redacted>""#);
        assert!(!result.contains("OpponentName"));
    }

    #[test]
    fn test_scrub_raw_log_player_name_both_players_redacted() {
        let input =
            r#"{"players": [{"playerName": "LocalPlayer#111"}, {"playerName": "Opponent#222"}]}"#;
        let result = scrub_raw_log(input);
        assert!(!result.contains("LocalPlayer"));
        assert!(!result.contains("Opponent"));
        assert_eq!(result.matches(r#""playerName": "<redacted>""#).count(), 2);
    }

    #[test]
    fn test_scrub_raw_log_player_name_uppercase_key() {
        let input = r#"{"PlayerName": "SomeUser#42"}"#;
        let result = scrub_raw_log(input);
        assert!(result.contains(r#""playerName": "<redacted>""#));
        assert!(!result.contains("SomeUser"));
    }

    // --- Hardware fingerprint patterns ---

    #[test]
    fn test_scrub_raw_log_hardware_fingerprint_all_lines_redacted() {
        let input =
            "  Renderer: NVIDIA GeForce RTX 3080\n  Vendor: NVIDIA\n  VRAM: 10240\n  Driver: 537.58";
        let result = scrub_raw_log(input);
        assert!(!result.contains("NVIDIA GeForce RTX 3080"));
        assert!(!result.contains("NVIDIA"));
        assert!(!result.contains("10240"));
        assert!(!result.contains("537.58"));
        assert!(result.contains("Renderer: <redacted>"));
        assert!(result.contains("Vendor: <redacted>"));
        assert!(result.contains("VRAM: <redacted>"));
        assert!(result.contains("Driver: <redacted>"));
    }

    #[test]
    fn test_scrub_raw_log_hardware_fingerprint_in_full_log_header() {
        // NOTE: "Version: 1.2.3.4" is intentionally redacted to "<ip-redacted>"
        // by the IPv4 pattern. A 4-segment numeric string is syntactically
        // indistinguishable from an IPv4 address without semantic context, and
        // the `regex` crate (DFA-based) provides no lookbehind to exclude the
        // version-line context. Redacting it is acceptable as defense-in-depth
        // (AC-PRIV-8): the version number is not PII and its loss in the
        // scrubbed upload blob does not affect replay correctness or analytics.
        let input = "\
[UnityCrossThreadLogger] Version: 1.2.3.4
  SystemInfo:
  Renderer: AMD Radeon RX 6800 XT
  Vendor: AMD
  VRAM: 16384
  Driver: 23.12.1
[UnityCrossThreadLogger] Game starting";
        let result = scrub_raw_log(input);
        assert!(!result.contains("AMD Radeon RX 6800 XT"));
        assert!(!result.contains("16384"));
        assert!(!result.contains("23.12.1"));
        // Version string is redacted by the IPv4 pattern (deliberate tradeoff —
        // see comment above).
        assert!(!result.contains("1.2.3.4"));
        assert!(result.contains("Version: <ip-redacted>"));
        assert!(result.contains("Game starting"));
    }

    #[test]
    fn test_scrub_raw_log_hardware_renderer_not_matched_without_leading_whitespace() {
        let input = "Renderer: some game object reference";
        assert_eq!(scrub_raw_log(input), input);
    }

    #[test]
    fn test_scrub_raw_log_hardware_vendor_not_matched_without_leading_whitespace() {
        let input = "Vendor: some vendor string in game data";
        assert_eq!(scrub_raw_log(input), input);
    }

    // --- macOS Metal GPU fingerprint patterns ---

    #[test]
    fn test_scrub_raw_log_macos_metal_gpu_fingerprint_all_lines_redacted() {
        // Verbatim macOS Player.log GfxDevice header format — GPU model is
        // the fingerprint present in four distinct line shapes. Whitespace,
        // casing, and punctuation match the real log exactly; the GPU model
        // value has been replaced by "Apple M1 Pro" as a representative token.
        let input = concat!(
            " preferred device: Apple M1 Pro (high power)\n",
            "Metal devices available: 1\n",
            "0: Apple M1 Pro (high power)\n",
            "Using device Apple M1 Pro (high power)\n",
            "Initializing Metal device caps: Apple M1 Pro",
        );
        let result = scrub_raw_log(input);

        // GPU model must not survive in any form.
        assert!(
            !result.contains("Apple M1 Pro"),
            "GPU model leaked: {result:?}"
        );

        // Each line shape must be replaced with its canonical placeholder.
        assert!(result.contains("preferred device: <redacted>"));
        assert!(result.contains("Metal devices available: <redacted>"));
        assert!(result.contains("<N>: <redacted>"));
        assert!(result.contains("Using device <redacted>"));
        assert!(result.contains("Initializing Metal device caps: <redacted>"));
    }

    #[test]
    fn test_scrub_raw_log_macos_metal_gpu_fingerprint_high_and_low_power_redacted() {
        // The enumerated device line uses either "(high power)" or "(low power)".
        // Both suffixes must be matched so that multi-GPU Macs (iGPU + dGPU)
        // are fully redacted.
        let input = concat!(
            "0: Apple M1 Pro (high power)\n",
            "1: Apple M2 Ultra (low power)\n",
        );
        let result = scrub_raw_log(input);
        assert!(!result.contains("Apple M1 Pro"), "high-power model leaked");
        assert!(!result.contains("Apple M2 Ultra"), "low-power model leaked");
        assert_eq!(result.matches("<N>: <redacted>").count(), 2);
    }

    #[test]
    fn test_scrub_raw_log_macos_metal_gpu_fingerprint_in_full_log_header() {
        // Realistic macOS Player.log header fragment — Metal GfxDevice block
        // embedded between Unity cross-thread logger lines.
        let input = concat!(
            "[UnityCrossThreadLogger] GfxDevice init\n",
            " preferred device: AMD Radeon Pro 5500M (high power)\n",
            "Metal devices available: 2\n",
            "0: AMD Radeon Pro 5500M (high power)\n",
            "1: Intel UHD Graphics 630 (low power)\n",
            "Using device AMD Radeon Pro 5500M (high power)\n",
            "Initializing Metal device caps: AMD Radeon Pro 5500M\n",
            "[UnityCrossThreadLogger] Game started\n",
        );
        let result = scrub_raw_log(input);

        // GPU model strings must be gone.
        assert!(
            !result.contains("AMD Radeon Pro 5500M"),
            "dGPU model leaked"
        );
        assert!(
            !result.contains("Intel UHD Graphics 630"),
            "iGPU model leaked"
        );

        // Non-sensitive surrounding context must be preserved.
        assert!(result.contains("[UnityCrossThreadLogger] GfxDevice init"));
        assert!(result.contains("[UnityCrossThreadLogger] Game started"));

        // Placeholder shapes must appear.
        assert!(result.contains("preferred device: <redacted>"));
        assert!(result.contains("Metal devices available: <redacted>"));
        assert!(result.contains("Using device <redacted>"));
        assert!(result.contains("Initializing Metal device caps: <redacted>"));
        assert_eq!(result.matches("<N>: <redacted>").count(), 2);
    }

    #[test]
    fn test_scrub_raw_log_macos_metal_enumerated_device_crlf_terminator_preserved() {
        // Metal GfxDevice lines can also appear with \r\n endings (e.g. a
        // macOS log relayed through a Windows-hosted collector). The `$`
        // anchor must match past the `\r`, and the `\r` must survive in the
        // output byte-for-byte.
        let input = "0: Apple M1 Pro (high power)\r\n1: Apple M2 Ultra (low power)\r\n";
        let result = scrub_raw_log(input);
        assert_eq!(result, "<N>: <redacted>\r\n<N>: <redacted>\r\n");
    }

    #[test]
    fn test_scrub_raw_log_mixed_lf_and_crlf_line_terminators_preserved_independently() {
        // A log with mixed line endings (e.g. concatenated from different
        // sources) must have each line's own terminator preserved
        // independently -- scrubbing must not normalize CRLF to LF or vice
        // versa on either matched line.
        let input = "0: Apple M1 Pro (high power)\nCan't find pet for deck My Cool Deck (1a2b3c4d-5e6f-7890-abcd-ef1234567890)\r\n";
        let result = scrub_raw_log(input);
        assert_eq!(
            result,
            "<N>: <redacted>\nCan't find pet for deck Deck-1a2b3c4d (1a2b3c4d-5e6f-7890-abcd-ef1234567890)\r\n"
        );
    }

    #[test]
    fn test_scrub_raw_log_macos_metal_numbered_line_not_matched_without_power_suffix() {
        // The enumerated-device pattern is anchored on "(high|low power)" to
        // avoid false-positives on other numbered-list log lines that happen to
        // start with "N: …". Without the power suffix, the line must not be
        // redacted.
        let input = "0: some other numbered log entry";
        assert_eq!(scrub_raw_log(input), input);
    }

    // --- Unity SystemInfo block (newer MTGA builds, both platforms) ---

    #[test]
    fn test_scrub_raw_log_systeminfo_macos_fingerprint_redacted() {
        // Verbatim header format from a real macOS Player.log (MTGA >= 2026.58):
        // `key value` lines at column 0, no colon. GPU / CPU / device / OS strings
        // are the fingerprint; capability counts are innocuous and must survive.
        let input = concat!(
            "graphicsDeviceName Apple M5 Pro\n",
            "graphicsDeviceType Metal\n",
            "graphicsDeviceVendor Apple\n",
            "graphicsDeviceVersion Metal\n",
            "graphicsMemorySize 38338\n",
            "deviceModel Mac17,8\n",
            "deviceType Desktop\n",
            "operatingSystem Mac OS X 26.5.1\n",
            "operatingSystemFamily MacOSX\n",
            "processorCount 18\n",
            "processorType Apple M5 Pro\n",
            "systemMemorySize 49152",
        );
        let result = scrub_raw_log(input);

        // Identifying hardware values must not survive (GPU + CPU share a string).
        assert!(
            !result.contains("Apple M5 Pro"),
            "GPU/CPU model leaked: {result:?}"
        );
        assert!(!result.contains("Mac17,8"), "device model leaked");
        assert!(!result.contains("Mac OS X 26.5.1"), "OS version leaked");

        // Each identifying key redacted in place.
        assert!(result.contains("graphicsDeviceName <redacted>"));
        assert!(result.contains("graphicsDeviceVendor <redacted>"));
        assert!(result.contains("graphicsDeviceVersion <redacted>"));
        assert!(result.contains("deviceModel <redacted>"));
        assert!(result.contains("operatingSystem <redacted>"));
        assert!(result.contains("processorType <redacted>"));

        // Innocuous capability keys must be preserved verbatim (no over-redaction).
        assert!(result.contains("graphicsDeviceType Metal"));
        assert!(result.contains("graphicsMemorySize 38338"));
        assert!(result.contains("deviceType Desktop"));
        assert!(result.contains("operatingSystemFamily MacOSX"));
        assert!(result.contains("processorCount 18"));
        assert!(result.contains("systemMemorySize 49152"));
    }

    #[test]
    fn test_scrub_raw_log_systeminfo_windows_fingerprint_redacted() {
        // Same SystemInfo format on Windows — the values differ but the keys are
        // identical, so the same patterns redact them (one fix covers both OSes).
        let input = concat!(
            "graphicsDeviceName NVIDIA GeForce RTX 4060 Laptop GPU\n",
            "graphicsDeviceVendor NVIDIA\n",
            "graphicsDeviceVersion Direct3D 11.0 [level 11.1]\n",
            "deviceModel Zenbook UX8402VV_UX8402VV (ASUSTeK COMPUTER INC.)\n",
            "operatingSystem Windows 11  (10.0.26200) 64bit\n",
            "processorType 13th Gen Intel(R) Core(TM) i9-13900H\n",
        );
        let result = scrub_raw_log(input);

        assert!(
            !result.contains("NVIDIA GeForce RTX 4060 Laptop GPU"),
            "GPU leaked"
        );
        assert!(!result.contains("Zenbook"), "device model leaked");
        assert!(!result.contains("i9-13900H"), "CPU leaked");
        assert!(!result.contains("10.0.26200"), "OS build leaked");
        assert!(result.contains("graphicsDeviceName <redacted>"));
        assert!(result.contains("deviceModel <redacted>"));
        assert!(result.contains("processorType <redacted>"));
    }

    #[test]
    fn test_scrub_raw_log_systeminfo_does_not_redact_innocuous_family_key() {
        // `^operatingSystem ` (trailing space) must not match `operatingSystemFamily`
        // — the space falls at a different offset, so the family key is preserved.
        let input = "operatingSystemFamily MacOSX";
        assert_eq!(scrub_raw_log(input), input);
    }

    // --- Multiple patterns in one block ---

    #[test]
    fn test_scrub_raw_log_mixed_sensitive_data_all_redacted() {
        let input = "\
[Auth] Token: eyJhbGciOiJSUzI1NiJ9.payload.sig
[HTTP] Authorization: Bearer eyToken123.payload.sig
[Init] Loading config from C:\\Users\\JaneDoe\\AppData\\Local\\manasight\\config.toml
[Init] Log path: /Users/johndoe/Library/Logs/manasight.log
[Init] Linux path: /home/linuxuser/.local/share/manasight/data.db
[Game] Match started: event=PlayQueue";

        let result = scrub_raw_log(input);

        assert!(!result.contains("eyJhbGciOiJSUzI1NiJ9"));
        assert!(!result.contains("eyToken123"));
        assert!(!result.contains("JaneDoe"));
        assert!(!result.contains("johndoe"));
        assert!(!result.contains("linuxuser"));

        assert!(result.contains("Token: <redacted>"));
        assert!(result.contains("Bearer <redacted>"));
        assert!(result.contains(r"<user-path>\AppData"));
        assert!(result.contains("<user-path>/Library/Logs"));
        assert!(result.contains("<user-path>/.local/share"));

        assert!(result.contains("[Game] Match started: event=PlayQueue"));
    }

    // --- Edge cases ---

    #[test]
    fn test_scrub_raw_log_preserves_line_endings() {
        let input = "Line 1\r\nToken: secret_value\r\nLine 3\r\n";
        let result = scrub_raw_log(input);
        assert!(result.contains("\r\n"));
        assert!(result.contains("Token: <redacted>"));
    }

    #[test]
    fn test_scrub_raw_log_large_input_does_not_panic() {
        let line = "Normal log line without sensitive data\n";
        let large_input: String = line.repeat(25_000);
        let result = scrub_raw_log(&large_input);
        assert_eq!(result.len(), large_input.len());
    }

    #[test]
    fn test_scrub_raw_log_token_at_end_of_line_no_trailing_space() {
        let input = "Token: abc123";
        let result = scrub_raw_log(input);
        assert_eq!(result, "Token: <redacted>");
    }

    #[test]
    fn test_scrub_raw_log_bearer_at_end_of_line_no_trailing_space() {
        let input = "Bearer abc123";
        let result = scrub_raw_log(input);
        assert_eq!(result, "Bearer <redacted>");
    }

    #[test]
    fn test_scrub_raw_log_path_only_line() {
        let input = r"C:\Users\SomeUser\";
        let result = scrub_raw_log(input);
        assert_eq!(result, r"<user-path>\");
    }

    #[test]
    fn test_scrub_raw_log_multiple_paths_on_same_line() {
        let input = "Copied /Users/alice/source.txt to /Users/bob/dest.txt";
        let result = scrub_raw_log(input);
        assert!(!result.contains("alice"));
        assert!(!result.contains("bob"));
        assert_eq!(
            result,
            "Copied <user-path>/source.txt to <user-path>/dest.txt"
        );
    }

    #[test]
    fn test_scrub_raw_log_idempotent() {
        let input = "Token: secret123\n/home/user/.config/app.toml";
        let first_pass = scrub_raw_log(input);
        let second_pass = scrub_raw_log(&first_pass);
        assert_eq!(first_pass, second_pass, "Scrubbing should be idempotent");
    }

    // --- Patterns that should NOT be redacted ---

    #[test]
    fn test_scrub_raw_log_lowercase_token_not_redacted() {
        let input = "token: not_a_real_token";
        assert_eq!(scrub_raw_log(input), input);
    }

    #[test]
    fn test_scrub_raw_log_lowercase_bearer_not_redacted() {
        let input = "bearer not_a_real_token";
        assert_eq!(scrub_raw_log(input), input);
    }

    #[test]
    fn test_scrub_raw_log_non_user_paths_not_redacted() {
        let input = "/usr/local/bin/mtga\n/etc/config.toml\n/var/log/syslog";
        assert_eq!(scrub_raw_log(input), input);
    }

    // --- ScrubOptions / keep_player_names ---

    #[test]
    fn test_scrub_raw_log_with_keep_player_names_false_redacts_names() {
        let opts = ScrubOptions {
            keep_player_names: false,
            keep_deck_names: false,
        };
        let input = r#""screenName": "Alice#123", "playerName": "Bob#456""#;
        let result = scrub_raw_log_with(input, &opts);
        assert!(!result.contains("Alice"));
        assert!(!result.contains("Bob"));
        assert!(result.contains(r#""screenName": "<redacted>""#));
        assert!(result.contains(r#""playerName": "<redacted>""#));
    }

    #[test]
    fn test_scrub_raw_log_with_keep_player_names_true_preserves_names() {
        let opts = ScrubOptions {
            keep_player_names: true,
            keep_deck_names: false,
        };
        let input = r#""screenName": "Alice#123", "playerName": "Bob#456""#;
        let result = scrub_raw_log_with(input, &opts);
        assert!(result.contains("Alice#123"));
        assert!(result.contains("Bob#456"));
    }

    #[test]
    fn test_scrub_raw_log_with_keep_player_names_true_still_redacts_tokens() {
        let opts = ScrubOptions {
            keep_player_names: true,
            keep_deck_names: false,
        };
        let input = r#"Token: secret123 and "screenName": "Alice#123""#;
        let result = scrub_raw_log_with(input, &opts);
        assert!(result.contains("Token: <redacted>"));
        assert!(!result.contains("secret123"));
        assert!(result.contains("Alice#123"));
    }

    #[test]
    fn test_scrub_raw_log_with_keep_player_names_true_still_redacts_session_ids() {
        let opts = ScrubOptions {
            keep_player_names: true,
            keep_deck_names: false,
        };
        let input = r#"{"sessionId": "sess_xyz789", "screenName": "Alice#123"}"#;
        let result = scrub_raw_log_with(input, &opts);
        assert!(result.contains(r#""sessionId": "<redacted>""#));
        assert!(!result.contains("sess_xyz789"));
        assert!(result.contains("Alice#123"));
    }

    #[test]
    fn test_scrub_raw_log_with_keep_player_names_true_still_redacts_paths() {
        let opts = ScrubOptions {
            keep_player_names: true,
            keep_deck_names: false,
        };
        let input = r#""playerName": "Alice#123" at /home/alice/.config/app"#;
        let result = scrub_raw_log_with(input, &opts);
        assert!(result.contains("Alice#123"));
        assert!(!result.contains("/home/alice/"));
        assert!(result.contains("<user-path>/"));
    }

    #[test]
    fn test_scrub_raw_log_with_keep_player_names_true_still_redacts_client_id() {
        let opts = ScrubOptions {
            keep_player_names: true,
            keep_deck_names: false,
        };
        let input = r#"{"clientId": "CR4QJUQP", "screenName": "Alice#123"}"#;
        let result = scrub_raw_log_with(input, &opts);
        assert!(result.contains(r#""clientId": "<redacted>""#));
        assert!(!result.contains("CR4QJUQP"));
        assert!(result.contains("Alice#123"));
    }

    #[test]
    fn test_scrub_raw_log_with_keep_player_names_true_still_redacts_hardware_fingerprints() {
        let opts = ScrubOptions {
            keep_player_names: true,
            keep_deck_names: false,
        };
        let input = "\"playerName\": \"Alice#123\"\n  Renderer: NVIDIA GeForce RTX 3080";
        let result = scrub_raw_log_with(input, &opts);
        assert!(result.contains("Alice#123"));
        assert!(!result.contains("NVIDIA GeForce RTX 3080"));
        assert!(result.contains("Renderer: <redacted>"));
    }

    #[test]
    fn test_scrub_raw_log_with_default_opts_equals_scrub_raw_log() {
        // scrub_raw_log_with(.., &ScrubOptions::default()) must produce
        // identical output to scrub_raw_log(..) for the same input.
        let inputs = [
            r#""screenName": "Alice#123", Token: secret"#,
            "Token: abc Bearer tok123",
            r#"{"sessionId": "s1", "playerName": "Bob#99"}"#,
            "[UnityCrossThreadLogger] Game started",
            r#"{"DeckId":"1a2b3c4d-5e6f-7890-abcd-ef1234567890","Name":"My Cool Deck"}"#,
            "",
        ];
        for input in &inputs {
            assert_eq!(
                scrub_raw_log(input),
                scrub_raw_log_with(input, &ScrubOptions::default()),
                "scrub_raw_log and scrub_raw_log_with(default) differ for input: {input:?}"
            );
        }
    }

    // --- Email redaction ---

    #[test]
    fn test_scrub_raw_log_email_address_redacted() {
        let input = "Contact: user@example.com for support";
        let result = scrub_raw_log(input);
        assert!(!result.contains("user@example.com"));
        assert!(result.contains("<email-redacted>"));
    }

    #[test]
    fn test_scrub_raw_log_email_in_json_value_redacted() {
        let input = r#"{"email": "player.one+mtga@arena.wizards.com"}"#;
        let result = scrub_raw_log(input);
        assert!(!result.contains("player.one+mtga@arena.wizards.com"));
        assert!(result.contains("<email-redacted>"));
    }

    #[test]
    fn test_scrub_raw_log_multiple_emails_on_same_line_redacted() {
        let input = "From: alice@example.com To: bob@example.org";
        let result = scrub_raw_log(input);
        assert!(!result.contains("alice@example.com"));
        assert!(!result.contains("bob@example.org"));
        assert_eq!(result.matches("<email-redacted>").count(), 2);
    }

    // --- IPv4 redaction ---

    #[test]
    fn test_scrub_raw_log_ipv4_address_redacted() {
        let input = "Server address: 192.168.1.100 port 443";
        let result = scrub_raw_log(input);
        assert!(!result.contains("192.168.1.100"));
        assert!(result.contains("<ip-redacted>"));
    }

    #[test]
    fn test_scrub_raw_log_ipv4_loopback_redacted() {
        let input = "Connecting to 127.0.0.1:8080";
        let result = scrub_raw_log(input);
        assert!(!result.contains("127.0.0.1"));
        assert!(result.contains("<ip-redacted>"));
    }

    #[test]
    fn test_scrub_raw_log_ipv4_public_address_redacted() {
        let input = "WotC endpoint: 52.23.1.200";
        let result = scrub_raw_log(input);
        assert!(!result.contains("52.23.1.200"));
        assert!(result.contains("<ip-redacted>"));
    }

    #[test]
    fn test_scrub_raw_log_version_string_redacted_as_ipv4_deliberate_tradeoff() {
        // A 4-segment version string is syntactically indistinguishable from
        // an IPv4 address without semantic context. The regex crate (DFA-based)
        // provides no lookbehind to exclude version-line context, so version
        // strings like "1.2.3.4" are redacted. This is an acceptable
        // defense-in-depth tradeoff (AC-PRIV-8).
        let input = "Version: 1.2.3.4";
        let result = scrub_raw_log(input);
        assert!(!result.contains("1.2.3.4"));
        assert!(result.contains("<ip-redacted>"));
    }

    // --- IPv6 redaction ---

    #[test]
    fn test_scrub_raw_log_ipv6_loopback_redacted() {
        let input = "Listening on ::1 port 3000";
        let result = scrub_raw_log(input);
        assert!(!result.contains("::1"));
        assert!(result.contains("<ip-redacted>"));
    }

    #[test]
    fn test_scrub_raw_log_ipv6_link_local_redacted() {
        let input = "Interface address: fe80::1%eth0";
        let result = scrub_raw_log(input);
        assert!(!result.contains("fe80::1"));
        assert!(result.contains("<ip-redacted>"));
    }

    #[test]
    fn test_scrub_raw_log_ipv6_full_address_redacted() {
        let input = "IPv6: 2001:0db8:85a3:0000:0000:8a2e:0370:7334";
        let result = scrub_raw_log(input);
        assert!(!result.contains("2001:0db8:85a3:0000:0000:8a2e:0370:7334"));
        assert!(result.contains("<ip-redacted>"));
    }

    #[test]
    fn test_scrub_raw_log_ipv6_compressed_redacted() {
        let input = "Remote: 2001:db8::1";
        let result = scrub_raw_log(input);
        assert!(!result.contains("2001:db8::1"));
        assert!(result.contains("<ip-redacted>"));
    }

    // --- ScrubOptions / keep_deck_names ---
    //
    // Synthetic deck GUID used throughout: "1a2b3c4d-5e6f-7890-abcd-ef1234567890"
    // (first 8 hex = "1a2b3c4d" → expected label "Deck-1a2b3c4d"). All deck
    // names below are synthetic placeholders, not real MTGA deck names.

    #[test]
    fn test_scrub_raw_log_deck_name_escaped_context_with_intervening_field_redacted() {
        // Simulates a ==> DeckUpsertDeckV3 request payload: the client
        // escapes the JSON-in-string, and inserts a "Mana" scalar field
        // between DeckId and Name.
        let input = r#"==> DeckUpsertDeckV3 {"id":5,"request":"{\"DeckId\":\"1a2b3c4d-5e6f-7890-abcd-ef1234567890\",\"Mana\":\"\",\"Name\":\"My Cool Deck\"}"}"#;
        let result = scrub_raw_log(input);
        assert!(!result.contains("My Cool Deck"));
        assert!(result.contains(r#"\"DeckId\":\"1a2b3c4d-5e6f-7890-abcd-ef1234567890\""#));
        assert!(result.contains(r#"\"Name\":\"Deck-1a2b3c4d"#));
    }

    #[test]
    fn test_scrub_raw_log_deck_name_plain_context_response_redacted() {
        // Simulates a <== DeckUpsertDeckV3 response echo (plain JSON,
        // DeckId/Name directly adjacent).
        let input = r#"<== DeckUpsertDeckV3 {"DeckId":"1a2b3c4d-5e6f-7890-abcd-ef1234567890","Name":"My Cool Deck","Format":"Standard"}"#;
        let result = scrub_raw_log(input);
        assert!(!result.contains("My Cool Deck"));
        assert!(result.contains(r#""DeckId":"1a2b3c4d-5e6f-7890-abcd-ef1234567890""#));
        assert!(result.contains(r#""Name":"Deck-1a2b3c4d""#));
        assert!(result.contains(r#""Format":"Standard""#));
    }

    #[test]
    fn test_scrub_raw_log_deck_name_starthook_roster_shape_redacted() {
        // Simulates a <== StartHook account-bootstrap deck roster entry,
        // with an intervening numeric scalar field between DeckId and Name.
        let input = r#"{"Decks":[{"DeckId":"1a2b3c4d-5e6f-7890-abcd-ef1234567890","Version":1,"Name":"Test Deck","Format":"Standard"}]}"#;
        let result = scrub_raw_log(input);
        assert!(!result.contains("Test Deck"));
        assert!(result.contains(r#""Name":"Deck-1a2b3c4d""#));
        assert!(result.contains(r#""Version":1"#));
    }

    #[test]
    fn test_scrub_raw_log_deck_name_course_summary_shape_redacted() {
        // Simulates a <== EventGetCoursesV2 Courses[].CourseDeckSummary entry.
        let input = r#"{"Courses":[{"CourseId":"c1","CourseDeckSummary":{"DeckId":"1a2b3c4d-5e6f-7890-abcd-ef1234567890","Name":"My Cool Deck","Format":"Standard"}}]}"#;
        let result = scrub_raw_log(input);
        assert!(!result.contains("My Cool Deck"));
        assert!(result.contains(r#""Name":"Deck-1a2b3c4d""#));
        assert!(result.contains(r#""CourseId":"c1""#));
    }

    #[test]
    fn test_scrub_raw_log_deck_name_precon_catalog_shape_redacted() {
        // Simulates a <== DeckGetAllPreconDecksV3 catalog entry. Precon
        // names are WotC localization keys, but are scrubbed anyway by the
        // uniform structural rule.
        let input = r#"{"PreconDecks":[{"DeckId":"1a2b3c4d-5e6f-7890-abcd-ef1234567890","Name":"?=?Loc/Decks/Precon/GreenWhite","Format":"Standard"}]}"#;
        let result = scrub_raw_log(input);
        assert!(!result.contains("Loc/Decks/Precon"));
        assert!(result.contains(r#""Name":"Deck-1a2b3c4d""#));
    }

    #[test]
    fn test_scrub_raw_log_deck_name_pet_diagnostic_line_redacted() {
        let input = "Can't find pet for deck My Cool Deck (1a2b3c4d-5e6f-7890-abcd-ef1234567890)";
        let result = scrub_raw_log(input);
        assert_eq!(
            result,
            "Can't find pet for deck Deck-1a2b3c4d (1a2b3c4d-5e6f-7890-abcd-ef1234567890)"
        );
    }

    #[test]
    fn test_scrub_raw_log_deck_name_pet_diagnostic_line_with_frame_prefix_redacted() {
        // Unstripped archive logs prefix each line with a "[N] " frame index.
        let input =
            "[42] Can't find pet for deck My Cool Deck (1a2b3c4d-5e6f-7890-abcd-ef1234567890)";
        let result = scrub_raw_log(input);
        assert_eq!(
            result,
            "[42] Can't find pet for deck Deck-1a2b3c4d (1a2b3c4d-5e6f-7890-abcd-ef1234567890)"
        );
    }

    #[test]
    fn test_scrub_raw_log_deck_name_pet_diagnostic_line_crlf_redacted_terminator_preserved() {
        // Windows-written Player.log files use \r\n line endings. The pet-
        // diagnostic pattern's `$` anchor must match past the `\r`, and the
        // `\r` must survive in the output byte-for-byte (not be normalized
        // to LF-only).
        let input = "Line 1\r\nCan't find pet for deck My Cool Deck (1a2b3c4d-5e6f-7890-abcd-ef1234567890)\r\nLine 3\r\n";
        let result = scrub_raw_log(input);
        assert_eq!(
            result,
            "Line 1\r\nCan't find pet for deck Deck-1a2b3c4d (1a2b3c4d-5e6f-7890-abcd-ef1234567890)\r\nLine 3\r\n"
        );
    }

    #[test]
    fn test_scrub_raw_log_with_keep_deck_names_true_crlf_passthrough() {
        let opts = ScrubOptions {
            keep_player_names: false,
            keep_deck_names: true,
        };
        let input =
            "Can't find pet for deck My Cool Deck (1a2b3c4d-5e6f-7890-abcd-ef1234567890)\r\n";
        let result = scrub_raw_log_with(input, &opts);
        assert_eq!(result, input);
    }

    #[test]
    fn test_scrub_raw_log_with_keep_deck_names_true_preserves_names() {
        let opts = ScrubOptions {
            keep_player_names: false,
            keep_deck_names: true,
        };
        let input = r#"{"DeckId":"1a2b3c4d-5e6f-7890-abcd-ef1234567890","Name":"My Cool Deck","Format":"Standard"}"#;
        let result = scrub_raw_log_with(input, &opts);
        assert!(result.contains("My Cool Deck"));
    }

    #[test]
    fn test_scrub_raw_log_with_keep_deck_names_true_still_redacts_other_patterns() {
        let opts = ScrubOptions {
            keep_player_names: false,
            keep_deck_names: true,
        };
        let input = r#"Token: secret123 and {"DeckId":"1a2b3c4d-5e6f-7890-abcd-ef1234567890","Name":"My Cool Deck"}"#;
        let result = scrub_raw_log_with(input, &opts);
        assert!(result.contains("My Cool Deck"));
        assert!(result.contains("Token: <redacted>"));
        assert!(!result.contains("secret123"));
    }

    #[test]
    fn test_scrub_raw_log_deck_name_structure_preserved() {
        // DeckId, Format, Attributes, and card-list structure must survive
        // scrubbing unchanged; only the Name value is replaced, with the
        // label derived from the first 8 hex of the adjacent DeckId.
        let input = r#"{"DeckId":"1a2b3c4d-5e6f-7890-abcd-ef1234567890","Name":"My Cool Deck","Format":"Standard","Attributes":[{"name":"Version","value":"1"}],"MainDeck":[{"cardId":1,"quantity":4}]}"#;
        let result = scrub_raw_log(input);
        assert!(result.contains(r#""DeckId":"1a2b3c4d-5e6f-7890-abcd-ef1234567890""#));
        assert!(result.contains(r#""Format":"Standard""#));
        assert!(result.contains(r#""Attributes":[{"name":"Version","value":"1"}]"#));
        assert!(result.contains(r#""MainDeck":[{"cardId":1,"quantity":4}]"#));
        assert!(result.contains(r#""Name":"Deck-1a2b3c4d""#));
        assert!(!result.contains("My Cool Deck"));
    }

    #[test]
    fn test_scrub_raw_log_deck_name_determinism_same_guid_across_carriers() {
        // The same DeckId in the plain and escaped carrier shapes must
        // pseudonymize to the same label.
        let plain = r#"{"DeckId":"1a2b3c4d-5e6f-7890-abcd-ef1234567890","Name":"My Cool Deck"}"#;
        let escaped = r#"{"request":"{\"DeckId\":\"1a2b3c4d-5e6f-7890-abcd-ef1234567890\",\"Name\":\"My Cool Deck\"}"}"#;
        let plain_result = scrub_raw_log(plain);
        let escaped_result = scrub_raw_log(escaped);
        assert!(plain_result.contains(r#""Name":"Deck-1a2b3c4d""#));
        assert!(escaped_result.contains(r#"\"Name\":\"Deck-1a2b3c4d"#));
    }

    #[test]
    fn test_scrub_raw_log_deck_name_determinism_rename_chain_collapses_to_one_label() {
        // Same DeckId, two different Name values (simulating a rename within
        // one log session) — both must pseudonymize to the same label.
        let input = concat!(
            r#"{"DeckId":"1a2b3c4d-5e6f-7890-abcd-ef1234567890","Name":"Old Name"}"#,
            "\n",
            r#"{"DeckId":"1a2b3c4d-5e6f-7890-abcd-ef1234567890","Name":"New Name"}"#,
        );
        let result = scrub_raw_log(input);
        assert_eq!(result.matches(r#""Name":"Deck-1a2b3c4d""#).count(), 2);
        assert!(!result.contains("Old Name"));
        assert!(!result.contains("New Name"));
    }

    #[test]
    fn test_scrub_raw_log_deck_name_embedded_escaped_quote_no_fragment_survives() {
        // Plain-context name value with embedded escaped quotes.
        let input = r#""DeckId":"1a2b3c4d-5e6f-7890-abcd-ef1234567890","Name":"My \"Cool\" Deck""#;
        let result = scrub_raw_log(input);
        assert!(!result.contains("My"));
        assert!(!result.contains("Cool"));
        assert!(result.contains(r#""Name":"Deck-1a2b3c4d""#));
    }

    #[test]
    fn test_scrub_raw_log_deck_name_malformed_mixed_escaping_no_fragment_survives() {
        // A malformed mid-rename ==> save request: a raw `\"` appears inside
        // the escaped name value instead of the value being cleanly closed.
        // No fragment of the name must survive.
        let input =
            r#"{\"DeckId\":\"1a2b3c4d-5e6f-7890-abcd-ef1234567890\",\"Name\":\"Old\" New Deck\"}"#;
        let result = scrub_raw_log(input);
        assert!(!result.contains("Old"));
        assert!(!result.contains("New Deck"));
        assert!(result.contains(r#"\"Name\":\"Deck-1a2b3c4d"#));
    }

    #[test]
    fn test_scrub_raw_log_deck_name_cosmetics_pets_emotes_byte_identical() {
        // Cosmetic "Id"+"Name" pairs (Pets/Emotes) use the key "Id", not
        // "DeckId", so the deck-name patterns must not fire on them.
        let input =
            r#"{"Pets":[{"Id":12345,"Name":"Fluffy"}],"Emotes":[{"Id":67890,"Name":"Wave"}]}"#;
        assert_eq!(scrub_raw_log(input), input);
    }

    #[test]
    fn test_scrub_raw_log_deck_name_idempotent() {
        let input = r#"{"DeckId":"1a2b3c4d-5e6f-7890-abcd-ef1234567890","Name":"My Cool Deck"}"#;
        let first_pass = scrub_raw_log(input);
        let second_pass = scrub_raw_log(&first_pass);
        assert_eq!(
            first_pass, second_pass,
            "Deck-name scrubbing should be idempotent"
        );
    }

    // --- Corpus validation (env-gated, not run in CI) ---

    /// Returns `true` if `val` is `"<redacted>"` — the flat-redaction
    /// placeholder used by every non-deck-name scrub pattern.
    fn is_redacted(val: &str) -> bool {
        val == "<redacted>"
    }

    /// Returns `true` if `val` matches `^Deck-[0-9a-fA-F]{8}$` — the
    /// deterministic pseudonymization label used by the deck-name patterns.
    fn is_deck_label(val: &str) -> bool {
        val.len() == 13
            && val.starts_with("Deck-")
            && val[5..].bytes().all(|b| b.is_ascii_hexdigit())
    }

    /// A PII-detection entry: a human-readable label, a regex that captures
    /// the sensitive value in group 1, and a validator that returns `true`
    /// when the captured (post-scrub) value indicates successful redaction.
    type PiiPattern = (&'static str, Regex, fn(&str) -> bool);

    /// Build the PII-detection patterns used by [`test_corpus_scrub_no_pii_survives`].
    ///
    /// Combines [`hardware_and_name_pii_patterns`] (flat `"<redacted>"`
    /// placeholders, via [`is_redacted`]) with [`deck_name_pii_patterns`]
    /// (`Deck-<8hex>` labels, via [`is_deck_label`]).
    fn corpus_pii_patterns() -> Vec<PiiPattern> {
        let mut patterns = hardware_and_name_pii_patterns();
        patterns.extend(deck_name_pii_patterns());
        patterns
    }

    /// PII-detection patterns for display names and hardware fingerprints —
    /// all redact to the flat `"<redacted>"` placeholder.
    fn hardware_and_name_pii_patterns() -> Vec<PiiPattern> {
        vec![
            (
                "screenName",
                Regex::new(r#""[Ss]creen[Nn]ame"\s*:\s*"([^"]+)""#)
                    .unwrap_or_else(|_| unreachable!()),
                is_redacted,
            ),
            (
                "playerName",
                Regex::new(r#""[Pp]layer[Nn]ame"\s*:\s*"([^"]+)""#)
                    .unwrap_or_else(|_| unreachable!()),
                is_redacted,
            ),
            // Windows GPU fingerprint patterns.
            (
                "Renderer",
                Regex::new(r"(?m)^\s+Renderer:\s+(.+)").unwrap_or_else(|_| unreachable!()),
                is_redacted,
            ),
            (
                "Vendor",
                Regex::new(r"(?m)^\s+Vendor:\s+(.+)").unwrap_or_else(|_| unreachable!()),
                is_redacted,
            ),
            (
                "VRAM",
                Regex::new(r"(?m)^\s+VRAM:\s+(.+)").unwrap_or_else(|_| unreachable!()),
                is_redacted,
            ),
            (
                "Driver",
                Regex::new(r"(?m)^\s+Driver:\s+(.+)").unwrap_or_else(|_| unreachable!()),
                is_redacted,
            ),
            // macOS Metal GPU fingerprint patterns.
            (
                "macOS preferred device",
                Regex::new(r"(?m)^\s*preferred device:\s+(.+)").unwrap_or_else(|_| unreachable!()),
                is_redacted,
            ),
            (
                "macOS Metal devices available",
                Regex::new(r"(?m)^\s*Metal devices available:\s+(.+)")
                    .unwrap_or_else(|_| unreachable!()),
                is_redacted,
            ),
            (
                "macOS enumerated Metal device",
                Regex::new(r"(?m)^\s*\d+:\s+(.+?)\s*\((?:high|low) power\)")
                    .unwrap_or_else(|_| unreachable!()),
                is_redacted,
            ),
            (
                "macOS Using device",
                Regex::new(r"(?m)^\s*Using device\s+(.+)").unwrap_or_else(|_| unreachable!()),
                is_redacted,
            ),
            (
                "macOS Initializing Metal device caps",
                Regex::new(r"(?m)^\s*Initializing Metal device caps:\s+(.+)")
                    .unwrap_or_else(|_| unreachable!()),
                is_redacted,
            ),
            // Unity SystemInfo block (newer MTGA builds, both platforms).
            (
                "SystemInfo graphicsDeviceName",
                Regex::new(r"(?m)^graphicsDeviceName (.+)").unwrap_or_else(|_| unreachable!()),
                is_redacted,
            ),
            (
                "SystemInfo graphicsDeviceVendor",
                Regex::new(r"(?m)^graphicsDeviceVendor (.+)").unwrap_or_else(|_| unreachable!()),
                is_redacted,
            ),
            (
                "SystemInfo graphicsDeviceVersion",
                Regex::new(r"(?m)^graphicsDeviceVersion (.+)").unwrap_or_else(|_| unreachable!()),
                is_redacted,
            ),
            (
                "SystemInfo deviceModel",
                Regex::new(r"(?m)^deviceModel (.+)").unwrap_or_else(|_| unreachable!()),
                is_redacted,
            ),
            (
                "SystemInfo operatingSystem",
                Regex::new(r"(?m)^operatingSystem (.+)").unwrap_or_else(|_| unreachable!()),
                is_redacted,
            ),
            (
                "SystemInfo processorType",
                Regex::new(r"(?m)^processorType (.+)").unwrap_or_else(|_| unreachable!()),
                is_redacted,
            ),
        ]
    }

    /// PII-detection patterns for deck names — all pseudonymize to a
    /// deterministic `Deck-<8hex>` label rather than the flat
    /// `"<redacted>"` placeholder.
    fn deck_name_pii_patterns() -> Vec<PiiPattern> {
        vec![
            // Deck names: plain-context `"DeckId"`/`"Name"` pairing (covers
            // `<==` responses, `StartHook`, precon catalog, course
            // summaries). Captures only the `Name` value.
            (
                "deck Name (plain)",
                Regex::new(
                    r#""DeckId":\s*"[0-9a-fA-F]{8}[0-9a-fA-F-]*",\s*(?:"[A-Za-z0-9_]+":(?:"[^"\\]*"|-?[0-9]+|true|false|null),\s*)*"Name":\s*"((?:[^"\\]|\\.)*)""#,
                )
                .unwrap_or_else(|_| unreachable!()),
                is_deck_label,
            ),
            // Deck names: escaped-context `\"DeckId\"`/`\"Name\"` pairing
            // (covers `==>` request payloads). Captures only the `Name`
            // value.
            (
                "deck Name (escaped)",
                Regex::new(
                    r#"\\"DeckId\\":\s*\\"[0-9a-fA-F]{8}[0-9a-fA-F-]*\\",\s*(?:\\"[A-Za-z0-9_]+\\":(?:\\"[^"\\]*\\"|-?[0-9]+|true|false|null),\s*)*\\"Name\\":\s*\\"((?:[^"\\]|\\\\.|\\"[^,}\]"])*)"#,
                )
                .unwrap_or_else(|_| unreachable!()),
                is_deck_label,
            ),
            // Deck names: Unity console "Can't find pet for deck" diagnostic
            // (plain text, not JSON). Captures the name-or-label token.
            // Detection-only, so `\r?` before `$` is sufficient here (no
            // capture-and-re-emit needed — this pattern never rewrites text).
            (
                "deck Name (pet-diagnostic line)",
                Regex::new(
                    r"(?m)^(?:\[\d+\] )?Can't find pet for deck (.+) \([0-9a-fA-F]{8}[0-9a-fA-F-]{28}\)\r?$",
                )
                .unwrap_or_else(|_| unreachable!()),
                is_deck_label,
            ),
        ]
    }

    /// Run `scrub_raw_log` against every `.log` file in the corpus directory
    /// and verify that none of the PII patterns survive scrubbing.
    ///
    /// Skipped unless `SCRUBBER_CORPUS_DIR` is set:
    /// ```sh
    /// SCRUBBER_CORPUS_DIR=/tmp/smoke-corpus cargo test corpus_scrub -- --nocapture
    /// ```
    #[test]
    fn test_corpus_scrub_no_pii_survives() {
        let Ok(dir) = std::env::var("SCRUBBER_CORPUS_DIR") else {
            return;
        };
        let corpus_dir = std::path::PathBuf::from(dir);
        let pii_patterns = corpus_pii_patterns();

        let mut total_before = 0u32;
        let mut failures: Vec<String> = Vec::new();

        let entries: Vec<_> = std::fs::read_dir(&corpus_dir)
            .unwrap_or_else(|_| unreachable!())
            .filter_map(Result::ok)
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "log"))
            .collect();

        for entry in &entries {
            let path = entry.path();
            let filename = path
                .file_name()
                .unwrap_or_else(|| unreachable!())
                .to_string_lossy();
            let Ok(raw) = std::fs::read_to_string(&path) else {
                continue;
            };

            let scrubbed = scrub_raw_log(&raw);

            for (name, re, is_valid) in &pii_patterns {
                let before = u32::try_from(re.find_iter(&raw).count()).unwrap_or(u32::MAX);
                total_before += before;

                let leaked: Vec<String> = re
                    .captures_iter(&scrubbed)
                    .filter_map(|cap| {
                        let val = cap.get(1).map_or("", |m| m.as_str());
                        if is_valid(val) {
                            None
                        } else {
                            Some(val.to_owned())
                        }
                    })
                    .collect();

                for val in &leaked {
                    failures.push(format!("{filename}: {name} leaked: {val:?}"));
                }
            }
        }

        assert!(
            total_before > 0,
            "corpus should contain at least one PII match to be a meaningful test"
        );
        assert!(
            failures.is_empty(),
            "PII survived scrubbing in {} location(s) (of {total_before} raw matches):\n{}",
            failures.len(),
            failures.join("\n")
        );
    }
}
