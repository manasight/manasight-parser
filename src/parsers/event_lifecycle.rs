//! Event lifecycle parser: `EventJoin`, `EventClaimPrize`, `EventEnterPairing`.
//!
//! Recognizes API requests (`==>`) for all listed methods. Inbound responses
//! (`<==`) are parsed only for `EventClaimPrize` (wins/losses);
//! `EventJoin` and `EventEnterPairing` responses are ignored so each action
//! emits at most one lifecycle event (the request).
//!
//! | Method | Meaning | `==>` request | `<==` response |
//! |--------|---------|---------------|----------------|
//! | `EventJoin` | Player joins a competitive event | Yes | No |
//! | `EventClaimPrize` | Player claims rewards from a completed event | Yes | Yes |
//! | `EventEnterPairing` | Player enters the pairing queue | Yes | No |
//!
//! # Real log format
//!
//! ### Requests
//! ```text
//! [UnityCrossThreadLogger]==> EventJoin {"id":"abc-123","request":"{\"EventName\":\"PremierDraft_MKM_20260201\"}"}
//! ```
//! Request entries use the `==>` prefix followed by the method name and a
//! JSON payload. The payload often contains a string-escaped `request` field.
//!
//! ### Responses
//! ```text
//! [UnityCrossThreadLogger]4/21/2026 11:14:45 PM
//! <== EventClaimPrize(c5c3c263-bde8-4a03-b5fa-345c26196fb2)
//! {"Course":{"InternalEventName":"PremierDraft_SOS_20260421","CurrentWins":7,"CurrentLosses":1,...}, ...}
//! ```
//! Responses use the `<==` prefix followed by the method name and a UUID
//! in parentheses. The JSON payload follows on a subsequent line and contains
//! the detailed results of the action (e.g., event outcome, rewards).
//!
//! `EventClaimPrize` response payloads use `Course.InternalEventName` (and
//! wins/losses under `Course`).

use crate::events::{EventLifecycleEvent, EventMetadata, GameEvent};
use crate::log::entry::LogEntry;
use crate::parsers::api_common;

const JOIN_METHOD: &str = "EventJoin";
const CLAIM_PRIZE_METHOD: &str = "EventClaimPrize";
const ENTER_PAIRING_METHOD: &str = "EventEnterPairing";

/// API method names recognized for outbound lifecycle requests (`==>`).
const LIFECYCLE_METHODS: &[&str] = &[JOIN_METHOD, CLAIM_PRIZE_METHOD, ENTER_PAIRING_METHOD];

/// Attempts to parse a [`LogEntry`] as an event lifecycle event.
///
/// Returns `Some(GameEvent::EventLifecycle(_))` if the entry is a recognized
/// outbound request (`==>`) for any lifecycle method, or an inbound
/// `EventClaimPrize` response (`<==`).
///
/// The `timestamp` is `None` when the log entry header did not contain a
/// parseable timestamp. It is passed through to [`EventMetadata`] so
/// downstream consumers can distinguish real vs missing timestamps.
pub fn try_parse(
    entry: &LogEntry,
    timestamp: Option<chrono::DateTime<chrono::Utc>>,
) -> Option<GameEvent> {
    let body = &entry.body;
    let payload = try_parse_request(body).or_else(|| try_parse_response(body))?;
    let metadata = EventMetadata::new(timestamp, body.as_bytes().to_vec());
    Some(GameEvent::EventLifecycle(EventLifecycleEvent::new(
        metadata, payload,
    )))
}

/// Attempts to parse an outbound API request (`==>`) as a lifecycle event.
fn try_parse_request(body: &str) -> Option<serde_json::Value> {
    let method = LIFECYCLE_METHODS
        .iter()
        .find(|m| api_common::is_api_request(body, m))?;

    let ctx = format!("{method} request");
    let parsed = api_common::parse_json_from_body(body, &ctx)?;
    let event_name = api_common::extract_event_name(&parsed);

    Some(serde_json::json!({
        "type": "event_lifecycle",
        "action": *method,
        "event_name": event_name,
        "raw_request": parsed,
    }))
}

