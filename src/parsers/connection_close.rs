//! Connection-closure parsers: TCP and WebSocket close events.
//!
//! Parses two paired `[UnityCrossThreadLogger]` entry types that together
//! describe MTG Arena match-connection teardown:
//!
//! | Marker | Event |
//! |--------|-------|
//! | `Client.TcpConnection.Close` | [`GameEvent::TcpConnectionClose`] |
//! | `GREConnection.HandleWebSocketClosed` | [`GameEvent::WebSocketClosed`] |
//!
//! Both payloads are passed through unchanged as [`serde_json::Value`];
//! the parser does not interpret `status` or `closeType` semantics. The
//! desktop connection-health monitor matches on event name +
//! `closeType`/`status` per ADR-011.
//!
//! # Bare-marker TCP close entries
//!
//! `Client.TcpConnection.Close` is emitted as two consecutive lines on
//! both Windows and macOS — a bare marker (no JSON), then the
//! JSON-carrying line. Bare-marker entries return `None`; the paired
//! JSON line emits the event.
//!
//! `GREConnection.HandleWebSocketClosed` is always emitted with a JSON
//! payload (no bare-marker variant observed in the corpus).
//!
//! Satisfies feature spec `connection-health-indicator.md` **AC-DET-2**
//! and **AC-DET-3**.

use crate::events::{EventMetadata, GameEvent, TcpConnectionCloseEvent, WebSocketClosedEvent};
use crate::log::entry::{EntryHeader, LogEntry};
use crate::parsers::api_common;

/// Marker text that identifies a `Client.TcpConnection.Close` entry.
const TCP_CONNECTION_CLOSE_MARKER: &str = "Client.TcpConnection.Close";

/// Marker text that identifies a `GREConnection.HandleWebSocketClosed`
/// entry.
const WEBSOCKET_CLOSED_MARKER: &str = "GREConnection.HandleWebSocketClosed";

/// Attempts to parse a [`LogEntry`] as a TCP or WebSocket connection
/// close event.
///
/// Forks on marker presence in the entry body:
///
/// - `Client.TcpConnection.Close` → [`GameEvent::TcpConnectionClose`]
///   when a JSON payload is present; `None` for bare-marker entries.
/// - `GREConnection.HandleWebSocketClosed` → [`GameEvent::WebSocketClosed`]
///   (always carries JSON in practice).
///
/// Returns `None` for any other body and for non-`UnityCrossThreadLogger`
/// headers.
///
/// The `timestamp` is `None` when the log entry header did not contain a
/// parseable timestamp. It is passed through to [`EventMetadata`] so
/// downstream consumers can distinguish real vs missing timestamps.
pub fn try_parse(
    entry: &LogEntry,
    timestamp: Option<chrono::DateTime<chrono::Utc>>,
) -> Option<GameEvent> {
    if entry.header != EntryHeader::UnityCrossThreadLogger {
        return None;
    }

    if entry.body.contains(TCP_CONNECTION_CLOSE_MARKER) {
        return try_parse_tcp_close(entry, timestamp);
    }

    if entry.body.contains(WEBSOCKET_CLOSED_MARKER) {
        return try_parse_websocket_closed(entry, timestamp);
    }

    None
}

/// Parses a `Client.TcpConnection.Close` entry carrying a JSON payload.
///
/// Returns `None` for bare-marker entries (no JSON on the line) — these
/// are emitted as a preceding duplicate of the JSON-carrying line on
/// both Windows and macOS.
fn try_parse_tcp_close(
    entry: &LogEntry,
    timestamp: Option<chrono::DateTime<chrono::Utc>>,
) -> Option<GameEvent> {
    let json_str = api_common::extract_json_from_body(&entry.body)?;
    let payload: serde_json::Value = match serde_json::from_str(json_str) {
        Ok(v) => v,
        Err(e) => {
            ::log::warn!("Client.TcpConnection.Close: malformed JSON payload: {e}");
            return None;
        }
    };

    let metadata = EventMetadata::new(timestamp, entry.body.as_bytes().to_vec());
    Some(GameEvent::TcpConnectionClose(TcpConnectionCloseEvent::new(
        metadata, payload,
    )))
}

