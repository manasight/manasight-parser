//! Bot draft parser for Quick Draft (`BotDraftDraftStatus` and
//! `BotDraftDraftPick`) events.
//!
//! In Quick Draft (bot draft), the player drafts against AI opponents.
//! Two API method names capture the draft flow:
//!
//! | Direction | Signature | Meaning |
//! |-----------|-----------|---------|
//! | Response (`<==`) | `BotDraftDraftStatus` | Initial pack (Pack 0, Pick 0) |
//! | Request (`==>`) | `BotDraftDraftPick`   | Card selected |
//! | Response (`<==`) | `BotDraftDraftPick`   | Card selected and next pack |
//!
//! **Key Fields:**
//!
//! - `BotDraftDraftStatus`: `Payload` { `EventName`, `DraftStatus`,
//!   `PackNumber`, `PickNumber`, `DraftPack` }
//! - `BotDraftDraftPick` (Request): `request` { `EventName`, `PickInfo` {
//!   `EventName`, `CardIds`, `PackNumber`, `PickNumber` } }
//! - `BotDraftDraftPick` (Response): `Payload` { `EventName`, `DraftStatus`,
//!   `PackNumber`, `PickNumber`, `DraftPack`, `PickedCards` }
//!
//! Quick Draft has 3 packs of 14 picks each (42 total picks). Pack and
//! pick numbers are zero-indexed in the log.
//!
//! Both events are Class 2 (Durable Per-Event) -- each pick is
//! independently valuable and must survive crashes.

use crate::events::{DraftBotEvent, EventMetadata, GameEvent};
use crate::log::entry::LogEntry;
use crate::parsers::api_common;

/// Marker for bot draft status events.
///
/// This marker is used for the initial pack presentation (Pack 0, Pick 0).
const DRAFT_STATUS_MARKER: &str = "BotDraftDraftStatus";

/// Marker for bot draft pick events.
///
/// This marker is used both for the player's pick (request) and for presenting
/// subsequent packs (response).
const DRAFT_PICK_MARKER: &str = "BotDraftDraftPick";

/// Attempts to parse a [`LogEntry`] as a bot draft event.
///
/// Returns `Some(GameEvent::DraftBot(_))` if the entry matches either:
/// - A pack presentation (`BotDraftDraftStatus` or `BotDraftDraftPick` response).
/// - A pick confirmation (`BotDraftDraftPick` request).
///
/// Returns `None` if the entry does not match either signature.
///
/// The `timestamp` is `None` when the log entry header did not contain a
/// parseable timestamp. It is passed through to [`EventMetadata`] so
/// downstream consumers can distinguish real vs missing timestamps.
pub fn try_parse(
    entry: &LogEntry,
    timestamp: Option<chrono::DateTime<chrono::Utc>>,
) -> Option<GameEvent> {
    let body = &entry.body;

    // Try bot draft pack presentation first.
    if let Some(payload) = try_parse_pack_presentation(body) {
        let metadata = EventMetadata::new(timestamp, body.as_bytes().to_vec());
        return Some(GameEvent::DraftBot(DraftBotEvent::new(metadata, payload)));
    }

    // Try bot draft pick confirmation.
    if let Some(payload) = try_parse_draft_pick(body) {
        let metadata = EventMetadata::new(timestamp, body.as_bytes().to_vec());
        return Some(GameEvent::DraftBot(DraftBotEvent::new(metadata, payload)));
    }

    None
}

