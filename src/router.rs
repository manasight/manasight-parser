//! Raw log entry to parser dispatch routing.
//!
//! Examines the header prefix and payload of each raw log entry to
//! determine which category-specific parser should handle it. Unrecognized
//! entries are counted and logged at debug level.
//!
//! # Dispatch strategy
//!
//! Each [`LogEntry`] is offered to category parsers in a fixed priority
//! order (most frequent first). The first parser that returns one or
//! more events claims the entry. GRE entries may produce multiple
//! events from batched `GameStateMessage` values. If no parser matches,
//! the entry is counted as unrecognized and discarded.
//!
//! # Timestamp extraction
//!
//! The router extracts the timestamp from the entry header line
//! (e.g., `[UnityCrossThreadLogger]2/25/2026 12:00:00 PM ...`) and
//! parses it using [`parse_log_timestamp`]. If the timestamp cannot be
//! parsed, `None` is passed to parsers so downstream consumers can
//! distinguish real timestamps from missing ones.
//!
//! [`LogEntry`]: crate::log::entry::LogEntry
//! [`parse_log_timestamp`]: crate::log::timestamp::parse_log_timestamp

use std::sync::atomic::{AtomicU64, Ordering};

use chrono::{DateTime, Utc};

use crate::events::GameEvent;
use crate::log::entry::LogEntry;
use crate::log::timestamp::parse_log_timestamp;
use crate::parsers;
use crate::util::truncate_for_log;

// ---------------------------------------------------------------------------
// RouterStats
// ---------------------------------------------------------------------------

/// Counters for router health monitoring.
///
/// Tracks the number of entries routed successfully and the number of
/// unrecognized entries. The unknown-entry count is exposed for upload
/// health status — a spike after an MTGA update signals that new event
/// types may need parser support.
#[derive(Debug, Default)]
pub struct RouterStats {
    /// Number of entries successfully routed to a parser.
    routed: AtomicU64,
    /// Number of entries not claimed by any parser.
    unknown: AtomicU64,
    /// Number of entries where the timestamp could not be parsed.
    timestamp_failures: AtomicU64,
}

impl RouterStats {
    /// Creates a new `RouterStats` with all counters at zero.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the number of entries successfully routed to a parser.
    pub fn routed_count(&self) -> u64 {
        self.routed.load(Ordering::Relaxed)
    }

    /// Returns the number of unrecognized entries (not claimed by any parser).
    pub fn unknown_count(&self) -> u64 {
        self.unknown.load(Ordering::Relaxed)
    }

    /// Returns the number of entries where the timestamp could not be parsed.
    pub fn timestamp_failure_count(&self) -> u64 {
        self.timestamp_failures.load(Ordering::Relaxed)
    }

    /// Resets all counters to zero.
    pub fn reset(&self) {
        self.routed.store(0, Ordering::Relaxed);
        self.unknown.store(0, Ordering::Relaxed);
        self.timestamp_failures.store(0, Ordering::Relaxed);
    }
}

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

/// Dispatch router that matches raw log entries to category-specific parsers.
///
/// Holds a [`RouterStats`] that tracks routing outcomes for health monitoring.
/// The router is designed to be long-lived — create one at startup and reuse
/// it for every entry.
///
/// # Example
///
/// ```
/// use manasight_parser::router::Router;
/// use manasight_parser::log::entry::{LogEntry, EntryHeader};
///
/// let router = Router::new();
///
/// let entry = LogEntry {
///     header: EntryHeader::UnityCrossThreadLogger,
///     body: "[UnityCrossThreadLogger]some unrecognized line".to_owned(),
/// };
///
/// let events = router.route(&entry);
/// assert!(events.is_empty());
/// assert_eq!(router.stats().unknown_count(), 1);
/// ```
pub struct Router {
    /// Routing statistics for health monitoring.
    stats: RouterStats,
}