/// Parses a `GREConnection.HandleWebSocketClosed` entry.
///
/// The payload always includes `closeType`, `reason`, and a nested
/// `tcpConn` object. The parser preserves the full parsed JSON.
fn try_parse_websocket_closed(
    entry: &LogEntry,
    timestamp: Option<chrono::DateTime<chrono::Utc>>,
) -> Option<GameEvent> {
    let json_str = api_common::extract_json_from_body(&entry.body)?;
    let payload: serde_json::Value = match serde_json::from_str(json_str) {
        Ok(v) => v,
        Err(e) => {
            ::log::warn!("GREConnection.HandleWebSocketClosed: malformed JSON payload: {e}");
            return None;
        }
    };

    let metadata = EventMetadata::new(timestamp, entry.body.as_bytes().to_vec());
    Some(GameEvent::WebSocketClosed(WebSocketClosedEvent::new(
        metadata, payload,
    )))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parsers::test_helpers::{
        tcp_connection_close_payload, test_timestamp, unity_entry, websocket_closed_payload,
    };

    /// Build a `[UnityCrossThreadLogger]Client.TcpConnection.Close {...}`
    /// entry body from a raw JSON string.
    fn tcp_close_body(json: &str) -> String {
        format!("[UnityCrossThreadLogger]Client.TcpConnection.Close {json}")
    }

    /// Build a `[UnityCrossThreadLogger]GREConnection.HandleWebSocketClosed {...}`
    /// entry body from a raw JSON string.
    fn websocket_closed_body(json: &str) -> String {
        format!("[UnityCrossThreadLogger]GREConnection.HandleWebSocketClosed {json}")
    }

    // -- Client.TcpConnection.Close: normal closes ---------------------------

    mod tcp_close_normal {
        use super::*;

        #[test]
        fn test_status_7_closed_by_remote_end() {
            let body =
                tcp_close_body(r#"{"status":7,"reason":"Closed by remote end","connectionId":42}"#);
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            assert!(
                matches!(event, GameEvent::TcpConnectionClose(_)),
                "expected TcpConnectionClose, got {event:?}"
            );
            let payload = tcp_connection_close_payload(event);
            assert_eq!(payload["status"], 7);
            assert_eq!(payload["reason"], "Closed by remote end");
            assert_eq!(payload["connectionId"], 42);
        }

        #[test]
        fn test_status_2_cleanup_before_reconnecting() {
            let body = tcp_close_body(r#"{"status":2,"reason":"Cleanup before reconnecting"}"#);
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = tcp_connection_close_payload(event);
            assert_eq!(payload["status"], 2);
            assert_eq!(payload["reason"], "Cleanup before reconnecting");
        }

        #[test]
        fn test_status_2_match_manager_reset() {
            let body = tcp_close_body(r#"{"status":2,"reason":"MatchManager.Reset"}"#);
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = tcp_connection_close_payload(event);
            assert_eq!(payload["status"], 2);
            assert_eq!(payload["reason"], "MatchManager.Reset");
        }

        #[test]
        fn test_status_2_on_destroy() {
            let body = tcp_close_body(r#"{"status":2,"reason":"OnDestroy"}"#);
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = tcp_connection_close_payload(event);
            assert_eq!(payload["status"], 2);
            assert_eq!(payload["reason"], "OnDestroy");
        }

        #[test]
        fn test_status_2_match_manager_dispose() {
            let body = tcp_close_body(r#"{"status":2,"reason":"MatchManager.Dispose"}"#);
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = tcp_connection_close_payload(event);
            assert_eq!(payload["status"], 2);
            assert_eq!(payload["reason"], "MatchManager.Dispose");
        }

        #[test]
        fn test_status_5_inactivity_timeout() {
            let body = tcp_close_body(r#"{"status":5,"reason":"Inactivity timeout"}"#);
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = tcp_connection_close_payload(event);
            assert_eq!(payload["status"], 5);
            assert_eq!(payload["reason"], "Inactivity timeout");
        }
    }

    // -- Client.TcpConnection.Close: abnormal closes -------------------------

    mod tcp_close_abnormal {
        use super::*;

        #[test]
        fn test_status_1_with_inner_exception_native_error_code_10054_windows() {
            let body = tcp_close_body(
                r#"{
                    "status":1,
                    "reason":"",
                    "function":"ReadAsync",
                    "description":"An established connection was aborted by the software in your host machine",
                    "exception":{
                        "Message":"Unable to read data from the transport connection",
                        "InnerException":{
                            "Message":"An established connection was aborted",
                            "NativeErrorCode":10054,
                            "SocketErrorCode":"ConnectionAborted"
                        }
                    }
                }"#,
            );
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = tcp_connection_close_payload(event);
            assert_eq!(payload["status"], 1);
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
        fn test_status_1_with_inner_exception_native_error_code_10060_macos() {
            let body = tcp_close_body(
                r#"{
                    "status":1,
                    "reason":"",
                    "function":"ReadAsync",
                    "description":"Connection timed out",
                    "exception":{
                        "Message":"Unable to read data",
                        "InnerException":{
                            "Message":"Operation timed out",
                            "NativeErrorCode":10060,
                            "SocketErrorCode":"TimedOut"
                        }
                    }
                }"#,
            );
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = tcp_connection_close_payload(event);
            assert_eq!(payload["status"], 1);
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
        fn test_status_9_connection_timed_out() {
            let body = tcp_close_body(
                r#"{
                    "status":9,
                    "reason":"Connection timed out",
                    "function":"WriteAsync"
                }"#,
            );
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = tcp_connection_close_payload(event);
            assert_eq!(payload["status"], 9);
            assert_eq!(payload["reason"], "Connection timed out");
            assert_eq!(payload["function"], "WriteAsync");
        }

        #[test]
        fn test_status_9_firewall_permissions_with_embedded_null_bytes() {
            // Real corpus: the reason string for this firewall/permissions
            // error contains embedded `\u0000` characters. JSON string
            // escape `\u0000` decodes to a NUL byte in the resulting Rust
            // String — verify it round-trips through serde_json without
            // truncation or replacement.
            let body = tcp_close_body(
                r#"{
                    "status":9,
                    "reason":"An attempt was made to access a socket in a way forbidden by its access permissions\u0000.\u0000",
                    "function":"ConnectAsync"
                }"#,
            );
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = tcp_connection_close_payload(event);
            assert_eq!(payload["status"], 9);

            let reason = payload["reason"].as_str().unwrap_or("");
            assert!(
                reason.starts_with("An attempt was made to access a socket"),
                "reason prefix preserved, got: {reason:?}"
            );
            // Embedded NUL bytes must survive the round-trip.
            assert!(
                reason.contains('\u{0000}'),
                "reason must preserve embedded NUL bytes, got: {reason:?}"
            );
            assert_eq!(
                reason.matches('\u{0000}').count(),
                2,
                "reason must preserve both embedded NUL bytes, got: {reason:?}"
            );
        }
    }

    // -- Client.TcpConnection.Close: bare marker -----------------------------

    mod tcp_close_bare_marker {
        use super::*;

        #[test]
        fn test_bare_marker_no_json_returns_none() {
            // Observed in corpus on both Windows and macOS: a preceding
            // bare-marker line appears before every JSON-carrying entry.
            let body = "[UnityCrossThreadLogger]Client.TcpConnection.Close";
            let entry = unity_entry(body);
            assert!(try_parse(&entry, Some(test_timestamp())).is_none());
        }

        #[test]
        fn test_bare_marker_with_trailing_whitespace_returns_none() {
            let body = "[UnityCrossThreadLogger]Client.TcpConnection.Close   ";
            let entry = unity_entry(body);
            assert!(try_parse(&entry, Some(test_timestamp())).is_none());
        }

        #[test]
        fn test_bare_marker_followed_by_newline_returns_none() {
            // Just the marker line, no JSON payload anywhere in the body.
            let body = "[UnityCrossThreadLogger]Client.TcpConnection.Close\n";
            let entry = unity_entry(body);
            assert!(try_parse(&entry, Some(test_timestamp())).is_none());
        }
    }

    // -- Client.TcpConnection.Close: numeric types round-trip ----------------

    mod tcp_close_numeric_types {
        use super::*;

        #[test]
        fn test_status_and_native_error_code_stay_numeric() {
            let body = tcp_close_body(
                r#"{"status":1,"exception":{"InnerException":{"NativeErrorCode":10054}}}"#,
            );
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = tcp_connection_close_payload(event);

            // Numeric values must not be coerced to strings.
            assert!(
                payload["status"].is_number(),
                "status must remain numeric, got {:?}",
                payload["status"]
            );
            assert!(
                payload["exception"]["InnerException"]["NativeErrorCode"].is_number(),
                "NativeErrorCode must remain numeric, got {:?}",
                payload["exception"]["InnerException"]["NativeErrorCode"]
            );
        }
    }

    // -- GREConnection.HandleWebSocketClosed ---------------------------------

    mod websocket_closed {
        use super::*;

        /// Build a WebSocket closed JSON payload with the given
        /// `closeType`, `reason`, and a realistic nested `tcpConn` object.
        fn websocket_closed_payload_json(close_type: u32, reason: &str) -> String {
            format!(
                r#"{{
                    "closeType":{close_type},
                    "reason":"{reason}",
                    "tcpConn":{{
                        "host":"mtgarena-prod.example.com",
                        "port":443,
                        "rtTicksRollingAvg":123.45,
                        "rtTicksSamples":[100,110,125,140,130],
                        "lastLocalActivity":637123456789,
                        "lastRemoteActivity":637123456999,
                        "lastRemotePing":637123456800,
                        "inactivityTimeoutMs":30000
                    }}
                }}"#
            )
        }

        #[test]
        fn test_close_type_1_abnormal() {
            let body = websocket_closed_body(&websocket_closed_payload_json(1, "Abnormal closure"));
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            assert!(
                matches!(event, GameEvent::WebSocketClosed(_)),
                "expected WebSocketClosed, got {event:?}"
            );
            let payload = websocket_closed_payload(event);
            assert_eq!(payload["closeType"], 1);
            assert_eq!(payload["reason"], "Abnormal closure");
        }

        #[test]
        fn test_close_type_7_closed_by_remote() {
            let body = websocket_closed_body(&websocket_closed_payload_json(7, "Closed by remote"));
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = websocket_closed_payload(event);
            assert_eq!(payload["closeType"], 7);
            assert_eq!(payload["reason"], "Closed by remote");
        }

        #[test]
        fn test_close_type_9_timeout() {
            let body =
                websocket_closed_body(&websocket_closed_payload_json(9, "Connection timed out"));
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = websocket_closed_payload(event);
            assert_eq!(payload["closeType"], 9);
            assert_eq!(payload["reason"], "Connection timed out");
        }

        #[test]
        fn test_payload_preserves_nested_tcp_conn_object() {
            let body = websocket_closed_body(&websocket_closed_payload_json(1, "Abnormal"));
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = websocket_closed_payload(event);

            let tcp = &payload["tcpConn"];
            assert!(tcp.is_object(), "tcpConn must be preserved as object");
            assert_eq!(tcp["host"], "mtgarena-prod.example.com");
            assert_eq!(tcp["port"], 443);
            assert_eq!(tcp["inactivityTimeoutMs"], 30000);
        }

        #[test]
        fn test_tcp_conn_numeric_types_round_trip() {
            let body = websocket_closed_body(&websocket_closed_payload_json(7, "Closed"));
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = websocket_closed_payload(event);

            let tcp = &payload["tcpConn"];

            // Float must remain numeric.
            assert!(
                tcp["rtTicksRollingAvg"].is_number(),
                "rtTicksRollingAvg must be numeric, got {:?}",
                tcp["rtTicksRollingAvg"]
            );
            assert_eq!(tcp["rtTicksRollingAvg"].as_f64(), Some(123.45));

            // Array of numbers — each element must remain numeric.
            assert!(
                tcp["rtTicksSamples"].is_array(),
                "rtTicksSamples must be an array, got {:?}",
                tcp["rtTicksSamples"]
            );
            let samples = tcp["rtTicksSamples"].as_array().unwrap_or_else(|| {
                // Safe: is_array() asserted above.
                unreachable!()
            });
            assert_eq!(samples.len(), 5);
            for sample in samples {
                assert!(
                    sample.is_number(),
                    "each rtTicksSamples entry must be numeric, got {sample:?}"
                );
            }
            assert_eq!(samples[0].as_u64(), Some(100));
            assert_eq!(samples[4].as_u64(), Some(130));
        }
    }

    // -- Non-matching entries -----------------------------------------------

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
        fn test_front_door_connection_close_returns_none() {
            // Session parser claims this marker; the connection_close parser
            // must NOT also match. Corpus confirms FrontDoorConnection.Close
            // and Client.TcpConnection.Close never appear on the same line.
            let body = "[UnityCrossThreadLogger]FrontDoorConnection.Close";
            let entry = unity_entry(body);
            assert!(try_parse(&entry, Some(test_timestamp())).is_none());
        }

        #[test]
        fn test_similar_but_different_marker_returns_none() {
            let body = "[UnityCrossThreadLogger]Client.TcpConnection.Open {\"status\":0}";
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
        fn test_non_unity_cross_thread_logger_header_returns_none() {
            // Correct marker text but wrong header — must not parse.
            let entry = LogEntry {
                header: EntryHeader::ClientGre,
                body: "[Client GRE]Client.TcpConnection.Close {\"status\":7,\"reason\":\"Closed by remote end\"}"
                    .to_owned(),
            };
            assert!(try_parse(&entry, Some(test_timestamp())).is_none());
        }

        #[test]
        fn test_metadata_header_returns_none() {
            let entry = LogEntry {
                header: EntryHeader::Metadata,
                body: "Client.TcpConnection.Close {\"status\":7}".to_owned(),
            };
            assert!(try_parse(&entry, Some(test_timestamp())).is_none());
        }

        #[test]
        fn test_connection_manager_header_returns_none() {
            let entry = LogEntry {
                header: EntryHeader::ConnectionManager,
                body: "[ConnectionManager] GREConnection.HandleWebSocketClosed {\"closeType\":1}"
                    .to_owned(),
            };
            assert!(try_parse(&entry, Some(test_timestamp())).is_none());
        }

        #[test]
        fn test_tcp_close_malformed_json_returns_none() {
            let body =
                "[UnityCrossThreadLogger]Client.TcpConnection.Close {\"status\":7,\"reason\":";
            let entry = unity_entry(body);
            assert!(try_parse(&entry, Some(test_timestamp())).is_none());
        }

        #[test]
        fn test_websocket_closed_malformed_json_returns_none() {
            let body =
                "[UnityCrossThreadLogger]GREConnection.HandleWebSocketClosed {\"closeType\":";
            let entry = unity_entry(body);
            assert!(try_parse(&entry, Some(test_timestamp())).is_none());
        }
    }

    // -- Metadata preservation ----------------------------------------------

    mod metadata {
        use super::*;

        #[test]
        fn test_tcp_close_preserves_raw_bytes() {
            let body = tcp_close_body(r#"{"status":7,"reason":"Closed by remote end"}"#);
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            assert_eq!(event.metadata().raw_bytes(), body.as_bytes());
        }

        #[test]
        fn test_websocket_closed_preserves_raw_bytes() {
            let body = websocket_closed_body(
                r#"{"closeType":7,"reason":"Closed","tcpConn":{"host":"h"}}"#,
            );
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            assert_eq!(event.metadata().raw_bytes(), body.as_bytes());
        }

        #[test]
        fn test_tcp_close_preserves_timestamp() {
            let body = tcp_close_body(r#"{"status":7}"#);
            let entry = unity_entry(&body);
            let ts = Some(test_timestamp());
            let result = try_parse(&entry, ts);

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            assert_eq!(event.metadata().timestamp(), ts);
        }

        #[test]
        fn test_tcp_close_passes_through_none_timestamp() {
            let body = tcp_close_body(r#"{"status":7}"#);
            let entry = unity_entry(&body);
            let result = try_parse(&entry, None);

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            assert!(event.metadata().timestamp().is_none());
        }

        #[test]
        fn test_websocket_closed_passes_through_none_timestamp() {
            let body = websocket_closed_body(r#"{"closeType":7,"reason":"Closed","tcpConn":{}}"#);
            let entry = unity_entry(&body);
            let result = try_parse(&entry, None);

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            assert!(event.metadata().timestamp().is_none());
        }
    }
}
