//! Connection-error parsers: JSON-bearing and plain-text error-path markers.
//!
//! Parses seven error-path entry types that together form the Layer 1 red
//! triggers for the desktop connection health monitor. The markers live
//! under three different entry headers and use two different payload
//! strategies depending on whether the source line carries a structured
//! JSON payload or is plain text.
//!
//! # Markers handled
//!
//! ## JSON-marker variants (`[UnityCrossThreadLogger]` header)
//!
//! | Marker | `error_type` |
//! |--------|--------------|
//! | `TcpConnection.ProcessRead.Exception` | `tcp_process_read_exception` |
//! | `Client.TcpConnection.ProcessFailure` | `tcp_process_failure_socket_error` |
//! | `GREConnection.MatchDoorConnectionError` | `gre_match_door_connection_error` |
//! | `TcpConnection.Close.Exception` | `tcp_close_exception` |
//!
//! ## Plain-text variants (`[ConnectionManager]` and `Matchmaking:` headers)
//!
//! | Line pattern | Header | `error_type` |
//! |--------------|--------|--------------|
//! | `Reconnect result : <value>` | `[ConnectionManager]` | `reconnect_result` |
//! | `Reconnect succeeded after N attempts` / `Reconnect failed` / `Reconnect timed out` | `[ConnectionManager]` | `reconnect_outcome` |
//! | `Matchmaking: GRE connection lost` | `Matchmaking:` | `gre_connection_lost` |
//!
//! # Bare-marker entries (JSON variants only)
//!
//! All four JSON-marker variants are observed in the disconnect corpus as
//! paired lines — a bare marker (no JSON) followed by a JSON-carrying
//! line. Bare-marker entries return `None`; the paired JSON line on a
//! subsequent entry emits the event.
//!
//! # Payload shapes
//!
//! Two payload strategies coexist under the single `ConnectionError`
//! event type. Desktop consumers switch on `error_type` and read either
//! `payload` (JSON variants) or flat top-level named fields (plain-text
//! variants).
//!
//! ## Strategy 1: JSON passthrough (under a `payload` key)
//!
//! ```json
//! {
//!   "error_type": "<discriminant>",
//!   "payload": { /* full parsed JSON from the log line */ }
//! }
//! ```
//!
//! The parser is agnostic to inner error-code semantics (e.g., platform
//! differences in `NativeErrorCode` — Windows `10054`, macOS `10060` /
//! `10049`). Downstream consumers read fields from `payload` per ADR-011.
//!
//! ## Strategy 2: plain-text flattened fields
//!
//! Plain-text lines carry a small, fixed set of structured fields. Rather
//! than wrapping them under a `payload` key, they are flattened alongside
//! `error_type`:
//!
//! ```json
//! {"error_type": "gre_connection_lost"}
//! {"error_type": "reconnect_result", "result": "Connected" | "Error" | "None"}
//! {"error_type": "reconnect_outcome", "outcome": "succeeded" | "failed" | "timed_out", "attempts": <i64 or null>}
//! ```
//!
//! # Header dispatch
//!
//! [`try_parse`] dispatches on `entry.header` to one of three sub-parsers:
//!
//! - [`EntryHeader::UnityCrossThreadLogger`] → [`try_unity_error`] (JSON variants)
//! - [`EntryHeader::ConnectionManager`] → [`try_connection_manager`] (plain-text)
//! - [`EntryHeader::Matchmaking`] → [`try_matchmaking`] (plain-text)
//! - All other headers → `None`
//!
//! Satisfies feature spec `connection-health-indicator.md` **AC-DET-5**
//! (JSON-marker variants and plain-text variants).

use crate::events::{ConnectionErrorEvent, EventMetadata, GameEvent};
use crate::log::entry::{EntryHeader, LogEntry};
use crate::parsers::api_common;

/// Marker text for `TcpConnection.ProcessRead.Exception` entries.
const PROCESS_READ_EXCEPTION_MARKER: &str = "TcpConnection.ProcessRead.Exception";

/// Marker text for `Client.TcpConnection.ProcessFailure` entries.
const PROCESS_FAILURE_MARKER: &str = "Client.TcpConnection.ProcessFailure";

/// Marker text for `GREConnection.MatchDoorConnectionError` entries.
const MATCH_DOOR_ERROR_MARKER: &str = "GREConnection.MatchDoorConnectionError";

