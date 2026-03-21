//! Game result detection and payload builder for `GameStage_GameOver` messages.

/// Returns `true` if the GRE message contains `GameStage_GameOver` in
/// `gameStateMessage.gameInfo.stage`.
pub(super) fn is_game_over(gre_msg: &serde_json::Value) -> bool {
    gre_msg
        .get("gameStateMessage")
        .and_then(|gsm| gsm.get("gameInfo"))
        .and_then(|gi| gi.get("stage"))
        .and_then(serde_json::Value::as_str)
        == Some(super::GAME_STAGE_GAME_OVER)
}

/// Match state value indicating the overall match has ended.
const MATCH_STATE_MATCH_COMPLETE: &str = "MatchState_MatchComplete";

/// Returns `true` if the GRE message has `matchState == MatchState_MatchComplete`.
///
/// Arena batches two `GameStage_GameOver` messages per game end:
/// `MatchState_GameComplete` (game scope) and `MatchState_MatchComplete`
/// (match scope). We emit only the game-complete signal to avoid duplicates
/// and to keep consistent semantics for Bo1 and Bo3.
pub(super) fn is_match_complete(gre_msg: &serde_json::Value) -> bool {
    gre_msg
        .get("gameStateMessage")
        .and_then(|gsm| gsm.get("gameInfo"))
        .and_then(|gi| gi.get("matchState"))
        .and_then(serde_json::Value::as_str)
        == Some(MATCH_STATE_MATCH_COMPLETE)
}

