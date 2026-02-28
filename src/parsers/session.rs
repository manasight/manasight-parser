//! Session event parser: login, account ID, display name, and logout.
//!
//! Recognizes three log signatures that establish and terminate player
//! identity within a session:
//!
//! | Signature | Meaning |
//! |-----------|---------|
//! | `Updated account. DisplayName:` | Account identity (display name + account ID) |
//! | `authenticateResponse` | Login confirmation (screen name in JSON) |
//! | `FrontDoorConnection.Close` | Logout / disconnect |
//!
//! These are the first meaningful events in any log and are used to tag
//! all subsequent events with the active player identity.

use crate::events::{EventMetadata, GameEvent, SessionEvent};
use crate::log::entry::LogEntry;
use crate::parsers::api_common;

/// Prefix that introduces account identity lines in the log.
const ACCOUNT_UPDATE_PREFIX: &str = "Updated account. DisplayName:";

/// Marker for authentication response entries.
const AUTHENTICATE_RESPONSE_MARKER: &str = "authenticateResponse";

/// Marker for front door connection close (logout/disconnect).
const FRONT_DOOR_CLOSE_MARKER: &str = "FrontDoorConnection.Close";

/// Attempts to parse a [`LogEntry`] as a session event.
///
/// Returns `Some(GameEvent::Session(_))` if the entry matches one of the
/// three recognized session signatures, or `None` if the entry is not a
/// session event.
///
/// The `timestamp` is `None` when the log entry header did not contain a
/// parseable timestamp. It is passed through to [`EventMetadata`] so
/// downstream consumers can distinguish real vs missing timestamps.
pub fn try_parse(
    entry: &LogEntry,
    timestamp: Option<chrono::DateTime<chrono::Utc>>,
) -> Option<GameEvent> {
    let body = &entry.body;

    // Strip the header prefix (e.g., "[UnityCrossThreadLogger]") to get
    // the content portion of the first line.
    let content = strip_header_prefix(body);

    if let Some(payload) = try_parse_account_update(content) {
        let metadata = EventMetadata::new(timestamp, body.as_bytes().to_vec());
        return Some(GameEvent::Session(SessionEvent::new(metadata, payload)));
    }

    if let Some(payload) = try_parse_authenticate_response(body) {
        let metadata = EventMetadata::new(timestamp, body.as_bytes().to_vec());
        return Some(GameEvent::Session(SessionEvent::new(metadata, payload)));
    }

    if try_match_front_door_close(content) {
        let metadata = EventMetadata::new(timestamp, body.as_bytes().to_vec());
        let payload = serde_json::json!({
            "type": "session_disconnect",
        });
        return Some(GameEvent::Session(SessionEvent::new(metadata, payload)));
    }

    None
}

/// Strips the `[UnityCrossThreadLogger]` or `[Client GRE]` bracket prefix
/// from the first line of the body, returning the remaining content.
///
/// If the body does not start with a recognized bracket prefix, returns
/// the full body unchanged.
fn strip_header_prefix(body: &str) -> &str {
    // The first line contains the header. Find the closing bracket.
    let first_line = body.lines().next().unwrap_or(body);
    if let Some(pos) = first_line.find(']') {
        first_line[pos + 1..].trim_start()
    } else {
        first_line
    }
}

/// Attempts to parse an `Updated account. DisplayName:` line.
///
/// Expected format (after header stripping):
/// ```text
/// Updated account. DisplayName:SomeName, AccountID:abc123def456, ...
/// ```
///
/// Extracts `DisplayName` and `AccountID` fields and returns them as a
/// JSON payload with `type: "session_account_update"`.
fn try_parse_account_update(content: &str) -> Option<serde_json::Value> {
    if !content.contains(ACCOUNT_UPDATE_PREFIX) {
        return None;
    }

    // Extract text after "DisplayName:" up to the next comma or end of line.
    let after_prefix = content.split(ACCOUNT_UPDATE_PREFIX).nth(1)?;
    let display_name = after_prefix.split(',').next().unwrap_or("").trim();

    // Extract AccountID if present.
    let account_id = content
        .split("AccountID:")
        .nth(1)
        .and_then(|s| s.split(',').next())
        .map_or("", str::trim);

    Some(serde_json::json!({
        "type": "session_account_update",
        "display_name": display_name,
        "account_id": account_id,
    }))
}

