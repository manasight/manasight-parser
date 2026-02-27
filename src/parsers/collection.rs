//! Collection parser for `<== StartHook` responses containing `PlayerCards`.
//!
//! Extracts the card collection map from the `PlayerCards` field in
//! `StartHook` API responses. Best-effort: `WotC` removed the structured
//! API in 2021; the parser extracts what the log provides.
//!
//! # Real log format
//!
//! ```text
//! [UnityCrossThreadLogger]2/22/2026 11:59:51 AM
//! <== StartHook(e3f1a2b4-...)
//! {
//!   "InventoryInfo": { ... },
//!   "PlayerCards": { "98535": 4, "12345": 2, ... },
//!   ...
//! }
//! ```
//!
//! The `<==` response line and JSON payload are continuation lines within
//! the `[UnityCrossThreadLogger]` entry.

use crate::events::{CollectionEvent, EventMetadata, GameEvent};
use crate::log::entry::LogEntry;
use crate::parsers::api_common;

/// API method name for the `StartHook` response.
const START_HOOK_METHOD: &str = "StartHook";

/// Field name within the `StartHook` JSON that contains collection data.
const PLAYER_CARDS_FIELD: &str = "PlayerCards";

/// Attempts to parse a [`LogEntry`] as a collection event.
///
/// Returns `Some(GameEvent::Collection(_))` if the entry is a `<== StartHook`
/// response containing a `PlayerCards` field, or `None` otherwise.
///
/// The `timestamp` is used to construct [`EventMetadata`] for the resulting
/// event. Callers are responsible for parsing the timestamp from the log
/// entry header before invoking this function.
pub fn try_parse(entry: &LogEntry, timestamp: chrono::DateTime<chrono::Utc>) -> Option<GameEvent> {
    let body = &entry.body;

    if !api_common::is_api_response(body, START_HOOK_METHOD) {
        return None;
    }

    let json_str = api_common::extract_json_from_body(body)?;

    let parsed: serde_json::Value = match serde_json::from_str(json_str) {
        Ok(v) => v,
        Err(e) => {
            ::log::warn!("StartHook collection: malformed JSON payload: {e}");
            return None;
        }
    };

    // Only claim this entry if the PlayerCards field is present.
    let player_cards = parsed.get(PLAYER_CARDS_FIELD)?;

    let payload = serde_json::json!({
        "type": "collection_snapshot",
        "cards": player_cards.clone(),
        "raw_start_hook": parsed,
    });

    let metadata = EventMetadata::new(timestamp, body.as_bytes().to_vec());
    Some(GameEvent::Collection(CollectionEvent::new(
        metadata, payload,
    )))
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

    /// Helper: extract the JSON payload from a `GameEvent::Collection` variant.
    fn collection_payload(event: &GameEvent) -> &serde_json::Value {
        static EMPTY: std::sync::LazyLock<serde_json::Value> =
            std::sync::LazyLock::new(|| serde_json::json!(null));
        match event {
            GameEvent::Collection(e) => e.payload(),
            _ => &EMPTY,
        }
    }

    // -- Matching entries (StartHook with PlayerCards) -------------------------

    mod matching {
        use super::*;

        #[test]
        fn test_try_parse_start_hook_with_player_cards() {
            let body = "[UnityCrossThreadLogger]2/22/2026 11:59:51 AM\n\
                         <== StartHook(e3f1a2b4-5678-9abc-def0-123456789abc)\n\
                         {\n\
                           \"InventoryInfo\": {\"Gems\": 1234},\n\
                           \"PlayerCards\": {\"98535\": 4, \"12345\": 2}\n\
                         }";
            let entry = unity_entry(body);
            let result = try_parse(&entry, test_timestamp());

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = collection_payload(event);

            assert_eq!(payload["type"], "collection_snapshot");
            assert_eq!(payload["cards"]["98535"], 4);
            assert_eq!(payload["cards"]["12345"], 2);
        }

        #[test]
        fn test_try_parse_large_collection() {
            let body = "[UnityCrossThreadLogger]2/22/2026 12:00:00 PM\n\
                         <== StartHook(large-uuid)\n\
                         {\"PlayerCards\": {\
                           \"10001\": 4, \"10002\": 3, \"10003\": 2, \"10004\": 1\
                         }}";
            let entry = unity_entry(body);
            let result = try_parse(&entry, test_timestamp());

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = collection_payload(event);

            assert_eq!(payload["cards"]["10001"], 4);
            assert_eq!(payload["cards"]["10004"], 1);
        }

        #[test]
        fn test_try_parse_preserves_raw_start_hook() {
            let body = "[UnityCrossThreadLogger]2/22/2026 12:00:00 PM\n\
                         <== StartHook(raw-uuid)\n\
                         {\"PlayerCards\": {\"1\": 1}, \"ExtraField\": true}";
            let entry = unity_entry(body);
            let result = try_parse(&entry, test_timestamp());

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = collection_payload(event);

            assert_eq!(payload["raw_start_hook"]["ExtraField"], true);
        }

        #[test]
        fn test_try_parse_empty_collection() {
            let body = "[UnityCrossThreadLogger]2/22/2026 12:00:00 PM\n\
                         <== StartHook(empty-uuid)\n\
                         {\"PlayerCards\": {}}";
            let entry = unity_entry(body);
            let result = try_parse(&entry, test_timestamp());

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = collection_payload(event);

            assert!(payload["cards"].is_object());
        }
    }

    // -- Metadata preservation ------------------------------------------------

    mod metadata {
        use super::*;

        #[test]
        fn test_try_parse_preserves_raw_bytes() {
            let body = "[UnityCrossThreadLogger]2/22/2026 12:00:00 PM\n\
                         <== StartHook(meta-uuid)\n\
                         {\"PlayerCards\": {\"1\": 1}}";
            let entry = unity_entry(body);
            let result = try_parse(&entry, test_timestamp());

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            assert_eq!(event.metadata().raw_bytes(), body.as_bytes());
        }

        #[test]
        fn test_try_parse_stores_timestamp() {
            let body = "[UnityCrossThreadLogger]2/22/2026 12:00:00 PM\n\
                         <== StartHook(ts-uuid)\n\
                         {\"PlayerCards\": {\"1\": 1}}";
            let entry = unity_entry(body);
            let ts = test_timestamp();
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
        fn test_try_parse_start_hook_without_player_cards_returns_none() {
            let body = "[UnityCrossThreadLogger]2/22/2026 12:00:00 PM\n\
                         <== StartHook(no-cards-uuid)\n\
                         {\"InventoryInfo\": {\"Gems\": 1234}}";
            let entry = unity_entry(body);
            assert!(try_parse(&entry, test_timestamp()).is_none());
        }

        #[test]
        fn test_try_parse_different_api_response_returns_none() {
            let body = "[UnityCrossThreadLogger]2/22/2026 12:00:00 PM\n\
                         <== RankGetCombinedRankInfo(uuid)\n\
                         {\"constructedClass\": \"Gold\"}";
            let entry = unity_entry(body);
            assert!(try_parse(&entry, test_timestamp()).is_none());
        }

        #[test]
        fn test_try_parse_api_request_returns_none() {
            let body = "[UnityCrossThreadLogger]==> StartHook {\"data\": 1}";
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
        fn test_try_parse_old_marker_returns_none() {
            let body = "[UnityCrossThreadLogger]PlayerInventory.GetPlayerCardsV3\n{\"98535\": 4}";
            let entry = unity_entry(body);
            assert!(try_parse(&entry, test_timestamp()).is_none());
        }

        #[test]
        fn test_try_parse_malformed_json_returns_none() {
            let body = "[UnityCrossThreadLogger]2/22/2026 12:00:00 PM\n\
                         <== StartHook(uuid)\n\
                         {broken json!!!}";
            let entry = unity_entry(body);
            assert!(try_parse(&entry, test_timestamp()).is_none());
        }
    }

    // -- Performance class ----------------------------------------------------

    mod performance_class {
        use super::*;

        #[test]
        fn test_collection_event_is_durable_per_event() {
            let body = "[UnityCrossThreadLogger]2/22/2026 12:00:00 PM\n\
                         <== StartHook(perf-uuid)\n\
                         {\"PlayerCards\": {\"1\": 1}}";
            let entry = unity_entry(body);
            let result = try_parse(&entry, test_timestamp());

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            assert_eq!(event.performance_class(), PerformanceClass::DurablePerEvent);
        }
    }
}