/// Marker text for `TcpConnection.Close.Exception` entries.
const CLOSE_EXCEPTION_MARKER: &str = "TcpConnection.Close.Exception";

/// Stable `error_type` discriminant: `TcpConnection.ProcessRead.Exception`.
const ERROR_TYPE_PROCESS_READ: &str = "tcp_process_read_exception";

/// Stable `error_type` discriminant: `Client.TcpConnection.ProcessFailure`.
const ERROR_TYPE_PROCESS_FAILURE: &str = "tcp_process_failure_socket_error";

/// Stable `error_type` discriminant: `GREConnection.MatchDoorConnectionError`.
const ERROR_TYPE_MATCH_DOOR: &str = "gre_match_door_connection_error";

/// Stable `error_type` discriminant: `TcpConnection.Close.Exception`.
const ERROR_TYPE_CLOSE_EXCEPTION: &str = "tcp_close_exception";

/// Attempts to parse a [`LogEntry`] as a connection-error event.
///
/// Dispatches on `entry.header`:
///
/// - [`EntryHeader::UnityCrossThreadLogger`] — inspect the body for one of
///   the four JSON-bearing error markers. Bare-marker bodies (no JSON
///   payload) return `None`.
/// - [`EntryHeader::ConnectionManager`] — parse the body for
///   `Reconnect result : <value>` or `Reconnect <outcome>` plain-text
///   markers.
/// - [`EntryHeader::Matchmaking`] — parse the body for
///   `Matchmaking: GRE connection lost`.
/// - Any other header — return `None`.
///
/// The `timestamp` is `None` when the log entry header did not contain a
/// parseable timestamp. It is passed through to [`EventMetadata`] so
/// downstream consumers can distinguish real vs missing timestamps.
pub fn try_parse(
    entry: &LogEntry,
    timestamp: Option<chrono::DateTime<chrono::Utc>>,
) -> Option<GameEvent> {
    let payload = match entry.header {
        EntryHeader::UnityCrossThreadLogger => try_unity_error(&entry.body)?,
        EntryHeader::ConnectionManager => try_connection_manager(&entry.body)?,
        EntryHeader::Matchmaking => try_matchmaking(&entry.body)?,
        _ => return None,
    };

    let metadata = EventMetadata::new(timestamp, entry.body.as_bytes().to_vec());
    Some(GameEvent::ConnectionError(ConnectionErrorEvent::new(
        metadata, payload,
    )))
}

/// Matches a `[UnityCrossThreadLogger]` body against the four JSON-marker
/// variants and returns the discriminated payload.
///
/// Returns `None` for bodies that don't contain any known marker, for
/// bare-marker bodies without a JSON payload, and for malformed JSON
/// payloads.
fn try_unity_error(body: &str) -> Option<serde_json::Value> {
    if body.contains(PROCESS_READ_EXCEPTION_MARKER) {
        return try_exception_marker(body, ERROR_TYPE_PROCESS_READ);
    }
    if body.contains(PROCESS_FAILURE_MARKER) {
        return try_exception_marker(body, ERROR_TYPE_PROCESS_FAILURE);
    }
    if body.contains(MATCH_DOOR_ERROR_MARKER) {
        return try_exception_marker(body, ERROR_TYPE_MATCH_DOOR);
    }
    if body.contains(CLOSE_EXCEPTION_MARKER) {
        return try_exception_marker(body, ERROR_TYPE_CLOSE_EXCEPTION);
    }
    None
}

/// Extracts and parses the JSON payload from the given body and wraps it
/// in the discriminated `{error_type, payload}` envelope.
///
/// Returns `None` when the body has no JSON payload (bare-marker entries),
/// when the JSON fails to parse, or when the body is otherwise malformed.
/// A warning is logged on parse failure; bare-marker entries are silent
/// because they are the expected leading half of a paired emission.
fn try_exception_marker(body: &str, error_type: &str) -> Option<serde_json::Value> {
    let json_str = api_common::extract_json_from_body(body)?;
    let parsed: serde_json::Value = match serde_json::from_str(json_str) {
        Ok(v) => v,
        Err(e) => {
            ::log::warn!("{error_type}: malformed JSON payload: {e}");
            return None;
        }
    };
    Some(serde_json::json!({
        "error_type": error_type,
        "payload": parsed,
    }))
}