/// Builds a structured payload for a game result extracted from a GRE
/// `GameStateMessage` with `GameStage_GameOver`.
///
/// Extracts result details from `gameInfo.results[]` and metadata from
/// `gameInfo`. The output payload has the shape:
///
/// ```json
/// {
///   "type": "game_result",
///   "source": "gre_game_state",
///   "stage": "GameStage_GameOver",
///   "match_state": "MatchState_GameComplete",
///   "results": [...],
///   "winning_team_id": 1,
///   "result_type": "ResultType_WinLoss",
///   "reason": "ResultReason_Game",
///   "game_info": { ... }
/// }
/// ```
pub(super) fn build_game_result_payload(gre_msg: &serde_json::Value) -> serde_json::Value {
    let game_info = gre_msg
        .get("gameStateMessage")
        .and_then(|gsm| gsm.get("gameInfo"));

    let stage = game_info
        .and_then(|gi| gi.get("stage"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");

    let match_state = game_info
        .and_then(|gi| gi.get("matchState"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");

    let results = game_info
        .and_then(|gi| gi.get("results"))
        .cloned()
        .unwrap_or(serde_json::Value::Array(Vec::new()));

    // Find the latest MatchScope_Game result for top-level convenience fields.
    // We search in reverse (.rev()) because Arena appends new game results to
    // the array in Bo3 matches. Searching from the start would always return
    // the result of Game 1, even when processing Game 2 or 3.
    let game_scope_result = game_info
        .and_then(|gi| gi.get("results"))
        .and_then(serde_json::Value::as_array)
        .and_then(|arr| {
            arr.iter().rev().find(|r| {
                r.get("scope").and_then(serde_json::Value::as_str) == Some("MatchScope_Game")
            })
        });

    let winning_team_id = game_scope_result
        .and_then(|r| r.get("winningTeamId"))
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0);

    let result_type = game_scope_result
        .and_then(|r| r.get("result"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");

    let reason = game_scope_result
        .and_then(|r| r.get("reason"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");

    let raw_game_info = game_info.cloned().unwrap_or(serde_json::Value::Null);

    serde_json::json!({
        "type": "game_result",
        "source": "gre_game_state",
        "stage": stage,
        "match_state": match_state,
        "results": results,
        "winning_team_id": winning_team_id,
        "result_type": result_type,
        "reason": reason,
        "game_info": raw_game_info,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::super::test_fixtures::*;
    use super::super::try_parse;
    use crate::events::{GameEvent, PerformanceClass};
    use crate::parsers::test_helpers::{game_result_payload, test_timestamp, unity_entry};

    /// Helper: build a GRE event body with a `GameStateMessage` containing
    /// `GameStage_GameOver` and a results array.
    fn game_over_body() -> String {
        format!(
            "[UnityCrossThreadLogger]greToClientEvent\n{}",
            serde_json::json!({
                "greToClientEvent": {
                    "greToClientMessages": [{
                        "type": "GREMessageType_GameStateMessage",
                        "msgId": 99,
                        "gameStateId": 200,
                        "gameStateMessage": {
                            "gameInfo": {
                                "matchID": "match-abc-123",
                                "gameNumber": 1,
                                "stage": "GameStage_GameOver",
                                "matchState": "MatchState_GameComplete",
                                "type": "GameType_Standard",
                                "variant": "GameVariant_Normal",
                                "mulliganType": "MulliganType_London",
                                "results": [
                                    {
                                        "scope": "MatchScope_Game",
                                        "result": "ResultType_WinLoss",
                                        "winningTeamId": 1,
                                        "reason": "ResultReason_Game"
                                    }
                                ]
                            }
                        }
                    }]
                }
            })
        )
    }

    /// Helper: build a `QueuedGameStateMessage` with `GameStage_GameOver`.
    fn queued_game_over_body() -> String {
        format!(
            "[UnityCrossThreadLogger]greToClientEvent\n{}",
            serde_json::json!({
                "greToClientEvent": {
                    "greToClientMessages": [{
                        "type": "GREMessageType_QueuedGameStateMessage",
                        "msgId": 101,
                        "gameStateId": 210,
                        "gameStateMessage": {
                            "gameInfo": {
                                "stage": "GameStage_GameOver",
                                "matchState": "MatchState_GameComplete",
                                "results": [
                                    {
                                        "scope": "MatchScope_Game",
                                        "result": "ResultType_WinLoss",
                                        "winningTeamId": 2,
                                        "reason": "ResultReason_Concede"
                                    }
                                ]
                            }
                        }
                    }]
                }
            })
        )
    }

    mod is_match_complete_tests {
        use crate::parsers::gre::game_result;

        #[test]
        fn test_is_match_complete_true_for_match_complete() {
            let msg = serde_json::json!({
                "gameStateMessage": {
                    "gameInfo": {
                        "stage": "GameStage_GameOver",
                        "matchState": "MatchState_MatchComplete"
                    }
                }
            });
            assert!(game_result::is_match_complete(&msg));
        }

        #[test]
        fn test_is_match_complete_false_for_game_complete() {
            let msg = serde_json::json!({
                "gameStateMessage": {
                    "gameInfo": {
                        "stage": "GameStage_GameOver",
                        "matchState": "MatchState_GameComplete"
                    }
                }
            });
            assert!(!game_result::is_match_complete(&msg));
        }

        #[test]
        fn test_is_match_complete_false_for_missing_match_state() {
            let msg = serde_json::json!({
                "gameStateMessage": {
                    "gameInfo": {
                        "stage": "GameStage_GameOver"
                    }
                }
            });
            assert!(!game_result::is_match_complete(&msg));
        }
    }

    mod game_result_detection {
        use super::*;

        #[test]
        fn test_try_parse_game_state_message_game_over_emits_game_result() {
            let entry = unity_entry(&game_over_body());
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(!result.is_empty());
            let event = &result[0];
            assert!(matches!(event, GameEvent::GameResult(_)));
        }

        #[test]
        fn test_try_parse_game_over_performance_class_post_game_batch() {
            let entry = unity_entry(&game_over_body());
            let event = try_parse(&entry, Some(test_timestamp()))
                .into_iter()
                .next()
                .unwrap_or_else(|| unreachable!());
            assert_eq!(event.performance_class(), PerformanceClass::PostGameBatch);
        }

        #[test]
        fn test_try_parse_game_over_payload_type_and_source() {
            let entry = unity_entry(&game_over_body());
            let event = try_parse(&entry, Some(test_timestamp()))
                .into_iter()
                .next()
                .unwrap_or_else(|| unreachable!());
            let payload = game_result_payload(&event);
            assert_eq!(payload["type"], "game_result");
            assert_eq!(payload["source"], "gre_game_state");
        }

        #[test]
        fn test_try_parse_game_over_extracts_stage_and_match_state() {
            let entry = unity_entry(&game_over_body());
            let event = try_parse(&entry, Some(test_timestamp()))
                .into_iter()
                .next()
                .unwrap_or_else(|| unreachable!());
            let payload = game_result_payload(&event);
            assert_eq!(payload["stage"], "GameStage_GameOver");
            assert_eq!(payload["match_state"], "MatchState_GameComplete");
        }

        #[test]
        fn test_try_parse_game_over_extracts_winning_team_id() {
            let entry = unity_entry(&game_over_body());
            let event = try_parse(&entry, Some(test_timestamp()))
                .into_iter()
                .next()
                .unwrap_or_else(|| unreachable!());
            let payload = game_result_payload(&event);
            assert_eq!(payload["winning_team_id"], 1);
        }

        #[test]
        fn test_try_parse_game_over_extracts_result_type() {
            let entry = unity_entry(&game_over_body());
            let event = try_parse(&entry, Some(test_timestamp()))
                .into_iter()
                .next()
                .unwrap_or_else(|| unreachable!());
            let payload = game_result_payload(&event);
            assert_eq!(payload["result_type"], "ResultType_WinLoss");
        }

        #[test]
        fn test_try_parse_game_over_extracts_reason() {
            let entry = unity_entry(&game_over_body());
            let event = try_parse(&entry, Some(test_timestamp()))
                .into_iter()
                .next()
                .unwrap_or_else(|| unreachable!());
            let payload = game_result_payload(&event);
            assert_eq!(payload["reason"], "ResultReason_Game");
        }

        #[test]
        fn test_try_parse_game_over_preserves_results_array() {
            let entry = unity_entry(&game_over_body());
            let event = try_parse(&entry, Some(test_timestamp()))
                .into_iter()
                .next()
                .unwrap_or_else(|| unreachable!());
            let payload = game_result_payload(&event);
            let results = payload["results"]
                .as_array()
                .unwrap_or_else(|| unreachable!());
            assert_eq!(results.len(), 1);
            assert_eq!(results[0]["scope"], "MatchScope_Game");
        }

        #[test]
        fn test_try_parse_game_over_preserves_raw_game_info() {
            let entry = unity_entry(&game_over_body());
            let event = try_parse(&entry, Some(test_timestamp()))
                .into_iter()
                .next()
                .unwrap_or_else(|| unreachable!());
            let payload = game_result_payload(&event);
            let gi = &payload["game_info"];
            assert_eq!(gi["matchID"], "match-abc-123");
            assert_eq!(gi["gameNumber"], 1);
            assert_eq!(gi["mulliganType"], "MulliganType_London");
        }

        #[test]
        fn test_try_parse_game_over_preserves_timestamp() {
            let entry = unity_entry(&game_over_body());
            let event = try_parse(&entry, Some(test_timestamp()))
                .into_iter()
                .next()
                .unwrap_or_else(|| unreachable!());
            assert_eq!(event.metadata().timestamp(), Some(test_timestamp()));
        }

        #[test]
        fn test_try_parse_game_over_preserves_raw_bytes() {
            let body = game_over_body();
            let entry = unity_entry(&body);
            let event = try_parse(&entry, Some(test_timestamp()))
                .into_iter()
                .next()
                .unwrap_or_else(|| unreachable!());
            assert_eq!(event.metadata().raw_bytes(), body.as_bytes());
        }

        #[test]
        fn test_try_parse_queued_game_state_message_game_over_emits_game_result() {
            let entry = unity_entry(&queued_game_over_body());
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(!result.is_empty());
            let event = &result[0];
            assert!(matches!(event, GameEvent::GameResult(_)));
            let payload = game_result_payload(event);
            assert_eq!(payload["winning_team_id"], 2);
            assert_eq!(payload["reason"], "ResultReason_Concede");
        }

        #[test]
        fn test_try_parse_non_game_over_stage_emits_game_state() {
            // GameStage_Play should still emit GameState, not GameResult.
            let entry = unity_entry(&game_state_message_body());
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(!result.is_empty());
            let event = &result[0];
            assert!(matches!(event, GameEvent::GameState(_)));
        }

        #[test]
        fn test_try_parse_game_over_missing_results_defaults() {
            let body = format!(
                "[UnityCrossThreadLogger]greToClientEvent\n{}",
                serde_json::json!({
                    "greToClientEvent": {
                        "greToClientMessages": [{
                            "type": "GREMessageType_GameStateMessage",
                            "msgId": 50,
                            "gameStateMessage": {
                                "gameInfo": {
                                    "stage": "GameStage_GameOver",
                                    "matchState": "MatchState_GameComplete"
                                }
                            }
                        }]
                    }
                })
            );
            let entry = unity_entry(&body);
            let event = try_parse(&entry, Some(test_timestamp()))
                .into_iter()
                .next()
                .unwrap_or_else(|| unreachable!());
            assert!(matches!(event, GameEvent::GameResult(_)));
            let payload = game_result_payload(&event);
            assert_eq!(payload["winning_team_id"], 0);
            assert_eq!(payload["result_type"], "");
            assert_eq!(payload["reason"], "");
            let results = payload["results"]
                .as_array()
                .unwrap_or_else(|| unreachable!());
            assert!(results.is_empty());
        }

        #[test]
        fn test_try_parse_game_over_multiple_results_uses_game_scope() {
            let body = format!(
                "[UnityCrossThreadLogger]greToClientEvent\n{}",
                serde_json::json!({
                    "greToClientEvent": {
                        "greToClientMessages": [{
                            "type": "GREMessageType_GameStateMessage",
                            "msgId": 60,
                            "gameStateMessage": {
                                "gameInfo": {
                                    "stage": "GameStage_GameOver",
                                    "matchState": "MatchState_GameComplete",
                                    "results": [
                                        {
                                            "scope": "MatchScope_Match",
                                            "result": "ResultType_WinLoss",
                                            "winningTeamId": 1,
                                            "reason": "ResultReason_Game"
                                        },
                                        {
                                            "scope": "MatchScope_Game",
                                            "result": "ResultType_Draw",
                                            "winningTeamId": 0,
                                            "reason": "ResultReason_Draw"
                                        }
                                    ]
                                }
                            }
                        }]
                    }
                })
            );
            let entry = unity_entry(&body);
            let event = try_parse(&entry, Some(test_timestamp()))
                .into_iter()
                .next()
                .unwrap_or_else(|| unreachable!());
            let payload = game_result_payload(&event);
            // Top-level fields come from the MatchScope_Game entry.
            assert_eq!(payload["winning_team_id"], 0);
            assert_eq!(payload["result_type"], "ResultType_Draw");
            assert_eq!(payload["reason"], "ResultReason_Draw");
            // Both results are preserved.
            let results = payload["results"]
                .as_array()
                .unwrap_or_else(|| unreachable!());
            assert_eq!(results.len(), 2);
        }

        #[test]
        fn test_try_parse_game_over_bo3_full_match_sequence() {
            // This test simulates the sequence of GameStage_GameOver messages
            // received throughout a full Best-of-3 match.

            // --- Game 1 End (Team 1 wins) ---
            let body1 = format!(
                "[UnityCrossThreadLogger]greToClientEvent\n{}",
                serde_json::json!({
                    "greToClientEvent": {
                        "greToClientMessages": [{
                            "type": "GREMessageType_GameStateMessage",
                            "gameStateMessage": {
                                "gameInfo": {
                                    "stage": "GameStage_GameOver",
                                    "matchState": "MatchState_GameComplete",
                                    "results": [
                                        { "scope": "MatchScope_Game", "winningTeamId": 1 }
                                    ]
                                }
                            }
                        }]
                    }
                })
            );
            let entry1 = unity_entry(&body1);
            let event1 = &try_parse(&entry1, Some(test_timestamp()))[0];
            assert_eq!(game_result_payload(event1)["winning_team_id"], 1);

            // --- Game 2 End (Team 2 wins) ---
            // The results array now contains BOTH game results.
            let body2 = format!(
                "[UnityCrossThreadLogger]greToClientEvent\n{}",
                serde_json::json!({
                    "greToClientEvent": {
                        "greToClientMessages": [{
                            "type": "GREMessageType_GameStateMessage",
                            "gameStateMessage": {
                                "gameInfo": {
                                    "stage": "GameStage_GameOver",
                                    "matchState": "MatchState_GameComplete",
                                    "results": [
                                        { "scope": "MatchScope_Game", "winningTeamId": 1 },
                                        { "scope": "MatchScope_Game", "winningTeamId": 2 }
                                    ]
                                }
                            }
                        }]
                    }
                })
            );
            let entry2 = unity_entry(&body2);
            let event2 = &try_parse(&entry2, Some(test_timestamp()))[0];
            // Without .rev(), this would incorrectly return 1 (the first entry).
            assert_eq!(game_result_payload(event2)["winning_team_id"], 2);

            // --- Game 3 End (Team 1 wins match) ---
            // The results array now contains all 3 game results + the match result.
            let body3 = format!(
                "[UnityCrossThreadLogger]greToClientEvent\n{}",
                serde_json::json!({
                    "greToClientEvent": {
                        "greToClientMessages": [{
                            "type": "GREMessageType_GameStateMessage",
                            "gameStateMessage": {
                                "gameInfo": {
                                    "stage": "GameStage_GameOver",
                                    "matchState": "MatchState_GameComplete",
                                    "results": [
                                        { "scope": "MatchScope_Game", "winningTeamId": 1 },
                                        { "scope": "MatchScope_Game", "winningTeamId": 2 },
                                        { "scope": "MatchScope_Game", "winningTeamId": 1 },
                                        { "scope": "MatchScope_Match", "winningTeamId": 1 }
                                    ]
                                }
                            }
                        }]
                    }
                })
            );
            let entry3 = unity_entry(&body3);
            let event3 = &try_parse(&entry3, Some(test_timestamp()))[0];
            // Should correctly extract the latest game winner (Team 1).
            assert_eq!(game_result_payload(event3)["winning_team_id"], 1);
        }

        #[test]
        fn test_try_parse_no_stage_field_emits_game_state() {
            let body = format!(
                "[UnityCrossThreadLogger]greToClientEvent\n{}",
                serde_json::json!({
                    "greToClientEvent": {
                        "greToClientMessages": [{
                            "type": "GREMessageType_GameStateMessage",
                            "msgId": 70,
                            "gameStateMessage": {
                                "gameInfo": {
                                    "matchID": "match-xyz",
                                    "gameNumber": 1
                                }
                            }
                        }]
                    }
                })
            );
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(!result.is_empty());
            let event = &result[0];
            assert!(matches!(event, GameEvent::GameState(_)));
        }
    }
}
