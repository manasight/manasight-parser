//! Connection-error parsers: JSON-bearing error-path markers.
//!
//! Parses four error-path entry types that together form the Layer 1 red
//! triggers for the desktop connection health monitor. All four markers
//! currently live under `[UnityCrossThreadLogger]` and all four share a
//! single payload strategy: the full parsed JSON is passed through
//! unchanged under a `payload` key, alongside a stable `error_type`
//! discriminant.
//!
//! # Markers handled
//!
//! | Marker | `error_type` |
//! |--------|--------------|
//! | `TcpConnection.ProcessRead.Exception` | `tcp_process_read_exception` |
//! | `Client.TcpConnection.ProcessFailure` | `tcp_process_failure_socket_error` |
//! | `GREConnection.MatchDoorConnectionError` | `gre_match_door_connection_error` |
//! | `TcpConnection.Close.Exception` | `tcp_close_exception` |
//!
//! # Bare-marker entries
//!
//! All four markers are observed in the disconnect corpus as paired lines —
//! a bare marker (no JSON) followed by a JSON-carrying line. Bare-marker
//! entries return `None`; the paired JSON line on a subsequent entry emits
//! the event.
//!
//! # Payload shape
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
//! # Header dispatch and future extension
//!
//! [`try_parse`] dispatches on `entry.header`. For A-3 only
//! `EntryHeader::UnityCrossThreadLogger` is handled; all other headers
//! return `None`. A-4 will extend this parser to handle three plain-text
//! error markers under `EntryHeader::ConnectionManager` and
//! `EntryHeader::Matchmaking` (`Reconnect result`, `Reconnect succeeded` /
//! `Reconnect failed`, and `Matchmaking: GRE connection lost`). Those
//! variants use a different, flattened payload strategy — they do not wrap
//! data in a `payload` key — so each marker group keeps its own helper
//! function.
//!
//! Satisfies feature spec `connection-health-indicator.md` **AC-DET-5**
//! (JSON-marker variants).

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
/// - Any other header — return `None`.
///
/// A-4 will extend this dispatch to handle `EntryHeader::ConnectionManager`
/// and `EntryHeader::Matchmaking` with plain-text markers.
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
        fn test_connection_manager_header_returns_none() {
            // A-4 will later claim ConnectionManager headers. For A-3, this
            // parser must ignore them so A-4 can extend dispatch without
            // breaking existing behavior.
            let entry = connection_manager_entry(
                "[ConnectionManager] Reconnect succeeded after 2 attempts (1.5s)",
            );
            assert!(try_parse(&entry, Some(test_timestamp())).is_none());
        }

        #[test]
        fn test_matchmaking_header_returns_none() {
            // A-4 will later claim Matchmaking headers. For A-3, this parser
            // must ignore them.
            let entry = matchmaking_entry("Matchmaking: GRE connection lost, attempting reconnect");
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
