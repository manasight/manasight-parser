//! Integration tests for [`manasight_parser::parse_whole_log`].
//!
//! Verifies that the sync entry point produces the same events as the
//! entry-by-entry [`Router`] path, including trailing entries not followed
//! by a header.

use manasight_parser::log::entry::LineBuffer;
use manasight_parser::router::Router;
use manasight_parser::{parse_whole_log, GameEvent};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Feeds `input` line-by-line through a fresh `LineBuffer` + `Router`,
/// returning all collected events (the reference path).
fn parse_via_router(input: &str) -> Vec<GameEvent> {
    let mut buffer = LineBuffer::new();
    let router = Router::new();
    let mut events = Vec::new();

    for line in input.lines() {
        for entry in buffer.push_line(line) {
            events.extend(router.route(&entry));
        }
    }
    if let Some(entry) = buffer.flush() {
        events.extend(router.route(&entry));
    }

    events
}

// ---------------------------------------------------------------------------
// Parity tests: parse_whole_log vs. entry-by-entry Router path
// ---------------------------------------------------------------------------

#[test]
fn test_parse_whole_log_empty_input_returns_empty_vec() {
    let events = parse_whole_log("");
    assert!(events.is_empty());
}

#[test]
fn test_parse_whole_log_metadata_parity_with_router() {
    let input = "DETAILED LOGS: ENABLED\n";
    let via_fn = parse_whole_log(input);
    let via_router = parse_via_router(input);
    assert_eq!(
        via_fn.len(),
        via_router.len(),
        "parse_whole_log and router path must emit the same number of events"
    );
    assert_eq!(via_fn.len(), 1);
    assert!(matches!(via_fn[0], GameEvent::DetailedLoggingStatus(_)));
}

#[test]
fn test_parse_whole_log_session_event_parity_with_router() {
    let input = "[UnityCrossThreadLogger]authenticateResponse\n\
                 {\"screenName\":\"TestPlayer\"}\n\
                 [UnityCrossThreadLogger]2/25/2026 12:00:00 PM\n\
                 some filler\n";

    let via_fn = parse_whole_log(input);
    let via_router = parse_via_router(input);

    assert_eq!(
        via_fn.len(),
        via_router.len(),
        "event count must match between parse_whole_log and router path"
    );
    assert_eq!(via_fn.len(), 1);
    assert!(matches!(via_fn[0], GameEvent::Session(_)));
}

#[test]
fn test_parse_whole_log_multiple_events_parity_with_router() {
    let gs_payload = serde_json::json!({
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

    let input = format!(
        "DETAILED LOGS: ENABLED\n\
         [UnityCrossThreadLogger]authenticateResponse\n\
         {{\"screenName\":\"TestPlayer\"}}\n\
         [UnityCrossThreadLogger]2/25/2026 12:00:00 PM\n{gs_payload}\n\
         [UnityCrossThreadLogger]2/25/2026 12:00:01 PM\nfiller\n"
    );

    let via_fn = parse_whole_log(&input);
    let via_router = parse_via_router(&input);

    assert_eq!(
        via_fn.len(),
        via_router.len(),
        "event count must match: parse_whole_log={}, router={}",
        via_fn.len(),
        via_router.len()
    );
    // Expected: DetailedLoggingStatus + Session + GameState = 3
    assert_eq!(via_fn.len(), 3);
    assert!(matches!(via_fn[0], GameEvent::DetailedLoggingStatus(_)));
    assert!(matches!(via_fn[1], GameEvent::Session(_)));
    assert!(matches!(via_fn[2], GameEvent::GameState(_)));
}

/// This test specifically exercises the trailing-entry flush path: the last
/// entry has no following header to trigger an implicit flush, so `flush()`
/// must drain it.
#[test]
fn test_parse_whole_log_trailing_entry_not_followed_by_header_parity() {
    // The session entry at the end has no subsequent header — it will only
    // be emitted if flush() is called after iterating all lines.
    let input = "[UnityCrossThreadLogger]authenticateResponse\n\
                 {\"screenName\":\"TrailingEntry\"}\n";

    let via_fn = parse_whole_log(input);
    let via_router = parse_via_router(input);

    assert_eq!(
        via_fn.len(),
        via_router.len(),
        "trailing entry must be drained by flush(): parse_whole_log={}, router={}",
        via_fn.len(),
        via_router.len()
    );
    assert_eq!(
        via_fn.len(),
        1,
        "expected exactly one event from trailing entry"
    );
    assert!(matches!(via_fn[0], GameEvent::Session(_)));
}

#[test]
fn test_parse_whole_log_unrecognized_entries_parity_with_router() {
    // Content with no parseable events — should return empty vec.
    let input = "[UnityCrossThreadLogger]2/25/2026 12:00:00 PM\n\
                 some completely unrecognized content\n\
                 [UnityCrossThreadLogger]2/25/2026 12:00:01 PM\n\
                 more unrecognized content\n";

    let via_fn = parse_whole_log(input);
    let via_router = parse_via_router(input);

    assert_eq!(
        via_fn.len(),
        via_router.len(),
        "unrecognized entries must produce identical (empty) results"
    );
    assert!(via_fn.is_empty());
}
