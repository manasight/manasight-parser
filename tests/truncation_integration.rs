//! End-to-end integration test for the GSM truncation event (#200).
//!
//! Feeds a synthetic `Player.log` slice through [`LineBuffer`] + [`Router`]
//! and asserts:
//!
//! 1. A truncation marker block produces exactly one [`GameEvent::Truncation`]
//!    with the parsed `object_count` / `annotation_count`.
//! 2. Adjacent valid GRE JSON envelopes before and after the marker do NOT
//!    spuriously emit additional `Truncation` events (no false positives).

use manasight_parser::events::GameEvent;
use manasight_parser::log::entry::LineBuffer;
use manasight_parser::router::Router;

/// Walks the synthetic log, returning every event the router emits.
fn route_log(log: &str) -> Vec<GameEvent> {
    let mut buf = LineBuffer::new();
    let router = Router::new();
    let mut events = Vec::new();
    for line in log.lines() {
        for entry in buf.push_line(line) {
            events.extend(router.route(&entry));
        }
    }
    if let Some(entry) = buf.flush() {
        events.extend(router.route(&entry));
    }
    events
}

/// Synthetic log fragment: a valid GRE envelope, then the truncation marker
/// block, then a second valid GRE envelope. Mirrors the real Player.log
/// shape where Arena emits the marker in place of one specific GSM body
/// while surrounding GSMs continue to log normally.
fn synthetic_log_with_truncation_between_valid_gsms() -> String {
    let valid_gsm_before = r#"[UnityCrossThreadLogger]5/13/2026 10:01:11 AM
{"greToClientEvent":{"greToClientMessages":[{"type":"GREMessageType_GameStateMessage","msgId":1,"gameStateId":100,"gameStateMessage":{"type":"GameStateType_Diff","prevGameStateId":99,"zones":[],"gameObjects":[]}}]}}"#;
    let marker_block = "[UnityCrossThreadLogger]5/13/2026 10:01:12 AM: Match to <transaction>: GreToClientEvent\n\
        [Message summarized because one or more GameStateMessages exceeded the 50 GameObject or 50 Annotation limit.]\n\
        ::: GameStateMessage\n\
        :: GameObject Count = 63\n\
        :: Annotation Count = 4\n\
        ::: ActionsAvailableReq";
    let valid_gsm_after = r#"[UnityCrossThreadLogger]5/13/2026 10:01:13 AM
{"greToClientEvent":{"greToClientMessages":[{"type":"GREMessageType_GameStateMessage","msgId":3,"gameStateId":102,"gameStateMessage":{"type":"GameStateType_Diff","prevGameStateId":101,"zones":[],"gameObjects":[]}}]}}"#;

    format!("{valid_gsm_before}\n{marker_block}\n{valid_gsm_after}\n")
}

#[test]
fn test_truncation_marker_emits_exactly_one_truncation_event() {
    let log = synthetic_log_with_truncation_between_valid_gsms();
    let events = route_log(&log);

    let truncations: Vec<_> = events
        .iter()
        .filter(|e| matches!(e, GameEvent::Truncation(_)))
        .collect();

    assert_eq!(
        truncations.len(),
        1,
        "expected exactly 1 Truncation event from the marker block, got {} (all events: {events:?})",
        truncations.len(),
    );

    let GameEvent::Truncation(ref event) = truncations[0] else {
        unreachable!("filter guard");
    };
    assert_eq!(event.object_count(), Some(63));
    assert_eq!(event.annotation_count(), Some(4));
}

#[test]
fn test_truncation_does_not_swallow_adjacent_valid_gsms() {
    // The surrounding GRE envelopes must still emit `GameState` events —
    // the truncation routing inserts in the dispatch chain before GRE but
    // only claims entries with `EntryHeader::TruncationMarker`.
    let log = synthetic_log_with_truncation_between_valid_gsms();
    let events = route_log(&log);

    let game_states: Vec<_> = events
        .iter()
        .filter(|e| matches!(e, GameEvent::GameState(_)))
        .collect();
    assert_eq!(
        game_states.len(),
        2,
        "expected 2 GameState events (one before + one after the marker), got {} (all events: {events:?})",
        game_states.len(),
    );
}

#[test]
fn test_truncation_marker_carries_log_timestamp() {
    // The marker line itself has no embedded timestamp, but the prior
    // UCTL envelope carries one in its header. The marker becomes its own
    // entry, so the router extracts the marker entry's own timestamp from
    // its body — which is None (marker text has no date prefix). The event
    // must still emit; this matches the parser's tolerance for missing
    // timestamps.
    let log = "\
[Message summarized because one or more GameStateMessages exceeded the 50 GameObject or 50 Annotation limit.]
::: GameStateMessage
:: GameObject Count = 51
:: Annotation Count = 0
::: ActionsAvailableReq
[UnityCrossThreadLogger]5/13/2026 10:01:13 AM Next
";
    let events = route_log(log);

    let truncations: Vec<_> = events
        .iter()
        .filter(|e| matches!(e, GameEvent::Truncation(_)))
        .collect();
    assert_eq!(truncations.len(), 1);

    let GameEvent::Truncation(ref event) = truncations[0] else {
        unreachable!();
    };
    // Timestamp is None — marker entry's body has no parseable date.
    assert!(event.metadata().timestamp().is_none());
    assert_eq!(event.object_count(), Some(51));
    assert_eq!(event.annotation_count(), Some(0));
}

#[test]
fn test_no_truncation_event_from_valid_gsm_envelopes() {
    // Two adjacent valid GSMs, no marker — must emit zero Truncation events.
    let log = "\
[UnityCrossThreadLogger]5/13/2026 10:01:11 AM
{\"greToClientEvent\":{\"greToClientMessages\":[{\"type\":\"GREMessageType_GameStateMessage\",\"msgId\":1,\"gameStateId\":100,\"gameStateMessage\":{\"type\":\"GameStateType_Diff\",\"prevGameStateId\":99,\"zones\":[],\"gameObjects\":[]}}]}}
[UnityCrossThreadLogger]5/13/2026 10:01:12 AM
{\"greToClientEvent\":{\"greToClientMessages\":[{\"type\":\"GREMessageType_GameStateMessage\",\"msgId\":2,\"gameStateId\":101,\"gameStateMessage\":{\"type\":\"GameStateType_Diff\",\"prevGameStateId\":100,\"zones\":[],\"gameObjects\":[]}}]}}
";
    let events = route_log(log);

    let truncations = events
        .iter()
        .filter(|e| matches!(e, GameEvent::Truncation(_)))
        .count();
    assert_eq!(truncations, 0);
}
