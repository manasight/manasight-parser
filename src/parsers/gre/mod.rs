//! GRE message parsers for `greToClientEvent` payloads.
//!
//! Covers `GameStateMessage` (zones, game objects), `ConnectResp` (initial
//! game configuration), `QueuedGameStateMessage`, and low-value noise types
//! (`UIMessage`, `TimerStateMessage`, `SetSettingsResp`).
//!
//! # Message types handled
//!
//! | GRE Type | Payload Key | Extracted Fields |
//! |----------|-------------|------------------|
//! | `GREMessageType_ConnectResp` | `connectResp` | Decklist, sideboard, seat IDs, settings |
//! | `GREMessageType_GameStateMessage` | `gameStateMessage` | Zones, game objects, game info |
//! | `GREMessageType_QueuedGameStateMessage` | `gameStateMessage` | Same as `GameStateMessage` |
//! | `GREMessageType_UIMessage` | — | Claimed as noise (emotes, hover notifications) |
//! | `GREMessageType_TimerStateMessage` | — | Claimed as noise (turn timer state) |
//! | `GREMessageType_SetSettingsResp` | — | Claimed as noise (settings acknowledgment) |
//!
//! `ConnectResp` is emitted once at game start; `GameStateMessage` fires on
//! every game state change (the most frequent event in a game);
//! `QueuedGameStateMessage` wraps a deferred game state update with the same
//! structure.
//!
//! Most messages are Class 1 (Interactive Dispatch). The exception is when
//! `gameInfo.stage` equals `GameStage_GameOver` with
//! `matchState != MatchState_MatchComplete` — these are emitted as
//! `GameEvent::GameResult` (Class 3, Post-Game Batch) to trigger batch
//! assembly in downstream consumers. Arena sends two `GameStage_GameOver`
//! messages per game end (`MatchState_GameComplete` + `MatchState_MatchComplete`);
//! only the game-complete signal is emitted to avoid duplicate results.
//!
//! ## `GameStateMessage` structure
//!
//! The `gameStateMessage` payload contains:
//!
//! - **`zones`**: array of zone descriptors (hand, library, battlefield,
//!   graveyard, exile, stack, limbo, etc.) each with `zoneId`, `type`,
//!   `ownerSeatId`, and `objectInstanceIds`.
//! - **`gameObjects`**: array of game object descriptors, each with
//!   `instanceId`, `grpId` (Arena card ID), `zoneId`, `ownerSeatId`,
//!   `controllerSeatId`, `type`, `visibility`, `cardTypes`, `subtypes`,
//!   `name`, `power`, `toughness`, etc.
//! - **`gameInfo`**: game-level metadata (match/game IDs, mulligan type,
//!   stage, variant, etc.).
//!
//! Incremental updates include only changed zones/objects. The parser
//! extracts whatever is present without requiring all fields.
//!
//! Turn info is extracted as a structured `turn_info` sub-object (B-7d partial).
//! Annotations are extracted from the `annotations` array (B-7d-b), with
//! special handling for `ZoneTransfer`, `ObjectIdChanged`, `DamageDealt`,
//! `CounterAdded`, `TargetSpec`, and `ModifiedLife` (B-15). Inline timers
//! are extracted from `gameStateMessage.timers` (B-15).

mod annotations;
mod connect_resp;
mod game_result;
mod game_state;
mod helpers;
mod turn_info;

#[cfg(test)]
mod test_fixtures;

use crate::events::{EventMetadata, GameEvent, GameResultEvent, GameStateEvent};
use crate::log::entry::LogEntry;
use crate::parsers::api_common;

/// Marker that identifies a GRE-to-client event entry in the log.
const GRE_TO_CLIENT_MARKER: &str = "greToClientEvent";

/// GRE message type for the initial connection response.
const CONNECT_RESP_TYPE: &str = "GREMessageType_ConnectResp";

/// GRE message type for game state updates.
const GAME_STATE_MESSAGE_TYPE: &str = "GREMessageType_GameStateMessage";

/// GRE message type for queued (deferred) game state updates.
const QUEUED_GAME_STATE_MESSAGE_TYPE: &str = "GREMessageType_QueuedGameStateMessage";