/// Parses a `[ConnectionManager]` entry body into a flattened plain-text
/// payload.
///
/// The body begins with the literal `[ConnectionManager] ` prefix (the
/// [`LineBuffer`][crate::log::entry::LineBuffer] keeps the header line in
/// the entry body). Recognized suffixes:
///
/// - `Reconnect result : <value>` — only the enumerated values
///   `Connected`, `Error`, and `None` are accepted. Any other value
///   returns `None`.
/// - `Reconnect succeeded after <N> attempts` — emits
///   `reconnect_outcome` with `outcome = "succeeded"`. The attempts count
///   is parsed as `i64`; if unparseable, `attempts` is `null` rather than
///   causing the whole entry to be rejected.
/// - `Reconnect failed` — emits `reconnect_outcome` with
///   `outcome = "failed"` and `attempts = null`.
/// - `Reconnect timed out` — emits `reconnect_outcome` with
///   `outcome = "timed_out"` and `attempts = null`.
///
/// Returns `None` for any body that lacks the `[ConnectionManager] `
/// prefix or that does not match one of the recognized suffixes.
fn try_connection_manager(body: &str) -> Option<serde_json::Value> {
    let content = body.strip_prefix("[ConnectionManager] ")?;

    if let Some(rest) = content.strip_prefix("Reconnect result : ") {
        let result = rest.trim();
        return match result {
            "Connected" | "Error" | "None" => Some(serde_json::json!({
                "error_type": "reconnect_result",
                "result": result,
            })),
            _ => None,
        };
    }

    if let Some(rest) = content.strip_prefix("Reconnect succeeded after ") {
        let attempts = rest
            .split_whitespace()
            .next()
            .and_then(|s| s.parse::<i64>().ok());
        return Some(serde_json::json!({
            "error_type": "reconnect_outcome",
            "outcome": "succeeded",
            "attempts": attempts,
        }));
    }

    if content.starts_with("Reconnect failed") {
        return Some(serde_json::json!({
            "error_type": "reconnect_outcome",
            "outcome": "failed",
            "attempts": serde_json::Value::Null,
        }));
    }

    if content.starts_with("Reconnect timed out") {
        return Some(serde_json::json!({
            "error_type": "reconnect_outcome",
            "outcome": "timed_out",
            "attempts": serde_json::Value::Null,
        }));
    }

    None
}