/// Attempts to parse an `authenticateResponse` entry.
///
/// The authenticate response can appear in two forms:
///
/// 1. As a label on the first line, with a JSON body on subsequent lines
///    containing a `screenName` field.
/// 2. As a key within a JSON payload on the first or subsequent lines.
///
/// In either case, the function extracts the `screenName` from the JSON
/// and returns a payload with `type: "session_authenticate"`.
fn try_parse_authenticate_response(full_body: &str) -> Option<serde_json::Value> {
    // Check full_body (which includes all lines) for the marker.
    if !full_body.contains(AUTHENTICATE_RESPONSE_MARKER) {
        return None;
    }

    // Try to extract JSON from the body (lines after the header line).
    let json_body = api_common::extract_json_from_body(full_body);

    if let Some(json_str) = json_body {
        match serde_json::from_str::<serde_json::Value>(json_str) {
            Ok(parsed) => {
                // Look for screenName at the top level or nested in the response.
                let screen_name = find_screen_name(&parsed);
                return Some(serde_json::json!({
                    "type": "session_authenticate",
                    "screen_name": screen_name.unwrap_or_default(),
                    "raw_response": parsed,
                }));
            }
            Err(e) => {
                ::log::warn!(
                    "authenticateResponse: malformed JSON body, falling back to empty screen_name: {e}"
                );
            }
        }
    }

    // If no JSON body found or JSON was malformed, emit a simpler payload.
    Some(serde_json::json!({
        "type": "session_authenticate",
        "screen_name": "",
    }))
}

/// Returns `true` if the content matches a `FrontDoorConnection.Close` entry.
fn try_match_front_door_close(content: &str) -> bool {
    content.contains(FRONT_DOOR_CLOSE_MARKER)
}

