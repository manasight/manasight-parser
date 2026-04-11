//! Privacy scrubber for raw MTGA log text.
//!
//! Strips sensitive data (auth tokens, bearer tokens, OS-specific user paths,
//! session identifiers, display names, and hardware fingerprint lines) from
//! unstructured `Player.log` text. This is a best-effort filter; novel token
//! formats may slip through.
//!
//! Regex patterns are compiled once via [`std::sync::LazyLock`] and reused
//! across all calls.

use std::sync::LazyLock;

use regex::Regex;

/// A compiled regex pattern paired with its replacement string.
struct ScrubPattern {
    regex: Regex,
    replacement: &'static str,
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
/// - Hardware fingerprint lines (Renderer, Vendor, VRAM, Driver)
static SCRUB_PATTERNS: LazyLock<Vec<ScrubPattern>> = LazyLock::new(|| {
    // Patterns and replacements. Each regex is compiled exactly once.
    // Order matters: more specific patterns should come before general ones
    // if there is overlap. Currently there is no overlap between categories.
    let definitions: &[(&str, &str)] = &[
        // Auth tokens: "Token: <base64-or-hex-value>"
        // Matches "Token:" followed by optional whitespace and a non-whitespace token value.
        (r"Token:\s*\S+", "Token: <redacted>"),
        // Bearer tokens in HTTP Authorization headers.
        // Uses word boundary to avoid matching game cosmetics like
        // "Title_StandardBearer" where "Bearer" appears as a substring
        // of a larger word. The \b anchor matches at the start of the
        // string or after a non-word character, so "Bearer" following
        // a letter (as in "StandardBearer") does not match.
        (r"\bBearer\s+\S+", "Bearer <redacted>"),
        // WotC account IDs in log line prefixes.
        // Arena logs game messages prefixed with the player's account ID:
        //   "Match to CR4QJUQPDBCVVMGCGNZLWGDFJE: AuthenticateResponse"
        (r"Match to [A-Z0-9_]+:", "Match to <redacted>:"),
        // JSON "clientId" values from authenticateResponse blocks.
        (
            r#""[Cc]lient[Ii]d"\s*:\s*"[^"]+""#,
            r#""clientId": "<redacted>""#,
        ),
        // JSON "userId" values from matchGameRoomStateChangedEvent blocks.
        (
            r#""[Uu]ser[Ii]d"\s*:\s*"[^"]+""#,
            r#""userId": "<redacted>""#,
        ),
        // Windows paths: C:\Users\<username>\ (any drive letter)
        (r"[A-Z]:\\Users\\[^\\]+\\", r"<user-path>\"),
        // macOS paths: /Users/<username>/
        (r"/Users/[^/]+/", "<user-path>/"),
        // Linux paths: /home/<username>/
        (r"/home/[^/]+/", "<user-path>/"),
        // Session identifiers: JSON "token" values from authenticateResponse
        // and similar auth payloads.
        (r#""[Tt]oken"\s*:\s*"[^"]+""#, r#""token": "<redacted>""#),
        // Session identifiers: JSON "sessionId" values from auth responses.
        (
            r#""[Ss]ession[Ii]d"\s*:\s*"[^"]+""#,
            r#""sessionId": "<redacted>""#,
        ),
        // Display names: JSON "screenName" values from authenticateResponse.
        (
            r#""[Ss]creen[Nn]ame"\s*:\s*"[^"]+""#,
            r#""screenName": "<redacted>""#,
        ),
        // Display names: JSON "playerName" values from match state.
        // Contains BOTH players' display names, meaning opponent PII
        // is leaked without this pattern.
        (
            r#""[Pp]layer[Nn]ame"\s*:\s*"[^"]+""#,
            r#""playerName": "<redacted>""#,
        ),
        // Hardware fingerprint: GPU renderer line in log header.
        // (?m) enables per-line ^ matching since we scrub the full text buffer.
        // Leading whitespace (^\s+) is required to avoid false positives.
        (r"(?m)^\s+Renderer:\s+.+", "  Renderer: <redacted>"),
        // Hardware fingerprint: GPU vendor.
        (r"(?m)^\s+Vendor:\s+.+", "  Vendor: <redacted>"),
        // Hardware fingerprint: VRAM size in MB.
        (r"(?m)^\s+VRAM:\s+.+", "  VRAM: <redacted>"),
        // Hardware fingerprint: GPU driver version.
        (r"(?m)^\s+Driver:\s+.+", "  Driver: <redacted>"),
    ];

    definitions
        .iter()
        .filter_map(|(pattern, replacement)| {
            // These patterns are static string literals validated by tests.
            // A compilation failure here indicates a programmer error in the
            // pattern definitions above, not a runtime data issue.
            match Regex::new(pattern) {
                Ok(regex) => Some(ScrubPattern { regex, replacement }),
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
    if input.is_empty() {
        return String::new();
    }

    let mut result = input.to_owned();
    for pattern in SCRUB_PATTERNS.iter() {
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
        assert!(result.contains("Version: 1.2.3.4"));
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

    // --- Corpus validation (env-gated, not run in CI) ---

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

        let pii_patterns: Vec<(&str, Regex)> = vec![
            (
                "screenName",
                Regex::new(r#""[Ss]creen[Nn]ame"\s*:\s*"([^"]+)""#)
                    .unwrap_or_else(|_| unreachable!()),
            ),
            (
                "playerName",
                Regex::new(r#""[Pp]layer[Nn]ame"\s*:\s*"([^"]+)""#)
                    .unwrap_or_else(|_| unreachable!()),
            ),
            (
                "Renderer",
                Regex::new(r"(?m)^\s+Renderer:\s+(.+)").unwrap_or_else(|_| unreachable!()),
            ),
            (
                "Vendor",
                Regex::new(r"(?m)^\s+Vendor:\s+(.+)").unwrap_or_else(|_| unreachable!()),
            ),
            (
                "VRAM",
                Regex::new(r"(?m)^\s+VRAM:\s+(.+)").unwrap_or_else(|_| unreachable!()),
            ),
            (
                "Driver",
                Regex::new(r"(?m)^\s+Driver:\s+(.+)").unwrap_or_else(|_| unreachable!()),
            ),
        ];

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

            for (name, re) in &pii_patterns {
                let before = u32::try_from(re.find_iter(&raw).count()).unwrap_or(u32::MAX);
                total_before += before;

                let leaked: Vec<String> = re
                    .captures_iter(&scrubbed)
                    .filter_map(|cap| {
                        let val = cap.get(1).map_or("", |m| m.as_str());
                        if val == "<redacted>" {
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
