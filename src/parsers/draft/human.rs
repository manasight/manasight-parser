//! Human draft parser for Premier Draft and Traditional Draft events.
//!
//! In human (pod) drafts, the player drafts against other human players.
//! Three log signatures capture the draft flow:
//!
//! | Signature | Meaning | Key Fields |
//! |-----------|---------|------------|
//! | `Draft.Notify` | Draft state notification (pack presented) | `draftId`, `SelfPack`, `SelfPick`, `PackCards` |
//! | `EventPlayerDraftMakePick` | Player's pick selection | `EventName`, `PickInfo` with `CardId`, `PackNumber`, `PickNumber` |
//! | `LogBusinessEvents` with `PickGrpId` | Pick confirmation (business event) | `PickGrpId`, `PackCards`, `EventName` |
//!
//! Human drafts have 3 packs of 14 picks each (42 total picks). Pack and
//! pick numbers are zero-indexed in the log.
//!
//! All three events are Class 2 (Durable Per-Event) -- each pick is
//! independently valuable and must survive crashes.

use crate::events::{DraftHumanEvent, EventMetadata, GameEvent};
use crate::log::entry::LogEntry;
use crate::parsers::api_common;

/// Marker for human draft state notification events.
///
/// `Draft.Notify` appears in the log when a new pack is presented to the
/// player during a Premier or Traditional Draft.
const DRAFT_NOTIFY_MARKER: &str = "Draft.Notify";

/// Marker for human draft pick selection events.
///
/// `EventPlayerDraftMakePick` appears after the player selects a card
/// from the presented pack.
const MAKE_PICK_MARKER: &str = "EventPlayerDraftMakePick";

/// Marker that identifies business event entries in the log.
///
/// `LogBusinessEvents` is a shared container used for multiple event types
/// (game results, draft picks, etc.). We further discriminate by checking
/// for the `PickGrpId` field.
const BUSINESS_EVENTS_MARKER: &str = "LogBusinessEvents";

/// Field that distinguishes draft pick business events from other
/// `LogBusinessEvents` entries (e.g., game results with `WinningType`).
const PICK_GRP_ID_FIELD: &str = "PickGrpId";

/// Attempts to parse a [`LogEntry`] as a human draft event.
///
/// Returns `Some(GameEvent::DraftHuman(_))` if the entry matches any of:
/// - A `Draft.Notify` pack presentation
/// - An `EventPlayerDraftMakePick` pick selection
/// - A `LogBusinessEvents` with `PickGrpId` pick confirmation
///
/// Returns `None` if the entry does not match any human draft signature.
///
/// The `timestamp` is `None` when the log entry header did not contain a
/// parseable timestamp. It is passed through to [`EventMetadata`] so
/// downstream consumers can distinguish real vs missing timestamps.
pub fn try_parse(
    entry: &LogEntry,
    timestamp: Option<chrono::DateTime<chrono::Utc>>,
) -> Option<GameEvent> {
    let body = &entry.body;

    // Try Draft.Notify first (pack presentation -- most common during drafting).
    if let Some(payload) = try_parse_draft_notify(body) {
        let metadata = EventMetadata::new(timestamp, body.as_bytes().to_vec());
        return Some(GameEvent::DraftHuman(DraftHumanEvent::new(
            metadata, payload,
        )));
    }

    // Try EventPlayerDraftMakePick (pick selection).
    if let Some(payload) = try_parse_make_pick(body) {
        let metadata = EventMetadata::new(timestamp, body.as_bytes().to_vec());
        return Some(GameEvent::DraftHuman(DraftHumanEvent::new(
            metadata, payload,
        )));
    }

    // Try LogBusinessEvents with PickGrpId (pick confirmation).
    if let Some(payload) = try_parse_pick_business_event(body) {
        let metadata = EventMetadata::new(timestamp, body.as_bytes().to_vec());
        return Some(GameEvent::DraftHuman(DraftHumanEvent::new(
            metadata, payload,
        )));
    }

    None
}

