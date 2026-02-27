//! Event lifecycle parser: `EventJoin`, `EventClaimPrize`, `EventEnterPairing`.
//!
//! Recognizes `==>` API request entries for event lifecycle actions:
//!
//! | Method | Meaning |
//! |--------|---------|
//! | `EventJoin` | Player joins a competitive event |
//! | `EventClaimPrize` | Player claims rewards from a completed event |
//! | `EventEnterPairing` | Player enters the pairing queue |
//!
//! # Real log format
//!
//! ```text
//! [UnityCrossThreadLogger]==> EventJoin {"id":"abc-123","request":"{\"EventName\":\"PremierDraft_MKM_20260201\"}"}
//! ```
//!
//! Request entries use the `==>` prefix followed by the method name and a
//! JSON payload, all on the same line or split across continuation lines.

use crate::events::{EventLifecycleEvent, EventMetadata, GameEvent};
use crate::log::entry::LogEntry;
use crate::parsers::api_common;

/// API method names recognized as event lifecycle actions.
const LIFECYCLE_METHODS: &[&str] = &["EventJoin", "EventClaimPrize", "EventEnterPairing"];

/// Attempts to parse a [`LogEntry`] as an event lifecycle event.
///
/// Returns `Some(GameEvent::EventLifecycle(_))` if the entry is an `==>`
/// request for one of the recognized lifecycle methods, or `None` otherwise.
///
/// The `timestamp` is used to construct [`EventMetadata`] for the resulting
/// event. Callers are responsible for parsing the timestamp from the log
/// entry header before invoking this function.
pub fn try_parse(entry: &LogEntry, timestamp: chrono::DateTime<chrono::Utc>) -> Option<GameEvent> {
    let body = &entry.body;

    let method = LIFECYCLE_METHODS
        .iter()
        .find(|m| api_common::is_api_request(body, m))?;

    let json_str = api_common::extract_json_from_body(body)?;

    let parsed: serde_json::Value = match serde_json::from_str(json_str) {
        Ok(v) => v,
        Err(e) => {
            ::log::warn!("{method}: malformed JSON payload: {e}");
            return None;
        }
    };

    let event_name = event_name_from_request(&parsed);

    let payload = serde_json::json!({
        "type": "event_lifecycle",
        "action": *method,
        "event_name": event_name,
        "raw_request": parsed,
    });

    let metadata = EventMetadata::new(timestamp, body.as_bytes().to_vec());
    Some(GameEvent::EventLifecycle(EventLifecycleEvent::new(
        metadata, payload,
    )))
}