impl Router {
    /// Creates a new router with zeroed statistics.
    pub fn new() -> Self {
        Self {
            stats: RouterStats::new(),
        }
    }

    /// Returns a reference to the router's statistics.
    pub fn stats(&self) -> &RouterStats {
        &self.stats
    }

    /// Routes a [`LogEntry`] to the appropriate parser.
    ///
    /// Extracts the timestamp from the entry header line, then offers the
    /// entry to each category parser in priority order. Returns a
    /// `Vec<GameEvent>` with one or more events if a parser claims the
    /// entry, or an empty `Vec` if unrecognized.
    ///
    /// GRE entries may contain multiple batched `GameStateMessage` values
    /// in a single log entry, producing multiple events from one entry.
    ///
    /// When the timestamp cannot be parsed, `None` is passed to parsers
    /// so downstream consumers can distinguish real timestamps from
    /// missing ones. The timestamp failure is counted in [`RouterStats`]
    /// and logged at debug level.
    pub fn route(&self, entry: &LogEntry) -> Vec<GameEvent> {
        let timestamp = extract_timestamp(&entry.body);

        if timestamp.is_none() {
            self.stats
                .timestamp_failures
                .fetch_add(1, Ordering::Relaxed);
            ::log::debug!(
                "No timestamp in entry header: {:?}",
                truncate_for_log(&entry.body, 120),
            );
        }

        let events = dispatch_to_parsers(entry, timestamp);

        if events.is_empty() {
            self.stats.unknown.fetch_add(1, Ordering::Relaxed);
            ::log::debug!(
                "Unrecognized entry (header={}, body={:?})",
                entry.header,
                truncate_for_log(&entry.body, 120),
            );
        } else {
            self.stats.routed.fetch_add(1, Ordering::Relaxed);
        }

        events
    }
}

