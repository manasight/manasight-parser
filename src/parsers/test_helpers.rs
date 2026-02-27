//! Shared test helpers for parser unit tests.
//!
//! Consolidates `test_timestamp()`, `unity_entry()`, and per-variant payload
//! extractors that were previously duplicated across all parser test modules.

pub use crate::events::GameEvent;
pub use crate::log::entry::{EntryHeader, LogEntry};
use chrono::{TimeZone, Utc};

/// Build a UTC timestamp for test use (2026-02-25 12:00:00 UTC).
///
/// UTC datetimes are never ambiguous so `single()` always returns `Some`.
/// Uses `unwrap_or_default()` because `clippy::expect_used` is denied
/// crate-wide. The epoch fallback (1970-01-01) would visibly fail any
/// timestamp assertion rather than passing silently.
pub fn test_timestamp() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 2, 25, 12, 0, 0)
        .single()
        .unwrap_or_default()
}

/// Build a [`LogEntry`] with `UnityCrossThreadLogger` header from body text.
pub fn unity_entry(body: &str) -> LogEntry {
    LogEntry {
        header: EntryHeader::UnityCrossThreadLogger,
        body: body.to_owned(),
    }
}

/// Generate a per-variant payload extractor for test use.
///
/// Each generated function matches one [`GameEvent`] variant and returns its
/// JSON payload via `.payload()`. Non-matching variants return a static null
/// value so that assertion failures clearly indicate the wrong variant was
/// produced rather than panicking.
macro_rules! define_payload_extractor {
    ($fn_name:ident, $variant:ident) => {
        pub fn $fn_name(event: &GameEvent) -> &serde_json::Value {
            static EMPTY: std::sync::LazyLock<serde_json::Value> =
                std::sync::LazyLock::new(|| serde_json::json!(null));
            match event {
                GameEvent::$variant(e) => e.payload(),
                _ => &EMPTY,
            }
        }
    };
}

define_payload_extractor!(session_payload, Session);
define_payload_extractor!(match_state_payload, MatchState);
define_payload_extractor!(game_state_payload, GameState);
define_payload_extractor!(game_result_payload, GameResult);
define_payload_extractor!(draft_bot_payload, DraftBot);
define_payload_extractor!(draft_human_payload, DraftHuman);
define_payload_extractor!(draft_complete_payload, DraftComplete);
define_payload_extractor!(lifecycle_payload, EventLifecycle);
define_payload_extractor!(rank_payload, Rank);
define_payload_extractor!(collection_payload, Collection);
define_payload_extractor!(inventory_payload, Inventory);
