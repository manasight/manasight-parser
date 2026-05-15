//! Corpus-replay regression test for `prev_game_state_id` extraction (#200).
//!
//! Replays a sanitized corpus slice through [`LineBuffer`] + [`Router`] and
//! asserts that every emitted `GameEvent::GameState` whose payload reports
//! `game_state_type == "GameStateType_Diff"` carries a non-null
//! `prev_game_state_id`.
//!
//! The fixture is checked into
//! `tests/fixtures/diff_gsm_prev_id_corpus_slice.log` and read via
//! `include_str!`, so this test runs unconditionally on every `cargo test`
//! invocation. Comment lines (`#`-prefixed) are stripped before replay.

use manasight_parser::events::GameEvent;
use manasight_parser::log::entry::LineBuffer;
use manasight_parser::router::Router;

const FIXTURE: &str = include_str!("fixtures/diff_gsm_prev_id_corpus_slice.log");

/// Loads the fixture body, stripping comment lines and trailing `\r`.
fn fixture_lines() -> Vec<&'static str> {
    FIXTURE
        .lines()
        .filter(|line| !line.starts_with('#'))
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .collect()
}

/// Routes every line in the fixture and collects all resulting events.
fn route_fixture() -> Vec<GameEvent> {
    let mut buf = LineBuffer::new();
    let router = Router::new();
    let mut events = Vec::new();
    for line in fixture_lines() {
        for entry in buf.push_line(line) {
            events.extend(router.route(&entry));
        }
    }
    if let Some(entry) = buf.flush() {
        events.extend(router.route(&entry));
    }
    events
}

/// The fixture must produce at least one `GameStateType_Diff` GSM so the
/// invariant assertion below is exercising real Diff data, not a no-op over
/// an empty filter result.
#[test]
fn test_fixture_contains_at_least_one_diff_gsm() {
    let events = route_fixture();
    let diff_count = events
        .iter()
        .filter_map(|e| match e {
            GameEvent::GameState(state)
                if state.payload()["game_state_type"] == "GameStateType_Diff" =>
            {
                Some(state)
            }
            _ => None,
        })
        .count();
    assert!(
        diff_count >= 1,
        "fixture must contain ≥1 Diff GSM (sanity check) — got {diff_count}",
    );
}

/// Core invariant: every Diff GSM payload in the corpus replay carries a
/// non-null `prev_game_state_id`. Arena emits the `prevGameStateId` field on
/// every Diff GSM wire payload; if the parser drops it, downstream gap
/// detection silently regresses.
#[test]
fn test_every_diff_gsm_has_non_null_prev_game_state_id() {
    let events = route_fixture();
    let mut checked = 0usize;
    for event in &events {
        let GameEvent::GameState(state) = event else {
            continue;
        };
        if state.payload()["game_state_type"] != "GameStateType_Diff" {
            continue;
        }
        let prev = &state.payload()["prev_game_state_id"];
        assert!(
            !prev.is_null(),
            "Diff GSM at game_state_id {gsid} has null prev_game_state_id; full payload: {payload}",
            gsid = state.payload()["game_state_id"],
            payload = state.payload(),
        );
        assert!(
            prev.is_i64(),
            "prev_game_state_id must serialize as a number; got {prev:?}",
        );
        checked += 1;
    }
    assert!(
        checked >= 1,
        "expected the invariant loop to run at least once",
    );
}

/// Full GSMs may legitimately omit `prevGameStateId` — assert the parser
/// emits JSON `null` for the field in that case rather than dropping the key.
#[test]
fn test_full_gsm_in_fixture_serializes_prev_id_as_null_if_absent() {
    let events = route_fixture();
    for event in &events {
        let GameEvent::GameState(state) = event else {
            continue;
        };
        if state.payload()["game_state_type"] != "GameStateType_Full" {
            continue;
        }
        // Key must be present; value may be a number (when the wire payload
        // included it) or null (when omitted). Both are acceptable.
        assert!(
            state.payload().get("prev_game_state_id").is_some(),
            "Full GSM payload must always carry the `prev_game_state_id` key",
        );
    }
}
