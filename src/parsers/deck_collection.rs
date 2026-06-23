//! Deck collection parser for `<== StartHook` responses containing
//! `DeckSummaries` and `Decks`.
//!
//! Extracts the user's deck summaries and correlates them with the
//! corresponding deck-list payloads.
//!
//! Some deck summaries may include `"IsNetDeck": true`. These entries are
//! not necessarily decks the player created themselves; they can represent
//! preconstructed/event-assigned decks or other Arena-provided deck records.
//! Library consumers may want to surface or hide these separately in the UI.
//!
//! # Real log format
//!
//! ```text
//! [UnityCrossThreadLogger]2/22/2026 11:59:51 AM
//! <== StartHook(e3f1a2b4-...)
//! {
//!   "InventoryInfo": { "Gems": 1234, "Gold": 5678, ... },
//!   "DeckSummaries": [
//!     { "DeckId": "xxxxxxxx", "Name": "Reanimator", ... }
//!   ],
//!   "Decks": {
//!     "xxxxxxxx": {
//!       "MainDeck": [ { "cardId": 1, "quantity": 2 }, ... ],
//!       "Sideboard": [ ... ]
//!     }
//!   }
//!   ...
//! }
//! ```
//!
//! The `<==` response line and JSON payload are continuation lines within
//! the `[UnityCrossThreadLogger]` entry.

use crate::events::{DeckCollectionEvent, EventMetadata, GameEvent};
use crate::log::entry::LogEntry;
use crate::parsers::api_common;

/// API method name for the `StartHook` response.
const START_HOOK_METHOD: &str = "StartHook";

/// Field name within the `StartHook` JSON that contains deck summaries data.
const DECK_SUMMARIES_FIELD: &str = "DeckSummaries";
/// Field name within the `StartHook` JSON that contains deck lists data.
const DECKS_FIELD: &str = "Decks";

/// Attempts to parse a [`LogEntry`] as a deck collection event.
///
/// Returns `Some(GameEvent::DeckCollection(_))` if the entry is a
/// `<== StartHook` response containing both `DeckSummaries` and `Decks`, or
/// `None` otherwise.
///
/// Each `DeckSummaries` entry is correlated with its `Decks` payload by
/// `DeckId`. Summaries whose `DeckId` is missing or absent from the `Decks`
/// map are silently dropped from the emitted event, so the correlated deck
/// count may be smaller than the input `DeckSummaries` array. The full,
/// uncorrelated payload is preserved under `raw_start_hook` for consumers
/// that need to detect or recover dropped entries.
///
/// The `timestamp` is `None` when the log entry header did not contain a
/// parseable timestamp. It is passed through to [`EventMetadata`] so
/// downstream consumers can distinguish real vs missing timestamps.
///
/// Under the `lean` feature, `raw_start_hook` and `decks` are omitted from
/// the emitted event payload; only the `type` envelope remains. Consumers
/// that need the full deck data must use the non-lean build.
pub fn try_parse(
    entry: &LogEntry,
    timestamp: Option<chrono::DateTime<chrono::Utc>>,
) -> Option<GameEvent> {
    let body = &entry.body;

    if !api_common::is_api_response(body, START_HOOK_METHOD) {
        return None;
    }

    let parsed = api_common::parse_json_from_body(body, "StartHook deck collection")?;

    // Under the `lean` feature, drop `raw_start_hook` (the full StartHook
    // JSON, ~large) and the `decks` payload (deck lists, also large).
    // Keep the variant envelope key (`type`) so consumers doing
    // `Object.keys(e)[0]` still see `DeckCollection`.
    //
    // We must still validate that DeckSummaries and Decks are present (to
    // match/not-match correctly) but we don't build the correlated payload.
    #[cfg(feature = "lean")]
    let payload = {
        // Validate presence of required fields for entry-matching purposes.
        let _deck_summaries = parsed.get(DECK_SUMMARIES_FIELD)?.as_array()?;
        let _decks = parsed.get(DECKS_FIELD)?.as_object()?;
        serde_json::json!({ "type": "deck_collection_snapshot" })
    };

    #[cfg(not(feature = "lean"))]
    let payload = {
        let deck_summaries = parsed.get(DECK_SUMMARIES_FIELD)?.as_array()?;
        let decks = parsed.get(DECKS_FIELD)?.as_object()?;
        serde_json::json!({
            "type": "deck_collection_snapshot",
            "decks": correlate_decks(deck_summaries, decks),
            "raw_start_hook": parsed,
        })
    };

    let metadata = EventMetadata::new(timestamp, body.as_bytes().to_vec());
    Some(GameEvent::DeckCollection(DeckCollectionEvent::new(
        metadata, payload,
    )))
}