/// Parses a `Matchmaking:` entry body into a flattened plain-text payload.
///
/// Currently only one marker is recognized:
///
/// - `Matchmaking: GRE connection lost` → `gre_connection_lost`.
///
/// The body is matched with `starts_with` so downstream extensions
/// (trailing descriptors such as `, attempting reconnect`) remain part of
/// the same marker. Any other `Matchmaking:` suffix returns `None`.
fn try_matchmaking(body: &str) -> Option<serde_json::Value> {
    if body.starts_with("Matchmaking: GRE connection lost") {
        return Some(serde_json::json!({"error_type": "gre_connection_lost"}));
    }
    None
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parsers::test_helpers::{
        connection_error_payload, connection_manager_entry, matchmaking_entry, test_timestamp,
        unity_entry,
    };

    /// Build a `[UnityCrossThreadLogger]<marker> <json>` body.
    fn unity_body(marker: &str, json: &str) -> String {
        format!("[UnityCrossThreadLogger]{marker} {json}")
    }

    /// Assert that parsing the entry yielded `Some(GameEvent::ConnectionError)`
    /// with the given `error_type`, and return the inner `payload` field.
    fn assert_connection_error<'a>(
        event: &'a GameEvent,
        expected_error_type: &str,
    ) -> &'a serde_json::Value {
        assert!(
            matches!(event, GameEvent::ConnectionError(_)),
            "expected ConnectionError, got {event:?}"
        );
        let outer = connection_error_payload(event);
        assert_eq!(
            outer["error_type"], expected_error_type,
            "error_type mismatch"
        );
        &outer["payload"]
    }

    // -- TcpConnection.ProcessRead.Exception -------------------------------

    mod process_read_exception {
        use super::*;

        #[test]
        fn test_windows_native_error_code_10054() {
            let body = unity_body(
                PROCESS_READ_EXCEPTION_MARKER,
                r#"{
                    "function":"ReadAsync",
                    "description":"An established connection was aborted by the software in your host machine",
                    "exception":{
                        "Message":"Unable to read data from the transport connection",
                        "ClassName":"System.IO.IOException",
                        "InnerException":{
                            "ClassName":"System.Net.Sockets.SocketException",
                            "NativeErrorCode":10054,
                            "SocketErrorCode":"ConnectionAborted",
                            "Message":"An established connection was aborted"
                        }
                    }
                }"#,
            );
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = assert_connection_error(event, ERROR_TYPE_PROCESS_READ);
            assert_eq!(payload["function"], "ReadAsync");
            assert_eq!(
                payload["exception"]["InnerException"]["NativeErrorCode"],
                10054
            );
            assert_eq!(
                payload["exception"]["InnerException"]["SocketErrorCode"],
                "ConnectionAborted"
            );
        }

        #[test]
        fn test_macos_native_error_code_10060() {
            let body = unity_body(
                PROCESS_READ_EXCEPTION_MARKER,
                r#"{
                    "function":"ReadAsync",
                    "description":"Connection timed out",
                    "exception":{
                        "ClassName":"System.IO.IOException",
                        "InnerException":{
                            "ClassName":"System.Net.Sockets.SocketException",
                            "NativeErrorCode":10060,
                            "SocketErrorCode":"TimedOut",
                            "Message":"Operation timed out"
                        }
                    }
                }"#,
            );
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = assert_connection_error(event, ERROR_TYPE_PROCESS_READ);
            assert_eq!(
                payload["exception"]["InnerException"]["NativeErrorCode"],
                10060
            );
            assert_eq!(
                payload["exception"]["InnerException"]["SocketErrorCode"],
                "TimedOut"
            );
        }

        #[test]
        fn test_macos_native_error_code_10049() {
            let body = unity_body(
                PROCESS_READ_EXCEPTION_MARKER,
                r#"{
                    "function":"ReadAsync",
                    "description":"Address not valid",
                    "exception":{
                        "InnerException":{
                            "NativeErrorCode":10049,
                            "SocketErrorCode":"AddressNotAvailable"
                        }
                    }
                }"#,
            );
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = assert_connection_error(event, ERROR_TYPE_PROCESS_READ);
            assert_eq!(
                payload["exception"]["InnerException"]["NativeErrorCode"],
                10049
            );
        }

        #[test]
        fn test_bare_marker_returns_none() {
            let body = format!("[UnityCrossThreadLogger]{PROCESS_READ_EXCEPTION_MARKER}");
            let entry = unity_entry(&body);
            assert!(try_parse(&entry, Some(test_timestamp())).is_none());
        }

        #[test]
        fn test_bare_marker_with_trailing_whitespace_returns_none() {
            let body = format!("[UnityCrossThreadLogger]{PROCESS_READ_EXCEPTION_MARKER}   ");
            let entry = unity_entry(&body);
            assert!(try_parse(&entry, Some(test_timestamp())).is_none());
        }

        #[test]
        fn test_numeric_native_error_code_stays_numeric() {
            let body = unity_body(
                PROCESS_READ_EXCEPTION_MARKER,
                r#"{"exception":{"InnerException":{"NativeErrorCode":10054}}}"#,
            );
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = assert_connection_error(event, ERROR_TYPE_PROCESS_READ);
            assert!(
                payload["exception"]["InnerException"]["NativeErrorCode"].is_number(),
                "NativeErrorCode must remain numeric"
            );
        }
    }

    // -- Client.TcpConnection.ProcessFailure -------------------------------

    mod process_failure {
        use super::*;

        #[test]
        fn test_socket_error_firewall_block() {
            let body = unity_body(
                PROCESS_FAILURE_MARKER,
                r#"{"SocketError":"AccessDenied","function":"ConnectAsync"}"#,
            );
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = assert_connection_error(event, ERROR_TYPE_PROCESS_FAILURE);
            assert_eq!(payload["SocketError"], "AccessDenied");
            assert_eq!(payload["function"], "ConnectAsync");
        }

        #[test]
        fn test_bare_marker_returns_none() {
            let body = format!("[UnityCrossThreadLogger]{PROCESS_FAILURE_MARKER}");
            let entry = unity_entry(&body);
            assert!(try_parse(&entry, Some(test_timestamp())).is_none());
        }
    }

    // -- GREConnection.MatchDoorConnectionError ----------------------------

    mod match_door_error {
        use super::*;

        #[test]
        fn test_close_type_and_tcp_conn() {
            let body = unity_body(
                MATCH_DOOR_ERROR_MARKER,
                r#"{
                    "closeType":1,
                    "reason":"Connection lost",
                    "tcpConn":{
                        "host":"mtgarena-match.example.com",
                        "port":443,
                        "inactivityTimeoutMs":30000
                    }
                }"#,
            );
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = assert_connection_error(event, ERROR_TYPE_MATCH_DOOR);
            assert_eq!(payload["closeType"], 1);
            assert_eq!(payload["reason"], "Connection lost");
            assert_eq!(payload["tcpConn"]["host"], "mtgarena-match.example.com");
            assert_eq!(payload["tcpConn"]["port"], 443);
        }

        #[test]
        fn test_bare_marker_returns_none() {
            let body = format!("[UnityCrossThreadLogger]{MATCH_DOOR_ERROR_MARKER}");
            let entry = unity_entry(&body);
            assert!(try_parse(&entry, Some(test_timestamp())).is_none());
        }
    }

    // -- TcpConnection.Close.Exception (macOS-only in corpus) --------------

    mod close_exception {
        use super::*;

        #[test]
        fn test_single_exception_top_level_key() {
            let body = unity_body(
                CLOSE_EXCEPTION_MARKER,
                r#"{
                    "exception":{
                        "NativeErrorCode":10049,
                        "ClassName":"System.Net.Sockets.SocketException",
                        "Message":"The requested address is not valid in this context",
                        "InnerException":null
                    }
                }"#,
            );
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = assert_connection_error(event, ERROR_TYPE_CLOSE_EXCEPTION);
            // Single top-level `exception` key — the SocketException is
            // the direct value, NOT wrapped in an outer IOException.
            assert!(payload["exception"].is_object());
            assert_eq!(payload["exception"]["NativeErrorCode"], 10049);
            assert_eq!(
                payload["exception"]["ClassName"],
                "System.Net.Sockets.SocketException"
            );
            assert!(payload["exception"]["InnerException"].is_null());
        }

        #[test]
        fn test_bare_marker_returns_none() {
            let body = format!("[UnityCrossThreadLogger]{CLOSE_EXCEPTION_MARKER}");
            let entry = unity_entry(&body);
            assert!(try_parse(&entry, Some(test_timestamp())).is_none());
        }
    }

    // -- Non-matching bodies -----------------------------------------------

    mod non_matching {
        use super::*;

        #[test]
        fn test_plain_gre_message_returns_none() {
            let body =
                "[UnityCrossThreadLogger]2/25/2026 12:00:00 PM greToClientEvent\n{\"data\":1}";
            let entry = unity_entry(body);
            assert!(try_parse(&entry, Some(test_timestamp())).is_none());
        }

        #[test]
        fn test_tcp_connection_close_returns_none() {
            // A-2 claims `Client.TcpConnection.Close`; ensure A-3 does NOT
            // also match that body (would be a double-claim regression).
            let body =
                "[UnityCrossThreadLogger]Client.TcpConnection.Close {\"status\":7,\"reason\":\"x\"}";
            let entry = unity_entry(body);
            assert!(try_parse(&entry, Some(test_timestamp())).is_none());
        }

        #[test]
        fn test_websocket_closed_returns_none() {
            let body =
                "[UnityCrossThreadLogger]GREConnection.HandleWebSocketClosed {\"closeType\":1}";
            let entry = unity_entry(body);
            assert!(try_parse(&entry, Some(test_timestamp())).is_none());
        }

        #[test]
        fn test_empty_unity_body_returns_none() {
            let body = "[UnityCrossThreadLogger]";
            let entry = unity_entry(body);
            assert!(try_parse(&entry, Some(test_timestamp())).is_none());
        }

        #[test]
        fn test_malformed_json_returns_none() {
            let body = format!(
                "[UnityCrossThreadLogger]{PROCESS_READ_EXCEPTION_MARKER} {{\"function\":\"ReadAsync\""
            );
            let entry = unity_entry(&body);
            assert!(try_parse(&entry, Some(test_timestamp())).is_none());
        }
    }

    // -- Non-UnityCrossThreadLogger headers --------------------------------

    mod non_unity_headers {
        use super::*;

        #[test]
        fn test_client_gre_header_returns_none() {
            let entry = LogEntry {
                header: EntryHeader::ClientGre,
                body: format!(
                    "[Client GRE]{PROCESS_READ_EXCEPTION_MARKER} {{\"function\":\"ReadAsync\"}}"
                ),
            };
            assert!(try_parse(&entry, Some(test_timestamp())).is_none());
        }

        #[test]
        fn test_metadata_header_returns_none() {
            let entry = LogEntry {
                header: EntryHeader::Metadata,
                body: format!(
                    "{PROCESS_READ_EXCEPTION_MARKER} {{\"exception\":{{\"InnerException\":{{\"NativeErrorCode\":10054}}}}}}"
                ),
            };
            assert!(try_parse(&entry, Some(test_timestamp())).is_none());
        }

        #[test]
        fn test_unrecognized_connection_manager_body_returns_none() {
            // A-4 claims ConnectionManager entries, but only the enumerated
            // Reconnect markers. Unrelated bodies must still return None.
            let entry =
                connection_manager_entry("[ConnectionManager] Some unrelated diagnostic line");
            assert!(try_parse(&entry, Some(test_timestamp())).is_none());
        }

        #[test]
        fn test_unrecognized_matchmaking_body_returns_none() {
            // A-4 claims Matchmaking entries, but only the GRE-connection-lost
            // marker. Unrelated bodies must still return None.
            let entry = matchmaking_entry("Matchmaking: queue entered");
            assert!(try_parse(&entry, Some(test_timestamp())).is_none());
        }
    }

    // -- [ConnectionManager] Reconnect result ------------------------------

    mod reconnect_result {
        use super::*;

        fn parse(body: &str) -> Option<GameEvent> {
            let entry = connection_manager_entry(body);
            try_parse(&entry, Some(test_timestamp()))
        }

        fn assert_result(body: &str, expected: &str) {
            let result = parse(body);
            assert!(result.is_some(), "expected Some for {body:?}, got None");
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = connection_error_payload(event);
            assert_eq!(payload["error_type"], "reconnect_result");
            assert_eq!(payload["result"], expected);
        }

        #[test]
        fn test_reconnect_result_connected() {
            assert_result(
                "[ConnectionManager] Reconnect result : Connected",
                "Connected",
            );
        }

        #[test]
        fn test_reconnect_result_error() {
            assert_result("[ConnectionManager] Reconnect result : Error", "Error");
        }

        #[test]
        fn test_reconnect_result_none() {
            assert_result("[ConnectionManager] Reconnect result : None", "None");
        }

        #[test]
        fn test_reconnect_result_invalid_value_returns_none() {
            // Only the enumerated Connected/Error/None values are accepted.
            assert!(parse("[ConnectionManager] Reconnect result : Unknown").is_none());
        }

        #[test]
        fn test_reconnect_result_empty_value_returns_none() {
            assert!(parse("[ConnectionManager] Reconnect result : ").is_none());
        }
    }

    // -- [ConnectionManager] Reconnect outcome -----------------------------

    mod reconnect_outcome {
        use super::*;

        fn parse(body: &str) -> Option<GameEvent> {
            let entry = connection_manager_entry(body);
            try_parse(&entry, Some(test_timestamp()))
        }

        #[test]
        fn test_reconnect_succeeded_after_1_attempts() {
            let result = parse("[ConnectionManager] Reconnect succeeded after 1 attempts");
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = connection_error_payload(event);
            assert_eq!(payload["error_type"], "reconnect_outcome");
            assert_eq!(payload["outcome"], "succeeded");
            assert_eq!(payload["attempts"], 1);
        }

        #[test]
        fn test_reconnect_succeeded_with_trailing_descriptor() {
            // `Reconnect succeeded after 1 attempts (0.8s)` — attempts is
            // parsed from the first whitespace-delimited token.
            let result = parse("[ConnectionManager] Reconnect succeeded after 3 attempts (1.5s)");
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = connection_error_payload(event);
            assert_eq!(payload["outcome"], "succeeded");
            assert_eq!(payload["attempts"], 3);
        }

        #[test]
        fn test_reconnect_failed() {
            let result = parse("[ConnectionManager] Reconnect failed");
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = connection_error_payload(event);
            assert_eq!(payload["error_type"], "reconnect_outcome");
            assert_eq!(payload["outcome"], "failed");
            assert!(payload["attempts"].is_null());
        }

        #[test]
        fn test_reconnect_timed_out() {
            let result = parse("[ConnectionManager] Reconnect timed out");
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = connection_error_payload(event);
            assert_eq!(payload["error_type"], "reconnect_outcome");
            assert_eq!(payload["outcome"], "timed_out");
            assert!(payload["attempts"].is_null());
        }

        #[test]
        fn test_reconnect_succeeded_unparseable_attempts_is_null() {
            // Unparseable attempts should fall back to null, NOT return
            // None — the outcome itself is still useful downstream.
            let result = parse("[ConnectionManager] Reconnect succeeded after banana attempts");
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = connection_error_payload(event);
            assert_eq!(payload["error_type"], "reconnect_outcome");
            assert_eq!(payload["outcome"], "succeeded");
            assert!(
                payload["attempts"].is_null(),
                "unparseable attempts must be null, got {:?}",
                payload["attempts"]
            );
        }
    }

    // -- Matchmaking: GRE connection lost ----------------------------------

    mod gre_connection_lost {
        use super::*;

        #[test]
        fn test_gre_connection_lost_bare() {
            let entry = matchmaking_entry("Matchmaking: GRE connection lost");
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = connection_error_payload(event);
            assert_eq!(payload["error_type"], "gre_connection_lost");
            // Plain-text flattened strategy: no `payload` wrapper key.
            assert!(payload.get("payload").is_none());
        }

        #[test]
        fn test_gre_connection_lost_with_trailing_descriptor() {
            // Trailing descriptors are permitted — the marker is matched
            // via starts_with.
            let entry = matchmaking_entry("Matchmaking: GRE connection lost, attempting reconnect");
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = connection_error_payload(event);
            assert_eq!(payload["error_type"], "gre_connection_lost");
        }

        #[test]
        fn test_non_matching_matchmaking_suffix_returns_none() {
            // `Matchmaking: GRE connected` (wrong suffix, not "lost") must
            // not match the gre_connection_lost marker.
            let entry = matchmaking_entry("Matchmaking: GRE connected");
            assert!(try_parse(&entry, Some(test_timestamp())).is_none());
        }
    }

    // -- Plain-text dispatch edge cases ------------------------------------

    mod plain_text_dispatch {
        use super::*;

        #[test]
        fn test_connection_manager_without_prefix_returns_none() {
            // The body is required to begin with `[ConnectionManager] `.
            let entry = LogEntry {
                header: EntryHeader::ConnectionManager,
                body: "Reconnect result : Connected".to_owned(),
            };
            assert!(try_parse(&entry, Some(test_timestamp())).is_none());
        }

        #[test]
        fn test_matchmaking_empty_body_returns_none() {
            let entry = matchmaking_entry("");
            assert!(try_parse(&entry, Some(test_timestamp())).is_none());
        }

        #[test]
        fn test_client_gre_header_with_reconnect_body_returns_none() {
            // Confirms dispatch is header-gated: a ConnectionManager-shaped
            // body under the wrong header (ClientGre) must return None.
            let entry = LogEntry {
                header: EntryHeader::ClientGre,
                body: "[ConnectionManager] Reconnect result : Connected".to_owned(),
            };
            assert!(try_parse(&entry, Some(test_timestamp())).is_none());
        }
    }

    // -- Metadata preservation ---------------------------------------------

    mod metadata {
        use super::*;

        #[test]
        fn test_preserves_raw_bytes() {
            let body = unity_body(PROCESS_READ_EXCEPTION_MARKER, r#"{"function":"ReadAsync"}"#);
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            assert_eq!(event.metadata().raw_bytes(), body.as_bytes());
        }

        #[test]
        fn test_preserves_timestamp() {
            let body = unity_body(PROCESS_READ_EXCEPTION_MARKER, r#"{"function":"ReadAsync"}"#);
            let entry = unity_entry(&body);
            let ts = Some(test_timestamp());
            let result = try_parse(&entry, ts);

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            assert_eq!(event.metadata().timestamp(), ts);
        }

        #[test]
        fn test_passes_through_none_timestamp() {
            let body = unity_body(PROCESS_READ_EXCEPTION_MARKER, r#"{"function":"ReadAsync"}"#);
            let entry = unity_entry(&body);
            let result = try_parse(&entry, None);

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            assert!(event.metadata().timestamp().is_none());
        }
    }
}