/// Attempts to parse an inbound `EventClaimPrize` response (`<==`).
///
/// Other lifecycle method responses are mostly success confirmations,
/// so we return `None` and avoid dual-emitting.
fn try_parse_response(body: &str) -> Option<serde_json::Value> {
    if !api_common::is_api_response(body, CLAIM_PRIZE_METHOD) {
        return None;
    }

    let ctx = format!("{CLAIM_PRIZE_METHOD} response");
    let parsed = api_common::parse_json_from_body(body, &ctx)?;

    if let Some(payload) = try_parse_claim_prize_course(&parsed, CLAIM_PRIZE_METHOD) {
        return Some(payload);
    }

    // `try_parse_claim_prize_course` returns `None` when `Course` is missing;
    // still surface the inbound claim so `raw_response` is available.
    let event_name = api_common::extract_event_name(&parsed);

    Some(serde_json::json!({
        "type": "event_lifecycle",
        "action": CLAIM_PRIZE_METHOD,
        "event_name": event_name,
        "raw_response": parsed,
    }))
}

/// Parses the specific JSON structure of an `EventClaimPrize` response.
///
/// Extracts `InternalEventName`, `CurrentWins`, and `CurrentLosses` from the
/// nested `Course` object.
///
/// Additional fields (like `CardPool`, `CourseDeck`, and `InventoryInfo`)
/// are preserved in the `raw_response` field for downstream consumers.
///
/// Returns `None` when the response does not contain `Course`, allowing the
/// caller to fall back to the generic lifecycle response parser.
fn try_parse_claim_prize_course(
    parsed: &serde_json::Value,
    method: &str,
) -> Option<serde_json::Value> {
    let course = parsed.get("Course")?;

    let event_name = course
        .get("InternalEventName")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");

    let mut payload = serde_json::json!({
        "type": "event_lifecycle",
        "action": method,
        "event_name": event_name,
        "raw_response": parsed,
    });

    if let Some(wins) = course
        .get("CurrentWins")
        .and_then(serde_json::Value::as_i64)
    {
        payload["current_wins"] = serde_json::json!(wins);
    }
    if let Some(losses) = course
        .get("CurrentLosses")
        .and_then(serde_json::Value::as_i64)
    {
        payload["current_losses"] = serde_json::json!(losses);
    }

    Some(payload)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::PerformanceClass;
    use crate::parsers::test_helpers::{
        lifecycle_payload, test_timestamp, unity_entry, EntryHeader,
    };

    // -- EventJoin requests ---------------------------------------------------

    mod event_join {
        use super::*;

        #[test]
        fn test_try_parse_event_join_basic() {
            let body = r#"[UnityCrossThreadLogger]==> EventJoin {"id":"abc-123","request":"{\"EventName\":\"PremierDraft_MKM_20260201\"}"}"#;
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = lifecycle_payload(event);

            assert_eq!(payload["type"], "event_lifecycle");
            assert_eq!(payload["action"], JOIN_METHOD);
            assert_eq!(payload["event_name"], "PremierDraft_MKM_20260201");
        }

        #[test]
        fn test_try_parse_event_join_with_timestamp() {
            let body = r#"[UnityCrossThreadLogger]2/25/2026 12:00:00 PM ==> EventJoin {"id":"ts-join","request":"{\"EventName\":\"QuickDraft_DSK_20260115\"}"}"#;
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = lifecycle_payload(event);

            assert_eq!(payload["action"], JOIN_METHOD);
            assert_eq!(payload["event_name"], "QuickDraft_DSK_20260115");
        }

        #[test]
        fn test_try_parse_event_join_direct_event_name() {
            let body = r#"[UnityCrossThreadLogger]==> EventJoin {"EventName":"DirectEvent_Test"}"#;
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

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
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = lifecycle_payload(event);

            assert_eq!(payload["type"], "event_lifecycle");
            assert_eq!(payload["action"], CLAIM_PRIZE_METHOD);
            assert_eq!(payload["event_name"], "PremierDraft_MKM_20260201");
        }

        #[test]
        fn test_try_parse_event_claim_prize_empty_request() {
            let body = r#"[UnityCrossThreadLogger]==> EventClaimPrize {"id":"empty-prize","request":"{}"}"#;
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = lifecycle_payload(event);

            assert_eq!(payload["action"], CLAIM_PRIZE_METHOD);
            assert_eq!(payload["event_name"], "");
        }

        #[test]
        fn test_try_parse_event_claim_prize_response_with_course() {
            let body = "[UnityCrossThreadLogger]3/11/2026 11:14:45 PM\n\
                         <== EventClaimPrize(big-win)\n\
                         {\"Course\":{\"InternalEventName\":\"PremierDraft_TMT_20260303\",\
                         \"CurrentWins\":7,\"CurrentLosses\":1},\"InventoryInfo\":{}}";
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = lifecycle_payload(event);

            assert_eq!(payload["type"], "event_lifecycle");
            assert_eq!(payload["action"], CLAIM_PRIZE_METHOD);
            assert_eq!(payload["event_name"], "PremierDraft_TMT_20260303");
            assert_eq!(payload["current_wins"], 7);
            assert_eq!(payload["current_losses"], 1);
            assert!(payload["raw_response"].is_object());
        }

        #[test]
        fn test_try_parse_event_claim_prize_response_without_course_falls_back_to_generic() {
            let body = "[UnityCrossThreadLogger]3/11/2026 11:14:45 PM\n\
                         <== EventClaimPrize(fff-888)\n\
                         {\"EventName\":\"PremierDraft_Fallback\"}";
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = lifecycle_payload(event);

            assert_eq!(payload["action"], CLAIM_PRIZE_METHOD);
            assert_eq!(payload["event_name"], "PremierDraft_Fallback");
            assert!(payload["raw_response"].is_object());
            assert!(payload.get("current_wins").is_none());
            assert!(payload.get("current_losses").is_none());
        }
    }

    // -- EventEnterPairing requests -------------------------------------------

    mod event_enter_pairing {
        use super::*;

        #[test]
        fn test_try_parse_event_enter_pairing() {
            let body = r#"[UnityCrossThreadLogger]==> EventEnterPairing {"id":"pair-123","request":"{\"EventName\":\"Ladder\"}"}"#;
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = lifecycle_payload(event);

            assert_eq!(payload["type"], "event_lifecycle");
            assert_eq!(payload["action"], ENTER_PAIRING_METHOD);
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
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            assert_eq!(event.metadata().raw_bytes(), body.as_bytes());
        }

        #[test]
        fn test_try_parse_stores_timestamp() {
            let body = r#"[UnityCrossThreadLogger]==> EventJoin {"id":"ts-test","request":"{\"EventName\":\"Test\"}"}"#;
            let entry = unity_entry(body);
            let ts = Some(test_timestamp());
            let result = try_parse(&entry, ts);

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            assert_eq!(event.metadata().timestamp(), ts);
        }

        #[test]
        fn test_try_parse_preserves_raw_request() {
            let body = r#"[UnityCrossThreadLogger]==> EventJoin {"id":"raw-req","request":"{\"EventName\":\"Test\"}","extraField":"preserved"}"#;
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = lifecycle_payload(event);

            assert_eq!(payload["raw_request"]["extraField"], "preserved");
        }
    }

    // -- Body-level parsing (request / response helpers) ----------------------

    mod parse_body {
        use super::*;

        #[test]
        fn test_try_parse_request_event_join_extracts_event_name() {
            let body = r#"[UnityCrossThreadLogger]==> EventJoin {"id":"x","request":"{\"EventName\":\"Draft_A\"}"}"#;
            let payload = try_parse_request(body).unwrap_or_else(|| unreachable!());
            assert_eq!(payload["action"], JOIN_METHOD);
            assert_eq!(payload["event_name"], "Draft_A");
            assert!(payload["raw_request"].is_object());
        }

        #[test]
        fn test_try_parse_request_response_only_body_returns_none() {
            let body = "[UnityCrossThreadLogger]\n<== EventJoin(uuid)\n{}";
            assert!(try_parse_request(body).is_none());
        }

        #[test]
        fn test_try_parse_response_event_join_returns_none() {
            let body = "[UnityCrossThreadLogger]3/11/2026 9:41:37 PM\n\
                        <== EventJoin(ghjkl-123456)\n\
                        {\n\"Course\": {\n\"CourseId\": \"qwerty-123456\",\n\
                        \"InternalEventName\": \"PremierDraft_TMT_20260303\"\n}\n}";
            assert!(try_parse_response(body).is_none());
        }

        #[test]
        fn test_try_parse_response_without_lifecycle_marker_returns_none() {
            let body = "[UnityCrossThreadLogger]==> EventJoin {}";
            assert!(try_parse_response(body).is_none());
        }

        #[test]
        fn test_try_parse_response_event_claim_prize_extracts_course() {
            let body = "[UnityCrossThreadLogger]4/21/2026 11:14:45 PM\n\
                        <== EventClaimPrize(c5c3c263-bde8-4a03-b5fa-345c26196fb2)\n\
                        {\"Course\":{\"InternalEventName\":\"PremierDraft_SOS_20260421\",\
                        \"CurrentWins\":7,\"CurrentLosses\":0}}";
            let payload = try_parse_response(body).unwrap_or_else(|| unreachable!());
            assert_eq!(payload["action"], CLAIM_PRIZE_METHOD);
            assert_eq!(payload["event_name"], "PremierDraft_SOS_20260421");
            assert_eq!(payload["current_wins"], 7);
            assert_eq!(payload["current_losses"], 0);
            assert!(payload["raw_response"].is_object());
        }

        #[test]
        fn test_try_parse_claim_prize_response_extracts_course_fields() {
            let parsed = serde_json::json!({
                "Course": {
                    "InternalEventName": "PremierDraft_SOS_20260421",
                    "CurrentWins": 7,
                    "CurrentLosses": 0
                }
            });
            let payload = try_parse_claim_prize_course(&parsed, CLAIM_PRIZE_METHOD)
                .unwrap_or_else(|| unreachable!());
            assert_eq!(payload["action"], CLAIM_PRIZE_METHOD);
            assert_eq!(payload["event_name"], "PremierDraft_SOS_20260421");
            assert_eq!(payload["current_wins"], 7);
            assert_eq!(payload["current_losses"], 0);
        }

        #[test]
        fn test_try_parse_claim_prize_response_without_course_returns_none() {
            let parsed = serde_json::json!({"EventName": "SoloOnly"});
            assert!(try_parse_claim_prize_course(&parsed, CLAIM_PRIZE_METHOD).is_none());
        }

        #[test]
        fn test_try_parse_claim_prize_response_course_without_wins_losses_omits_fields() {
            let parsed = serde_json::json!({
                "Course": {
                    "InternalEventName": "PremierDraft_SOS_20260421"
                }
            });
            let payload = try_parse_claim_prize_course(&parsed, CLAIM_PRIZE_METHOD)
                .unwrap_or_else(|| unreachable!());
            assert_eq!(payload["event_name"], "PremierDraft_SOS_20260421");
            assert!(payload.get("current_wins").is_none());
            assert!(payload.get("current_losses").is_none());
        }
    }

    // -- Non-matching entries (should return None) ----------------------------

    mod non_matching {
        use super::*;

        #[test]
        fn test_try_parse_event_join_response_returns_none() {
            let body = "[UnityCrossThreadLogger]2/25/2026 12:00:00 PM\n\
                         <== EventJoin(uuid)\n\
                         {\"result\": \"success\"}";
            let entry = unity_entry(body);
            assert!(try_parse(&entry, Some(test_timestamp())).is_none());
        }

        #[test]
        fn test_try_parse_unrecognized_method_returns_none() {
            let body = r#"[UnityCrossThreadLogger]==> EventGetCourses {"id":"courses-123"}"#;
            let entry = unity_entry(body);
            assert!(try_parse(&entry, Some(test_timestamp())).is_none());
        }

        #[test]
        fn test_try_parse_unrelated_entry_returns_none() {
            let body = "[UnityCrossThreadLogger]greToClientEvent\n{\"data\": 1}";
            let entry = unity_entry(body);
            assert!(try_parse(&entry, Some(test_timestamp())).is_none());
        }

        #[test]
        fn test_try_parse_old_underscore_marker_returns_none() {
            let body = "[UnityCrossThreadLogger]Event_Join\n{\"EventName\": \"Test\"}";
            let entry = unity_entry(body);
            assert!(try_parse(&entry, Some(test_timestamp())).is_none());
        }

        #[test]
        fn test_try_parse_old_claim_prize_marker_returns_none() {
            let body = "[UnityCrossThreadLogger]Event_ClaimPrize\n{\"EventName\": \"Test\"}";
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
        fn test_try_parse_malformed_json_returns_none() {
            let body = "[UnityCrossThreadLogger]==> EventJoin {broken json!!!}";
            let entry = unity_entry(body);
            assert!(try_parse(&entry, Some(test_timestamp())).is_none());
        }

        #[test]
        fn test_try_parse_client_gre_header_returns_none() {
            let entry = LogEntry {
                header: EntryHeader::ClientGre,
                body: "[Client GRE]some GRE message".to_owned(),
            };
            assert!(try_parse(&entry, Some(test_timestamp())).is_none());
        }
    }

    // -- Performance class ----------------------------------------------------

    mod performance_class {
        use super::*;

        #[test]
        fn test_event_lifecycle_is_durable_per_event() {
            let body = r#"[UnityCrossThreadLogger]==> EventJoin {"id":"perf-test","request":"{\"EventName\":\"Test\"}"}"#;
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            assert_eq!(event.performance_class(), PerformanceClass::DurablePerEvent);
        }
    }
}
