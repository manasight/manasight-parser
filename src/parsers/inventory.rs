//! Inventory parser for `<== StartHook` responses containing `InventoryInfo`.
//!
//! Extracts currency, wildcards, boosters, and vault progress from the
//! `InventoryInfo` field in `StartHook` API responses.
//!
//! # Real log format
//!
//! ```text
//! [UnityCrossThreadLogger]2/22/2026 11:59:51 AM
//! <== StartHook(e3f1a2b4-...)
//! {
//!   "InventoryInfo": { "Gems": 1234, "Gold": 5678, ... },
//!   "PlayerCards": { ... },
//!   ...
//! }
//! ```
//!
//! The `<==` response line and JSON payload are continuation lines within
//! the `[UnityCrossThreadLogger]` entry.

use crate::events::{EventMetadata, GameEvent, InventoryEvent};
use crate::log::entry::LogEntry;
use crate::parsers::api_common;

/// API method name for the `StartHook` response.
const START_HOOK_METHOD: &str = "StartHook";

/// Field name within the `StartHook` JSON that contains inventory data.
const INVENTORY_FIELD: &str = "InventoryInfo";

/// Attempts to parse a [`LogEntry`] as an inventory event.
///
/// Returns `Some(GameEvent::Inventory(_))` if the entry is a `<== StartHook`
/// response containing an `InventoryInfo` field, or `None` otherwise.
///
/// The `timestamp` is `None` when the log entry header did not contain a
/// parseable timestamp. It is passed through to [`EventMetadata`] so
/// downstream consumers can distinguish real vs missing timestamps.
pub fn try_parse(
    entry: &LogEntry,
    timestamp: Option<chrono::DateTime<chrono::Utc>>,
) -> Option<GameEvent> {
    let body = &entry.body;

    if !api_common::is_api_response(body, START_HOOK_METHOD) {
        return None;
    }

    let parsed = api_common::parse_json_from_body(body, "StartHook inventory")?;

    // Only claim this entry if the InventoryInfo field is present.
    let inventory_info = parsed.get(INVENTORY_FIELD)?;

    let payload = serde_json::json!({
        "type": "inventory_snapshot",
        "inventory": inventory_info.clone(),
        "raw_start_hook": parsed,
    });

    let metadata = EventMetadata::new(timestamp, body.as_bytes().to_vec());
    Some(GameEvent::Inventory(InventoryEvent::new(metadata, payload)))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::PerformanceClass;
    use crate::parsers::test_helpers::{
        inventory_payload, test_timestamp, unity_entry, EntryHeader,
    };

    // -- Matching entries (StartHook with InventoryInfo) -----------------------

    mod matching {
        use super::*;

        #[test]
        fn test_try_parse_start_hook_with_inventory_info() {
            let body = "[UnityCrossThreadLogger]2/22/2026 11:59:51 AM\n\
                         <== StartHook(e3f1a2b4-5678-9abc-def0-123456789abc)\n\
                         {\n\
                           \"InventoryInfo\": {\"Gems\": 1234, \"Gold\": 5678},\n\
                           \"PlayerCards\": {\"98535\": 4}\n\
                         }";
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = inventory_payload(event);

            assert_eq!(payload["type"], "inventory_snapshot");
            assert_eq!(payload["inventory"]["Gems"], 1234);
            assert_eq!(payload["inventory"]["Gold"], 5678);
        }

        #[test]
        fn test_try_parse_extracts_wildcards() {
            let body = "[UnityCrossThreadLogger]2/22/2026 11:59:51 AM\n\
                         <== StartHook(uuid-123)\n\
                         {\"InventoryInfo\": {\
                           \"Gems\": 100, \"Gold\": 200,\
                           \"wcCommon\": 10, \"wcUncommon\": 5,\
                           \"wcRare\": 3, \"wcMythic\": 1\
                         }}";
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = inventory_payload(event);

            assert_eq!(payload["inventory"]["wcCommon"], 10);
            assert_eq!(payload["inventory"]["wcMythic"], 1);
        }

        #[test]
        fn test_try_parse_extracts_vault_progress() {
            let body = "[UnityCrossThreadLogger]2/22/2026 12:00:00 PM\n\
                         <== StartHook(vault-uuid)\n\
                         {\"InventoryInfo\": {\
                           \"Gems\": 0, \"Gold\": 0,\
                           \"VaultProgress\": 42.5\
                         }}";
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = inventory_payload(event);

            assert_eq!(payload["inventory"]["VaultProgress"], 42.5);
        }

        #[test]
        fn test_try_parse_preserves_raw_start_hook() {
            let body = "[UnityCrossThreadLogger]2/22/2026 12:00:00 PM\n\
                         <== StartHook(raw-uuid)\n\
                         {\"InventoryInfo\": {\"Gems\": 999}, \"ExtraField\": true}";
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = inventory_payload(event);

            assert_eq!(payload["raw_start_hook"]["ExtraField"], true);
        }

        #[test]
        fn test_try_parse_single_line_response() {
            let body =
                "[UnityCrossThreadLogger]<== StartHook(uuid) {\"InventoryInfo\": {\"Gems\": 42}}";
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = inventory_payload(event);

            assert_eq!(payload["inventory"]["Gems"], 42);
        }
    }

    // -- Metadata preservation ------------------------------------------------

    mod metadata {
        use super::*;

        #[test]
        fn test_try_parse_preserves_raw_bytes() {
            let body = "[UnityCrossThreadLogger]2/22/2026 12:00:00 PM\n\
                         <== StartHook(meta-uuid)\n\
                         {\"InventoryInfo\": {\"Gems\": 1}}";
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
                         {\"InventoryInfo\": {\"Gems\": 1}}";
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
        fn test_try_parse_start_hook_without_inventory_info_returns_none() {
            let body = "[UnityCrossThreadLogger]2/22/2026 12:00:00 PM\n\
                         <== StartHook(no-inv-uuid)\n\
                         {\"PlayerCards\": {\"98535\": 4}, \"DeckSummariesV2\": []}";
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
        fn test_try_parse_old_marker_returns_none() {
            let body = "[UnityCrossThreadLogger]DTO_InventoryInfo\n{\"Gems\": 1234}";
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
        fn test_inventory_event_is_durable_per_event() {
            let body = "[UnityCrossThreadLogger]2/22/2026 12:00:00 PM\n\
                         <== StartHook(perf-uuid)\n\
                         {\"InventoryInfo\": {\"Gems\": 1}}";
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            assert_eq!(event.performance_class(), PerformanceClass::DurablePerEvent);
        }
    }
}