/// GRE message type for UI messages (opponent emotes, hover notifications).
const UI_MESSAGE_TYPE: &str = "GREMessageType_UIMessage";

/// GRE message type for turn timer state updates.
const TIMER_STATE_MESSAGE_TYPE: &str = "GREMessageType_TimerStateMessage";

/// GRE message type for settings acknowledgment responses.
const SET_SETTINGS_RESP_TYPE: &str = "GREMessageType_SetSettingsResp";

/// Low-value GRE message types that are claimed without rich payload extraction.
///
/// These are recognized so they count as "claimed" entries in diagnostics,
/// reducing noise in the unclaimed-entry residual.
const NOISE_MESSAGE_TYPES: &[&str] = &[
    UI_MESSAGE_TYPE,
    TIMER_STATE_MESSAGE_TYPE,
    SET_SETTINGS_RESP_TYPE,
];

/// Game info stage value indicating the game has ended.
const GAME_STAGE_GAME_OVER: &str = "GameStage_GameOver";

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Attempts to parse a [`LogEntry`] as one or more GRE events.
///
/// Dispatches to the appropriate sub-parser based on the GRE message type:
/// - `GREMessageType_ConnectResp` -> `connect_resp` payload (single)
/// - `GREMessageType_GameStateMessage` -> `game_state_message` payload
///   (or `game_result` if `gameInfo.stage == GameStage_GameOver`)
/// - `GREMessageType_QueuedGameStateMessage` -> same as `GameStateMessage`
///
/// Arena frequently batches multiple `GameStateMessage` entries into a
/// single `greToClientMessages` array. This function iterates **all**
/// matching messages and returns a `Vec<GameEvent>` — one event per
/// message. `ConnectResp` and noise types remain single-event.
///
/// Returns an empty `Vec` if the entry does not match.
///
/// The `timestamp` is `None` when the log entry header did not contain a
/// parseable timestamp. It is passed through to [`EventMetadata`] so
/// downstream consumers can distinguish real vs missing timestamps.
pub fn try_parse(
    entry: &LogEntry,
    timestamp: Option<chrono::DateTime<chrono::Utc>>,
) -> Vec<GameEvent> {
    let body = &entry.body;

    // Quick check: bail early if the GRE marker is not present.
    if !body.contains(GRE_TO_CLIENT_MARKER) {
        return Vec::new();
    }

    // Extract and parse the JSON payload from the body.
    let Some(parsed) = api_common::parse_json_from_body(body, "greToClientEvent") else {
        return Vec::new();
    };

    let Some(messages) = extract_gre_messages(&parsed) else {
        return Vec::new();
    };

    // Try ConnectResp first (highest priority, emitted once at game start).
    if let Some(connect_resp_msg) = find_message_by_type(messages, CONNECT_RESP_TYPE) {
        let payload = connect_resp::build_connect_resp_payload(connect_resp_msg, &parsed);
        let metadata = EventMetadata::new(timestamp, body.as_bytes().to_vec());
        return vec![GameEvent::GameState(GameStateEvent::new(metadata, payload))];
    }

    // Iterate ALL GameStateMessage and QueuedGameStateMessage entries.
    // Arena batches multiple GSMs into a single greToClientMessages array;
    // processing only the first silently discards the majority of updates.
    let mut events = Vec::new();
    for msg in messages {
        let msg_type = msg.get("type").and_then(serde_json::Value::as_str);
        if let Some(GAME_STATE_MESSAGE_TYPE | QUEUED_GAME_STATE_MESSAGE_TYPE) = msg_type {
            let metadata = EventMetadata::new(timestamp, body.as_bytes().to_vec());
            if game_result::is_game_over(msg) && !game_result::is_match_complete(msg) {
                let payload = game_result::build_game_result_payload(msg);
                events.push(GameEvent::GameResult(GameResultEvent::new(
                    metadata, payload,
                )));
            } else if !game_result::is_game_over(msg) {
                let payload = game_state::build_game_state_message_payload(msg);
                events.push(GameEvent::GameState(GameStateEvent::new(metadata, payload)));
            }
        }
    }

    if !events.is_empty() {
        return events;
    }

    // Check for low-value noise types (UIMessage, TimerStateMessage,
    // SetSettingsResp). Claimed with a minimal payload so they don't inflate
    // the unclaimed-entry residual.
    for &noise_type in NOISE_MESSAGE_TYPES {
        if find_message_by_type(messages, noise_type).is_some() {
            ::log::trace!("greToClientEvent: claimed noise type {noise_type}");
            let payload = serde_json::json!({ "recognized_type": noise_type });
            let metadata = EventMetadata::new(timestamp, body.as_bytes().to_vec());
            return vec![GameEvent::GameState(GameStateEvent::new(metadata, payload))];
        }
    }

    // Unrecognized GRE message type — log and skip.
    ::log::debug!("greToClientEvent: no recognized message type found");
    Vec::new()
}

