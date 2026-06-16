//! Integration tests for seat attribution and corpus parity.
//!
//! Covers:
//! - AC-ING-3: `parse_whole_log` parity with the entry-by-entry `Router` path
//! - AC-OPP-3: `ConnectResp` seat signal survives scrub with `keep_player_names = true`
//! - Graceful degradation when no `ConnectResp` is present
//! - Scrub-path: `player_name` is redacted when `keep_player_names = false`

mod smoke_common;

use manasight_parser::log::entry::LineBuffer;
use manasight_parser::router::Router;
use manasight_parser::{
    parse_whole_log, scrub_raw_log, scrub_raw_log_with, GameEvent, ScrubOptions,
};

// ---------------------------------------------------------------------------
// Fixture content (synthetic — no real PII; uses fabricated handles)
// ---------------------------------------------------------------------------

const FIXTURE_WITH_CONNECT_RESP: &str =
    include_str!("fixtures/seat_attribution_with_connect_resp.log");
const FIXTURE_NO_CONNECT_RESP: &str = include_str!("fixtures/seat_attribution_no_connect_resp.log");

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Feeds `input` line-by-line through a fresh `LineBuffer` + `Router`,
/// returning all collected events — the reference entry-by-entry path.
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
// Test 1: Parity — AC-ING-3
// ---------------------------------------------------------------------------

/// Asserts that `parse_whole_log` and the entry-by-entry `Router` path produce
/// the same event sequence on a real corpus file.
///
/// Requires `MANASIGHT_TEST_LOGS` to point to a directory of `.log` files.
/// Skips cleanly when the environment variable is unset.
#[test]
fn test_parse_whole_log_corpus_parity_with_router() {
    let Some(logs_dir) =
        smoke_common::logs_dir_or_skip("test_parse_whole_log_corpus_parity_with_router")
    else {
        return;
    };

    let log_files = smoke_common::discover_log_files(&logs_dir);
    assert!(
        !log_files.is_empty(),
        "MANASIGHT_TEST_LOGS directory contains no .log files: {}",
        logs_dir.display(),
    );

    // Pick the first file deterministically (sorted by discover_log_files).
    let path = &log_files[0];
    let Ok(content) = std::fs::read_to_string(path) else {
        // Corpus file unreadable — treat as a skip rather than a hard failure.
        return;
    };

    let via_whole_log = parse_whole_log(&content);
    let via_router = parse_via_router(&content);

    assert_eq!(
        via_whole_log.len(),
        via_router.len(),
        "parse_whole_log and Router path must emit the same event count for {}: \
         parse_whole_log={}, router={}",
        path.display(),
        via_whole_log.len(),
        via_router.len(),
    );

    // Compare event-by-event for exact structural equality.
    for (i, (a, b)) in via_whole_log.iter().zip(via_router.iter()).enumerate() {
        assert_eq!(
            a,
            b,
            "event[{i}] differs between parse_whole_log and Router path in {}",
            path.display(),
        );
    }
}

// ---------------------------------------------------------------------------
// Test 2: Seat signal — AC-OPP-3 (keep_player_names: true)
// ---------------------------------------------------------------------------

