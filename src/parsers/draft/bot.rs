//! Bot draft parser for Quick Draft (`BotDraftDraftStatus` and
//! `BotDraftDraftPick`) events.
//!
//! In Quick Draft (bot draft), the player drafts against AI opponents.
//! Two log signatures capture the draft flow:
//!
//! | Direction | Signature | Meaning | Key Fields |
//! |-----------|-----------|---------|------------|
//! | Response (`<==`) | `BotDraftDraftStatus` | Initial pack presented (Pack 0, Pick 0) | `Payload` { `EventName`, `DraftStatus`, `PackNumber`, `PickNumber`, `DraftPack` } |
//! | Request (`==>`) | `BotDraftDraftPick` | Card selected | `request` { `EventName`, `PickInfo` { `CardIds`, `PackNumber`, `PickNumber` } } |
//! | Response (`<==`) | `BotDraftDraftPick` | Card selected and next pack | `Payload` { `EventName`, `DraftStatus`, `PackNumber`, `PickNumber`, `DraftPack`, `PickedCards` } |
//!
//! Legacy log signatures:
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

/// Marker for bot draft status events.
///
/// This marker is used for the initial pack presentation (Pack 0, Pick 0).
const BOT_DRAFT_STATUS_MARKER: &str = "BotDraftDraftStatus";
/// Legacy marker.
const LEGACY_DRAFT_STATUS_MARKER: &str = "DraftStatus";

/// The status value that indicates a pack is ready to pick from.
const PICK_NEXT_STATUS: &str = "PickNext";

/// Marker for bot draft pick events.
///
/// This marker is used both for the player's pick (request) and for presenting
/// subsequent packs (response).
const BOT_DRAFT_PICK_MARKER: &str = "BotDraftDraftPick";
// Legacy marker.
const LEGACY_DRAFT_PICK_MARKER: &str = "BotDraft_DraftPick";

/// Attempts to parse a [`LogEntry`] as a bot draft event.
///
/// Returns `Some(GameEvent::DraftBot(_))` if the entry matches either:
/// - A pack presentation (`BotDraftDraftStatus` or `BotDraftDraftPick` response), or
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