impl Default for Router {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Extracts and parses the timestamp from the first line of an entry body.
///
/// The expected format is:
/// ```text
/// [UnityCrossThreadLogger]2/25/2026 12:00:00 PM some content
/// [Client GRE]2/25/2026 12:00:00 PM GreToClientEvent
/// [UnityCrossThreadLogger]3/13/2026 11:34:51 PM: Match to ...
/// ```
///
/// Strips the bracket-enclosed header prefix and extracts the date/time
/// portion that follows. The timestamp string may be followed by
/// additional content on the same line (event name, method name, etc.)
/// or by a newline if the timestamp is on its own line.
///
/// Trims trailing punctuation (like colons in `... PM: MatchGameRoom...`) from
/// the extracted tokens before parsing to ensure robust matching.
fn extract_timestamp(body: &str) -> Option<DateTime<Utc>> {
    let first_line = body.lines().next()?;

    // Strip the header prefix: find the closing `]` bracket.
    let after_bracket = first_line.find(']').map(|pos| &first_line[pos + 1..])?;
    let trimmed = after_bracket.trim();

    if trimmed.is_empty() {
        return None;
    }

    // The timestamp may be followed by additional text (event name, etc.).
    // Try progressively shorter prefixes to find a valid timestamp.
    // Start with the full string and remove trailing words one at a time.
    let words: Vec<&str> = trimmed.split_whitespace().collect();

    // Timestamps typically use 2-3 tokens (date + time, or date + time + AM/PM).
    // Try from longest plausible prefix down to 2 tokens.
    let max_words = words.len().min(4);
    for end in (2..=max_words).rev() {
        let candidate = words[..end].join(" ");
        // Ensure trailing punctuation (like colons after AM/PM) doesn't break parsing.
        let cleaned = candidate.trim_end_matches(|c: char| c.is_ascii_punctuation());
        if let Ok(ts) = parse_log_timestamp(cleaned) {
            return Some(ts);
        }
    }

    None
}

/// Dispatches a log entry to category parsers in priority order.
///
/// Parsers are tried in order of expected frequency during typical
/// gameplay to minimize unnecessary parse attempts:
///
/// 0. Metadata — `DETAILED LOGS` status (header-type short-circuit)
/// 1. GRE messages (game state + game result) — most frequent in-game
/// 2. Client actions — frequent player decisions
/// 3. Match state — match boundaries
/// 4. Session — login/logout
/// 5. Draft bot — bot draft picks
/// 6. Draft human — human draft picks
/// 7. Draft complete — draft completion
/// 8. Event lifecycle — event joins/claims
/// 9. Rank — rank snapshots
/// 10. Inventory — inventory from `StartHook`
/// 11. Match connection state — `STATE CHANGED` transitions
/// 12. Connection close — `Client.TcpConnection.Close` / `GREConnection.HandleWebSocketClosed`
///
/// The GRE parser may return multiple events from a single entry
/// (batched `GameStateMessage` values). All other parsers return at
/// most one event.
///
/// `timestamp` is `None` when the log entry header did not contain a
/// parseable timestamp; parsers pass it through to `EventMetadata`.
fn dispatch_to_parsers(entry: &LogEntry, timestamp: Option<DateTime<Utc>>) -> Vec<GameEvent> {
    // Metadata entries are routed directly to the metadata parser.
    if let Some(event) = parsers::metadata::try_parse(entry, timestamp) {
        return vec![event];
    }

    // GRE parser returns Vec<GameEvent> (may contain multiple batched GSMs).
    let gre_events = parsers::gre::try_parse(entry, timestamp);
    if !gre_events.is_empty() {
        return gre_events;
    }

    // All other parsers return Option<GameEvent> (at most one event per entry).
    let event = None
        .or_else(|| parsers::client_actions::try_parse(entry, timestamp))
        .or_else(|| parsers::match_state::try_parse(entry, timestamp))
        .or_else(|| parsers::session::try_parse(entry, timestamp))
        .or_else(|| parsers::draft::bot::try_parse(entry, timestamp))
        .or_else(|| parsers::draft::human::try_parse(entry, timestamp))
        .or_else(|| parsers::draft::complete::try_parse(entry, timestamp))
        .or_else(|| parsers::event_lifecycle::try_parse(entry, timestamp))
        .or_else(|| parsers::rank::try_parse(entry, timestamp))
        .or_else(|| parsers::inventory::try_parse(entry, timestamp))
        .or_else(|| parsers::connection_state::try_parse(entry, timestamp))
        .or_else(|| parsers::connection_close::try_parse(entry, timestamp))
        .or_else(|| parsers::connection_error::try_parse(entry, timestamp));

    event.into_iter().collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log::entry::EntryHeader;
    use chrono::Timelike;

    /// Helper: build a `LogEntry` with `UnityCrossThreadLogger` header.
    fn unity_entry(body: &str) -> LogEntry {
        LogEntry {
            header: EntryHeader::UnityCrossThreadLogger,
            body: body.to_owned(),
        }
    }

    /// Helper: build a `LogEntry` with `ClientGre` header.
    fn gre_entry(body: &str) -> LogEntry {
        LogEntry {
            header: EntryHeader::ClientGre,
            body: body.to_owned(),
        }
    }

    // -- extract_timestamp ---------------------------------------------------

    mod extract_timestamp_tests {
        use super::*;

        #[test]
        fn test_extract_timestamp_us_format_with_pm() {
            let body = "[UnityCrossThreadLogger]2/25/2026 12:00:00 PM greToClientEvent";
            let ts = extract_timestamp(body);
            assert!(ts.is_some());
            if let Some(ts) = ts {
                assert_eq!(
                    ts.format("%Y-%m-%d %H:%M:%S").to_string(),
                    "2026-02-25 12:00:00"
                );
            }
        }

        #[test]
        fn test_extract_timestamp_us_format_with_am() {
            let body = "[UnityCrossThreadLogger]2/22/2026 11:59:51 AM";
            let ts = extract_timestamp(body);
            assert!(ts.is_some());
            if let Some(ts) = ts {
                assert_eq!(
                    ts.format("%Y-%m-%d %H:%M:%S").to_string(),
                    "2026-02-22 11:59:51"
                );
            }
        }

        #[test]
        fn test_extract_timestamp_with_trailing_colon() {
            let body = "[UnityCrossThreadLogger]3/13/2026 11:34:51 PM: Match to AAF4FC69CE47D53A";
            let ts = extract_timestamp(body);
            assert!(ts.is_some());
            if let Some(ts) = ts {
                assert_eq!(ts.hour(), 23); // Should correctly identify PM
            }
        }

        #[test]
        fn test_extract_timestamp_24h_format() {
            let body = "[UnityCrossThreadLogger]2026-02-25 14:30:00 some content";
            let ts = extract_timestamp(body);
            assert!(ts.is_some());
            if let Some(ts) = ts {
                assert_eq!(
                    ts.format("%Y-%m-%d %H:%M:%S").to_string(),
                    "2026-02-25 14:30:00"
                );
            }
        }

        #[test]
        fn test_extract_timestamp_client_gre_header() {
            let body = "[Client GRE]2/25/2026 12:00:00 PM GreToClientEvent";
            let ts = extract_timestamp(body);
            assert!(ts.is_some());
        }

        #[test]
        fn test_extract_timestamp_no_bracket_returns_none() {
            let body = "no bracket here";
            let ts = extract_timestamp(body);
            assert!(ts.is_none());
        }

        #[test]
        fn test_extract_timestamp_empty_after_bracket_returns_none() {
            let body = "[UnityCrossThreadLogger]";
            let ts = extract_timestamp(body);
            assert!(ts.is_none());
        }

        #[test]
        fn test_extract_timestamp_no_timestamp_content_returns_none() {
            let body = "[UnityCrossThreadLogger]Updated account. DisplayName:Player";
            let ts = extract_timestamp(body);
            assert!(ts.is_none());
        }

        #[test]
        fn test_extract_timestamp_timestamp_on_own_line() {
            let body = "[UnityCrossThreadLogger]2/22/2026 11:59:51 AM\n<== StartHook(abc-123)";
            let ts = extract_timestamp(body);
            assert!(ts.is_some());
            if let Some(ts) = ts {
                assert_eq!(
                    ts.format("%Y-%m-%d %H:%M:%S").to_string(),
                    "2026-02-22 11:59:51"
                );
            }
        }

        #[test]
        fn test_extract_timestamp_with_leading_space() {
            let body = "[UnityCrossThreadLogger] 2/25/2026 12:00:00 PM event";
            let ts = extract_timestamp(body);
            assert!(ts.is_some());
        }
    }

    // -- Router: known entry routing -----------------------------------------

    mod known_routing {
        use super::*;

        #[test]
        fn test_route_gre_game_state_message() {
            let router = Router::new();
            let payload = serde_json::json!({
                "greToClientEvent": {
                    "greToClientMessages": [{
                        "type": "GREMessageType_GameStateMessage",
                        "gameStateMessage": {
                            "gameInfo": { "stage": "GameStage_Play" },
                            "gameObjects": [],
                            "zones": []
                        }
                    }]
                }
            });
            let body = format!("[UnityCrossThreadLogger]2/25/2026 12:00:00 PM\n{payload}");
            let entry = unity_entry(&body);

            let results = router.route(&entry);
            assert_eq!(results.len(), 1);
            assert!(matches!(&results[0], GameEvent::GameState(_)));
            assert_eq!(router.stats().routed_count(), 1);
            assert_eq!(router.stats().unknown_count(), 0);
        }

        #[test]
        fn test_route_client_action() {
            let router = Router::new();
            let payload = serde_json::json!({
                "clientToMatchServiceMessageType":
                    "ClientToMatchServiceMessageType_ClientToGREMessage",
                "payload": {
                    "type": "ClientMessageType_MulliganResp",
                    "gameStateId": 5,
                    "respId": 1,
                    "mulliganResp": { "decision": "MulliganOption_Mulligan" }
                },
                "requestId": 12345,
                "timestamp": "637123456789"
            });
            let body = format!("[UnityCrossThreadLogger]2/25/2026 12:00:00 PM\n{payload}");
            let entry = unity_entry(&body);

            let results = router.route(&entry);
            assert_eq!(results.len(), 1);
            assert!(matches!(&results[0], GameEvent::ClientAction(_)));
        }

        #[test]
        fn test_route_match_state() {
            let router = Router::new();
            let payload = serde_json::json!({
                "matchGameRoomStateChangedEvent": {
                    "gameRoomInfo": {
                        "stateType": "MatchGameRoomStateType_Playing",
                        "gameRoomConfig": {
                            "matchId": "match-123",
                            "reservedPlayers": []
                        }
                    }
                }
            });
            let body = format!("[UnityCrossThreadLogger]2/25/2026 12:00:00 PM\n{payload}");
            let entry = unity_entry(&body);

            let results = router.route(&entry);
            assert_eq!(results.len(), 1);
            assert!(matches!(&results[0], GameEvent::MatchState(_)));
        }

        #[test]
        fn test_route_session_account_update() {
            let router = Router::new();
            let body = "[UnityCrossThreadLogger]Updated account. \
                         DisplayName:TestPlayer, \
                         AccountID:abc123, \
                         Token:sometoken";
            let entry = unity_entry(body);

            let results = router.route(&entry);
            assert_eq!(results.len(), 1);
            assert!(matches!(&results[0], GameEvent::Session(_)));
        }

        #[test]
        fn test_route_rank_event() {
            let router = Router::new();
            let payload = serde_json::json!({
                "constructedClass": "Gold",
                "constructedLevel": 2,
                "limitedClass": "Silver",
                "limitedLevel": 1
            });
            let body = format!(
                "[UnityCrossThreadLogger]2/25/2026 12:00:00 PM\n\
                 <== RankGetCombinedRankInfo(abc-123)\n{payload}",
            );
            let entry = unity_entry(&body);

            let results = router.route(&entry);
            assert_eq!(results.len(), 1);
            assert!(matches!(&results[0], GameEvent::Rank(_)));
        }

        #[test]
        fn test_route_event_lifecycle() {
            let router = Router::new();
            let body = "[UnityCrossThreadLogger]2/25/2026 12:00:00 PM \
                         ==> EventJoin {\"id\":\"abc-123\",\
                         \"request\":\"{\\\"EventName\\\":\\\"PremierDraft_MKM\\\"}\"}";
            let entry = unity_entry(body);

            let results = router.route(&entry);
            assert_eq!(results.len(), 1);
            assert!(matches!(&results[0], GameEvent::EventLifecycle(_)));
        }

        #[test]
        fn test_route_draft_complete() {
            let router = Router::new();
            let payload = serde_json::json!({
                "CourseId": "draft-123",
                "InternalEventName": "PremierDraft_MKM",
                "CardPool": [12345, 67890]
            });
            let body = format!(
                "[UnityCrossThreadLogger]2/25/2026 12:00:00 PM\n\
                 <== DraftCompleteDraft(abc-123)\n{payload}",
            );
            let entry = unity_entry(&body);

            let results = router.route(&entry);
            assert_eq!(results.len(), 1);
            assert!(matches!(&results[0], GameEvent::DraftComplete(_)));
        }

        #[test]
        fn test_route_draft_bot_pack_presentation() {
            let router = Router::new();
            let payload = serde_json::json!({
                "CurrentModule": "BotDraft",
                "Payload":"{\"DraftStatus\":\"PickNext\",\"PackNumber\":0,\"PickNumber\":0,\"DraftPack\":[\"12345\",\"67890\",\"11111\"]}"
            });
            let body = format!("[UnityCrossThreadLogger]2/25/2026 12:00:00 PM\n<== BotDraftDraftStatus(uuid)\n{payload}",);
            let entry = unity_entry(&body);

            let results = router.route(&entry);
            assert_eq!(results.len(), 1);
            assert!(matches!(&results[0], GameEvent::DraftBot(_)));
        }

        #[test]
        fn test_route_draft_human_notify() {
            let router = Router::new();
            let payload = serde_json::json!({
                "draftId": "abc-123-def",
                "SelfPack": 0,
                "SelfPick": 0,
                "PackCards": "12345,67890,11111"
            });
            let body = format!("[UnityCrossThreadLogger]Draft.Notify\n{payload}",);
            let entry = unity_entry(&body);

            let results = router.route(&entry);
            assert_eq!(results.len(), 1);
            assert!(matches!(&results[0], GameEvent::DraftHuman(_)));
        }

        #[test]
        fn test_route_start_hook_with_additional_fields_routes_to_inventory() {
            let router = Router::new();
            let payload = serde_json::json!({
                "InventoryInfo": { "Gems": 100 },
                "DeckSummariesV2": []
            });
            let body = format!(
                "[UnityCrossThreadLogger]2/25/2026 12:00:00 PM\n\
                 <== StartHook(abc-123)\n{payload}",
            );
            let entry = unity_entry(&body);

            let results = router.route(&entry);
            assert_eq!(results.len(), 1);
            assert!(matches!(&results[0], GameEvent::Inventory(_)));
        }

        #[test]
        fn test_route_inventory_event() {
            let router = Router::new();
            let payload = serde_json::json!({
                "InventoryInfo": { "Gems": 100, "Gold": 5000 }
            });
            let body = format!(
                "[UnityCrossThreadLogger]2/25/2026 12:00:00 PM\n\
                 <== StartHook(abc-123)\n{payload}",
            );
            let entry = unity_entry(&body);

            let results = router.route(&entry);
            assert_eq!(results.len(), 1);
            assert!(matches!(&results[0], GameEvent::Inventory(_)));
        }
    }

    // -- Router: unknown entry handling --------------------------------------

    mod unknown_entries {
        use super::*;

        #[test]
        fn test_route_unknown_entry_returns_empty() {
            let router = Router::new();
            let body = "[UnityCrossThreadLogger]2/25/2026 12:00:00 PM\n\
                         some unrecognized content here";
            let entry = unity_entry(body);

            let results = router.route(&entry);
            assert!(results.is_empty());
        }

        #[test]
        fn test_route_unknown_entry_increments_counter() {
            let router = Router::new();
            let body = "[UnityCrossThreadLogger]2/25/2026 12:00:00 PM\n\
                         unrecognized content";
            let entry = unity_entry(body);

            router.route(&entry);
            assert_eq!(router.stats().unknown_count(), 1);
            assert_eq!(router.stats().routed_count(), 0);
        }

        #[test]
        fn test_route_multiple_unknown_entries_accumulates() {
            let router = Router::new();

            for i in 0..5 {
                let body = format!("[UnityCrossThreadLogger]2/25/2026 12:00:00 PM\nunknown_{i}",);
                let entry = unity_entry(&body);
                router.route(&entry);
            }

            assert_eq!(router.stats().unknown_count(), 5);
            assert_eq!(router.stats().routed_count(), 0);
        }

        #[test]
        fn test_route_empty_body_after_header_returns_empty() {
            let router = Router::new();
            let body = "[UnityCrossThreadLogger]";
            let entry = unity_entry(body);

            let results = router.route(&entry);
            // No timestamp -> passes None, but no parser matches.
            assert!(results.is_empty());
            assert_eq!(router.stats().timestamp_failure_count(), 1);
            assert_eq!(router.stats().unknown_count(), 1);
        }

        #[test]
        fn test_route_no_timestamp_increments_timestamp_failure() {
            let router = Router::new();
            let body = "[UnityCrossThreadLogger]just some text without a timestamp";
            let entry = unity_entry(body);

            let results = router.route(&entry);
            // No parseable timestamp and no parser claims this entry.
            assert!(results.is_empty());
            assert_eq!(router.stats().timestamp_failure_count(), 1);
            assert_eq!(router.stats().unknown_count(), 1);
        }

        #[test]
        fn test_route_no_timestamp_session_still_routes() {
            let router = Router::new();
            // Real-world session entries without timestamps should still route.
            let body = "[UnityCrossThreadLogger]Updated account. \
                         DisplayName:Player, \
                         AccountID:abc123, \
                         Token:token";
            let entry = unity_entry(body);

            let results = router.route(&entry);
            assert_eq!(results.len(), 1);
            // Session routed even without a timestamp in header.
            assert!(matches!(&results[0], GameEvent::Session(_)));
            assert_eq!(router.stats().timestamp_failure_count(), 1);
            assert_eq!(router.stats().routed_count(), 1);
        }

        #[test]
        fn test_route_no_timestamp_passes_none_to_metadata() {
            let router = Router::new();
            // Session entries without timestamps should have None timestamp
            // in metadata rather than a synthetic Utc::now().
            let body = "[UnityCrossThreadLogger]Updated account. \
                         DisplayName:Player, \
                         AccountID:abc123, \
                         Token:token";
            let entry = unity_entry(body);

            let results = router.route(&entry);
            assert_eq!(results.len(), 1);
            assert!(
                results[0].metadata().timestamp().is_none(),
                "entries without parseable timestamps should have None timestamp"
            );
        }

        #[test]
        fn test_route_with_timestamp_passes_some_to_metadata() {
            let router = Router::new();
            let payload = serde_json::json!({
                "greToClientEvent": {
                    "greToClientMessages": [{
                        "type": "GREMessageType_GameStateMessage",
                        "gameStateMessage": {
                            "gameInfo": { "stage": "GameStage_Play" },
                            "gameObjects": [],
                            "zones": []
                        }
                    }]
                }
            });
            let body = format!("[UnityCrossThreadLogger]2/25/2026 12:00:00 PM\n{payload}");
            let entry = unity_entry(&body);

            let results = router.route(&entry);
            assert_eq!(results.len(), 1);
            assert!(
                results[0].metadata().timestamp().is_some(),
                "entries with parseable timestamps should have Some timestamp"
            );
        }
    }

    // -- Router: statistics --------------------------------------------------

    mod stats {
        use super::*;

        #[test]
        fn test_stats_initial_values_are_zero() {
            let router = Router::new();
            assert_eq!(router.stats().routed_count(), 0);
            assert_eq!(router.stats().unknown_count(), 0);
            assert_eq!(router.stats().timestamp_failure_count(), 0);
        }

        #[test]
        fn test_stats_reset_clears_all_counters() {
            let router = Router::new();

            // Route a few entries to increment counters.
            let body = "[UnityCrossThreadLogger]2/25/2026 12:00:00 PM\nunknown";
            let entry = unity_entry(body);
            router.route(&entry);
            router.route(&entry);

            assert_eq!(router.stats().unknown_count(), 2);

            router.stats().reset();

            assert_eq!(router.stats().routed_count(), 0);
            assert_eq!(router.stats().unknown_count(), 0);
            assert_eq!(router.stats().timestamp_failure_count(), 0);
        }

        #[test]
        fn test_stats_mixed_routing() {
            let router = Router::new();

            // Route one known entry (session -- no timestamp in header).
            let known_body = "[UnityCrossThreadLogger]Updated account. \
                              DisplayName:Player, \
                              AccountID:abc123, \
                              Token:token";
            router.route(&unity_entry(known_body));

            // Route one unknown entry (with valid timestamp).
            let unknown_body = "[UnityCrossThreadLogger]2/25/2026 12:00:00 PM\nunknown";
            router.route(&unity_entry(unknown_body));

            // Route one entry with no timestamp and no parser match.
            let bad_ts_body = "[UnityCrossThreadLogger]";
            router.route(&unity_entry(bad_ts_body));

            assert_eq!(router.stats().routed_count(), 1);
            // Two unknown: one with valid timestamp, one with timestamp failure.
            assert_eq!(router.stats().unknown_count(), 2);
            // Two timestamp failures: the session entry and the empty entry.
            assert_eq!(router.stats().timestamp_failure_count(), 2);
        }
    }

    // -- Router: default impl -----------------------------------------------

    mod default_impl {
        use super::*;

        #[test]
        fn test_router_default_creates_functional_router() {
            let router = Router::default();
            assert_eq!(router.stats().routed_count(), 0);
            assert_eq!(router.stats().unknown_count(), 0);
        }
    }

    // -- Router: Client GRE header entries -----------------------------------

    mod client_gre_entries {
        use super::*;

        #[test]
        fn test_route_client_gre_entry() {
            let router = Router::new();
            let payload = serde_json::json!({
                "greToClientEvent": {
                    "greToClientMessages": [{
                        "type": "GREMessageType_GameStateMessage",
                        "gameStateMessage": {
                            "gameInfo": { "stage": "GameStage_Play" },
                            "gameObjects": [],
                            "zones": []
                        }
                    }]
                }
            });
            let body = format!("[Client GRE]2/25/2026 12:00:00 PM\n{payload}");
            let entry = gre_entry(&body);

            let results = router.route(&entry);
            assert_eq!(results.len(), 1);
            assert!(matches!(&results[0], GameEvent::GameState(_)));
        }
    }

    // -- Router: Metadata header entries --------------------------------------

    mod metadata_entries {
        use super::*;

        /// Helper: build a `LogEntry` with `Metadata` header.
        fn metadata_entry(body: &str) -> LogEntry {
            LogEntry {
                header: EntryHeader::Metadata,
                body: body.to_owned(),
            }
        }

        #[test]
        fn test_route_detailed_logs_enabled() {
            let router = Router::new();
            let entry = metadata_entry("DETAILED LOGS: ENABLED");

            let results = router.route(&entry);
            assert_eq!(results.len(), 1);
            assert!(matches!(&results[0], GameEvent::DetailedLoggingStatus(_)));
            if let GameEvent::DetailedLoggingStatus(ref e) = results[0] {
                assert_eq!(e.enabled(), Some(true));
            }
            assert_eq!(router.stats().routed_count(), 1);
        }

        #[test]
        fn test_route_detailed_logs_disabled() {
            let router = Router::new();
            let entry = metadata_entry("DETAILED LOGS: DISABLED");

            let results = router.route(&entry);
            assert_eq!(results.len(), 1);
            assert!(matches!(&results[0], GameEvent::DetailedLoggingStatus(_)));
            if let GameEvent::DetailedLoggingStatus(ref e) = results[0] {
                assert_eq!(e.enabled(), Some(false));
            }
        }

        #[test]
        fn test_route_metadata_no_timestamp_failure() {
            let router = Router::new();
            let entry = metadata_entry("DETAILED LOGS: ENABLED");

            router.route(&entry);
            // Metadata entries have no bracket prefix for timestamp extraction,
            // so they increment the timestamp failure counter.
            assert_eq!(router.stats().timestamp_failure_count(), 1);
            // But they should still be routed successfully.
            assert_eq!(router.stats().routed_count(), 1);
        }

        #[test]
        fn test_route_unrecognized_metadata_returns_empty() {
            let router = Router::new();
            let entry = metadata_entry("SOME OTHER METADATA");

            let results = router.route(&entry);
            assert!(results.is_empty());
            assert_eq!(router.stats().unknown_count(), 1);
        }
    }
}
