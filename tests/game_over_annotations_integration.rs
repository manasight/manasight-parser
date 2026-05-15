//! Regression test for issue #196: `GameOver` GSMs that carry annotations
//! must emit a [`GameEvent::GameState`] (with the annotation arrays) before
//! the [`GameEvent::GameResult`], so annotation-walker consumers see the
//! killing damage that ends the match.
//!
//! Replays a sanitized kill-shot-via-SBA `GameOver` entry through the public
//! [`Router`] surface — the same code path a real `Player.log` line would
//! take — and asserts the two-event sequence + annotation contents.

use manasight_parser::events::GameEvent;
use manasight_parser::log::entry::{EntryHeader, LogEntry};
use manasight_parser::router::Router;

/// Sanitized fixture: a `[UnityCrossThreadLogger]greToClientEvent` entry
/// whose `GameStateMessage` carries `GameStage_GameOver` plus the lethal
/// combat damage (`AnnotationType_DamageDealt` x2, `ModifiedLife`,
/// `LossOfGame`). Mirrors the real-game capture cited in the issue.
const FIXTURE: &str = include_str!("fixtures/game_over_with_damage_annotations.txt");

#[test]
fn test_game_over_with_damage_annotations_emits_game_state_then_game_result() {
    let entry = LogEntry {
        header: EntryHeader::UnityCrossThreadLogger,
        body: FIXTURE.trim_end_matches('\n').to_owned(),
    };

    let router = Router::new();
    let events = router.route(&entry);

    assert_eq!(
        events.len(),
        2,
        "GameOver GSM with annotations must emit GameState + GameResult, got {events:?}",
    );

    // Order matters: GameState first so the annotations reach the walker
    // before the GameResult arrives at downstream consumers.
    assert!(
        matches!(events[0], GameEvent::GameState(_)),
        "first event must be GameState (carries the killing-damage annotations)",
    );
    assert!(
        matches!(events[1], GameEvent::GameResult(_)),
        "second event must be GameResult (carries the result fields)",
    );

    // The GameState payload must surface the four annotations.
    let GameEvent::GameState(ref state) = events[0] else {
        unreachable!("guarded by matches! above");
    };
    let payload = state.payload();
    let annotations = payload["annotations"]
        .as_array()
        .unwrap_or_else(|| unreachable!("annotations field must be an array"));
    assert_eq!(annotations.len(), 4);

    let damage_count = annotations
        .iter()
        .filter(|a| a["type"] == "AnnotationType_DamageDealt")
        .count();
    assert_eq!(damage_count, 2, "two DamageDealt annotations must survive");

    let has_loss = annotations
        .iter()
        .any(|a| a["type"] == "AnnotationType_LossOfGame");
    assert!(has_loss, "LossOfGame annotation must survive");

    // The GameResult payload's existing fields are still emitted correctly.
    let GameEvent::GameResult(ref result) = events[1] else {
        unreachable!("guarded by matches! above");
    };
    let result_payload = result.payload();
    assert_eq!(result_payload["winning_team_id"], 1);
    assert_eq!(result_payload["result_type"], "ResultType_WinLoss");
    assert_eq!(result_payload["reason"], "ResultReason_Game");
}