/// Attempts to parse a `BotDraftDraftStatus` or `BotDraftDraftPick` pack
/// presentation response.
///
/// Returns `Some(serde_json::Value)` if parsing succeeds, otherwise `None`.
///
/// The log entry body must be an API response whose string-escaped `Payload`
/// field contains:
/// - `DraftStatus`: must be `"PickNext"` (required; otherwise returns `None`)
/// - `DraftPack`: array of card GRP IDs available in the pack
///
/// The following are extracted on a best-effort basis and default when absent:
/// - `PackNumber`: zero-indexed pack number (defaults to `0`)
/// - `PickNumber`: zero-indexed pick number within the pack (defaults to `0`)
/// - `EventName`: the Arena event identifier, e.g., `"QuickDraft_MKM_20260201"`
///   (defaults to `""`)
fn try_parse_pack_presentation(body: &str) -> Option<serde_json::Value> {
    let parsed = if api_common::is_api_response(body, DRAFT_STATUS_MARKER) {
        let top = api_common::parse_json_from_body(body, DRAFT_STATUS_MARKER)?;
        api_common::parse_nested_json(&top, "Payload", Some(DRAFT_STATUS_MARKER))
    } else if api_common::is_api_response(body, DRAFT_PICK_MARKER) {
        let top = api_common::parse_json_from_body(body, DRAFT_PICK_MARKER)?;
        api_common::parse_nested_json(&top, "Payload", Some(DRAFT_PICK_MARKER))
    } else {
        None
    }?;

    // Check if this is a pack presentation.
    let status_val = parsed.get("DraftStatus")?;
    if status_val.as_str() != Some("PickNext") {
        return None;
    }

    let pack_idx = parsed
        .get("PackNumber")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0);

    let selection_idx = parsed
        .get("PickNumber")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0);

    let event_name = api_common::extract_event_name(&parsed);

    // DraftPack is an array of card GRP IDs (integers) as strings.
    let draft_pack = extract_draft_pack(&parsed);

    Some(serde_json::json!({
        "type": "draft_bot_pack",
        "event_name": event_name,
        "pack_number": pack_idx,
        "pick_number": selection_idx,
        "draft_pack": draft_pack,
        "raw_draft_status": parsed,
    }))
}

/// Attempts to parse a `BotDraftDraftPick` pick confirmation.
///
/// Returns `Some(serde_json::Value)` if parsing succeeds.
///
/// Returns `None` if the entry is not a `BotDraftDraftPick` request, if the
/// nested `request` / `PickInfo` payload is missing, if `CardIds` is empty
/// or its first entry is `0` (a sentinel for "no card resolved"), or if
/// parsing fails.
///
/// The log entry body must be an API request whose string-escaped `request`
/// field contains `PickInfo` with:
/// - `CardIds`: the first GRP ID is treated as the selected card
/// - `PackNumber`: zero-indexed pack number
/// - `PickNumber`: zero-indexed pick number within the pack
fn try_parse_draft_pick(body: &str) -> Option<serde_json::Value> {
    let parsed = if api_common::is_api_request(body, DRAFT_PICK_MARKER) {
        let top = api_common::parse_json_from_body(body, DRAFT_PICK_MARKER)?;
        api_common::parse_nested_json(&top, "request", Some(DRAFT_PICK_MARKER))
    } else {
        None
    }?;

    let pick_info = parsed.get("PickInfo")?;

    // Ignore envelopes that do not actually carry pick fields.
    if pick_info.get("CardIds").is_none()
        && pick_info.get("PackNumber").is_none()
        && pick_info.get("PickNumber").is_none()
    {
        return None;
    }

    let pack_idx = pick_info
        .get("PackNumber")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0);

    let selection_idx = pick_info
        .get("PickNumber")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0);

    let card_ids: Vec<i64> = pick_info
        .get("CardIds")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| {
                    v.as_i64()
                        .or_else(|| v.as_str().and_then(|s| s.parse::<i64>().ok()))
                })
                .collect()
        })
        .unwrap_or_default();

    let card_id = *card_ids.first()?;
    if card_id == 0 {
        return None;
    }

    let event_name = api_common::extract_event_name(&parsed);

    Some(serde_json::json!({
        "type": "draft_bot_pick",
        "event_name": event_name,
        "card_id": card_id,
        "pack_number": pack_idx,
        "pick_number": selection_idx,
        "raw_pick_info": parsed,
    }))
}

