//! Draft completion parser for `DraftCompleteDraft` events.
//!
//! When a draft finishes (all picks made), the log emits a
//! `DraftCompleteDraft` entry that links the draft ID to the event
//! and marks the draft as finished. Two entry formats appear:
//!
//! | Direction | Format | Key Fields |
//! |-----------|--------|------------|
//! | Request (`==>`) | `{"id": "...", "request": "{\"EventName\": \"...\"}"}` | `id`, nested `EventName` |
//! | Response (`<==`) | `{"CourseId": "...", "InternalEventName": "...", "CardPool": [...]}` | `InternalEventName`, `CardPool` |
//!
//! This is a Class 2 (Durable Per-Event) event. The completion signal
//! must survive crashes to ensure the draft record is finalized.

use crate::events::{DraftCompleteEvent, EventMetadata, GameEvent};
use crate::log::entry::LogEntry;

/// Marker for draft completion events.
///
/// `DraftCompleteDraft` appears in the log when the player finishes
/// drafting all cards (all 42 picks made). Both request (`==>`) and
/// response (`<==`) entries share this marker.
const COMPLETE_DRAFT_MARKER: &str = "DraftCompleteDraft";

/// Attempts to parse a [`LogEntry`] as a draft completion event.
///
/// Returns `Some(GameEvent::DraftComplete(_))` if the entry matches the
/// `DraftCompleteDraft` signature, or `None` if it does not match.
///
/// The `timestamp` is used to construct [`EventMetadata`] for the resulting
/// event. Callers are responsible for parsing the timestamp from the log
/// entry header before invoking this function.
pub fn try_parse(entry: &LogEntry, timestamp: chrono::DateTime<chrono::Utc>) -> Option<GameEvent> {
    let body = &entry.body;

    if !body.contains(COMPLETE_DRAFT_MARKER) {
        return None;
    }

    let json_str = extract_json_from_body(body)?;

    let parsed: serde_json::Value = match serde_json::from_str(json_str) {
        Ok(v) => v,
        Err(e) => {
            ::log::warn!("DraftCompleteDraft: malformed JSON payload: {e}");
            return None;
        }
    };

    let draft_id_from_body = extract_draft_id_from_body(body);
    let payload = build_payload(&parsed, draft_id_from_body.as_deref());
    let metadata = EventMetadata::new(timestamp, body.as_bytes().to_vec());
    Some(GameEvent::DraftComplete(DraftCompleteEvent::new(
        metadata, payload,
    )))
}