/// Recursively searches a JSON value for a `screenName` field.
///
/// Checks the top level and one level of nesting (common in MTGA
/// authenticate responses).
fn find_screen_name(value: &serde_json::Value) -> Option<String> {
    // Check top-level.
    if let Some(name) = value.get("screenName").and_then(|v| v.as_str()) {
        return Some(name.to_owned());
    }

    // Check one level of nesting (e.g., `{"authenticateResponse": {"screenName": ...}}`).
    if let Some(obj) = value.as_object() {
        for (_key, nested) in obj {
            if let Some(name) = nested.get("screenName").and_then(|v| v.as_str()) {
                return Some(name.to_owned());
            }
        }
    }

    None
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parsers::test_helpers::{session_payload, test_timestamp, unity_entry, EntryHeader};

    // -- Account update parsing -----------------------------------------------

    mod account_update {
        use super::*;

        #[test]
        fn test_try_parse_account_update_basic() {
            let body = "[UnityCrossThreadLogger]Updated account. \
                         DisplayName:TestPlayer, \
                         AccountID:abcdef123456, \
                         Token:sometoken123";
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| {
                // Safe: we just asserted is_some() above.
                unreachable!()
            });
            let payload = session_payload(event);

            assert_eq!(payload["type"], "session_account_update");
            assert_eq!(payload["display_name"], "TestPlayer");
            assert_eq!(payload["account_id"], "abcdef123456");
        }

        #[test]
        fn test_try_parse_account_update_with_space_after_header() {
            let body = "[UnityCrossThreadLogger] Updated account. \
                         DisplayName:Player Two, \
                         AccountID:xyz789";
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = session_payload(event);

            assert_eq!(payload["type"], "session_account_update");
            assert_eq!(payload["display_name"], "Player Two");
            assert_eq!(payload["account_id"], "xyz789");
        }

        #[test]
        fn test_try_parse_account_update_empty_display_name() {
            let body = "[UnityCrossThreadLogger]Updated account. \
                         DisplayName:, AccountID:abc123";
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = session_payload(event);

            assert_eq!(payload["display_name"], "");
            assert_eq!(payload["account_id"], "abc123");
        }

        #[test]
        fn test_try_parse_account_update_no_account_id() {
            let body = "[UnityCrossThreadLogger]Updated account. DisplayName:Solo";
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = session_payload(event);

            assert_eq!(payload["display_name"], "Solo");
            assert_eq!(payload["account_id"], "");
        }

        #[test]
        fn test_try_parse_account_update_preserves_raw_bytes() {
            let body = "[UnityCrossThreadLogger]Updated account. \
                         DisplayName:RawTest, AccountID:raw123";
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            assert_eq!(event.metadata().raw_bytes(), body.as_bytes());
        }

        #[test]
        fn test_try_parse_account_update_stores_timestamp() {
            let body = "[UnityCrossThreadLogger]Updated account. \
                         DisplayName:TsTest, AccountID:ts123";
            let entry = unity_entry(body);
            let ts = Some(test_timestamp());
            let result = try_parse(&entry, ts);

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            assert_eq!(event.metadata().timestamp(), ts);
        }

        #[test]
        fn test_try_parse_account_update_with_timestamp_in_header() {
            let body = "[UnityCrossThreadLogger]2/25/2026 12:00:00 PM \
                         Updated account. DisplayName:TimestampPlayer, \
                         AccountID:ts456";
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = session_payload(event);

            assert_eq!(payload["display_name"], "TimestampPlayer");
            assert_eq!(payload["account_id"], "ts456");
        }
    }

    // -- Authenticate response parsing ----------------------------------------

    mod authenticate_response {
        use super::*;

        #[test]
        fn test_try_parse_authenticate_response_with_json_body() {
            let body = "[UnityCrossThreadLogger]authenticateResponse\n\
                         {\n\
                           \"screenName\": \"TestPlayer#12345\"\n\
                         }";
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = session_payload(event);

            assert_eq!(payload["type"], "session_authenticate");
            assert_eq!(payload["screen_name"], "TestPlayer#12345");
        }

        #[test]
        fn test_try_parse_authenticate_response_nested_screen_name() {
            let body = "[UnityCrossThreadLogger]authenticateResponse\n\
                         {\n\
                           \"authenticateResponse\": {\n\
                             \"screenName\": \"Nested#99999\"\n\
                           }\n\
                         }";
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = session_payload(event);

            assert_eq!(payload["screen_name"], "Nested#99999");
        }

        #[test]
        fn test_try_parse_authenticate_response_no_json() {
            let body = "[UnityCrossThreadLogger]authenticateResponse";
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = session_payload(event);

            assert_eq!(payload["type"], "session_authenticate");
            assert_eq!(payload["screen_name"], "");
        }

        #[test]
        fn test_try_parse_authenticate_response_no_screen_name_in_json() {
            let body = "[UnityCrossThreadLogger]authenticateResponse\n\
                         {\"otherField\": \"value\"}";
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = session_payload(event);

            assert_eq!(payload["type"], "session_authenticate");
            assert_eq!(payload["screen_name"], "");
        }

        #[test]
        fn test_try_parse_authenticate_response_preserves_raw_response() {
            let body = "[UnityCrossThreadLogger]authenticateResponse\n\
                         {\"screenName\": \"Player#1\", \"token\": \"abc\"}";
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = session_payload(event);

            assert!(payload.get("raw_response").is_some());
            assert_eq!(payload["raw_response"]["token"], "abc");
        }

        #[test]
        fn test_try_parse_authenticate_response_with_timestamp() {
            let body = "[UnityCrossThreadLogger]2/25/2026 12:00:00 PM \
                         authenticateResponse\n\
                         {\"screenName\": \"TsPlayer#555\"}";
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = session_payload(event);

            assert_eq!(payload["screen_name"], "TsPlayer#555");
        }
    }

    // -- FrontDoorConnection.Close parsing ------------------------------------

    mod front_door_close {
        use super::*;

        #[test]
        fn test_try_parse_front_door_close_basic() {
            let body = "[UnityCrossThreadLogger]FrontDoorConnection.Close";
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = session_payload(event);

            assert_eq!(payload["type"], "session_disconnect");
        }

        #[test]
        fn test_try_parse_front_door_close_with_details() {
            let body = "[UnityCrossThreadLogger]FrontDoorConnection.Close \
                         reason: server shutdown";
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = session_payload(event);

            assert_eq!(payload["type"], "session_disconnect");
        }

        #[test]
        fn test_try_parse_front_door_close_with_timestamp() {
            let body = "[UnityCrossThreadLogger]2/25/2026 12:00:00 PM \
                         FrontDoorConnection.Close";
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = session_payload(event);

            assert_eq!(payload["type"], "session_disconnect");
        }

        #[test]
        fn test_try_parse_front_door_close_preserves_metadata() {
            let body = "[UnityCrossThreadLogger]FrontDoorConnection.Close";
            let entry = unity_entry(body);
            let ts = Some(test_timestamp());
            let result = try_parse(&entry, ts);

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            assert_eq!(event.metadata().timestamp(), ts);
            assert_eq!(event.metadata().raw_bytes(), body.as_bytes());
        }
    }

    // -- Non-session entries (should return None) -----------------------------

    mod non_session {
        use super::*;

        #[test]
        fn test_try_parse_unrelated_entry_returns_none() {
            let body = "[UnityCrossThreadLogger]greToClientEvent\n{\"data\": 1}";
            let entry = unity_entry(body);
            assert!(try_parse(&entry, Some(test_timestamp())).is_none());
        }

        #[test]
        fn test_try_parse_empty_body_returns_none() {
            let body = "[UnityCrossThreadLogger]";
            let entry = unity_entry(body);
            assert!(try_parse(&entry, Some(test_timestamp())).is_none());
        }

        #[test]
        fn test_try_parse_client_gre_entry_returns_none() {
            let entry = LogEntry {
                header: EntryHeader::ClientGre,
                body: "[Client GRE]some GRE message".to_owned(),
            };
            assert!(try_parse(&entry, Some(test_timestamp())).is_none());
        }

        #[test]
        fn test_try_parse_similar_but_different_marker_returns_none() {
            let body = "[UnityCrossThreadLogger]FrontDoorConnection.Open";
            let entry = unity_entry(body);
            assert!(try_parse(&entry, Some(test_timestamp())).is_none());
        }

        #[test]
        fn test_try_parse_partial_account_marker_returns_none() {
            let body = "[UnityCrossThreadLogger]Updated account status";
            let entry = unity_entry(body);
            assert!(try_parse(&entry, Some(test_timestamp())).is_none());
        }
    }

    // -- Performance class ---------------------------------------------------

    mod performance_class {
        use super::*;
        use crate::events::PerformanceClass;

        #[test]
        fn test_session_event_is_durable_per_event() {
            let body = "[UnityCrossThreadLogger]Updated account. \
                         DisplayName:ClassTest, AccountID:class123";
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            assert_eq!(event.performance_class(), PerformanceClass::DurablePerEvent);
        }
    }

    // -- Internal helpers ----------------------------------------------------

    mod helpers {
        use super::*;

        #[test]
        fn test_strip_header_prefix_unity() {
            let result = strip_header_prefix("[UnityCrossThreadLogger]some content");
            assert_eq!(result, "some content");
        }

        #[test]
        fn test_strip_header_prefix_with_space() {
            let result = strip_header_prefix("[UnityCrossThreadLogger] spaced content");
            assert_eq!(result, "spaced content");
        }

        #[test]
        fn test_strip_header_prefix_client_gre() {
            let result = strip_header_prefix("[Client GRE]gre content");
            assert_eq!(result, "gre content");
        }

        #[test]
        fn test_strip_header_prefix_no_bracket() {
            let result = strip_header_prefix("no bracket here");
            assert_eq!(result, "no bracket here");
        }

        #[test]
        fn test_find_screen_name_top_level() {
            let value = serde_json::json!({"screenName": "Player#123"});
            assert_eq!(find_screen_name(&value), Some("Player#123".to_owned()));
        }

        #[test]
        fn test_find_screen_name_nested() {
            let value = serde_json::json!({
                "authenticateResponse": {"screenName": "Nested#456"}
            });
            assert_eq!(find_screen_name(&value), Some("Nested#456".to_owned()));
        }

        #[test]
        fn test_find_screen_name_not_present() {
            let value = serde_json::json!({"other": "data"});
            assert!(find_screen_name(&value).is_none());
        }
    }
}