/// Attempts to parse a `Draft.Notify` pack presentation event.
///
/// The log entry body contains a JSON object with:
/// - `draftId`: unique identifier for this draft session
/// - `SelfPack`: zero-indexed pack number (0, 1, 2)
/// - `SelfPick`: zero-indexed pick number within the pack
/// - `PackCards`: comma-separated string of card GRP IDs in the pack
fn try_parse_draft_notify(body: &str) -> Option<serde_json::Value> {
    if !body.contains(DRAFT_NOTIFY_MARKER) {
        return None;
    }

    let parsed = api_common::parse_json_from_body(body, "Draft.Notify")?;

    // Verify this is a Draft.Notify by checking for characteristic fields.
    // PackCards is the hallmark of a Draft.Notify payload.
    if parsed.get("PackCards").is_none() && parsed.get("SelfPack").is_none() {
        return None;
    }

    let draft_id = parsed
        .get("draftId")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");

    let pack_idx = parsed
        .get("SelfPack")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0);

    let selection_idx = parsed
        .get("SelfPick")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0);

    let pack_cards = extract_pack_cards_from_string(&parsed);

    Some(serde_json::json!({
        "type": "draft_human_notify",
        "draft_id": draft_id,
        "pack_number": pack_idx,
        "pick_number": selection_idx,
        "pack_cards": pack_cards,
        "raw_draft_notify": parsed,
    }))
}

/// Attempts to parse an `EventPlayerDraftMakePick` pick selection event.
///
/// The log entry body contains a JSON object with:
/// - `EventName`: the Arena event identifier
/// - `PickInfo` (optional wrapper) containing:
///   - `CardId`: the GRP ID of the selected card
///   - `PackNumber`: zero-indexed pack number
///   - `PickNumber`: zero-indexed pick number within the pack
fn try_parse_make_pick(body: &str) -> Option<serde_json::Value> {
    if !body.contains(MAKE_PICK_MARKER) {
        return None;
    }

    let parsed = api_common::parse_json_from_body(body, "EventPlayerDraftMakePick")?;

    // The pick info may be at top level or nested under a `PickInfo` key.
    let pick_info = parsed.get("PickInfo").unwrap_or(&parsed);

    let card_id = pick_info
        .get("CardId")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0);

    let pack_idx = pick_info
        .get("PackNumber")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0);

    let selection_idx = pick_info
        .get("PickNumber")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0);

    let event_name = parsed
        .get("EventName")
        .or_else(|| pick_info.get("EventName"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");

    // Some entries include the full list of card IDs that were in the pack.
    let card_ids = pick_info
        .get("CardIds")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(serde_json::Value::as_i64)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    Some(serde_json::json!({
        "type": "draft_human_pick",
        "event_name": event_name,
        "card_id": card_id,
        "pack_number": pack_idx,
        "pick_number": selection_idx,
        "card_ids": card_ids,
        "raw_make_pick": parsed,
    }))
}

/// Attempts to parse a `LogBusinessEvents` with `PickGrpId` pick confirmation.
///
/// The log entry body contains a JSON object (or array of objects) with:
/// - `PickGrpId`: the GRP ID of the picked card
/// - `PackCards`: card GRP IDs that were in the pack (may be comma-separated
///   string or array)
/// - `EventName` or `InternalEventName`: the Arena event identifier
fn try_parse_pick_business_event(body: &str) -> Option<serde_json::Value> {
    // Must contain both the business events marker and PickGrpId.
    if !body.contains(BUSINESS_EVENTS_MARKER) {
        return None;
    }

    if !body.contains(PICK_GRP_ID_FIELD) {
        return None;
    }

    let parsed = api_common::parse_json_from_body(body, "LogBusinessEvents (draft pick)")?;

    // Find the source object containing PickGrpId.
    let source = find_pick_source(&parsed)?;

    let pick_grp_id = source
        .get("PickGrpId")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0);

    let pack_cards = extract_business_event_pack_cards(source);

    let event_name = source
        .get("EventName")
        .or_else(|| source.get("InternalEventName"))
        .or_else(|| parsed.get("EventName"))
        .or_else(|| parsed.get("InternalEventName"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");

    let pack_idx = source
        .get("PackNumber")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0);

    let selection_idx = source
        .get("PickNumber")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0);

    Some(serde_json::json!({
        "type": "draft_human_pick_confirm",
        "pick_grp_id": pick_grp_id,
        "pack_cards": pack_cards,
        "event_name": event_name,
        "pack_number": pack_idx,
        "pick_number": selection_idx,
        "raw_business_event": parsed,
    }))
}