/// Builds a structured payload from the draft completion event.
///
/// Handles three JSON formats:
/// - **Request**: `{"id": "...", "request": "{\"EventName\": \"...\"}"}` — event name
///   is extracted from the string-escaped `request` field.
/// - **Response**: `{"CourseId": "...", "InternalEventName": "..."}` — event name is
///   a direct field, draft ID comes from the body header.
/// - **Flat** (legacy): `{"DraftId": "...", "EventName": "..."}` — both fields at
///   top level.
fn build_payload(
    parsed: &serde_json::Value,
    draft_id_from_body: Option<&str>,
) -> serde_json::Value {
    let draft_id = parsed
        .get("DraftId")
        .or_else(|| parsed.get("draftId"))
        .or_else(|| parsed.get("id"))
        .and_then(serde_json::Value::as_str)
        .or(draft_id_from_body)
        .unwrap_or("");

    let direct_event_name = parsed
        .get("EventName")
        .or_else(|| parsed.get("InternalEventName"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");

    let event_name = if direct_event_name.is_empty() {
        event_name_from_request(parsed)
    } else {
        direct_event_name.to_owned()
    };

    serde_json::json!({
        "type": "draft_complete",
        "draft_id": draft_id,
        "event_name": event_name,
        "raw_complete_draft": parsed.clone(),
    })
}

/// Extracts the draft ID from a `DraftCompleteDraft(uuid)` pattern in the body.
///
/// The response format includes the draft ID in a parenthesized suffix:
/// `<== DraftCompleteDraft(2c141a6f-49e9-4b73-8231-47212fc8d577)`
fn extract_draft_id_from_body(body: &str) -> Option<String> {
    let marker = "DraftCompleteDraft(";
    let start = body.find(marker)? + marker.len();
    let remaining = body.get(start..)?;
    let end = remaining.find(')')?;
    Some(remaining.get(..end)?.to_owned())
}

/// Extracts `EventName` from a nested `request` field containing string-escaped JSON.
///
/// The request format stores the event name inside a string-encoded JSON object:
/// `{"id": "...", "request": "{\"EventName\": \"PremierDraft_ECL_20260120\"}"}`
fn event_name_from_request(parsed: &serde_json::Value) -> String {
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

/// Extracts the first JSON object or array from a multi-line log body.
///
/// The log header line may contain brackets (e.g., `[UnityCrossThreadLogger]`)
/// that must not be confused with JSON array delimiters. This function
/// determines a safe search start offset by skipping any `[...]` header
/// prefix, then finds the first `{` or `[` from that offset.
fn extract_json_from_body(body: &str) -> Option<&str> {
    // If the body starts with a `[...]` header prefix, skip past it
    // so we don't match the header bracket as a JSON array start.
    let search_start = if body.starts_with('[') {
        body.find(']').map_or(0, |pos| pos + 1)
    } else {
        0
    };

    let search_region = &body[search_start..];
    let json_start = search_region.find(['{', '['])?;
    let json_start = search_start + json_start;

    let candidate = &body[json_start..];

    let first_byte = candidate.as_bytes().first().copied()?;
    let (open_char, close_char) = if first_byte == b'{' {
        ('{', '}')
    } else {
        ('[', ']')
    };

    let mut depth: i32 = 0;
    let mut in_string = false;
    let mut escape_next = false;
    let mut end_pos = None;

    for (i, ch) in candidate.char_indices() {
        if escape_next {
            escape_next = false;
            continue;
        }
        match ch {
            '\\' if in_string => {
                escape_next = true;
            }
            '"' => {
                in_string = !in_string;
            }
            c if !in_string && c == open_char => {
                depth += 1;
            }
            c if !in_string && c == close_char => {
                depth -= 1;
                if depth == 0 {
                    end_pos = Some(i + 1);
                    break;
                }
            }
            _ => {}
        }
    }

    end_pos.map(|end| &candidate[..end])
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
    ///
    /// Uses `unwrap_or_default()` because `clippy::expect_used` is denied
    /// crate-wide. The epoch fallback would visibly fail timestamp assertions.
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

    /// Helper: extract the JSON payload from a `GameEvent::DraftComplete` variant.
    ///
    /// Returns a static null value if the variant is not `DraftComplete`,
    /// which will cause assertion failures that clearly indicate the wrong
    /// variant was produced.
    fn draft_complete_payload(event: &GameEvent) -> &serde_json::Value {
        static EMPTY: std::sync::LazyLock<serde_json::Value> =
            std::sync::LazyLock::new(|| serde_json::json!(null));
        match event {
            GameEvent::DraftComplete(e) => e.payload(),
            _ => &EMPTY,
        }
    }

    // -- Request format (==>) ------------------------------------------------

    mod request_format {
        use super::*;

        #[test]
        fn test_try_parse_request_basic() {
            let body = r#"[UnityCrossThreadLogger]==> DraftCompleteDraft {"id":"abc-123-def","request":"{\"EventName\":\"PremierDraft_MKM_20260201\",\"IsBotDraft\":false}"}"#;
            let entry = unity_entry(body);
            let result = try_parse(&entry, test_timestamp());

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = draft_complete_payload(event);

            assert_eq!(payload["type"], "draft_complete");
            assert_eq!(payload["draft_id"], "abc-123-def");
            assert_eq!(payload["event_name"], "PremierDraft_MKM_20260201");
        }

        #[test]
        fn test_try_parse_request_traditional_draft() {
            let body = r#"[UnityCrossThreadLogger]==> DraftCompleteDraft {"id":"trad-456","request":"{\"EventName\":\"TradDraft_DSK_20260115\",\"IsBotDraft\":false}"}"#;
            let entry = unity_entry(body);
            let result = try_parse(&entry, test_timestamp());

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = draft_complete_payload(event);

            assert_eq!(payload["draft_id"], "trad-456");
            assert_eq!(payload["event_name"], "TradDraft_DSK_20260115");
        }

        #[test]
        fn test_try_parse_request_empty_request_string() {
            let body = r#"[UnityCrossThreadLogger]==> DraftCompleteDraft {"id":"empty-req","request":"{}"}"#;
            let entry = unity_entry(body);
            let result = try_parse(&entry, test_timestamp());

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = draft_complete_payload(event);

            assert_eq!(payload["draft_id"], "empty-req");
            assert_eq!(payload["event_name"], "");
        }
    }

    // -- Response format (<==) -----------------------------------------------

    mod response_format {
        use super::*;

        #[test]
        fn test_try_parse_response_basic() {
            let body = "[UnityCrossThreadLogger]2/25/2026 12:00:00 PM\n\
                         <== DraftCompleteDraft(abc-123-def)\n\
                         {\"CourseId\":\"course-456\",\
                          \"InternalEventName\":\"PremierDraft_MKM_20260201\",\
                          \"CurrentModule\":\"DeckSelect\",\
                          \"CardPool\":[98535,98381]}";
            let entry = unity_entry(body);
            let result = try_parse(&entry, test_timestamp());

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = draft_complete_payload(event);

            assert_eq!(payload["type"], "draft_complete");
            assert_eq!(payload["draft_id"], "abc-123-def");
            assert_eq!(payload["event_name"], "PremierDraft_MKM_20260201");
        }

        #[test]
        fn test_try_parse_response_preserves_card_pool() {
            let body = "[UnityCrossThreadLogger]2/25/2026 12:00:00 PM\n\
                         <== DraftCompleteDraft(pool-test)\n\
                         {\"InternalEventName\":\"PremierDraft_ECL_20260120\",\
                          \"CardPool\":[98535,98381,98366]}";
            let entry = unity_entry(body);
            let result = try_parse(&entry, test_timestamp());

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = draft_complete_payload(event);

            let raw = &payload["raw_complete_draft"];
            assert_eq!(raw["CardPool"][0], 98535);
            assert_eq!(raw["CardPool"][2], 98366);
        }

        #[test]
        fn test_try_parse_response_event_name_fallback() {
            let body = "[UnityCrossThreadLogger]2/25/2026 12:00:00 PM\n\
                         <== DraftCompleteDraft(fallback-test)\n\
                         {\"EventName\":\"QuickDraft_MKM_20260201\"}";
            let entry = unity_entry(body);
            let result = try_parse(&entry, test_timestamp());

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = draft_complete_payload(event);

            assert_eq!(payload["event_name"], "QuickDraft_MKM_20260201");
        }
    }

    // -- Flat/legacy format --------------------------------------------------

    mod flat_format {
        use super::*;

        #[test]
        fn test_try_parse_flat_basic() {
            let body = "[UnityCrossThreadLogger]DraftCompleteDraft\n\
                         {\n\
                           \"DraftId\": \"abc-123-def\",\n\
                           \"EventName\": \"PremierDraft_MKM_20260201\"\n\
                         }";
            let entry = unity_entry(body);
            let result = try_parse(&entry, test_timestamp());

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = draft_complete_payload(event);

            assert_eq!(payload["type"], "draft_complete");
            assert_eq!(payload["draft_id"], "abc-123-def");
            assert_eq!(payload["event_name"], "PremierDraft_MKM_20260201");
        }

        #[test]
        fn test_try_parse_flat_traditional() {
            let body = "[UnityCrossThreadLogger]DraftCompleteDraft\n\
                         {\n\
                           \"DraftId\": \"trad-456\",\n\
                           \"EventName\": \"TradDraft_DSK_20260115\"\n\
                         }";
            let entry = unity_entry(body);
            let result = try_parse(&entry, test_timestamp());

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = draft_complete_payload(event);

            assert_eq!(payload["draft_id"], "trad-456");
            assert_eq!(payload["event_name"], "TradDraft_DSK_20260115");
        }

        #[test]
        fn test_try_parse_flat_quick_draft() {
            let body = "[UnityCrossThreadLogger]DraftCompleteDraft\n\
                         {\n\
                           \"DraftId\": \"quick-789\",\n\
                           \"EventName\": \"QuickDraft_MKM_20260201\"\n\
                         }";
            let entry = unity_entry(body);
            let result = try_parse(&entry, test_timestamp());

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = draft_complete_payload(event);

            assert_eq!(payload["draft_id"], "quick-789");
            assert_eq!(payload["event_name"], "QuickDraft_MKM_20260201");
        }

        #[test]
        fn test_try_parse_flat_lowercase_draft_id() {
            let body = "[UnityCrossThreadLogger]DraftCompleteDraft\n\
                         {\n\
                           \"draftId\": \"lowercase-123\",\n\
                           \"EventName\": \"PremierDraft_MKM_20260201\"\n\
                         }";
            let entry = unity_entry(body);
            let result = try_parse(&entry, test_timestamp());

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = draft_complete_payload(event);

            assert_eq!(payload["draft_id"], "lowercase-123");
        }

        #[test]
        fn test_try_parse_flat_internal_event_name() {
            let body = "[UnityCrossThreadLogger]DraftCompleteDraft\n\
                         {\n\
                           \"DraftId\": \"intern-456\",\n\
                           \"InternalEventName\": \"PremierDraft_MKM_20260201\"\n\
                         }";
            let entry = unity_entry(body);
            let result = try_parse(&entry, test_timestamp());

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = draft_complete_payload(event);

            assert_eq!(payload["event_name"], "PremierDraft_MKM_20260201");
        }
    }

    // -- Missing / default fields --------------------------------------------

    mod missing_fields {
        use super::*;

        #[test]
        fn test_try_parse_missing_draft_id() {
            let body = "[UnityCrossThreadLogger]DraftCompleteDraft\n\
                         {\n\
                           \"EventName\": \"PremierDraft_MKM_20260201\"\n\
                         }";
            let entry = unity_entry(body);
            let result = try_parse(&entry, test_timestamp());

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = draft_complete_payload(event);

            assert_eq!(payload["draft_id"], "");
            assert_eq!(payload["event_name"], "PremierDraft_MKM_20260201");
        }

        #[test]
        fn test_try_parse_missing_event_name() {
            let body = "[UnityCrossThreadLogger]DraftCompleteDraft\n\
                         {\n\
                           \"DraftId\": \"no-event-name\"\n\
                         }";
            let entry = unity_entry(body);
            let result = try_parse(&entry, test_timestamp());

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = draft_complete_payload(event);

            assert_eq!(payload["draft_id"], "no-event-name");
            assert_eq!(payload["event_name"], "");
        }

        #[test]
        fn test_try_parse_minimal_payload() {
            let body = "[UnityCrossThreadLogger]DraftCompleteDraft\n\
                         {}";
            let entry = unity_entry(body);
            let result = try_parse(&entry, test_timestamp());

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = draft_complete_payload(event);

            assert_eq!(payload["draft_id"], "");
            assert_eq!(payload["event_name"], "");
        }
    }

    // -- Metadata preservation -----------------------------------------------

    mod metadata {
        use super::*;

        #[test]
        fn test_try_parse_preserves_raw_bytes() {
            let body = r#"[UnityCrossThreadLogger]==> DraftCompleteDraft {"id":"raw-test","request":"{\"EventName\":\"PremierDraft_MKM_20260201\"}"}"#;
            let entry = unity_entry(body);
            let result = try_parse(&entry, test_timestamp());

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            assert_eq!(event.metadata().raw_bytes(), body.as_bytes());
        }

        #[test]
        fn test_try_parse_stores_timestamp() {
            let body = "[UnityCrossThreadLogger]DraftCompleteDraft\n\
                         {\"DraftId\": \"ts-test\"}";
            let entry = unity_entry(body);
            let ts = test_timestamp();
            let result = try_parse(&entry, ts);

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            assert_eq!(event.metadata().timestamp(), ts);
        }

        #[test]
        fn test_try_parse_preserves_raw_payload() {
            let body = "[UnityCrossThreadLogger]DraftCompleteDraft\n\
                         {\n\
                           \"DraftId\": \"raw-payload\",\n\
                           \"ExtraField\": \"preserved\"\n\
                         }";
            let entry = unity_entry(body);
            let result = try_parse(&entry, test_timestamp());

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = draft_complete_payload(event);

            assert_eq!(payload["raw_complete_draft"]["ExtraField"], "preserved");
        }
    }

    // -- Non-matching entries (should return None) ----------------------------

    mod non_matching {
        use super::*;

        #[test]
        fn test_try_parse_unrelated_entry_returns_none() {
            let body = "[UnityCrossThreadLogger]greToClientEvent\n{\"data\": 1}";
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
        fn test_try_parse_bot_draft_entry_returns_none() {
            let body = "[UnityCrossThreadLogger]BotDraft_DraftPick\n\
                         {\"PickInfo\": {\"CardId\": 12345}}";
            let entry = unity_entry(body);
            assert!(try_parse(&entry, test_timestamp()).is_none());
        }

        #[test]
        fn test_try_parse_human_draft_entry_returns_none() {
            let body = "[UnityCrossThreadLogger]Draft.Notify\n\
                         {\"SelfPack\": 0, \"SelfPick\": 0, \
                          \"PackCards\": \"12345\"}";
            let entry = unity_entry(body);
            assert!(try_parse(&entry, test_timestamp()).is_none());
        }

        #[test]
        fn test_try_parse_malformed_json_returns_none() {
            let body = "[UnityCrossThreadLogger]DraftCompleteDraft\n\
                         {broken json!!!}";
            let entry = unity_entry(body);
            assert!(try_parse(&entry, test_timestamp()).is_none());
        }

        #[test]
        fn test_try_parse_marker_only_no_json_returns_none() {
            let body = "[UnityCrossThreadLogger]DraftCompleteDraft";
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

        #[test]
        fn test_try_parse_old_underscore_marker_returns_none() {
            let body = "[UnityCrossThreadLogger]Draft_CompleteDraft\n\
                         {\"DraftId\": \"old-marker\", \
                          \"EventName\": \"PremierDraft_MKM_20260201\"}";
            let entry = unity_entry(body);
            assert!(try_parse(&entry, test_timestamp()).is_none());
        }
    }

    // -- Header with timestamp -----------------------------------------------

    mod timestamp_in_header {
        use super::*;

        #[test]
        fn test_try_parse_with_timestamp_prefix() {
            let body = "[UnityCrossThreadLogger]2/25/2026 12:00:00 PM \
                         DraftCompleteDraft\n\
                         {\"DraftId\": \"ts-prefix\", \
                          \"EventName\": \"PremierDraft_MKM_20260201\"}";
            let entry = unity_entry(body);
            let result = try_parse(&entry, test_timestamp());

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = draft_complete_payload(event);

            assert_eq!(payload["type"], "draft_complete");
            assert_eq!(payload["draft_id"], "ts-prefix");
        }
    }

    // -- Performance class ---------------------------------------------------

    mod performance_class {
        use super::*;

        #[test]
        fn test_draft_complete_event_is_durable_per_event() {
            let body = "[UnityCrossThreadLogger]DraftCompleteDraft\n\
                         {\"DraftId\": \"perf-test\"}";
            let entry = unity_entry(body);
            let result = try_parse(&entry, test_timestamp());

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            assert_eq!(event.performance_class(), PerformanceClass::DurablePerEvent);
        }
    }

    // -- Internal helpers ----------------------------------------------------

    mod helpers {
        use super::*;

        #[test]
        fn test_extract_json_from_body_object() {
            let body = "header line\n{\"key\": \"value\"}";
            let json = extract_json_from_body(body);
            assert_eq!(json, Some("{\"key\": \"value\"}"));
        }

        #[test]
        fn test_extract_json_from_body_no_json() {
            assert!(extract_json_from_body("no json here").is_none());
        }

        #[test]
        fn test_extract_json_from_body_with_header_bracket() {
            let body = "[UnityCrossThreadLogger]some text\n{\"data\": 1}";
            let json = extract_json_from_body(body);
            assert_eq!(json, Some("{\"data\": 1}"));
        }

        #[test]
        fn test_build_payload_request_format() {
            let parsed = serde_json::json!({
                "id": "req-123",
                "request": "{\"EventName\":\"PremierDraft_MKM_20260201\",\"IsBotDraft\":false}"
            });
            let payload = build_payload(&parsed, None);
            assert_eq!(payload["type"], "draft_complete");
            assert_eq!(payload["draft_id"], "req-123");
            assert_eq!(payload["event_name"], "PremierDraft_MKM_20260201");
        }

        #[test]
        fn test_build_payload_response_format() {
            let parsed = serde_json::json!({
                "CourseId": "course-456",
                "InternalEventName": "PremierDraft_MKM_20260201",
                "CurrentModule": "DeckSelect"
            });
            let payload = build_payload(&parsed, Some("resp-789"));
            assert_eq!(payload["draft_id"], "resp-789");
            assert_eq!(payload["event_name"], "PremierDraft_MKM_20260201");
        }

        #[test]
        fn test_build_payload_flat_format() {
            let parsed = serde_json::json!({
                "DraftId": "test-id",
                "EventName": "PremierDraft_MKM_20260201"
            });
            let payload = build_payload(&parsed, None);
            assert_eq!(payload["type"], "draft_complete");
            assert_eq!(payload["draft_id"], "test-id");
            assert_eq!(payload["event_name"], "PremierDraft_MKM_20260201");
        }

        #[test]
        fn test_build_payload_empty() {
            let parsed = serde_json::json!({});
            let payload = build_payload(&parsed, None);
            assert_eq!(payload["draft_id"], "");
            assert_eq!(payload["event_name"], "");
        }

        #[test]
        fn test_extract_draft_id_from_body_response() {
            let body = "<== DraftCompleteDraft(abc-123-def)\n{\"some\":\"json\"}";
            assert_eq!(
                extract_draft_id_from_body(body),
                Some("abc-123-def".to_owned()),
            );
        }

        #[test]
        fn test_extract_draft_id_from_body_request_returns_none() {
            let body = r#"==> DraftCompleteDraft {"id":"abc-123-def"}"#;
            assert!(extract_draft_id_from_body(body).is_none());
        }

        #[test]
        fn test_extract_draft_id_from_body_no_marker() {
            assert!(extract_draft_id_from_body("no marker here").is_none());
        }

        #[test]
        fn test_event_name_from_request_valid() {
            let parsed = serde_json::json!({
                "request": "{\"EventName\":\"PremierDraft_ECL_20260120\"}"
            });
            assert_eq!(
                event_name_from_request(&parsed),
                "PremierDraft_ECL_20260120",
            );
        }

        #[test]
        fn test_event_name_from_request_no_field() {
            let parsed = serde_json::json!({"id": "test"});
            assert_eq!(event_name_from_request(&parsed), "");
        }

        #[test]
        fn test_event_name_from_request_invalid_json() {
            let parsed = serde_json::json!({"request": "not json"});
            assert_eq!(event_name_from_request(&parsed), "");
        }
    }
}