/// Attempts to parse a `BotDraftDraftStatus` or `BotDraftDraftPick` pack presentation.
/// Retains legacy `DraftStatus` and `BotDraft_DraftPick` markers for backward compatibility.
///
/// Returns `Some(serde_json::Value)` if parsing succeeds, otherwise `None`.
///
/// The log entry body contains a JSON object with:
/// - `DraftStatus`: must be `"PickNext"`
/// - `DraftPack`: array of card GRP IDs available in the pack
/// - `PackNumber`: zero-indexed pack number (0, 1, 2)
/// - `PickNumber`: zero-indexed pick number within the pack
/// - `EventName`: the Arena event identifier (e.g., `"QuickDraft_MKM_20260201"`)
fn try_parse_pack_presentation(body: &str) -> Option<serde_json::Value> {
    // Try new API response format first.
    let parsed = if api_common::is_api_response(body, BOT_DRAFT_STATUS_MARKER) {
        let top = api_common::parse_json_from_body(body, BOT_DRAFT_STATUS_MARKER)?;
        api_common::parse_nested_json(&top, "Payload", BOT_DRAFT_STATUS_MARKER)
    } else if api_common::is_api_response(body, BOT_DRAFT_PICK_MARKER) {
        let top = api_common::parse_json_from_body(body, BOT_DRAFT_PICK_MARKER)?;
        api_common::parse_nested_json(&top, "Payload", BOT_DRAFT_PICK_MARKER)
    } else {
        // Legacy format: check for old markers.
        if (body.contains(LEGACY_DRAFT_PICK_MARKER) || body.contains(LEGACY_DRAFT_STATUS_MARKER))
            && body.contains(PICK_NEXT_STATUS)
        {
            api_common::parse_json_from_body(body, "DraftStatus PickNext")
        } else {
            None
        }
    }?;

    // Check if this is a pack presentation.
    let status_val = parsed.get(LEGACY_DRAFT_STATUS_MARKER)?;
    if status_val.as_str() != Some(PICK_NEXT_STATUS) {
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
/// Retains legacy `BotDraft_DraftPick` marker for backward compatibility.
///
/// Returns `Some(serde_json::Value)` if parsing succeeds.
///
/// Returns `None` if `BotDraftDraftPick` is an API response, `PickInfo` is missing fields, or if parsing fails.
///
/// The log entry body contains a JSON object (or the marker label followed
/// by JSON) with `PickInfo` containing:
/// - `CardId`: the GRP ID of the selected card
/// - `PackNumber`: zero-indexed pack number
/// - `PickNumber`: zero-indexed pick number within the pack
fn try_parse_draft_pick(body: &str) -> Option<serde_json::Value> {
    // New API responses bundle the next pack in `Payload` and are handled by
    // `try_parse_pack_presentation` above. Do not reinterpret response
    // envelopes as pick confirmations.
    if api_common::is_api_response(body, BOT_DRAFT_PICK_MARKER) {
        return None;
    }

    let mut is_api = false;
    let parsed = if api_common::is_api_request(body, BOT_DRAFT_PICK_MARKER) {
        is_api = true;
        let top = api_common::parse_json_from_body(body, BOT_DRAFT_PICK_MARKER)?;
        api_common::parse_nested_json(&top, "request", BOT_DRAFT_PICK_MARKER).unwrap_or(top)
    } else if body.contains(LEGACY_DRAFT_PICK_MARKER) {
        api_common::parse_json_from_body(body, "BotDraft_DraftPick legacy")?
    } else {
        return None;
    };

    // The pick info may be at top level or nested under a `PickInfo` key.
    let pick_info = parsed.get("PickInfo").unwrap_or(&parsed);

    // Ignore envelopes that do not actually carry pick fields.
    if pick_info.get("CardId").is_none()
        && pick_info.get("PackNumber").is_none()
        && pick_info.get("PickNumber").is_none()
        && pick_info.get("CardIds").is_none()
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

    let card_id;
    let mut card_ids = Vec::new();

    if is_api {
        // Modern API format: CardIds array contains the actual pick.
        // The pack data is not present in the request.
        let selection: Vec<i64> = pick_info
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

        card_id = selection.first().copied().unwrap_or(0);
    } else {
        // Legacy format: CardId is the pick, CardIds array contains the pack data.
        card_id = pick_info
            .get("CardId")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0);

        card_ids = pick_info
            .get("CardIds")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(serde_json::Value::as_i64)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
    }

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

    // -- Pack presentation parsing (DraftStatus: "PickNext", BotDraftDraftStatus, BotDraftDraftPick)

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

        #[test]
        fn test_try_parse_bot_draft_status_api_response_returns_pack() {
            let body = "[UnityCrossThreadLogger]<== BotDraftDraftStatus(uuid)\n\
                         {\n\
                           \"Payload\": \"{\\\"DraftStatus\\\":\\\"PickNext\\\",\\\"PackNumber\\\":0,\\\"PickNumber\\\":0,\\\"DraftPack\\\":[\\\"1\\\",\\\"2\\\"]}\"\n\
                         }";
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = draft_bot_payload(event);

            assert_eq!(payload["type"], "draft_bot_pack");
            assert_eq!(payload["pack_number"], 0);
            assert_eq!(payload["pick_number"], 0);
            assert_eq!(payload["draft_pack"], serde_json::json!([1, 2]));
        }

        #[test]
        fn test_try_parse_bot_draft_pick_api_response_returns_next_pack() {
            let body = "[UnityCrossThreadLogger]<== BotDraftDraftPick(uuid)\n\
                         {\n\
                           \"Payload\": \"{\\\"DraftStatus\\\":\\\"PickNext\\\",\\\"PackNumber\\\":0,\\\"PickNumber\\\":1,\\\"DraftPack\\\":[\\\"3\\\",\\\"4\\\"]}\"\n\
                         }";
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = draft_bot_payload(event);

            assert_eq!(payload["type"], "draft_bot_pack");
            assert_eq!(payload["pack_number"], 0);
            assert_eq!(payload["pick_number"], 1);
            assert_eq!(payload["draft_pack"], serde_json::json!([3, 4]));
        }
    }

    // -- Draft pick parsing (BotDraft_DraftPick, BotDraftDraftPick) -----------

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

        #[test]
        fn test_try_parse_bot_draft_pick_api_request_returns_pick() {
            let body = "[UnityCrossThreadLogger]==> BotDraftDraftPick \
                         {\n\
                           \"id\": \"uuid\",\n\
                           \"request\": \"{\\\"PickInfo\\\":{\\\"CardIds\\\":[\\\"98546\\\"],\\\"PackNumber\\\":0,\\\"PickNumber\\\":0}}\"\n\
                         }";
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = draft_bot_payload(event);

            assert_eq!(payload["type"], "draft_bot_pick");
            assert_eq!(payload["card_id"], 98546);
            // In the modern API format, CardIds in the request is the selection, not pack data.
            // As such, the output card_ids (representing pack data) should be empty.
            assert_eq!(payload["card_ids"], serde_json::json!([]));
        }

        #[test]
        fn test_try_parse_bot_draft_pick_api_request_missing_pick_fields_returns_none() {
            let body = "[UnityCrossThreadLogger]==> BotDraftDraftPick \
                         {\n\
                           \"id\": \"uuid\",\n\
                           \"request\": \"{\\\"PickInfo\\\":{}}\"\n\
                         }";
            let entry = unity_entry(body);
            assert!(try_parse(&entry, Some(test_timestamp())).is_none());
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
        fn test_try_parse_bot_draft_status_api_response_wrong_status_returns_none() {
            let body = "[UnityCrossThreadLogger]<== BotDraftDraftStatus(uuid)\n\
                         {\n\
                           \"Payload\": \"{\\\"DraftStatus\\\":\\\"DraftComplete\\\"}\"\n\
                         }";
            let entry = unity_entry(body);
            assert!(try_parse(&entry, Some(test_timestamp())).is_none());
        }

        #[test]
        fn test_try_parse_bot_draft_pick_api_response_wrong_status_returns_none() {
            let body = "[UnityCrossThreadLogger]<== BotDraftDraftPick(uuid)\n\
                         {\n\
                           \"Payload\": \"{\\\"DraftStatus\\\":\\\"DraftComplete\\\"}\"\n\
                         }";
            let entry = unity_entry(body);
            assert!(try_parse(&entry, Some(test_timestamp())).is_none());
        }

        #[test]
        fn test_try_parse_bot_draft_pick_api_response_missing_payload_returns_none() {
            let body = "[UnityCrossThreadLogger]<== BotDraftDraftPick(uuid)\n\
                         {\n\
                           \"Result\": \"Success\"\n\
                         }";
            let entry = unity_entry(body);
            assert!(try_parse(&entry, Some(test_timestamp())).is_none());
        }

        #[test]
        fn test_try_parse_bot_draft_pick_api_response_malformed_payload_returns_none() {
            let body = "[UnityCrossThreadLogger]<== BotDraftDraftPick(uuid)\n\
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