// ---------------------------------------------------------------------------
// GRE message extraction helpers
// ---------------------------------------------------------------------------

/// Extracts the `greToClientMessages` array from the parsed JSON.
///
/// Handles both the nested format (`{ "greToClientEvent": { "greToClientMessages": [...] } }`)
/// and the flat format (`{ "greToClientMessages": [...] }`).
fn extract_gre_messages(parsed: &serde_json::Value) -> Option<&Vec<serde_json::Value>> {
    parsed
        .get("greToClientEvent")
        .and_then(|e| e.get("greToClientMessages"))
        .or_else(|| parsed.get("greToClientMessages"))
        .and_then(serde_json::Value::as_array)
        .filter(|msgs| !msgs.is_empty())
}

/// Searches the `greToClientMessages` array for a message with the given type.
fn find_message_by_type<'a>(
    messages: &'a [serde_json::Value],
    msg_type: &str,
) -> Option<&'a serde_json::Value> {
    messages
        .iter()
        .find(|msg| msg.get("type").and_then(serde_json::Value::as_str) == Some(msg_type))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parsers::test_helpers::{
        game_result_payload, game_state_payload, test_timestamp, unity_entry, EntryHeader,
    };
    use test_fixtures::*;

    // -- ConnectResp detection ------------------------------------------------

    mod connect_resp_detection {
        use super::*;

        #[test]
        fn test_try_parse_connect_resp_detected() {
            let body = connect_resp_body();
            let entry = unity_entry(&body);
            let results = try_parse(&entry, Some(test_timestamp()));
            assert_eq!(results.len(), 1);
        }

        #[test]
        fn test_try_parse_connect_resp_correct_variant() {
            let body = connect_resp_body();
            let entry = unity_entry(&body);
            let results = try_parse(&entry, Some(test_timestamp()));
            assert_eq!(results.len(), 1);
            assert!(matches!(&results[0], GameEvent::GameState(_)));
        }

        #[test]
        fn test_try_parse_connect_resp_type_field() {
            let body = connect_resp_body();
            let entry = unity_entry(&body);
            let results = try_parse(&entry, Some(test_timestamp()));
            assert_eq!(results.len(), 1);
            let payload = game_state_payload(&results[0]);
            assert_eq!(payload["type"], "connect_resp");
        }
    }

    // -- Flat format (no greToClientEvent wrapper) ----------------------------

    mod flat_format {
        use super::*;

        #[test]
        fn test_try_parse_flat_format_detected() {
            let body = flat_connect_resp_body();
            let entry = unity_entry(&body);
            let results = try_parse(&entry, Some(test_timestamp()));
            assert_eq!(results.len(), 1);
        }

        #[test]
        fn test_try_parse_flat_format_deck_cards() {
            let body = flat_connect_resp_body();
            let entry = unity_entry(&body);
            let results = try_parse(&entry, Some(test_timestamp()));
            assert_eq!(results.len(), 1);
            let payload = game_state_payload(&results[0]);

            let deck_cards = payload["deck_cards"].as_array();
            assert!(deck_cards.is_some());
            let deck_cards = deck_cards.unwrap_or_else(|| unreachable!());
            assert_eq!(deck_cards.len(), 3);
            assert_eq!(deck_cards[0], 11111);
            assert_eq!(deck_cards[1], 22222);
            assert_eq!(deck_cards[2], 33333);
        }

        #[test]
        fn test_try_parse_flat_format_sideboard() {
            let body = flat_connect_resp_body();
            let entry = unity_entry(&body);
            let results = try_parse(&entry, Some(test_timestamp()));
            assert_eq!(results.len(), 1);
            let payload = game_state_payload(&results[0]);

            let sideboard = payload["sideboard_cards"].as_array();
            assert!(sideboard.is_some());
            let sideboard = sideboard.unwrap_or_else(|| unreachable!());
            assert_eq!(sideboard.len(), 1);
            assert_eq!(sideboard[0], 44444);
        }

        #[test]
        fn test_try_parse_flat_format_settings() {
            let body = flat_connect_resp_body();
            let entry = unity_entry(&body);
            let results = try_parse(&entry, Some(test_timestamp()));
            assert_eq!(results.len(), 1);
            let payload = game_state_payload(&results[0]);

            assert_eq!(
                payload["settings"]["autoPassOption"],
                "AutoPassOption_ResolveAll"
            );
        }

        #[test]
        fn test_try_parse_flat_format_msg_id() {
            let body = flat_connect_resp_body();
            let entry = unity_entry(&body);
            let results = try_parse(&entry, Some(test_timestamp()));
            assert_eq!(results.len(), 1);
            let payload = game_state_payload(&results[0]);
            assert_eq!(payload["msg_id"], 2);
        }

        #[test]
        fn test_try_parse_flat_format_game_state_message() {
            let body = flat_game_state_message_body();
            let entry = unity_entry(&body);
            let results = try_parse(&entry, Some(test_timestamp()));
            assert_eq!(results.len(), 1);
            let payload = game_state_payload(&results[0]);
            assert_eq!(payload["type"], "game_state_message");
            assert_eq!(payload["msg_id"], 3);
        }
    }

    // -- Raw event preservation -----------------------------------------------

    mod raw_event {
        use super::*;

        #[test]
        fn test_try_parse_connect_resp_preserves_raw_connect_resp() {
            let body = connect_resp_body();
            let entry = unity_entry(&body);
            let results = try_parse(&entry, Some(test_timestamp()));
            assert_eq!(results.len(), 1);
            let payload = game_state_payload(&results[0]);

            assert!(payload.get("raw_connect_resp").is_some());
            // The raw event should contain the greToClientEvent wrapper.
            assert!(payload["raw_connect_resp"]
                .get("greToClientEvent")
                .is_some());
        }

        #[test]
        fn test_try_parse_connect_resp_preserves_raw_bytes() {
            let body = connect_resp_body();
            let entry = unity_entry(&body);
            let results = try_parse(&entry, Some(test_timestamp()));
            assert_eq!(results.len(), 1);
            assert_eq!(results[0].metadata().raw_bytes(), body.as_bytes());
        }

        #[test]
        fn test_try_parse_connect_resp_stores_timestamp() {
            let body = connect_resp_body();
            let entry = unity_entry(&body);
            let ts = Some(test_timestamp());
            let results = try_parse(&entry, ts);
            assert_eq!(results.len(), 1);
            assert_eq!(results[0].metadata().timestamp(), ts);
        }
    }

    // -- Non-GRE entries (should return empty Vec) ----------------------------

    mod non_gre_entries {
        use super::*;

        #[test]
        fn test_try_parse_unrelated_entry_returns_empty() {
            let body = "[UnityCrossThreadLogger]Updated account. DisplayName:Test";
            let entry = unity_entry(body);
            assert!(try_parse(&entry, Some(test_timestamp())).is_empty());
        }

        #[test]
        fn test_try_parse_empty_body_returns_empty() {
            let body = "[UnityCrossThreadLogger]";
            let entry = unity_entry(body);
            assert!(try_parse(&entry, Some(test_timestamp())).is_empty());
        }

        #[test]
        fn test_try_parse_match_state_event_returns_empty() {
            let body = "[UnityCrossThreadLogger]matchGameRoomStateChangedEvent\n\
                         {\"matchGameRoomStateChangedEvent\": {\"gameRoomInfo\": {}}}";
            let entry = unity_entry(body);
            assert!(try_parse(&entry, Some(test_timestamp())).is_empty());
        }

        #[test]
        fn test_try_parse_no_json_body_returns_empty() {
            let body = "[UnityCrossThreadLogger]greToClientEvent with no json";
            let entry = unity_entry(body);
            assert!(try_parse(&entry, Some(test_timestamp())).is_empty());
        }

        #[test]
        fn test_try_parse_malformed_json_returns_empty() {
            let body = "[UnityCrossThreadLogger]greToClientEvent\n{invalid json}";
            let entry = unity_entry(body);
            assert!(try_parse(&entry, Some(test_timestamp())).is_empty());
        }

        #[test]
        fn test_try_parse_gre_event_without_messages_returns_empty() {
            let body = format!(
                "[UnityCrossThreadLogger]greToClientEvent\n{}",
                serde_json::json!({
                    "greToClientEvent": {
                        "someOtherField": "value"
                    }
                })
            );
            let entry = unity_entry(&body);
            assert!(try_parse(&entry, Some(test_timestamp())).is_empty());
        }

        #[test]
        fn test_try_parse_gre_event_empty_messages_returns_empty() {
            let body = format!(
                "[UnityCrossThreadLogger]greToClientEvent\n{}",
                serde_json::json!({
                    "greToClientEvent": {
                        "greToClientMessages": []
                    }
                })
            );
            let entry = unity_entry(&body);
            assert!(try_parse(&entry, Some(test_timestamp())).is_empty());
        }

        #[test]
        fn test_try_parse_client_gre_header_is_accepted() {
            let body = format!(
                "[Client GRE]greToClientEvent\n{}",
                serde_json::json!({
                    "greToClientEvent": {
                        "greToClientMessages": [
                            {
                                "type": "GREMessageType_ConnectResp",
                                "systemSeatIds": [1, 2],
                                "connectResp": {
                                    "deckMessage": {"deckCards": [1, 2, 3]}
                                }
                            }
                        ]
                    }
                })
            );
            let entry = LogEntry {
                header: EntryHeader::ClientGre,
                body: body.clone(),
            };
            // Note: This returns events because the parser only checks the body
            // content, not the header type. The GRE marker is present in the
            // body. This is valid -- ConnectResp can appear under either header.
            let results = try_parse(&entry, Some(test_timestamp()));
            assert_eq!(results.len(), 1);
        }

        #[test]
        fn test_try_parse_unknown_gre_message_type_returns_empty() {
            let body = format!(
                "[UnityCrossThreadLogger]greToClientEvent\n{}",
                serde_json::json!({
                    "greToClientEvent": {
                        "greToClientMessages": [
                            {
                                "type": "GREMessageType_SomeUnknownType",
                                "data": {}
                            }
                        ]
                    }
                })
            );
            let entry = unity_entry(&body);
            assert!(try_parse(&entry, Some(test_timestamp())).is_empty());
        }
    }

    // -- Performance class ---------------------------------------------------

    mod performance_class {
        use super::*;
        use crate::events::PerformanceClass;

        #[test]
        fn test_try_parse_connect_resp_performance_class_interactive_dispatch() {
            let body = connect_resp_body();
            let entry = unity_entry(&body);
            let results = try_parse(&entry, Some(test_timestamp()));
            assert_eq!(results.len(), 1);
            assert_eq!(
                results[0].performance_class(),
                PerformanceClass::InteractiveDispatch
            );
        }

        #[test]
        fn test_try_parse_game_state_message_performance_class_interactive_dispatch() {
            let body = game_state_message_body();
            let entry = unity_entry(&body);
            let results = try_parse(&entry, Some(test_timestamp()));
            assert_eq!(results.len(), 1);
            assert_eq!(
                results[0].performance_class(),
                PerformanceClass::InteractiveDispatch
            );
        }

        #[test]
        fn test_try_parse_queued_game_state_message_performance_class_interactive_dispatch() {
            let body = queued_game_state_message_body();
            let entry = unity_entry(&body);
            let results = try_parse(&entry, Some(test_timestamp()));
            assert_eq!(results.len(), 1);
            assert_eq!(
                results[0].performance_class(),
                PerformanceClass::InteractiveDispatch
            );
        }
    }

    // -- Internal helpers ----------------------------------------------------

    mod helpers_tests {
        use super::*;

        #[test]
        fn test_extract_gre_messages_nested_format() {
            let parsed = serde_json::json!({
                "greToClientEvent": {
                    "greToClientMessages": [
                        {"type": "GREMessageType_ConnectResp"}
                    ]
                }
            });
            let messages = extract_gre_messages(&parsed);
            assert!(messages.is_some());
            let messages = messages.unwrap_or_else(|| unreachable!());
            assert_eq!(messages.len(), 1);
        }

        #[test]
        fn test_extract_gre_messages_flat_format() {
            let parsed = serde_json::json!({
                "greToClientMessages": [
                    {"type": "GREMessageType_GameStateMessage"}
                ]
            });
            let messages = extract_gre_messages(&parsed);
            assert!(messages.is_some());
            let messages = messages.unwrap_or_else(|| unreachable!());
            assert_eq!(messages.len(), 1);
        }

        #[test]
        fn test_extract_gre_messages_empty_returns_none() {
            let parsed = serde_json::json!({
                "greToClientEvent": {
                    "greToClientMessages": []
                }
            });
            assert!(extract_gre_messages(&parsed).is_none());
        }

        #[test]
        fn test_extract_gre_messages_missing_returns_none() {
            let parsed = serde_json::json!({
                "greToClientEvent": {
                    "someOtherField": "value"
                }
            });
            assert!(extract_gre_messages(&parsed).is_none());
        }

        #[test]
        fn test_find_message_by_type_found() {
            let messages = vec![
                serde_json::json!({"type": "GREMessageType_GameStateMessage"}),
                serde_json::json!({"type": "GREMessageType_ConnectResp"}),
            ];
            let result = find_message_by_type(&messages, CONNECT_RESP_TYPE);
            assert!(result.is_some());
        }

        #[test]
        fn test_find_message_by_type_not_found() {
            let messages = vec![serde_json::json!({"type": "GREMessageType_GameStateMessage"})];
            let result = find_message_by_type(&messages, CONNECT_RESP_TYPE);
            assert!(result.is_none());
        }
    }

    // -- Noise message types -------------------------------------------------

    mod noise_message_types {
        use super::*;

        /// Helper: build a GRE entry with a single message of the given type.
        fn gre_entry_with_type(msg_type: &str) -> LogEntry {
            let body = format!(
                "[UnityCrossThreadLogger]greToClientEvent\n{}",
                serde_json::json!({
                    "greToClientEvent": {
                        "greToClientMessages": [
                            {
                                "type": msg_type,
                                "data": {}
                            }
                        ]
                    }
                })
            );
            unity_entry(&body)
        }

        #[test]
        fn test_try_parse_ui_message_returns_event() {
            let entry = gre_entry_with_type("GREMessageType_UIMessage");
            let results = try_parse(&entry, Some(test_timestamp()));
            assert_eq!(results.len(), 1);
            assert!(matches!(&results[0], GameEvent::GameState(_)));
            let payload = game_state_payload(&results[0]);
            assert_eq!(payload["recognized_type"], "GREMessageType_UIMessage");
        }

        #[test]
        fn test_try_parse_timer_state_message_returns_event() {
            let entry = gre_entry_with_type("GREMessageType_TimerStateMessage");
            let results = try_parse(&entry, Some(test_timestamp()));
            assert_eq!(results.len(), 1);
            assert!(matches!(&results[0], GameEvent::GameState(_)));
            let payload = game_state_payload(&results[0]);
            assert_eq!(
                payload["recognized_type"],
                "GREMessageType_TimerStateMessage"
            );
        }

        #[test]
        fn test_try_parse_set_settings_resp_returns_event() {
            let entry = gre_entry_with_type("GREMessageType_SetSettingsResp");
            let results = try_parse(&entry, Some(test_timestamp()));
            assert_eq!(results.len(), 1);
            assert!(matches!(&results[0], GameEvent::GameState(_)));
            let payload = game_state_payload(&results[0]);
            assert_eq!(payload["recognized_type"], "GREMessageType_SetSettingsResp");
        }

        #[test]
        fn test_noise_types_preserve_metadata_raw_bytes() {
            let entry = gre_entry_with_type("GREMessageType_UIMessage");
            let results = try_parse(&entry, Some(test_timestamp()));
            assert_eq!(results.len(), 1);
            assert!(!results[0].metadata().raw_bytes().is_empty());
        }

        #[test]
        fn test_noise_types_prioritize_real_events() {
            // If a message array has both a GameStateMessage and a UIMessage,
            // the GameStateMessage should be returned (GSMs are iterated first).
            let body = format!(
                "[UnityCrossThreadLogger]greToClientEvent\n{}",
                serde_json::json!({
                    "greToClientEvent": {
                        "greToClientMessages": [
                            {
                                "type": "GREMessageType_UIMessage",
                                "data": {}
                            },
                            {
                                "type": "GREMessageType_GameStateMessage",
                                "gameStateMessage": {
                                    "gameInfo": {},
                                    "zones": [],
                                    "gameObjects": []
                                }
                            }
                        ]
                    }
                })
            );
            let entry = unity_entry(&body);
            let results = try_parse(&entry, Some(test_timestamp()));
            assert_eq!(results.len(), 1);
            // Should be GameState from GameStateMessage, not from UIMessage noise
            let payload = game_state_payload(&results[0]);
            assert!(payload.get("recognized_type").is_none());
        }

        #[test]
        fn test_truly_unknown_type_still_returns_empty() {
            let entry = gre_entry_with_type("GREMessageType_SomeFutureType");
            assert!(try_parse(&entry, Some(test_timestamp())).is_empty());
        }
    }

    // -- Batched message processing -------------------------------------------

    mod batched_messages {
        use super::*;

        #[test]
        fn test_try_parse_batched_gsms_returns_all_events() {
            let body = batched_game_state_messages_body();
            let entry = unity_entry(&body);
            let results = try_parse(&entry, Some(test_timestamp()));
            assert_eq!(results.len(), 3);
            for event in &results {
                assert!(matches!(event, GameEvent::GameState(_)));
            }
        }

        #[test]
        fn test_try_parse_batched_gsms_preserve_msg_ids() {
            let body = batched_game_state_messages_body();
            let entry = unity_entry(&body);
            let results = try_parse(&entry, Some(test_timestamp()));
            assert_eq!(results.len(), 3);
            let msg_ids: Vec<_> = results
                .iter()
                .map(|e| game_state_payload(e)["msg_id"].as_u64())
                .collect();
            assert_eq!(msg_ids, vec![Some(10), Some(11), Some(12)]);
        }

        #[test]
        fn test_try_parse_batched_gsms_preserve_game_state_ids() {
            let body = batched_game_state_messages_body();
            let entry = unity_entry(&body);
            let results = try_parse(&entry, Some(test_timestamp()));
            assert_eq!(results.len(), 3);
            let gs_ids: Vec<_> = results
                .iter()
                .map(|e| game_state_payload(e)["game_state_id"].as_u64())
                .collect();
            assert_eq!(gs_ids, vec![Some(100), Some(101), Some(102)]);
        }

        #[test]
        fn test_try_parse_batched_qgsms_returns_all_events() {
            let body = batched_queued_game_state_messages_body();
            let entry = unity_entry(&body);
            let results = try_parse(&entry, Some(test_timestamp()));
            assert_eq!(results.len(), 2);
            for event in &results {
                assert!(matches!(event, GameEvent::GameState(_)));
            }
        }

        #[test]
        fn test_try_parse_mixed_gsm_qgsm_returns_all_events() {
            let body = mixed_gsm_qgsm_body();
            let entry = unity_entry(&body);
            let results = try_parse(&entry, Some(test_timestamp()));
            assert_eq!(results.len(), 2);
            let msg_ids: Vec<_> = results
                .iter()
                .map(|e| game_state_payload(e)["msg_id"].as_u64())
                .collect();
            assert_eq!(msg_ids, vec![Some(30), Some(31)]);
        }

        #[test]
        fn test_try_parse_batched_gsm_with_game_over_returns_mixed_types() {
            let body = batched_gsm_with_game_over_body();
            let entry = unity_entry(&body);
            let results = try_parse(&entry, Some(test_timestamp()));
            assert_eq!(results.len(), 2);
            assert!(matches!(&results[0], GameEvent::GameState(_)));
            assert!(matches!(&results[1], GameEvent::GameResult(_)));
        }

        #[test]
        fn test_try_parse_dual_game_over_emits_single_game_result() {
            let body = batched_dual_game_over_body();
            let entry = unity_entry(&body);
            let results = try_parse(&entry, Some(test_timestamp()));
            // Should emit exactly 1 GameResult (GameComplete), not 2.
            let game_results: Vec<_> = results
                .iter()
                .filter(|e| matches!(e, GameEvent::GameResult(_)))
                .collect();
            assert_eq!(game_results.len(), 1);
        }

        #[test]
        fn test_try_parse_dual_game_over_uses_game_complete() {
            let body = batched_dual_game_over_body();
            let entry = unity_entry(&body);
            let results = try_parse(&entry, Some(test_timestamp()));
            let game_result = results
                .iter()
                .find(|e| matches!(e, GameEvent::GameResult(_)))
                .unwrap_or_else(|| unreachable!());
            let payload = game_result_payload(game_result);
            assert_eq!(payload["match_state"], "MatchState_GameComplete");
        }

        #[test]
        fn test_try_parse_dual_game_over_skips_match_complete_entirely() {
            let body = batched_dual_game_over_body();
            let entry = unity_entry(&body);
            let results = try_parse(&entry, Some(test_timestamp()));
            // MatchComplete should not appear as GameState or GameResult.
            assert_eq!(results.len(), 1);
            assert!(matches!(&results[0], GameEvent::GameResult(_)));
        }

        #[test]
        fn test_try_parse_gsm_plus_noise_ignores_noise() {
            // When GSMs and noise types coexist, only GSMs produce events.
            let body = format!(
                "[UnityCrossThreadLogger]greToClientEvent\n{}",
                serde_json::json!({
                    "greToClientEvent": {
                        "greToClientMessages": [
                            {
                                "type": "GREMessageType_TimerStateMessage",
                                "data": {}
                            },
                            {
                                "type": "GREMessageType_GameStateMessage",
                                "msgId": 50,
                                "gameStateId": 500,
                                "gameStateMessage": {
                                    "zones": [],
                                    "gameObjects": [],
                                    "gameInfo": {"stage": "GameStage_Play"}
                                }
                            },
                            {
                                "type": "GREMessageType_UIMessage",
                                "data": {}
                            }
                        ]
                    }
                })
            );
            let entry = unity_entry(&body);
            let results = try_parse(&entry, Some(test_timestamp()));
            assert_eq!(results.len(), 1);
            let payload = game_state_payload(&results[0]);
            assert_eq!(payload["msg_id"], 50);
            assert!(payload.get("recognized_type").is_none());
        }

        #[test]
        fn test_try_parse_batched_gsms_share_raw_bytes() {
            // All events from a batch share the same raw_bytes (full entry).
            let body = batched_game_state_messages_body();
            let entry = unity_entry(&body);
            let results = try_parse(&entry, Some(test_timestamp()));
            assert_eq!(results.len(), 3);
            let expected_bytes = body.as_bytes();
            for event in &results {
                assert_eq!(event.metadata().raw_bytes(), expected_bytes);
            }
        }

        #[test]
        fn test_try_parse_batched_gsms_share_timestamp() {
            let body = batched_game_state_messages_body();
            let entry = unity_entry(&body);
            let ts = Some(test_timestamp());
            let results = try_parse(&entry, ts);
            assert_eq!(results.len(), 3);
            for event in &results {
                assert_eq!(event.metadata().timestamp(), ts);
            }
        }
    }
}
