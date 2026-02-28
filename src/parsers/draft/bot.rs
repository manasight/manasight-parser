//! Bot draft parser for Quick Draft (`DraftStatus: "PickNext"` and
//! `BotDraft_DraftPick`) events.
//!
//! In Quick Draft (bot draft), the player drafts against AI opponents.
//! Two log signatures capture the draft flow:
//!
//! | Signature | Meaning | Key Fields |
//! |-----------|---------|------------|
//! | `DraftStatus: "PickNext"` | Pack presented to the player | `EventName`, `PackNumber`, `PickNumber`, `DraftPack` |
//! | `BotDraft_DraftPick` | Card selected by the player | `PickInfo` with `CardId`, `PackNumber`, `PickNumber` |
//!
//! Quick Draft has 3 packs of 14 picks each (42 total picks). Pack and
//! pick numbers are zero-indexed in the log.
//!
//! Both events are Class 2 (Durable Per-Event) -- each pick is
//! independently valuable and must survive crashes.

use crate::events::{DraftBotEvent, EventMetadata, GameEvent};
use crate::log::entry::LogEntry;
use crate::parsers::api_common;

/// Marker for bot draft pack presentation events.
///
/// When a new pack is presented to the player during Quick Draft, the log
/// contains a JSON payload with `"DraftStatus": "PickNext"` alongside the
/// pack contents in `DraftPack`.
const DRAFT_STATUS_MARKER: &str = "DraftStatus";

/// The status value that indicates a pack is ready to pick from.
const PICK_NEXT_STATUS: &str = "PickNext";

/// Marker for bot draft pick confirmation events.
///
/// After the player selects a card, the log emits a `BotDraft_DraftPick`
/// entry containing `PickInfo` with the selected card and remaining options.
const BOT_DRAFT_PICK_MARKER: &str = "BotDraft_DraftPick";

/// Attempts to parse a [`LogEntry`] as a bot draft event.
///
/// Returns `Some(GameEvent::DraftBot(_))` if the entry matches either:
/// - A `DraftStatus: "PickNext"` pack presentation, or
/// - A `BotDraft_DraftPick` pick confirmation.
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

    // Try bot draft pack presentation first (more common during drafting).
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

