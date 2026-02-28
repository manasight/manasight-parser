//! Rank parser for `<== RankGetCombinedRankInfo` API responses.
//!
//! Extracts constructed and limited rank data from the
//! `RankGetCombinedRankInfo` response.
//!
//! # Real log format
//!
//! ```text
//! [UnityCrossThreadLogger]2/22/2026 12:00:01 PM
//! <== RankGetCombinedRankInfo(a1b2c3d4-...)
//! { "constructedClass": "Gold", "constructedLevel": 2, "constructedStep": 3,
//!   "limitedClass": "Silver", ... }
//! ```
//!
//! The `<==` response line and JSON payload are continuation lines within
//! the `[UnityCrossThreadLogger]` entry.

use crate::events::{EventMetadata, GameEvent, RankEvent};
use crate::log::entry::LogEntry;
use crate::parsers::api_common;

/// API method name for rank information responses.
const RANK_METHOD: &str = "RankGetCombinedRankInfo";

/// Attempts to parse a [`LogEntry`] as a rank event.
///
/// Returns `Some(GameEvent::Rank(_))` if the entry is a
/// `<== RankGetCombinedRankInfo` response, or `None` otherwise.
///
/// The `timestamp` is `None` when the log entry header did not contain a
/// parseable timestamp. It is passed through to [`EventMetadata`] so
/// downstream consumers can distinguish real vs missing timestamps.
pub fn try_parse(
    entry: &LogEntry,
    timestamp: Option<chrono::DateTime<chrono::Utc>>,
) -> Option<GameEvent> {
    let body = &entry.body;

    if !api_common::is_api_response(body, RANK_METHOD) {
        return None;
    }

    let parsed = api_common::parse_json_from_body(body, "RankGetCombinedRankInfo")?;

    let payload = serde_json::json!({
        "type": "rank_snapshot",
        "rank": parsed,
    });

    let metadata = EventMetadata::new(timestamp, body.as_bytes().to_vec());
    Some(GameEvent::Rank(RankEvent::new(metadata, payload)))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::PerformanceClass;
    use crate::parsers::test_helpers::{rank_payload, test_timestamp, unity_entry, EntryHeader};

    // -- Matching entries (<== RankGetCombinedRankInfo) ------------------------

    mod matching {
        use super::*;

        #[test]
        fn test_try_parse_rank_response() {
            let body = "[UnityCrossThreadLogger]2/22/2026 12:00:01 PM\n\
                         <== RankGetCombinedRankInfo(a1b2c3d4-5678-9abc-def0-123456789abc)\n\
                         {\n\
                           \"constructedClass\": \"Gold\",\n\
                           \"constructedLevel\": 2,\n\
                           \"constructedStep\": 3,\n\
                           \"limitedClass\": \"Silver\",\n\
                           \"limitedLevel\": 1,\n\
                           \"limitedStep\": 0\n\
                         }";
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = rank_payload(event);

            assert_eq!(payload["type"], "rank_snapshot");
            assert_eq!(payload["rank"]["constructedClass"], "Gold");
            assert_eq!(payload["rank"]["constructedLevel"], 2);
            assert_eq!(payload["rank"]["constructedStep"], 3);
            assert_eq!(payload["rank"]["limitedClass"], "Silver");
        }

        #[test]
        fn test_try_parse_rank_minimal() {
            let body = "[UnityCrossThreadLogger]2/22/2026 12:00:01 PM\n\
                         <== RankGetCombinedRankInfo(uuid-123)\n\
                         {\"constructedClass\": \"Bronze\"}";
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = rank_payload(event);

            assert_eq!(payload["rank"]["constructedClass"], "Bronze");
        }

        #[test]
        fn test_try_parse_rank_mythic() {
            let body = "[UnityCrossThreadLogger]2/22/2026 12:00:01 PM\n\
                         <== RankGetCombinedRankInfo(mythic-uuid)\n\
                         {\"constructedClass\": \"Mythic\", \"constructedLevel\": 0,\
                          \"constructedPercentile\": 98.5, \"constructedLeaderboardPlace\": 42}";
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = rank_payload(event);

            assert_eq!(payload["rank"]["constructedClass"], "Mythic");
            assert_eq!(payload["rank"]["constructedPercentile"], 98.5);
            assert_eq!(payload["rank"]["constructedLeaderboardPlace"], 42);
        }
    }

    // -- Metadata preservation ------------------------------------------------

    mod metadata {
        use super::*;

        #[test]
        fn test_try_parse_preserves_raw_bytes() {
            let body = "[UnityCrossThreadLogger]2/22/2026 12:00:01 PM\n\
                         <== RankGetCombinedRankInfo(meta-uuid)\n\
                         {\"constructedClass\": \"Gold\"}";
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            assert_eq!(event.metadata().raw_bytes(), body.as_bytes());
        }

        #[test]
        fn test_try_parse_stores_timestamp() {
            let body = "[UnityCrossThreadLogger]2/22/2026 12:00:01 PM\n\
                         <== RankGetCombinedRankInfo(ts-uuid)\n\
                         {\"constructedClass\": \"Gold\"}";
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
        fn test_try_parse_different_api_response_returns_none() {
            let body = "[UnityCrossThreadLogger]2/22/2026 12:00:00 PM\n\
                         <== StartHook(uuid)\n\
                         {\"InventoryInfo\": {\"Gems\": 1234}}";
            let entry = unity_entry(body);
            assert!(try_parse(&entry, Some(test_timestamp())).is_none());
        }

        #[test]
        fn test_try_parse_rank_request_returns_none() {
            let body = "[UnityCrossThreadLogger]==> RankGetCombinedRankInfo {}";
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
        fn test_try_parse_old_marker_returns_none() {
            let body = "[UnityCrossThreadLogger]Rank_GetCombinedRankInfo\n\
                         {\"constructedClass\": \"Gold\"}";
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
            let body = "[UnityCrossThreadLogger]2/22/2026 12:00:01 PM\n\
                         <== RankGetCombinedRankInfo(uuid)\n\
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
        fn test_rank_event_is_durable_per_event() {
            let body = "[UnityCrossThreadLogger]2/22/2026 12:00:01 PM\n\
                         <== RankGetCombinedRankInfo(perf-uuid)\n\
                         {\"constructedClass\": \"Gold\"}";
            let entry = unity_entry(body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            assert_eq!(event.performance_class(), PerformanceClass::DurablePerEvent);
        }
    }
}