/// Extracts the `DraftPack` array from a pack presentation payload.
///
/// `DraftPack` may contain card GRP IDs as strings (e.g., `["12345", "67890"]`)
/// or as integers. This function normalizes them to a `Vec<i64>`.
fn extract_draft_pack(parsed: &serde_json::Value) -> Vec<i64> {
    let Some(pack) = parsed.get("DraftPack").and_then(|v| v.as_array()) else {
        return Vec::new();
    };

    pack.iter()
        .filter_map(|v| {
            // Try integer first, then string-encoded integer.
            v.as_i64()
                .or_else(|| v.as_str().and_then(|s| s.parse::<i64>().ok()))
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::PerformanceClass;
    use crate::parsers::test_helpers::{
        draft_bot_payload, test_timestamp, unity_entry, EntryHeader,
    };

    // -- Pack presentation parsing (BotDraftDraftStatus, BotDraftDraftPick) -

    mod pack_presentation {
        use super::*;

        #[test]
        fn test_try_parse_pack_presentation_basic() {
            let body = "[UnityCrossThreadLogger]2/01/2026 10:23:51 AM\n\
            <== BotDraftDraftStatus(uuid)\n\
            {\"Payload\":\"{\\\"EventName\\\":\\\"QuickDraft_MKM_20260201\\\",\\\"DraftStatus\\\":\\\"PickNext\\\",\\\"PackNumber\\\":0,\\\"PickNumber\\\":0,\\\"DraftPack\\\":[\\\"12345\\\",\\\"67890\\\",\\\"11111\\\"]}\"}";
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = draft_bot_payload(event);

            assert_eq!(payload["type"], "draft_bot_pack");
            assert_eq!(payload["event_name"], "QuickDraft_MKM_20260201");
            assert_eq!(payload["pack_number"], 0);
            assert_eq!(payload["pick_number"], 0);
            assert_eq!(
                payload["draft_pack"],
                serde_json::json!([12345, 67890, 11111])
            );
        }

        #[test]
        fn test_try_parse_pack_presentation_second_pack() {
            let body = "[UnityCrossThreadLogger]1/18/2026 8:42:01 PM\n\
            <== BotDraftDraftPick(uuid)\n\
            {\"Payload\":\"{\\\"EventName\\\":\\\"QuickDraft_DSK_20260115\\\",\\\"DraftStatus\\\":\\\"PickNext\\\",\\\"PackNumber\\\":1,\\\"PickNumber\\\":3,\\\"DraftPack\\\":[\\\"22222\\\",\\\"33333\\\"]}\"}";
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = draft_bot_payload(event);

            assert_eq!(payload["pack_number"], 1);
            assert_eq!(payload["pick_number"], 3);
            assert_eq!(payload["event_name"], "QuickDraft_DSK_20260115");
            assert_eq!(payload["draft_pack"], serde_json::json!([22222, 33333]));
        }

        #[test]
        fn test_try_parse_pack_presentation_third_pack_last_pick() {
            let body = "[UnityCrossThreadLogger]2/12/2026 1:11:11 PM\n\
            <== BotDraftDraftPick(uuid)\n\
            {\"Payload\":\"{\\\"EventName\\\":\\\"QuickDraft_MKM_20260201\\\",\\\"DraftStatus\\\":\\\"PickNext\\\",\\\"PackNumber\\\":2,\\\"PickNumber\\\":13,\\\"DraftPack\\\":[\\\"44444\\\"]}\"}";
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = draft_bot_payload(event);

            assert_eq!(payload["pack_number"], 2);
            assert_eq!(payload["pick_number"], 13);
            assert_eq!(payload["draft_pack"], serde_json::json!([44444]));
        }

        #[test]
        fn test_try_parse_pack_presentation_integer_card_ids() {
            let body = "[UnityCrossThreadLogger]2/01/2026 10:23:51 AM\n\
            <== BotDraftDraftStatus(uuid)\n\
            {\"Payload\":\"{\\\"DraftStatus\\\":\\\"PickNext\\\",\\\"PackNumber\\\":0,\\\"PickNumber\\\":0,\\\"DraftPack\\\":[12345, 67890]}\"}";
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = draft_bot_payload(event);

            assert_eq!(payload["draft_pack"], serde_json::json!([12345, 67890]));
        }

        #[test]
        fn test_try_parse_pack_presentation_empty_pack() {
            let body = "[UnityCrossThreadLogger]2/12/2026 1:11:11 PM\n\
            <== BotDraftDraftPick(uuid)\n\
            {\"Payload\":\"{\\\"DraftStatus\\\":\\\"PickNext\\\",\\\"PackNumber\\\":0,\\\"PickNumber\\\":0,\\\"DraftPack\\\":[]}\"}";
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = draft_bot_payload(event);

            assert_eq!(payload["draft_pack"], serde_json::json!([]));
        }

        #[test]
        fn test_try_parse_pack_presentation_missing_draft_pack() {
            let body = "[UnityCrossThreadLogger]2/12/2026 1:11:11 PM\n\
            <== BotDraftDraftPick(uuid)\n\
            {\"Payload\":\"{\\\"DraftStatus\\\":\\\"PickNext\\\",\\\"PackNumber\\\":0,\\\"PickNumber\\\":0}\"}";
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = draft_bot_payload(event);

            assert_eq!(payload["draft_pack"], serde_json::json!([]));
        }

        #[test]
        fn test_try_parse_pack_presentation_preserves_raw_payload() {
            let body = "[UnityCrossThreadLogger]2/01/2026 10:23:51 AM\n\
            <== BotDraftDraftStatus(uuid)\n\
            {\n\
                \"Payload\":\"{\\\"DraftStatus\\\":\\\"PickNext\\\",\\\"PackNumber\\\":0,\\\"PickNumber\\\":0,\\\"DraftPack\\\":[\\\"11111\\\"],\\\"ExtraField\\\":\\\"preserved\\\"}\"
            }";
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = draft_bot_payload(event);

            assert_eq!(payload["raw_draft_status"]["ExtraField"], "preserved");
        }
    }

    // -- Draft pick parsing (BotDraftDraftPick) -----------------------------

    mod draft_pick {
        use super::*;

        #[test]
        fn test_try_parse_draft_pick_returns_pick() {
            let body = r#"[UnityCrossThreadLogger]==> BotDraftDraftPick {"id":"uuid","request":"{\"EventName\":\"QuickDraft_TMT_20260313\",\"PickInfo\":{\"EventName\":\"QuickDraft_TMT_20260313\",\"CardIds\":[\"12345\"],\"PackNumber\":0,\"PickNumber\":0}}"}"#;
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = draft_bot_payload(event);

            assert_eq!(payload["type"], "draft_bot_pick");
            assert_eq!(payload["card_id"], 12345);
            assert_eq!(payload["pack_number"], 0);
            assert_eq!(payload["pick_number"], 0);
        }

        #[test]
        fn test_try_parse_draft_pick_later_in_draft() {
            let body = r#"[UnityCrossThreadLogger]==> BotDraftDraftPick {"id":"uuid","request":"{\"EventName\":\"QuickDraft_TMT_20260313\",\"PickInfo\":{\"EventName\":\"QuickDraft_TMT_20260313\",\"CardIds\":[\"67890\"],\"PackNumber\":1,\"PickNumber\":7}}"}"#;
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = draft_bot_payload(event);

            assert_eq!(payload["card_id"], 67890);
            assert_eq!(payload["pack_number"], 1);
            assert_eq!(payload["pick_number"], 7);
        }

        #[test]
        fn test_try_parse_draft_pick_event_name_in_request_root_returns_name() {
            let body = r#"[UnityCrossThreadLogger]==> BotDraftDraftPick {"id":"uuid","request":"{\"EventName\":\"QuickDraft_SOS_20260430\",\"PickInfo\":{\"CardIds\":[\"12345\"],\"PackNumber\":0,\"PickNumber\":0}}"}"#;
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = draft_bot_payload(event);

            assert_eq!(payload["event_name"], "QuickDraft_SOS_20260430");
        }

        #[test]
        fn test_try_parse_draft_pick_event_name_in_pick_info_returns_name() {
            let body = r#"[UnityCrossThreadLogger]==> BotDraftDraftPick {"id":"uuid","request":"{\"PickInfo\":{\"EventName\":\"QuickDraft_SOS_20260430\",\"CardIds\":[\"12345\"],\"PackNumber\":0,\"PickNumber\":0}}"}"#;
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = draft_bot_payload(event);

            assert_eq!(payload["event_name"], "QuickDraft_SOS_20260430");
        }

        #[test]
        fn test_try_parse_draft_pick_missing_event_name_defaults_to_empty() {
            let body = r#"[UnityCrossThreadLogger]==> BotDraftDraftPick {"id":"uuid","request":"{\"PickInfo\":{\"CardIds\":[\"98546\"],\"PackNumber\":0,\"PickNumber\":0}}"}"#;
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = draft_bot_payload(event);

            assert_eq!(payload["event_name"], "");
        }

        #[test]
        fn test_try_parse_draft_pick_missing_card_id_returns_none() {
            let body = r#"[UnityCrossThreadLogger]==> BotDraftDraftPick {"id":"uuid","request":"{\"PickInfo\":{\"EventName\":\"Test\",\"PackNumber\":0,\"PickNumber\":0}}"}"#;
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_none());
        }

        #[test]
        fn test_try_parse_draft_pick_preserves_raw_payload() {
            let body = r#"[UnityCrossThreadLogger]==> BotDraftDraftPick {"id":"uuid","request":"{\"PickInfo\":{\"EventName\":\"Test\",\"CardIds\":[\"12345\"],\"PackNumber\":0,\"PickNumber\":0},\"ExtraField\":\"kept\"}"}"#;
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = draft_bot_payload(event);

            assert_eq!(payload["raw_pick_info"]["ExtraField"], "kept");
        }
    }

    // -- Metadata preservation -----------------------------------------------

    mod metadata {
        use super::*;

        #[test]
        fn test_try_parse_preserves_raw_bytes_pack() {
            let body = "[UnityCrossThreadLogger]2/01/2026 10:23:51 AM\n\
            <== BotDraftDraftStatus(uuid)\n\
            {\"Payload\":\"{\\\"DraftStatus\\\":\\\"PickNext\\\",\\\"PackNumber\\\":0,\\\"PickNumber\\\":0,\\\"DraftPack\\\":[]}\"}";
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            assert_eq!(event.metadata().raw_bytes(), body.as_bytes());
        }

        #[test]
        fn test_try_parse_preserves_raw_bytes_pick() {
            let body = r#"[UnityCrossThreadLogger]==> BotDraftDraftPick {"id":"uuid","request":"{\"PickInfo\":{\"CardIds\":[\"1\"],\"PackNumber\":0,\"PickNumber\":0}}"}"#;
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            assert_eq!(event.metadata().raw_bytes(), body.as_bytes());
        }

        #[test]
        fn test_try_parse_stores_timestamp_pack() {
            let body = "[UnityCrossThreadLogger]2/01/2026 10:23:51 AM\n\
            <== BotDraftDraftStatus(uuid)\n\
            {\"Payload\":\"{\\\"DraftStatus\\\":\\\"PickNext\\\",\\\"PackNumber\\\":0,\\\"PickNumber\\\":0,\\\"DraftPack\\\":[]}\"}";
            let entry = unity_entry(body);
            let ts = Some(test_timestamp());
            let result = try_parse(&entry, ts);

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            assert_eq!(event.metadata().timestamp(), ts);
        }

        #[test]
        fn test_try_parse_stores_timestamp_pick() {
            let body = r#"[UnityCrossThreadLogger]==> BotDraftDraftPick {"id":"uuid","request":"{\"PickInfo\":{\"CardIds\":[\"1\"],\"PackNumber\":0,\"PickNumber\":0}}"}"#;
            let entry = unity_entry(body);
            let ts = Some(test_timestamp());
            let result = try_parse(&entry, ts);

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            assert_eq!(event.metadata().timestamp(), ts);
        }
    }

    // -- Non-matching entries (should return None) ----------------------------

    mod non_matching {
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
        fn test_try_parse_draft_status_not_pick_next_returns_none() {
            let body = "[UnityCrossThreadLogger]2/26/2026 1:11:11 PM\n\
                        <== BotDraftDraftStatus(uuid)\n\
                        {\"Payload\":\"{\\\"DraftStatus\\\":\\\"Completed\\\",\\\"PackNumber\\\":2,\\\"PickNumber\\\":13,\\\"DraftPack\\\":[]}\"}";
            let entry = unity_entry(body);
            assert!(try_parse(&entry, Some(test_timestamp())).is_none());
        }

        #[test]
        fn test_try_parse_pack_presentation_wrong_status_returns_none() {
            let body = "[UnityCrossThreadLogger]2/26/2026 1:11:11 PM\n\
                        <== BotDraftDraftPick(uuid)\n\
                        {\"Payload\":\"{\\\"DraftStatus\\\":\\\"Completed\\\",\\\"PackNumber\\\":2,\\\"PickNumber\\\":13,\\\"DraftPack\\\":[]}\"}";
            let entry = unity_entry(body);
            assert!(try_parse(&entry, Some(test_timestamp())).is_none());
        }

        #[test]
        fn test_try_parse_malformed_json_returns_none() {
            let body = r#"[UnityCrossThreadLogger]==> BotDraftDraftPick {"id":"uuid","request":"{\"PickInfo\":broken!!!}"}"#;
            let entry = unity_entry(body);
            assert!(try_parse(&entry, Some(test_timestamp())).is_none());
        }

        #[test]
        fn test_try_parse_marker_only_no_json_returns_none() {
            let body = "[UnityCrossThreadLogger]==> BotDraftDraftPick";
            let entry = unity_entry(body);
            assert!(try_parse(&entry, Some(test_timestamp())).is_none());
        }

        #[test]
        fn test_try_parse_missing_pick_fields_returns_none() {
            let body = r#"[UnityCrossThreadLogger]==> BotDraftDraftPick {"id":"uuid","request":"{\"PickInfo\":{}}"}"#;
            let entry = unity_entry(body);
            assert!(try_parse(&entry, Some(test_timestamp())).is_none());
        }

        #[test]
        fn test_try_parse_empty_card_ids_returns_none() {
            let body = r#"[UnityCrossThreadLogger]==> BotDraftDraftPick {"id":"uuid","request":"{\"PickInfo\":{\"CardIds\":[],\"PackNumber\":0,\"PickNumber\":0}}"}"#;
            let entry = unity_entry(body);
            assert!(try_parse(&entry, Some(test_timestamp())).is_none());
        }

        #[test]
        fn test_try_parse_zero_card_id_returns_none() {
            // GRP ID 0 is a sentinel for "no card resolved" and is never a
            // valid MTGA card; the parser must drop these envelopes.
            let body = r#"[UnityCrossThreadLogger]==> BotDraftDraftPick {"id":"uuid","request":"{\"PickInfo\":{\"CardIds\":[\"0\"],\"PackNumber\":0,\"PickNumber\":0}}"}"#;
            let entry = unity_entry(body);
            assert!(try_parse(&entry, Some(test_timestamp())).is_none());
        }

        #[test]
        fn test_try_parse_draft_status_marker_in_text_only_returns_none() {
            // The text mentions DraftStatus but no valid JSON payload.
            let body = "[UnityCrossThreadLogger]2/26/2026 1:11:11 PM\n\
                        <== BotDraftDraftStatus(uuid)\n\
                        DraftStatus is PickNext";
            let entry = unity_entry(body);
            assert!(try_parse(&entry, Some(test_timestamp())).is_none());
        }

        #[test]
        fn test_try_parse_draft_pick_missing_payload_returns_none() {
            let body = "[UnityCrossThreadLogger]2/26/2026 1:11:11 PM\n\
                        <== BotDraftDraftPick(uuid)\n\
                         {\n\
                           \"Result\": \"Success\"\n\
                         }";
            let entry = unity_entry(body);
            assert!(try_parse(&entry, Some(test_timestamp())).is_none());
        }

        #[test]
        fn test_try_parse_draft_pick_malformed_payload_returns_none() {
            let body = "[UnityCrossThreadLogger]2/26/2026 1:11:11 PM\n\
                         <== BotDraftDraftPick(uuid)\n\
                         {\n\
                           \"Payload\": \"not json\"\n\
                         }";
            let entry = unity_entry(body);
            assert!(try_parse(&entry, Some(test_timestamp())).is_none());
        }

        #[test]
        fn test_try_parse_human_draft_entry_returns_none() {
            // Human draft entries should not be parsed by the bot draft parser.
            let body = "[UnityCrossThreadLogger]Draft.Notify\n\
                         {\n\
                           \"draftId\": \"abc-123\",\n\
                           \"SelfPack\": 0,\n\
                           \"SelfPick\": 0,\n\
                           \"PackCards\": \"12345,67890\"\n\
                         }";
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

    // -- Performance class ---------------------------------------------------

    mod performance_class {
        use super::*;

        #[test]
        fn test_draft_bot_event_is_durable_per_event() {
            let body = "[UnityCrossThreadLogger]2/01/2026 10:23:51 AM\n\
            <== BotDraftDraftStatus(uuid)\n\
            {\"Payload\":\"{\\\"DraftStatus\\\":\\\"PickNext\\\",\\\"PackNumber\\\":0,\\\"PickNumber\\\":0,\\\"DraftPack\\\":[]}\"}";
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            assert_eq!(event.performance_class(), PerformanceClass::DurablePerEvent);
        }

        #[test]
        fn test_draft_bot_pick_event_is_durable_per_event() {
            let body = r#"[UnityCrossThreadLogger]==> BotDraftDraftPick {"id":"uuid","request":"{\"PickInfo\":{\"CardIds\":[\"1\"],\"PackNumber\":0,\"PickNumber\":0}}"}"#;
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
        fn test_extract_draft_pack_string_ids() {
            let parsed = serde_json::json!({
                "DraftPack": ["12345", "67890", "11111"]
            });
            let pack = extract_draft_pack(&parsed);
            assert_eq!(pack, vec![12345, 67890, 11111]);
        }

        #[test]
        fn test_extract_draft_pack_integer_ids() {
            let parsed = serde_json::json!({
                "DraftPack": [12345, 67890]
            });
            let pack = extract_draft_pack(&parsed);
            assert_eq!(pack, vec![12345, 67890]);
        }

        #[test]
        fn test_extract_draft_pack_empty() {
            let parsed = serde_json::json!({"DraftPack": []});
            let pack = extract_draft_pack(&parsed);
            assert!(pack.is_empty());
        }

        #[test]
        fn test_extract_draft_pack_missing_field() {
            let parsed = serde_json::json!({"other": "data"});
            let pack = extract_draft_pack(&parsed);
            assert!(pack.is_empty());
        }

        #[test]
        fn test_extract_draft_pack_mixed_types() {
            let parsed = serde_json::json!({
                "DraftPack": [12345, "67890", "not_a_number", 11111]
            });
            let pack = extract_draft_pack(&parsed);
            // "not_a_number" is silently skipped.
            assert_eq!(pack, vec![12345, 67890, 11111]);
        }
    }
}