/// Finds the source object containing `PickGrpId` within a parsed JSON value.
///
/// Searches the top level, inside a `Params` object, and inside a
/// top-level array of business events.
fn find_pick_source(parsed: &serde_json::Value) -> Option<&serde_json::Value> {
    // Top level.
    if parsed.get(PICK_GRP_ID_FIELD).is_some() {
        return Some(parsed);
    }

    // Inside a `Params` object.
    if let Some(params) = parsed.get("Params") {
        if params.get(PICK_GRP_ID_FIELD).is_some() {
            return Some(params);
        }
    }

    // Inside a top-level array of business events.
    if let Some(arr) = parsed.as_array() {
        return arr
            .iter()
            .find(|item| item.get(PICK_GRP_ID_FIELD).is_some());
    }

    None
}

/// Extracts pack card IDs from a `PackCards` field in a business event.
///
/// `PackCards` may be a comma-separated string of GRP IDs (e.g., `"12345,67890"`)
/// or an array of integers. This function normalizes to `Vec<i64>`.
fn extract_business_event_pack_cards(source: &serde_json::Value) -> Vec<i64> {
    if let Some(pack_cards) = source.get("PackCards") {
        // Try as comma-separated string first.
        if let Some(s) = pack_cards.as_str() {
            return parse_comma_separated_ids(s);
        }

        // Try as array.
        if let Some(arr) = pack_cards.as_array() {
            return arr
                .iter()
                .filter_map(|v| {
                    v.as_i64()
                        .or_else(|| v.as_str().and_then(|s| s.parse::<i64>().ok()))
                })
                .collect();
        }
    }

    Vec::new()
}

/// Extracts pack card IDs from a `PackCards` field in a `Draft.Notify` payload.
///
/// In `Draft.Notify`, `PackCards` is typically a comma-separated string of
/// GRP IDs (e.g., `"12345,67890,11111"`). This function also handles
/// array format for robustness.
fn extract_pack_cards_from_string(parsed: &serde_json::Value) -> Vec<i64> {
    if let Some(pack_cards) = parsed.get("PackCards") {
        // Comma-separated string (most common format in Draft.Notify).
        if let Some(s) = pack_cards.as_str() {
            return parse_comma_separated_ids(s);
        }

        // Array format (less common but possible).
        if let Some(arr) = pack_cards.as_array() {
            return arr
                .iter()
                .filter_map(|v| {
                    v.as_i64()
                        .or_else(|| v.as_str().and_then(|s| s.parse::<i64>().ok()))
                })
                .collect();
        }
    }

    Vec::new()
}