/// Extracts `EventName` from a nested `request` field containing string-escaped JSON.
///
/// The request format stores the event name inside a string-encoded JSON object:
/// `{"id": "...", "request": "{\"EventName\": \"PremierDraft_MKM_20260201\"}"}`
fn event_name_from_request(parsed: &serde_json::Value) -> String {
    // Try direct EventName field first.
    if let Some(name) = parsed
        .get("EventName")
        .or_else(|| parsed.get("InternalEventName"))
        .and_then(serde_json::Value::as_str)
    {
        return name.to_owned();
    }

    // Try nested string-escaped request field.
    let Some(request_str) = parsed.get("request").and_then(serde_json::Value::as_str) else {
        return String::new();
    };
    let Ok(request_json) = serde_json::from_str::<serde_json::Value>(request_str) else {
        return String::new();
    };
    request_json
        .get("EventName")
        .or_else(|| request_json.get("InternalEventName"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .to_owned()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::PerformanceClass;
    use crate::log::entry::EntryHeader;
    use chrono::{TimeZone, Utc};

    /// Helper: build a UTC timestamp for tests.
    fn test_timestamp() -> chrono::DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 2, 25, 12, 0, 0)
            .single()
            .unwrap_or_default()
    }

    /// Helper: build a `LogEntry` with `UnityCrossThreadLogger` header.
    fn unity_entry(body: &str) -> LogEntry {
        LogEntry {
            header: EntryHeader::UnityCrossThreadLogger,
            body: body.to_owned(),
        }
    }

    /// Helper: extract the JSON payload from a `GameEvent::EventLifecycle` variant.
    fn lifecycle_payload(event: &GameEvent) -> &serde_json::Value {
        static EMPTY: std::sync::LazyLock<serde_json::Value> =
            std::sync::LazyLock::new(|| serde_json::json!(null));
        match event {
            GameEvent::EventLifecycle(e) => e.payload(),
            _ => &EMPTY,
        }
    }

    // -- EventJoin requests ---------------------------------------------------

    mod event_join {
        use super::*;

        #[test]
        fn test_try_parse_event_join_basic() {
            let body = r#"[UnityCrossThreadLogger]==> EventJoin {"id":"abc-123","request":"{\"EventName\":\"PremierDraft_MKM_20260201\"}"}"#;
            let entry = unity_entry(body);
            let result = try_parse(&entry, test_timestamp());

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = lifecycle_payload(event);

            assert_eq!(payload["type"], "event_lifecycle");
            assert_eq!(payload["action"], "EventJoin");
            assert_eq!(payload["event_name"], "PremierDraft_MKM_20260201");
        }

        #[test]
        fn test_try_parse_event_join_with_timestamp() {
            let body = r#"[UnityCrossThreadLogger]2/25/2026 12:00:00 PM ==> EventJoin {"id":"ts-join","request":"{\"EventName\":\"QuickDraft_DSK_20260115\"}"}"#;
            let entry = unity_entry(body);
            let result = try_parse(&entry, test_timestamp());

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = lifecycle_payload(event);

            assert_eq!(payload["action"], "EventJoin");
            assert_eq!(payload["event_name"], "QuickDraft_DSK_20260115");
        }

        #[test]
        fn test_try_parse_event_join_direct_event_name() {
            let body = r#"[UnityCrossThreadLogger]==> EventJoin {"EventName":"DirectEvent_Test"}"#;
            let entry = unity_entry(body);
            let result = try_parse(&entry, test_timestamp());

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = lifecycle_payload(event);

            assert_eq!(payload["event_name"], "DirectEvent_Test");
        }
    }

    // -- EventClaimPrize requests ---------------------------------------------

    mod event_claim_prize {
        use super::*;

        #[test]
        fn test_try_parse_event_claim_prize() {
            let body = r#"[UnityCrossThreadLogger]==> EventClaimPrize {"id":"prize-123","request":"{\"EventName\":\"PremierDraft_MKM_20260201\"}"}"#;
            let entry = unity_entry(body);
            let result = try_parse(&entry, test_timestamp());

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = lifecycle_payload(event);

            assert_eq!(payload["type"], "event_lifecycle");
            assert_eq!(payload["action"], "EventClaimPrize");
            assert_eq!(payload["event_name"], "PremierDraft_MKM_20260201");
        }

        #[test]
        fn test_try_parse_event_claim_prize_empty_request() {
            let body = r#"[UnityCrossThreadLogger]==> EventClaimPrize {"id":"empty-prize","request":"{}"}"#;
            let entry = unity_entry(body);
            let result = try_parse(&entry, test_timestamp());

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = lifecycle_payload(event);

            assert_eq!(payload["action"], "EventClaimPrize");
            assert_eq!(payload["event_name"], "");
        }
    }

    // -- EventEnterPairing requests -------------------------------------------

    mod event_enter_pairing {
        use super::*;

        #[test]
        fn test_try_parse_event_enter_pairing() {
            let body = r#"[UnityCrossThreadLogger]==> EventEnterPairing {"id":"pair-123","request":"{\"EventName\":\"Ladder\"}"}"#;
            let entry = unity_entry(body);
            let result = try_parse(&entry, test_timestamp());

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = lifecycle_payload(event);

            assert_eq!(payload["type"], "event_lifecycle");
            assert_eq!(payload["action"], "EventEnterPairing");
            assert_eq!(payload["event_name"], "Ladder");
        }
    }

    // -- Metadata preservation ------------------------------------------------

    mod metadata {
        use super::*;

        #[test]
        fn test_try_parse_preserves_raw_bytes() {
            let body = r#"[UnityCrossThreadLogger]==> EventJoin {"id":"raw-test","request":"{\"EventName\":\"Test\"}"}"#;
            let entry = unity_entry(body);
            let result = try_parse(&entry, test_timestamp());

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            assert_eq!(event.metadata().raw_bytes(), body.as_bytes());
        }

        #[test]
        fn test_try_parse_stores_timestamp() {
            let body = r#"[UnityCrossThreadLogger]==> EventJoin {"id":"ts-test","request":"{\"EventName\":\"Test\"}"}"#;
            let entry = unity_entry(body);
            let ts = test_timestamp();
            let result = try_parse(&entry, ts);

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            assert_eq!(event.metadata().timestamp(), ts);
        }

        #[test]
        fn test_try_parse_preserves_raw_request() {
            let body = r#"[UnityCrossThreadLogger]==> EventJoin {"id":"raw-req","request":"{\"EventName\":\"Test\"}","extraField":"preserved"}"#;
            let entry = unity_entry(body);
            let result = try_parse(&entry, test_timestamp());

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = lifecycle_payload(event);

            assert_eq!(payload["raw_request"]["extraField"], "preserved");
        }
    }

    // -- Non-matching entries (should return None) ----------------------------

    mod non_matching {
        use super::*;

        #[test]
        fn test_try_parse_response_arrow_returns_none() {
            let body = "[UnityCrossThreadLogger]2/25/2026 12:00:00 PM\n\
                         <== EventJoin(uuid)\n\
                         {\"result\": \"success\"}";
            let entry = unity_entry(body);
            assert!(try_parse(&entry, test_timestamp()).is_none());
        }

        #[test]
        fn test_try_parse_unrecognized_method_returns_none() {
            let body = r#"[UnityCrossThreadLogger]==> EventGetCourses {"id":"courses-123"}"#;
            let entry = unity_entry(body);
            assert!(try_parse(&entry, test_timestamp()).is_none());
        }

        #[test]
        fn test_try_parse_unrelated_entry_returns_none() {
            let body = "[UnityCrossThreadLogger]greToClientEvent\n{\"data\": 1}";
            let entry = unity_entry(body);
            assert!(try_parse(&entry, test_timestamp()).is_none());
        }

        #[test]
        fn test_try_parse_old_underscore_marker_returns_none() {
            let body = "[UnityCrossThreadLogger]Event_Join\n{\"EventName\": \"Test\"}";
            let entry = unity_entry(body);
            assert!(try_parse(&entry, test_timestamp()).is_none());
        }

        #[test]
        fn test_try_parse_old_claim_prize_marker_returns_none() {
            let body = "[UnityCrossThreadLogger]Event_ClaimPrize\n{\"EventName\": \"Test\"}";
            let entry = unity_entry(body);
            assert!(try_parse(&entry, test_timestamp()).is_none());
        }

        #[test]
        fn test_try_parse_empty_body_returns_none() {
            let body = "[UnityCrossThreadLogger]";
            let entry = unity_entry(body);
            assert!(try_parse(&entry, test_timestamp()).is_none());
        }

        #[test]
        fn test_try_parse_malformed_json_returns_none() {
            let body = "[UnityCrossThreadLogger]==> EventJoin {broken json!!!}";
            let entry = unity_entry(body);
            assert!(try_parse(&entry, test_timestamp()).is_none());
        }

        #[test]
        fn test_try_parse_client_gre_header_returns_none() {
            let entry = LogEntry {
                header: EntryHeader::ClientGre,
                body: "[Client GRE]some GRE message".to_owned(),
            };
            assert!(try_parse(&entry, test_timestamp()).is_none());
        }
    }

    // -- Performance class ----------------------------------------------------

    mod performance_class {
        use super::*;

        #[test]
        fn test_event_lifecycle_is_durable_per_event() {
            let body = r#"[UnityCrossThreadLogger]==> EventJoin {"id":"perf-test","request":"{\"EventName\":\"Test\"}"}"#;
            let entry = unity_entry(body);
            let result = try_parse(&entry, test_timestamp());

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            assert_eq!(event.performance_class(), PerformanceClass::DurablePerEvent);
        }
    }

    // -- Internal helpers -----------------------------------------------------

    mod helpers {
        use super::*;

        #[test]
        fn test_event_name_from_request_nested() {
            let parsed = serde_json::json!({
                "id": "test",
                "request": "{\"EventName\":\"PremierDraft_ECL_20260120\"}"
            });
            assert_eq!(
                event_name_from_request(&parsed),
                "PremierDraft_ECL_20260120",
            );
        }

        #[test]
        fn test_event_name_from_request_direct() {
            let parsed = serde_json::json!({"EventName": "DirectEvent"});
            assert_eq!(event_name_from_request(&parsed), "DirectEvent");
        }

        #[test]
        fn test_event_name_from_request_no_field() {
            let parsed = serde_json::json!({"id": "test"});
            assert_eq!(event_name_from_request(&parsed), "");
        }

        #[test]
        fn test_event_name_from_request_invalid_nested_json() {
            let parsed = serde_json::json!({"request": "not json"});
            assert_eq!(event_name_from_request(&parsed), "");
        }

        #[test]
        fn test_event_name_from_request_internal_event_name() {
            let parsed = serde_json::json!({"InternalEventName": "InternalTest"});
            assert_eq!(event_name_from_request(&parsed), "InternalTest");
        }
    }
}