/// Correlates `DeckSummaries` entries with `Decks` payloads by `DeckId`.
/// Not compiled under the `lean` feature (deck list data is excluded from the lean payload).
#[cfg(not(feature = "lean"))]
fn correlate_decks(
    deck_summaries: &[serde_json::Value],
    decks: &serde_json::Map<String, serde_json::Value>,
) -> serde_json::Value {
    serde_json::Value::Object(
        deck_summaries
            .iter()
            .filter_map(|summary| correlate_summary(summary, decks))
            .collect(),
    )
}

/// Builds a single correlated deck record from a summary and deck-map lookup.
/// Not compiled under the `lean` feature.
#[cfg(not(feature = "lean"))]
fn correlate_summary(
    summary: &serde_json::Value,
    deck_map: &serde_json::Map<String, serde_json::Value>,
) -> Option<(String, serde_json::Value)> {
    let summary = summary.as_object()?;
    let deck_id = api_common::extract_deck_id(&serde_json::Value::Object(summary.clone()))?;

    let deck = deck_map.get(&deck_id).cloned()?;

    let mut enriched = summary.clone();
    enriched.insert("list".to_string(), deck);

    Some((deck_id, serde_json::Value::Object(enriched)))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(deprecated)]
mod tests {
    use super::*;
    use crate::events::PerformanceClass;
    use crate::parsers::test_helpers::{test_timestamp, unity_entry};
    // `deck_collection_payload` is only referenced by the `matching` sub-module,
    // which is gated under `not(lean)`.
    #[cfg(not(feature = "lean"))]
    use crate::parsers::test_helpers::deck_collection_payload;

    // -- Matching entries (not compiled under `lean` — deck payload excluded) --

    #[cfg(not(feature = "lean"))]
    mod matching {
        use super::*;

        #[test]
        fn test_try_parse_start_hook_with_deck_summaries_and_decks() {
            let body = "[UnityCrossThreadLogger]2/22/2026 11:59:51 AM\n\
                         <== StartHook(deck-uuid)\n\
                         {\n\
                           \"DeckSummaries\": [\n\
                             {\"DeckId\": \"deck-1\", \"Name\": \"Reanimator\"},\n\
                             {\"DeckId\": \"deck-2\", \"Name\": \"Artifacts\"}\n\
                           ],\n\
                           \"Decks\": {\n\
                             \"deck-1\": {\"MainDeck\": [{\"cardId\": 1, \"quantity\": 4}]},\n\
                             \"deck-2\": {\"MainDeck\": [{\"cardId\": 2, \"quantity\": 3}]}\n\
                           }\n\
                         }";
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = deck_collection_payload(event);

            assert_eq!(payload["type"], "deck_collection_snapshot");
            assert_eq!(payload["decks"]["deck-1"]["Name"], "Reanimator");
            assert_eq!(payload["decks"]["deck-1"]["DeckId"], "deck-1");
            assert_eq!(
                payload["decks"]["deck-2"]["list"]["MainDeck"][0]["cardId"],
                2
            );
            assert_eq!(
                payload["decks"]["deck-2"]["list"]["MainDeck"][0]["quantity"],
                3
            );
        }

        #[test]
        fn test_try_parse_skips_orphaned_summary() {
            let body = "[UnityCrossThreadLogger]2/22/2026 11:59:51 AM\n\
                         <== StartHook(deck-uuid)\n\
                         {\n\
                           \"DeckSummaries\": [\n\
                              {\"DeckId\": \"deck-1\", \"Name\": \"Orphaned\"},\n\
                              {\"DeckId\": \"deck-2\", \"Name\": \"Artifacts\"}\n\
                            ],\n\
                            \"Decks\": {\n\
                              \"deck-2\": {\"MainDeck\": [{\"cardId\": 2, \"quantity\": 3}]}\n\
                           }\n\
                         }";
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = deck_collection_payload(event);

            assert!(payload["decks"].get("deck-2").is_some());
            assert!(payload["decks"].get("deck-1").is_none());
        }

        #[test]
        fn test_try_parse_emits_only_non_null_lists() {
            let body = "[UnityCrossThreadLogger]2/22/2026 11:59:51 AM\n\
                         <== StartHook(deck-uuid)\n\
                         {\n\
                           \"DeckSummaries\": [\n\
                             {\"DeckId\": \"deck-1\", \"Name\": \"Reanimator\"},\n\
                             {\"DeckId\": \"deck-2\", \"Name\": \"Artifacts\"}\n\
                           ],\n\
                           \"Decks\": {\n\
                             \"deck-1\": {\"MainDeck\": [{\"cardId\": 1, \"quantity\": 4}]},\n\
                             \"deck-2\": {\"MainDeck\": [{\"cardId\": 2, \"quantity\": 3}]}\n\
                           }\n\
                         }";
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = deck_collection_payload(event);
            let decks = payload["decks"]
                .as_object()
                .unwrap_or_else(|| unreachable!());

            assert!(!decks.is_empty());
            assert!(decks
                .values()
                .all(|deck| deck.get("list").is_some_and(|value| !value.is_null())));
        }