/// Parses a comma-separated string of integer IDs into a `Vec<i64>`.
///
/// Silently skips any non-numeric segments.
fn parse_comma_separated_ids(s: &str) -> Vec<i64> {
    s.split(',')
        .filter_map(|segment| segment.trim().parse::<i64>().ok())
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
        draft_human_payload, test_timestamp, unity_entry, EntryHeader,
    };

    // -- Draft.Notify parsing ------------------------------------------------

    mod draft_notify {
        use super::*;

        #[test]
        fn test_try_parse_draft_notify_basic() {
            let body = "[UnityCrossThreadLogger]Draft.Notify\n\
                         {\n\
                           \"draftId\": \"abc-123-def\",\n\
                           \"SelfPack\": 0,\n\
                           \"SelfPick\": 0,\n\
                           \"PackCards\": \"12345,67890,11111\"\n\
                         }";
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = draft_human_payload(event);

            assert_eq!(payload["type"], "draft_human_notify");
            assert_eq!(payload["draft_id"], "abc-123-def");
            assert_eq!(payload["pack_number"], 0);
            assert_eq!(payload["pick_number"], 0);
            assert_eq!(
                payload["pack_cards"],
                serde_json::json!([12345, 67890, 11111])
            );
        }

        #[test]
        fn test_try_parse_draft_notify_second_pack() {
            let body = "[UnityCrossThreadLogger]Draft.Notify\n\
                         {\n\
                           \"draftId\": \"draft-456\",\n\
                           \"SelfPack\": 1,\n\
                           \"SelfPick\": 5,\n\
                           \"PackCards\": \"22222,33333,44444,55555\"\n\
                         }";
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = draft_human_payload(event);

            assert_eq!(payload["pack_number"], 1);
            assert_eq!(payload["pick_number"], 5);
            assert_eq!(payload["draft_id"], "draft-456");
            assert_eq!(
                payload["pack_cards"],
                serde_json::json!([22222, 33333, 44444, 55555])
            );
        }

        #[test]
        fn test_try_parse_draft_notify_last_pick_single_card() {
            let body = "[UnityCrossThreadLogger]Draft.Notify\n\
                         {\n\
                           \"draftId\": \"draft-789\",\n\
                           \"SelfPack\": 2,\n\
                           \"SelfPick\": 13,\n\
                           \"PackCards\": \"99999\"\n\
                         }";
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = draft_human_payload(event);

            assert_eq!(payload["pack_number"], 2);
            assert_eq!(payload["pick_number"], 13);
            assert_eq!(payload["pack_cards"], serde_json::json!([99999]));
        }

        #[test]
        fn test_try_parse_draft_notify_empty_pack_cards() {
            let body = "[UnityCrossThreadLogger]Draft.Notify\n\
                         {\n\
                           \"draftId\": \"draft-empty\",\n\
                           \"SelfPack\": 0,\n\
                           \"SelfPick\": 0,\n\
                           \"PackCards\": \"\"\n\
                         }";
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = draft_human_payload(event);

            assert_eq!(payload["pack_cards"], serde_json::json!([]));
        }

        #[test]
        fn test_try_parse_draft_notify_array_format_pack_cards() {
            let body = "[UnityCrossThreadLogger]Draft.Notify\n\
                         {\n\
                           \"draftId\": \"draft-arr\",\n\
                           \"SelfPack\": 0,\n\
                           \"SelfPick\": 0,\n\
                           \"PackCards\": [12345, 67890]\n\
                         }";
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = draft_human_payload(event);

            assert_eq!(payload["pack_cards"], serde_json::json!([12345, 67890]));
        }

        #[test]
        fn test_try_parse_draft_notify_missing_draft_id() {
            let body = "[UnityCrossThreadLogger]Draft.Notify\n\
                         {\n\
                           \"SelfPack\": 0,\n\
                           \"SelfPick\": 0,\n\
                           \"PackCards\": \"12345\"\n\
                         }";
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = draft_human_payload(event);

            assert_eq!(payload["draft_id"], "");
        }

        #[test]
        fn test_try_parse_draft_notify_preserves_raw_payload() {
            let body = "[UnityCrossThreadLogger]Draft.Notify\n\
                         {\n\
                           \"draftId\": \"draft-raw\",\n\
                           \"SelfPack\": 0,\n\
                           \"SelfPick\": 0,\n\
                           \"PackCards\": \"12345\",\n\
                           \"ExtraField\": \"preserved\"\n\
                         }";
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = draft_human_payload(event);

            assert_eq!(payload["raw_draft_notify"]["ExtraField"], "preserved");
        }

        #[test]
        fn test_try_parse_draft_notify_with_timestamp_in_header() {
            let body = "[UnityCrossThreadLogger]2/25/2026 12:00:00 PM \
                         Draft.Notify\n\
                         {\n\
                           \"draftId\": \"draft-ts\",\n\
                           \"SelfPack\": 0,\n\
                           \"SelfPick\": 0,\n\
                           \"PackCards\": \"12345\"\n\
                         }";
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = draft_human_payload(event);

            assert_eq!(payload["type"], "draft_human_notify");
        }
    }

    // -- EventPlayerDraftMakePick parsing ------------------------------------

    mod make_pick {
        use super::*;

        #[test]
        fn test_try_parse_make_pick_basic() {
            let body = "[UnityCrossThreadLogger]EventPlayerDraftMakePick\n\
                         {\n\
                           \"EventName\": \"PremierDraft_MKM_20260201\",\n\
                           \"PickInfo\": {\n\
                             \"CardId\": 12345,\n\
                             \"PackNumber\": 0,\n\
                             \"PickNumber\": 0\n\
                           }\n\
                         }";
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = draft_human_payload(event);

            assert_eq!(payload["type"], "draft_human_pick");
            assert_eq!(payload["event_name"], "PremierDraft_MKM_20260201");
            assert_eq!(payload["card_id"], 12345);
            assert_eq!(payload["pack_number"], 0);
            assert_eq!(payload["pick_number"], 0);
        }

        #[test]
        fn test_try_parse_make_pick_later_in_draft() {
            let body = "[UnityCrossThreadLogger]EventPlayerDraftMakePick\n\
                         {\n\
                           \"EventName\": \"TradDraft_DSK_20260115\",\n\
                           \"PickInfo\": {\n\
                             \"CardId\": 67890,\n\
                             \"PackNumber\": 1,\n\
                             \"PickNumber\": 7\n\
                           }\n\
                         }";
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = draft_human_payload(event);

            assert_eq!(payload["card_id"], 67890);
            assert_eq!(payload["pack_number"], 1);
            assert_eq!(payload["pick_number"], 7);
            assert_eq!(payload["event_name"], "TradDraft_DSK_20260115");
        }

        #[test]
        fn test_try_parse_make_pick_with_card_ids() {
            let body = "[UnityCrossThreadLogger]EventPlayerDraftMakePick\n\
                         {\n\
                           \"EventName\": \"PremierDraft_MKM_20260201\",\n\
                           \"PickInfo\": {\n\
                             \"CardId\": 11111,\n\
                             \"PackNumber\": 0,\n\
                             \"PickNumber\": 0,\n\
                             \"CardIds\": [11111, 22222, 33333]\n\
                           }\n\
                         }";
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = draft_human_payload(event);

            assert_eq!(payload["card_id"], 11111);
            assert_eq!(
                payload["card_ids"],
                serde_json::json!([11111, 22222, 33333])
            );
        }

        #[test]
        fn test_try_parse_make_pick_flat_format() {
            // Some log versions put fields at the top level instead of
            // nesting under PickInfo.
            let body = "[UnityCrossThreadLogger]EventPlayerDraftMakePick\n\
                         {\n\
                           \"CardId\": 55555,\n\
                           \"PackNumber\": 2,\n\
                           \"PickNumber\": 10\n\
                         }";
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = draft_human_payload(event);

            assert_eq!(payload["card_id"], 55555);
            assert_eq!(payload["pack_number"], 2);
            assert_eq!(payload["pick_number"], 10);
        }

        #[test]
        fn test_try_parse_make_pick_missing_card_id_defaults_to_zero() {
            let body = "[UnityCrossThreadLogger]EventPlayerDraftMakePick\n\
                         {\n\
                           \"PickInfo\": {\n\
                             \"PackNumber\": 0,\n\
                             \"PickNumber\": 0\n\
                           }\n\
                         }";
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = draft_human_payload(event);

            assert_eq!(payload["card_id"], 0);
        }

        #[test]
        fn test_try_parse_make_pick_preserves_raw_payload() {
            let body = "[UnityCrossThreadLogger]EventPlayerDraftMakePick\n\
                         {\n\
                           \"PickInfo\": {\n\
                             \"CardId\": 12345,\n\
                             \"PackNumber\": 0,\n\
                             \"PickNumber\": 0,\n\
                             \"ExtraField\": \"kept\"\n\
                           }\n\
                         }";
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = draft_human_payload(event);

            assert_eq!(payload["raw_make_pick"]["PickInfo"]["ExtraField"], "kept");
        }

        #[test]
        fn test_try_parse_make_pick_with_timestamp_in_header() {
            let body = "[UnityCrossThreadLogger]2/25/2026 12:00:00 PM \
                         EventPlayerDraftMakePick\n\
                         {\n\
                           \"PickInfo\": {\n\
                             \"CardId\": 77777,\n\
                             \"PackNumber\": 0,\n\
                             \"PickNumber\": 1\n\
                           }\n\
                         }";
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = draft_human_payload(event);

            assert_eq!(payload["card_id"], 77777);
        }
    }

    // -- LogBusinessEvents with PickGrpId parsing ----------------------------

    mod pick_business_event {
        use super::*;

        #[test]
        fn test_try_parse_pick_business_event_basic() {
            let body = "[UnityCrossThreadLogger]LogBusinessEvents\n\
                         {\n\
                           \"PickGrpId\": 12345,\n\
                           \"PackCards\": \"12345,67890,11111\",\n\
                           \"EventName\": \"PremierDraft_MKM_20260201\"\n\
                         }";
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = draft_human_payload(event);

            assert_eq!(payload["type"], "draft_human_pick_confirm");
            assert_eq!(payload["pick_grp_id"], 12345);
            assert_eq!(
                payload["pack_cards"],
                serde_json::json!([12345, 67890, 11111])
            );
            assert_eq!(payload["event_name"], "PremierDraft_MKM_20260201");
        }

        #[test]
        fn test_try_parse_pick_business_event_with_pack_pick_numbers() {
            let body = "[UnityCrossThreadLogger]LogBusinessEvents\n\
                         {\n\
                           \"PickGrpId\": 67890,\n\
                           \"PackNumber\": 1,\n\
                           \"PickNumber\": 5,\n\
                           \"PackCards\": \"67890,22222\"\n\
                         }";
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = draft_human_payload(event);

            assert_eq!(payload["pick_grp_id"], 67890);
            assert_eq!(payload["pack_number"], 1);
            assert_eq!(payload["pick_number"], 5);
        }

        #[test]
        fn test_try_parse_pick_business_event_params_wrapper() {
            let body = "[UnityCrossThreadLogger]LogBusinessEvents\n\
                         {\n\
                           \"Params\": {\n\
                             \"PickGrpId\": 33333,\n\
                             \"PackCards\": \"33333,44444,55555\"\n\
                           }\n\
                         }";
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = draft_human_payload(event);

            assert_eq!(payload["pick_grp_id"], 33333);
            assert_eq!(
                payload["pack_cards"],
                serde_json::json!([33333, 44444, 55555])
            );
        }

        #[test]
        fn test_try_parse_pick_business_event_array_format() {
            let body = "[UnityCrossThreadLogger]LogBusinessEvents\n\
                         [\n\
                           {\"SomeOtherField\": \"value\"},\n\
                           {\n\
                             \"PickGrpId\": 44444,\n\
                             \"PackCards\": \"44444,55555\"\n\
                           }\n\
                         ]";
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = draft_human_payload(event);

            assert_eq!(payload["pick_grp_id"], 44444);
        }

        #[test]
        fn test_try_parse_pick_business_event_array_pack_cards() {
            let body = "[UnityCrossThreadLogger]LogBusinessEvents\n\
                         {\n\
                           \"PickGrpId\": 12345,\n\
                           \"PackCards\": [12345, 67890, 11111]\n\
                         }";
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = draft_human_payload(event);

            assert_eq!(
                payload["pack_cards"],
                serde_json::json!([12345, 67890, 11111])
            );
        }

        #[test]
        fn test_try_parse_pick_business_event_preserves_raw_payload() {
            let body = "[UnityCrossThreadLogger]LogBusinessEvents\n\
                         {\n\
                           \"PickGrpId\": 12345,\n\
                           \"ExtraField\": \"preserved\"\n\
                         }";
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = draft_human_payload(event);

            assert_eq!(payload["raw_business_event"]["ExtraField"], "preserved");
        }

        #[test]
        fn test_try_parse_pick_business_event_with_timestamp_in_header() {
            let body = "[UnityCrossThreadLogger]2/25/2026 12:00:00 PM \
                         LogBusinessEvents\n\
                         {\n\
                           \"PickGrpId\": 88888,\n\
                           \"PackCards\": \"88888\"\n\
                         }";
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = draft_human_payload(event);

            assert_eq!(payload["pick_grp_id"], 88888);
        }
    }

    // -- Metadata preservation -----------------------------------------------

    mod metadata {
        use super::*;

        #[test]
        fn test_try_parse_preserves_raw_bytes_notify() {
            let body = "[UnityCrossThreadLogger]Draft.Notify\n\
                         {\"SelfPack\": 0, \"SelfPick\": 0, \
                          \"PackCards\": \"12345\"}";
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            assert_eq!(event.metadata().raw_bytes(), body.as_bytes());
        }

        #[test]
        fn test_try_parse_preserves_raw_bytes_make_pick() {
            let body = "[UnityCrossThreadLogger]EventPlayerDraftMakePick\n\
                         {\"PickInfo\": {\"CardId\": 1, \"PackNumber\": 0, \
                          \"PickNumber\": 0}}";
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            assert_eq!(event.metadata().raw_bytes(), body.as_bytes());
        }

        #[test]
        fn test_try_parse_preserves_raw_bytes_business_event() {
            let body = "[UnityCrossThreadLogger]LogBusinessEvents\n\
                         {\"PickGrpId\": 12345}";
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            assert_eq!(event.metadata().raw_bytes(), body.as_bytes());
        }

        #[test]
        fn test_try_parse_stores_timestamp_notify() {
            let body = "[UnityCrossThreadLogger]Draft.Notify\n\
                         {\"SelfPack\": 0, \"SelfPick\": 0, \
                          \"PackCards\": \"12345\"}";
            let entry = unity_entry(body);
            let ts = Some(test_timestamp());
            let result = try_parse(&entry, ts);

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            assert_eq!(event.metadata().timestamp(), ts);
        }

        #[test]
        fn test_try_parse_stores_timestamp_make_pick() {
            let body = "[UnityCrossThreadLogger]EventPlayerDraftMakePick\n\
                         {\"PickInfo\": {\"CardId\": 1, \"PackNumber\": 0, \
                          \"PickNumber\": 0}}";
            let entry = unity_entry(body);
            let ts = Some(test_timestamp());
            let result = try_parse(&entry, ts);

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            assert_eq!(event.metadata().timestamp(), ts);
        }

        #[test]
        fn test_try_parse_stores_timestamp_business_event() {
            let body = "[UnityCrossThreadLogger]LogBusinessEvents\n\
                         {\"PickGrpId\": 12345}";
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
        fn test_try_parse_bot_draft_entry_returns_none() {
            // Bot draft entries should not be parsed by the human draft parser.
            let body = "[UnityCrossThreadLogger]BotDraft_DraftPick\n\
                         {\n\
                           \"PickInfo\": {\n\
                             \"CardId\": 12345,\n\
                             \"PackNumber\": 0,\n\
                             \"PickNumber\": 0\n\
                           }\n\
                         }";
            let entry = unity_entry(body);
            assert!(try_parse(&entry, Some(test_timestamp())).is_none());
        }

        #[test]
        fn test_try_parse_game_result_business_event_returns_none() {
            // Game result LogBusinessEvents (with WinningType) should not
            // match the human draft parser.
            let body = "[UnityCrossThreadLogger]LogBusinessEvents\n\
                         {\n\
                           \"WinningType\": \"WinLoss\",\n\
                           \"WinningTeamId\": 1\n\
                         }";
            let entry = unity_entry(body);
            assert!(try_parse(&entry, Some(test_timestamp())).is_none());
        }

        #[test]
        fn test_try_parse_malformed_json_notify_returns_none() {
            let body = "[UnityCrossThreadLogger]Draft.Notify\n\
                         {\"PackCards\": broken!!!}";
            let entry = unity_entry(body);
            assert!(try_parse(&entry, Some(test_timestamp())).is_none());
        }

        #[test]
        fn test_try_parse_malformed_json_make_pick_returns_none() {
            let body = "[UnityCrossThreadLogger]EventPlayerDraftMakePick\n\
                         {not valid json}";
            let entry = unity_entry(body);
            assert!(try_parse(&entry, Some(test_timestamp())).is_none());
        }

        #[test]
        fn test_try_parse_malformed_json_business_event_returns_none() {
            let body = "[UnityCrossThreadLogger]LogBusinessEvents\n\
                         {\"PickGrpId\": broken!!!}";
            let entry = unity_entry(body);
            assert!(try_parse(&entry, Some(test_timestamp())).is_none());
        }

        #[test]
        fn test_try_parse_marker_only_no_json_notify_returns_none() {
            let body = "[UnityCrossThreadLogger]Draft.Notify";
            let entry = unity_entry(body);
            assert!(try_parse(&entry, Some(test_timestamp())).is_none());
        }

        #[test]
        fn test_try_parse_marker_only_no_json_make_pick_returns_none() {
            let body = "[UnityCrossThreadLogger]EventPlayerDraftMakePick";
            let entry = unity_entry(body);
            assert!(try_parse(&entry, Some(test_timestamp())).is_none());
        }

        #[test]
        fn test_try_parse_draft_notify_no_pack_or_self_pack_returns_none() {
            // Draft.Notify marker in text but JSON has no characteristic fields.
            let body = "[UnityCrossThreadLogger]Draft.Notify\n\
                         {\"unrelatedField\": \"value\"}";
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
        fn test_draft_human_notify_is_durable_per_event() {
            let body = "[UnityCrossThreadLogger]Draft.Notify\n\
                         {\"SelfPack\": 0, \"SelfPick\": 0, \
                          \"PackCards\": \"12345\"}";
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            assert_eq!(event.performance_class(), PerformanceClass::DurablePerEvent);
        }

        #[test]
        fn test_draft_human_pick_is_durable_per_event() {
            let body = "[UnityCrossThreadLogger]EventPlayerDraftMakePick\n\
                         {\"PickInfo\": {\"CardId\": 1, \"PackNumber\": 0, \
                          \"PickNumber\": 0}}";
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            assert_eq!(event.performance_class(), PerformanceClass::DurablePerEvent);
        }

        #[test]
        fn test_draft_human_business_event_is_durable_per_event() {
            let body = "[UnityCrossThreadLogger]LogBusinessEvents\n\
                         {\"PickGrpId\": 12345}";
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
        fn test_parse_comma_separated_ids_basic() {
            assert_eq!(
                parse_comma_separated_ids("12345,67890,11111"),
                vec![12345, 67890, 11111]
            );
        }

        #[test]
        fn test_parse_comma_separated_ids_with_spaces() {
            assert_eq!(
                parse_comma_separated_ids("12345, 67890, 11111"),
                vec![12345, 67890, 11111]
            );
        }

        #[test]
        fn test_parse_comma_separated_ids_single() {
            assert_eq!(parse_comma_separated_ids("12345"), vec![12345]);
        }

        #[test]
        fn test_parse_comma_separated_ids_empty() {
            let result: Vec<i64> = parse_comma_separated_ids("");
            assert!(result.is_empty());
        }

        #[test]
        fn test_parse_comma_separated_ids_with_invalid() {
            assert_eq!(
                parse_comma_separated_ids("12345,abc,67890"),
                vec![12345, 67890]
            );
        }

        #[test]
        fn test_find_pick_source_top_level() {
            let val = serde_json::json!({"PickGrpId": 12345});
            assert!(find_pick_source(&val).is_some());
        }

        #[test]
        fn test_find_pick_source_in_params() {
            let val = serde_json::json!({"Params": {"PickGrpId": 12345}});
            assert!(find_pick_source(&val).is_some());
        }

        #[test]
        fn test_find_pick_source_in_array() {
            let val = serde_json::json!([
                {"SomeField": 1},
                {"PickGrpId": 12345}
            ]);
            assert!(find_pick_source(&val).is_some());
        }

        #[test]
        fn test_find_pick_source_absent() {
            let val = serde_json::json!({"WinningType": "WinLoss"});
            assert!(find_pick_source(&val).is_none());
        }
    }
}