/// Verifies that with `keep_player_names = true`:
/// - The `ConnectResp` event has `system_seat_ids == [1]`
/// - The `reservedPlayers` entry at the non-local seat (seat 2) retains its
///   cleartext `player_name` after scrubbing.
///
/// Uses the `with_connect_resp` synthetic fixture (fabricated names + seats).
#[test]
fn test_seat_signal_keep_player_names_true_retains_opponent_name_and_local_seat() {
    let opts = ScrubOptions {
        keep_player_names: true,
    };
    let scrubbed = scrub_raw_log_with(FIXTURE_WITH_CONNECT_RESP, &opts);
    let events = parse_whole_log(&scrubbed);

    // Find the ConnectResp GameState event.
    let connect_resp_event = events
        .iter()
        .find(|e| matches!(e, GameEvent::GameState(_)) && e.payload()["type"] == "connect_resp")
        .unwrap_or_else(|| unreachable!("expected a ConnectResp GameState event"));

    // Local seat must be [1].
    let seat_ids = connect_resp_event.payload()["system_seat_ids"]
        .as_array()
        .unwrap_or_else(|| unreachable!("system_seat_ids must be a JSON array"));
    assert_eq!(seat_ids.len(), 1, "expected exactly one system_seat_id");
    assert_eq!(seat_ids[0], 1, "local seat must be 1");

    // Find the MatchState event and check the non-local player's name.
    let match_state_event = events
        .iter()
        .find(|e| matches!(e, GameEvent::MatchState(_)))
        .unwrap_or_else(|| unreachable!("expected a MatchState event"));

    let players = match_state_event.payload()["players"]
        .as_array()
        .unwrap_or_else(|| unreachable!("players must be a JSON array"));
    assert_eq!(players.len(), 2, "expected two players");

    // The non-local seat (seat_id != 1) must retain its cleartext name.
    let opponent = players
        .iter()
        .find(|p| p["system_seat_id"] == 2)
        .unwrap_or_else(|| unreachable!("expected a player with system_seat_id == 2"));

    assert_eq!(
        opponent["player_name"], "Bob#22222",
        "opponent player_name must be retained (not redacted) when keep_player_names=true"
    );
}

// ---------------------------------------------------------------------------
// Test 3: Graceful degradation — no ConnectResp
// ---------------------------------------------------------------------------

/// Verifies that when no `GREMessageType_ConnectResp` is present:
/// - The parser does not panic.
/// - No `ConnectResp` `GameState` event with `system_seat_ids` is emitted.
/// - The `MatchState` event (with both players) is still present.
///
/// Uses the `no_connect_resp` synthetic fixture.
#[test]
fn test_graceful_degradation_no_connect_resp_no_panic() {
    let events = parse_whole_log(FIXTURE_NO_CONNECT_RESP);

    // No ConnectResp event must be present — local seat is undetermined.
    let has_connect_resp = events
        .iter()
        .any(|e| matches!(e, GameEvent::GameState(_)) && e.payload()["type"] == "connect_resp");
    assert!(
        !has_connect_resp,
        "no ConnectResp event should be emitted when the log contains none"
    );

    // MatchState event must still be present (both players).
    let match_state_event = events
        .iter()
        .find(|e| matches!(e, GameEvent::MatchState(_)))
        .unwrap_or_else(|| unreachable!("expected a MatchState event even without ConnectResp"));

    let players = match_state_event.payload()["players"]
        .as_array()
        .unwrap_or_else(|| unreachable!("players must be a JSON array"));
    assert_eq!(
        players.len(),
        2,
        "both players must be present in the MatchState event"
    );
}

// ---------------------------------------------------------------------------
// Test 4: Scrub-path — keep_player_names: false
// ---------------------------------------------------------------------------

/// Verifies that with `keep_player_names = false` (default), all `player_name`
/// fields in the parsed `MatchState` event render as `<redacted>`.
///
/// Uses the `with_connect_resp` synthetic fixture.
#[test]
fn test_scrub_path_keep_player_names_false_redacts_all_names() {
    let scrubbed = scrub_raw_log(FIXTURE_WITH_CONNECT_RESP);
    let events = parse_whole_log(&scrubbed);

    let match_state_event = events
        .iter()
        .find(|e| matches!(e, GameEvent::MatchState(_)))
        .unwrap_or_else(|| unreachable!("expected a MatchState event"));

    let players = match_state_event.payload()["players"]
        .as_array()
        .unwrap_or_else(|| unreachable!("players must be a JSON array"));
    assert_eq!(players.len(), 2, "expected two players");

    for (i, player) in players.iter().enumerate() {
        assert_eq!(
            player["player_name"], "<redacted>",
            "player[{i}].player_name must be <redacted> when keep_player_names=false"
        );
    }
}