        #[test]
        fn test_try_parse_preserves_raw_start_hook() {
            let body = "[UnityCrossThreadLogger]2/22/2026 12:00:00 PM\n\
                         <== StartHook(raw-uuid)\n\
                         {\"DeckSummaries\": [], \"Decks\": {}, \"ExtraField\": true}";
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = deck_collection_payload(event);

            assert_eq!(payload["raw_start_hook"]["ExtraField"], true);
        }
    }

    // -- Metadata preservation ------------------------------------------------

    mod metadata {
        use super::*;

        #[test]
        fn test_try_parse_preserves_raw_bytes() {
            let body = "[UnityCrossThreadLogger]2/22/2026 12:00:00 PM\n\
                         <== StartHook(meta-uuid)\n\
                         {\"DeckSummaries\": [], \"Decks\": {}}";
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            assert_eq!(event.metadata().raw_bytes(), body.as_bytes());
        }

        #[test]
        fn test_try_parse_stores_timestamp() {
            let body = "[UnityCrossThreadLogger]2/22/2026 12:00:00 PM\n\
                         <== StartHook(ts-uuid)\n\
                         {\"DeckSummaries\": [], \"Decks\": {}}";
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
        fn test_try_parse_start_hook_without_deck_summaries_returns_none() {
            let body = "[UnityCrossThreadLogger]2/22/2026 12:00:00 PM\n\
                         <== StartHook(no-summaries-uuid)\n\
                         {\"Decks\": {\"deck-1\": {}}, \"InventoryInfo\": {\"Gems\": 10}}";
            let entry = unity_entry(body);
            assert!(try_parse(&entry, Some(test_timestamp())).is_none());
        }

        #[test]
        fn test_try_parse_start_hook_without_decks_returns_none() {
            let body = "[UnityCrossThreadLogger]2/22/2026 12:00:00 PM\n\
                         <== StartHook(no-decks-uuid)\n\
                         {\"DeckSummaries\": [{\"DeckId\": \"deck-1\"}]}";
            let entry = unity_entry(body);
            assert!(try_parse(&entry, Some(test_timestamp())).is_none());
        }

        #[test]
        fn test_try_parse_non_array_deck_summaries_returns_none() {
            let body = "[UnityCrossThreadLogger]2/22/2026 12:00:00 PM\n\
                         <== StartHook(non-array-uuid)\n\
                         {\"DeckSummaries\": {\"DeckId\": \"deck-1\"}, \"Decks\": {\"deck-1\": {}}}";
            let entry = unity_entry(body);
            assert!(try_parse(&entry, Some(test_timestamp())).is_none());
        }

        #[test]
        fn test_try_parse_non_object_decks_returns_none() {
            let body = "[UnityCrossThreadLogger]2/22/2026 12:00:00 PM\n\
                         <== StartHook(non-object-uuid)\n\
                         {\"DeckSummaries\": [{\"DeckId\": \"deck-1\"}], \"Decks\": []}";
            let entry = unity_entry(body);
            assert!(try_parse(&entry, Some(test_timestamp())).is_none());
        }

        #[test]
        fn test_try_parse_different_api_response_returns_none() {
            let body = "[UnityCrossThreadLogger]2/22/2026 12:00:00 PM\n\
                         <== RankGetCombinedRankInfo(uuid)\n\
                         {\"constructedClass\": \"Gold\"}";
            let entry = unity_entry(body);
            assert!(try_parse(&entry, Some(test_timestamp())).is_none());
        }

        #[test]
        fn test_try_parse_api_request_returns_none() {
            let body = "[UnityCrossThreadLogger]==> StartHook {\"data\": 1}";
            let entry = unity_entry(body);
            assert!(try_parse(&entry, Some(test_timestamp())).is_none());
        }

        #[test]
        fn test_try_parse_malformed_json_returns_none() {
            let body = "[UnityCrossThreadLogger]2/22/2026 12:00:00 PM\n\
                         <== StartHook(uuid)\n\
                         {broken json!!!}";
            let entry = unity_entry(body);
            assert!(try_parse(&entry, Some(test_timestamp())).is_none());
        }
    }

    // -- Performance class ----------------------------------------------------

    mod performance_class {
        use super::*;

        #[test]
        fn test_deck_collection_event_is_durable_per_event() {
            let body = "[UnityCrossThreadLogger]2/22/2026 12:00:00 PM\n\
                         <== StartHook(perf-uuid)\n\
                         {\"DeckSummaries\": [], \"Decks\": {}}";
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            assert_eq!(event.performance_class(), PerformanceClass::DurablePerEvent);
        }
    }
}