/// Attempts to parse a `DraftStatus: "PickNext"` pack presentation.
///
/// The log entry body contains a JSON object with:
/// - `DraftStatus`: must be `"PickNext"`
/// - `DraftPack`: array of card GRP IDs available in the pack
/// - `PackNumber`: zero-indexed pack number (0, 1, 2)
/// - `PickNumber`: zero-indexed pick number within the pack
/// - `EventName`: the Arena event identifier (e.g., `"QuickDraft_MKM_20260201"`)
fn try_parse_pack_presentation(body: &str) -> Option<serde_json::Value> {
    // Quick bail: both markers must be present in the body text.
    if !body.contains(DRAFT_STATUS_MARKER) || !body.contains(PICK_NEXT_STATUS) {
        return None;
    }

    let parsed = api_common::parse_json_from_body(body, "DraftStatus PickNext")?;

    // Verify this is actually a PickNext status (not just text mentioning it).
    let status = parsed.get(DRAFT_STATUS_MARKER).and_then(|v| v.as_str())?;
    if status != PICK_NEXT_STATUS {
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

    let event_name = parsed
        .get("EventName")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");

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

/// Attempts to parse a `BotDraft_DraftPick` pick confirmation.
///
/// The log entry body contains a JSON object (or the marker label followed
/// by JSON) with `PickInfo` containing:
/// - `CardId`: the GRP ID of the selected card
/// - `PackNumber`: zero-indexed pack number
/// - `PickNumber`: zero-indexed pick number within the pack
fn try_parse_draft_pick(body: &str) -> Option<serde_json::Value> {
    if !body.contains(BOT_DRAFT_PICK_MARKER) {
        return None;
    }

    let parsed = api_common::parse_json_from_body(body, "BotDraft_DraftPick")?;

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

    let event_name = parsed
        .get("EventName")
        .or_else(|| pick_info.get("EventName"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");

    Some(serde_json::json!({
        "type": "draft_bot_pick",
        "event_name": event_name,
        "card_id": card_id,
        "pack_number": pack_idx,
        "pick_number": selection_idx,
        "card_ids": card_ids,
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

    // -- Pack presentation parsing (DraftStatus: "PickNext") ------------------

    mod pack_presentation {
        use super::*;

        #[test]
        fn test_try_parse_pack_presentation_basic() {
            let body = "[UnityCrossThreadLogger]BotDraft_DraftStatus\n\
                         {\n\
                           \"DraftStatus\": \"PickNext\",\n\
                           \"PackNumber\": 0,\n\
                           \"PickNumber\": 0,\n\
                           \"DraftPack\": [\"12345\", \"67890\", \"11111\"],\n\
                           \"EventName\": \"QuickDraft_MKM_20260201\"\n\
                         }";
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
            let body = "[UnityCrossThreadLogger]BotDraft_DraftStatus\n\
                         {\n\
                           \"DraftStatus\": \"PickNext\",\n\
                           \"PackNumber\": 1,\n\
                           \"PickNumber\": 3,\n\
                           \"DraftPack\": [\"22222\", \"33333\"],\n\
                           \"EventName\": \"QuickDraft_DSK_20260115\"\n\
                         }";
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = draft_bot_payload(event);

            assert_eq!(payload["pack_number"], 1);
            assert_eq!(payload["pick_number"], 3);
            assert_eq!(payload["event_name"], "QuickDraft_DSK_20260115");
        }

        #[test]
        fn test_try_parse_pack_presentation_third_pack_last_pick() {
            let body = "[UnityCrossThreadLogger]BotDraft_DraftStatus\n\
                         {\n\
                           \"DraftStatus\": \"PickNext\",\n\
                           \"PackNumber\": 2,\n\
                           \"PickNumber\": 13,\n\
                           \"DraftPack\": [\"44444\"],\n\
                           \"EventName\": \"QuickDraft_MKM_20260201\"\n\
                         }";
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
            let body = "[UnityCrossThreadLogger]BotDraft_DraftStatus\n\
                         {\n\
                           \"DraftStatus\": \"PickNext\",\n\
                           \"PackNumber\": 0,\n\
                           \"PickNumber\": 0,\n\
                           \"DraftPack\": [12345, 67890]\n\
                         }";
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = draft_bot_payload(event);

            assert_eq!(payload["draft_pack"], serde_json::json!([12345, 67890]));
        }

        #[test]
        fn test_try_parse_pack_presentation_empty_pack() {
            let body = "[UnityCrossThreadLogger]BotDraft_DraftStatus\n\
                         {\n\
                           \"DraftStatus\": \"PickNext\",\n\
                           \"PackNumber\": 0,\n\
                           \"PickNumber\": 0,\n\
                           \"DraftPack\": []\n\
                         }";
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = draft_bot_payload(event);

            assert_eq!(payload["draft_pack"], serde_json::json!([]));
        }

        #[test]
        fn test_try_parse_pack_presentation_missing_draft_pack() {
            let body = "[UnityCrossThreadLogger]BotDraft_DraftStatus\n\
                         {\n\
                           \"DraftStatus\": \"PickNext\",\n\
                           \"PackNumber\": 0,\n\
                           \"PickNumber\": 0\n\
                         }";
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = draft_bot_payload(event);

            assert_eq!(payload["draft_pack"], serde_json::json!([]));
        }

        #[test]
        fn test_try_parse_pack_presentation_preserves_raw_payload() {
            let body = "[UnityCrossThreadLogger]BotDraft_DraftStatus\n\
                         {\n\
                           \"DraftStatus\": \"PickNext\",\n\
                           \"PackNumber\": 0,\n\
                           \"PickNumber\": 0,\n\
                           \"ExtraField\": \"preserved\"\n\
                         }";
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = draft_bot_payload(event);

            assert_eq!(payload["raw_draft_status"]["ExtraField"], "preserved");
        }

        #[test]
        fn test_try_parse_pack_presentation_with_timestamp_in_header() {
            let body = "[UnityCrossThreadLogger]2/25/2026 12:00:00 PM \
                         BotDraft_DraftStatus\n\
                         {\n\
                           \"DraftStatus\": \"PickNext\",\n\
                           \"PackNumber\": 0,\n\
                           \"PickNumber\": 0,\n\
                           \"DraftPack\": [\"99999\"]\n\
                         }";
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = draft_bot_payload(event);

            assert_eq!(payload["type"], "draft_bot_pack");
            assert_eq!(payload["draft_pack"], serde_json::json!([99999]));
        }

        #[test]
        fn test_try_parse_pack_presentation_wrong_status_returns_none() {
            let body = "[UnityCrossThreadLogger]BotDraft_DraftStatus\n\
                         {\n\
                           \"DraftStatus\": \"DraftComplete\",\n\
                           \"PackNumber\": 0,\n\
                           \"PickNumber\": 0\n\
                         }";
            let entry = unity_entry(body);
            assert!(try_parse(&entry, Some(test_timestamp())).is_none());
        }
    }

    // -- Draft pick parsing (BotDraft_DraftPick) ------------------------------

    mod draft_pick {
        use super::*;

        #[test]
        fn test_try_parse_draft_pick_basic() {
            let body = "[UnityCrossThreadLogger]BotDraft_DraftPick\n\
                         {\n\
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
            let payload = draft_bot_payload(event);

            assert_eq!(payload["type"], "draft_bot_pick");
            assert_eq!(payload["card_id"], 12345);
            assert_eq!(payload["pack_number"], 0);
            assert_eq!(payload["pick_number"], 0);
        }

        #[test]
        fn test_try_parse_draft_pick_later_in_draft() {
            let body = "[UnityCrossThreadLogger]BotDraft_DraftPick\n\
                         {\n\
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
            let payload = draft_bot_payload(event);

            assert_eq!(payload["card_id"], 67890);
            assert_eq!(payload["pack_number"], 1);
            assert_eq!(payload["pick_number"], 7);
        }

        #[test]
        fn test_try_parse_draft_pick_with_card_ids() {
            let body = "[UnityCrossThreadLogger]BotDraft_DraftPick\n\
                         {\n\
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
            let payload = draft_bot_payload(event);

            assert_eq!(payload["card_id"], 11111);
            assert_eq!(
                payload["card_ids"],
                serde_json::json!([11111, 22222, 33333])
            );
        }

        #[test]
        fn test_try_parse_draft_pick_flat_format() {
            // Some log versions put CardId at the top level instead of
            // nesting under PickInfo.
            let body = "[UnityCrossThreadLogger]BotDraft_DraftPick\n\
                         {\n\
                           \"CardId\": 55555,\n\
                           \"PackNumber\": 2,\n\
                           \"PickNumber\": 10\n\
                         }";
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = draft_bot_payload(event);

            assert_eq!(payload["card_id"], 55555);
            assert_eq!(payload["pack_number"], 2);
            assert_eq!(payload["pick_number"], 10);
        }

        #[test]
        fn test_try_parse_draft_pick_with_event_name() {
            let body = "[UnityCrossThreadLogger]BotDraft_DraftPick\n\
                         {\n\
                           \"EventName\": \"QuickDraft_MKM_20260201\",\n\
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
            let payload = draft_bot_payload(event);

            assert_eq!(payload["event_name"], "QuickDraft_MKM_20260201");
        }

        #[test]
        fn test_try_parse_draft_pick_missing_card_id_defaults_to_zero() {
            let body = "[UnityCrossThreadLogger]BotDraft_DraftPick\n\
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
            let payload = draft_bot_payload(event);

            assert_eq!(payload["card_id"], 0);
        }

        #[test]
        fn test_try_parse_draft_pick_preserves_raw_payload() {
            let body = "[UnityCrossThreadLogger]BotDraft_DraftPick\n\
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
            let payload = draft_bot_payload(event);

            assert_eq!(payload["raw_pick_info"]["PickInfo"]["ExtraField"], "kept");
        }

        #[test]
        fn test_try_parse_draft_pick_with_timestamp_in_header() {
            let body = "[UnityCrossThreadLogger]2/25/2026 12:00:00 PM \
                         BotDraft_DraftPick\n\
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
            let payload = draft_bot_payload(event);

            assert_eq!(payload["card_id"], 77777);
        }
    }

    // -- Metadata preservation -----------------------------------------------

    mod metadata {
        use super::*;

        #[test]
        fn test_try_parse_preserves_raw_bytes_pack() {
            let body = "[UnityCrossThreadLogger]BotDraft_DraftStatus\n\
                         {\"DraftStatus\": \"PickNext\", \"PackNumber\": 0, \
                          \"PickNumber\": 0, \"DraftPack\": []}";
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            assert_eq!(event.metadata().raw_bytes(), body.as_bytes());
        }

        #[test]
        fn test_try_parse_preserves_raw_bytes_pick() {
            let body = "[UnityCrossThreadLogger]BotDraft_DraftPick\n\
                         {\"PickInfo\": {\"CardId\": 1, \"PackNumber\": 0, \
                          \"PickNumber\": 0}}";
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            assert_eq!(event.metadata().raw_bytes(), body.as_bytes());
        }

        #[test]
        fn test_try_parse_stores_timestamp_pack() {
            let body = "[UnityCrossThreadLogger]BotDraft_DraftStatus\n\
                         {\"DraftStatus\": \"PickNext\", \"PackNumber\": 0, \
                          \"PickNumber\": 0}";
            let entry = unity_entry(body);
            let ts = Some(test_timestamp());
            let result = try_parse(&entry, ts);

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            assert_eq!(event.metadata().timestamp(), ts);
        }

        #[test]
        fn test_try_parse_stores_timestamp_pick() {
            let body = "[UnityCrossThreadLogger]BotDraft_DraftPick\n\
                         {\"PickInfo\": {\"CardId\": 1, \"PackNumber\": 0, \
                          \"PickNumber\": 0}}";
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
            let body = "[UnityCrossThreadLogger]BotDraft_DraftStatus\n\
                         {\"DraftStatus\": \"DraftComplete\"}";
            let entry = unity_entry(body);
            assert!(try_parse(&entry, Some(test_timestamp())).is_none());
        }

        #[test]
        fn test_try_parse_malformed_json_returns_none() {
            let body = "[UnityCrossThreadLogger]BotDraft_DraftPick\n\
                         {\"PickInfo\": broken!!!}";
            let entry = unity_entry(body);
            assert!(try_parse(&entry, Some(test_timestamp())).is_none());
        }

        #[test]
        fn test_try_parse_marker_only_no_json_returns_none() {
            let body = "[UnityCrossThreadLogger]BotDraft_DraftPick";
            let entry = unity_entry(body);
            assert!(try_parse(&entry, Some(test_timestamp())).is_none());
        }

        #[test]
        fn test_try_parse_draft_status_marker_in_text_only_returns_none() {
            // The text mentions DraftStatus but no valid JSON payload.
            let body = "[UnityCrossThreadLogger]DraftStatus is PickNext (note)\n\
                         not valid json here";
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
            let body = "[UnityCrossThreadLogger]BotDraft_DraftStatus\n\
                         {\"DraftStatus\": \"PickNext\", \"PackNumber\": 0, \
                          \"PickNumber\": 0}";
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            assert_eq!(event.performance_class(), PerformanceClass::DurablePerEvent);
        }

        #[test]
        fn test_draft_bot_pick_event_is_durable_per_event() {
            let body = "[UnityCrossThreadLogger]BotDraft_DraftPick\n\
                         {\"PickInfo\": {\"CardId\": 1, \"PackNumber\": 0, \
                          \"PickNumber\": 0}}";
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
