//! Match state parser for `matchGameRoomStateChangedEvent`.
//!
//! Detects match/game start and end from room state transitions, extracts
//! player seat assignments, and correlates games within a match (Bo3 support).
//!
//! The `matchGameRoomStateChangedEvent` is the structural backbone of any
//! game record. It carries:
//!
//! | Field | Purpose |
//! |-------|---------|
//! | `gameRoomInfo.stateType` | Room state: `Playing`, `MatchCompleted` |
//! | `gameRoomInfo.gameRoomConfig.matchId` | Unique match identifier |
//! | `gameRoomInfo.gameRoomConfig.reservedPlayers` | Player seat assignments, user IDs |
//! | `gameRoomInfo.finalMatchResult` | Final result with `matchCompletedReason` |
//!
//! This is Class 1 (Interactive Dispatch) -- the first event emitted for
//! each match, used to open game buffers and configure the overlay.

use crate::events::{EventMetadata, GameEvent, MatchStateEvent};
use crate::log::entry::LogEntry;
use crate::parsers::api_common;

/// Marker that identifies a match state change entry in the log.
const MATCH_STATE_MARKER: &str = "matchGameRoomStateChangedEvent";

/// Attempts to parse a [`LogEntry`] as a match state event.
///
/// Returns `Some(GameEvent::MatchState(_))` if the entry body contains a
/// `matchGameRoomStateChangedEvent` JSON payload, or `None` if the entry
/// does not match.
///
/// The `timestamp` is `None` when the log entry header did not contain a
/// parseable timestamp. It is passed through to [`EventMetadata`] so
/// downstream consumers can distinguish real vs missing timestamps.
pub fn try_parse(
    entry: &LogEntry,
    timestamp: Option<chrono::DateTime<chrono::Utc>>,
) -> Option<GameEvent> {
    let body = &entry.body;

    // Quick check: bail early if the marker is not present.
    if !body.contains(MATCH_STATE_MARKER) {
        return None;
    }

    // Extract and parse the JSON payload from the body.
    let parsed = api_common::parse_json_from_body(body, "matchGameRoomStateChangedEvent")?;

    // The JSON should contain a `matchGameRoomStateChangedEvent` key.
    let state_event = parsed.get(MATCH_STATE_MARKER).or_else(|| {
        // Some log formats embed the data at the top level without the wrapper key.
        if parsed.get("gameRoomInfo").is_some() {
            Some(&parsed)
        } else {
            None
        }
    })?;

    let payload = build_payload(state_event);
    let metadata = EventMetadata::new(timestamp, body.as_bytes().to_vec());
    Some(GameEvent::MatchState(MatchStateEvent::new(
        metadata, payload,
    )))
}

/// Builds a structured payload from the match state change event data.
///
/// Extracts key fields from the nested JSON structure into a flat(ter)
/// payload for downstream consumers.
fn build_payload(state_event: &serde_json::Value) -> serde_json::Value {
    let game_room_info = state_event.get("gameRoomInfo");

    // State type: "Playing", "MatchCompleted", etc.
    let state_type = game_room_info
        .and_then(|info| info.get("stateType"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");

    // Match lifecycle type based on state.
    let match_lifecycle = match state_type {
        "MatchGameRoomStateType_Playing" => "match_started",
        "MatchGameRoomStateType_MatchCompleted" => "match_completed",
        _ => "state_changed",
    };

    // Game room config: matchId, event ID, reserved players.
    let game_room_config = game_room_info.and_then(|info| info.get("gameRoomConfig"));

    let match_id = game_room_config
        .and_then(|cfg| cfg.get("matchId"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");

    let event_id = game_room_config
        .and_then(|cfg| cfg.get("eventId"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");

    // Reserved players: seat assignments, user IDs, team IDs.
    let players = extract_players(game_room_config);

    // Final match result (present on match completion).
    let final_result = game_room_info.and_then(|info| info.get("finalMatchResult"));

    let result_list = final_result
        .and_then(|r| r.get("resultList"))
        .and_then(serde_json::Value::as_array);

    let completed_reason = final_result
        .and_then(|r| r.get("matchCompletedReason"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");

    // Game results within the match (for Bo3 correlation).
    let game_results = build_game_results(result_list);

    let mut payload = serde_json::json!({
        "type": match_lifecycle,
        "state_type": state_type,
        "match_id": match_id,
        "event_id": event_id,
        "players": players,
    });

    // Only include final result fields when match is completed.
    if final_result.is_some() {
        payload["match_completed_reason"] = serde_json::json!(completed_reason);
        payload["game_results"] = game_results;
    }

    // Preserve the full raw event for consumers that need deeper fields.
    payload["raw_match_state"] = state_event.clone();

    payload
}

/// Extracts player information from `reservedPlayers` in the game room config.
///
/// Each player entry contains `userId`, `playerName`, `systemSeatId`,
/// `teamId`, and potentially connection info.
fn extract_players(game_room_config: Option<&serde_json::Value>) -> serde_json::Value {
    let reserved = game_room_config
        .and_then(|cfg| cfg.get("reservedPlayers"))
        .and_then(serde_json::Value::as_array);

    let Some(players) = reserved else {
        return serde_json::json!([]);
    };

    let extracted: Vec<serde_json::Value> = players
        .iter()
        .map(|p| {
            serde_json::json!({
                "user_id": p.get("userId").and_then(serde_json::Value::as_str).unwrap_or(""),
                "player_name": p.get("playerName").and_then(serde_json::Value::as_str).unwrap_or(""),
                "system_seat_id": p.get("systemSeatId").and_then(serde_json::Value::as_i64).unwrap_or(0),
                "team_id": p.get("teamId").and_then(serde_json::Value::as_i64).unwrap_or(0),
            })
        })
        .collect();

    serde_json::json!(extracted)
}

/// Builds game result entries from the `resultList` in `finalMatchResult`.
///
/// Each entry in the result list represents one game within the match,
/// enabling Bo3 correlation. Includes `scope` (per-game or per-match),
/// `result`, and `winningTeamId`.
fn build_game_results(result_list: Option<&Vec<serde_json::Value>>) -> serde_json::Value {
    let Some(results) = result_list else {
        return serde_json::json!([]);
    };

    let game_results: Vec<serde_json::Value> = results
        .iter()
        .map(|r| {
            serde_json::json!({
                "scope": r.get("scope").and_then(serde_json::Value::as_str).unwrap_or(""),
                "result": r.get("result").and_then(serde_json::Value::as_str).unwrap_or(""),
                "winning_team_id": r.get("winningTeamId").and_then(serde_json::Value::as_i64).unwrap_or(0),
            })
        })
        .collect();

    serde_json::json!(game_results)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parsers::test_helpers::{
        match_state_payload, test_timestamp, unity_entry, EntryHeader,
    };

    /// Helper: build a realistic `matchGameRoomStateChangedEvent` JSON body
    /// for a match start (state type = Playing).
    fn match_start_body() -> String {
        format!(
            "[UnityCrossThreadLogger]matchGameRoomStateChangedEvent\n{}",
            serde_json::json!({
                "matchGameRoomStateChangedEvent": {
                    "gameRoomInfo": {
                        "stateType": "MatchGameRoomStateType_Playing",
                        "gameRoomConfig": {
                            "matchId": "abc123-match-id",
                            "eventId": "Ladder",
                            "reservedPlayers": [
                                {
                                    "userId": "user-001",
                                    "playerName": "Player1#12345",
                                    "systemSeatId": 1,
                                    "teamId": 1
                                },
                                {
                                    "userId": "user-002",
                                    "playerName": "Player2#67890",
                                    "systemSeatId": 2,
                                    "teamId": 2
                                }
                            ]
                        }
                    }
                }
            })
        )
    }

    /// Helper: build a realistic match completion JSON body.
    fn match_completed_body() -> String {
        format!(
            "[UnityCrossThreadLogger]matchGameRoomStateChangedEvent\n{}",
            serde_json::json!({
                "matchGameRoomStateChangedEvent": {
                    "gameRoomInfo": {
                        "stateType": "MatchGameRoomStateType_MatchCompleted",
                        "gameRoomConfig": {
                            "matchId": "abc123-match-id",
                            "eventId": "Ladder",
                            "reservedPlayers": [
                                {
                                    "userId": "user-001",
                                    "playerName": "Player1#12345",
                                    "systemSeatId": 1,
                                    "teamId": 1
                                },
                                {
                                    "userId": "user-002",
                                    "playerName": "Player2#67890",
                                    "systemSeatId": 2,
                                    "teamId": 2
                                }
                            ]
                        },
                        "finalMatchResult": {
                            "matchId": "abc123-match-id",
                            "matchCompletedReason": "MatchCompletedReasonType_Success",
                            "resultList": [
                                {
                                    "scope": "MatchScope_Game",
                                    "result": "ResultType_WinLoss",
                                    "winningTeamId": 1
                                },
                                {
                                    "scope": "MatchScope_Match",
                                    "result": "ResultType_WinLoss",
                                    "winningTeamId": 1
                                }
                            ]
                        }
                    }
                }
            })
        )
    }

    /// Helper: build a Bo3 match completion body with multiple game results.
    fn bo3_match_completed_body() -> String {
        format!(
            "[UnityCrossThreadLogger]matchGameRoomStateChangedEvent\n{}",
            serde_json::json!({
                "matchGameRoomStateChangedEvent": {
                    "gameRoomInfo": {
                        "stateType": "MatchGameRoomStateType_MatchCompleted",
                        "gameRoomConfig": {
                            "matchId": "bo3-match-id",
                            "eventId": "Traditional_Ladder",
                            "reservedPlayers": [
                                {
                                    "userId": "user-001",
                                    "playerName": "Player1#12345",
                                    "systemSeatId": 1,
                                    "teamId": 1
                                },
                                {
                                    "userId": "user-002",
                                    "playerName": "Player2#67890",
                                    "systemSeatId": 2,
                                    "teamId": 2
                                }
                            ]
                        },
                        "finalMatchResult": {
                            "matchId": "bo3-match-id",
                            "matchCompletedReason": "MatchCompletedReasonType_Success",
                            "resultList": [
                                {
                                    "scope": "MatchScope_Game",
                                    "result": "ResultType_WinLoss",
                                    "winningTeamId": 1
                                },
                                {
                                    "scope": "MatchScope_Game",
                                    "result": "ResultType_WinLoss",
                                    "winningTeamId": 2
                                },
                                {
                                    "scope": "MatchScope_Game",
                                    "result": "ResultType_WinLoss",
                                    "winningTeamId": 1
                                },
                                {
                                    "scope": "MatchScope_Match",
                                    "result": "ResultType_WinLoss",
                                    "winningTeamId": 1
                                }
                            ]
                        }
                    }
                }
            })
        )
    }

    // -- Match start parsing -----------------------------------------------

    mod match_start {
        use super::*;

        #[test]
        fn test_try_parse_match_start_detected() {
            let body = match_start_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
        }

        #[test]
        fn test_try_parse_match_start_lifecycle_type() {
            let body = match_start_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = match_state_payload(event);
            assert_eq!(payload["type"], "match_started");
        }

        #[test]
        fn test_try_parse_match_start_state_type() {
            let body = match_start_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = match_state_payload(event);
            assert_eq!(payload["state_type"], "MatchGameRoomStateType_Playing");
        }

        #[test]
        fn test_try_parse_match_start_match_id() {
            let body = match_start_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = match_state_payload(event);
            assert_eq!(payload["match_id"], "abc123-match-id");
        }

        #[test]
        fn test_try_parse_match_start_event_id() {
            let body = match_start_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = match_state_payload(event);
            assert_eq!(payload["event_id"], "Ladder");
        }

        #[test]
        fn test_try_parse_match_start_player_count() {
            let body = match_start_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = match_state_payload(event);
            let players = payload["players"].as_array();
            assert!(players.is_some());
            let players = players.unwrap_or_else(|| unreachable!());
            assert_eq!(players.len(), 2);
        }

        #[test]
        fn test_try_parse_match_start_player_seat_assignments() {
            let body = match_start_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = match_state_payload(event);
            let players = payload["players"].as_array();
            assert!(players.is_some());
            let players = players.unwrap_or_else(|| unreachable!());

            assert_eq!(players[0]["system_seat_id"], 1);
            assert_eq!(players[0]["player_name"], "Player1#12345");
            assert_eq!(players[0]["user_id"], "user-001");
            assert_eq!(players[0]["team_id"], 1);

            assert_eq!(players[1]["system_seat_id"], 2);
            assert_eq!(players[1]["player_name"], "Player2#67890");
            assert_eq!(players[1]["user_id"], "user-002");
            assert_eq!(players[1]["team_id"], 2);
        }

        #[test]
        fn test_try_parse_match_start_no_final_result() {
            let body = match_start_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = match_state_payload(event);
            // Final result fields should not be present on match start.
            assert!(payload.get("match_completed_reason").is_none());
            assert!(payload.get("game_results").is_none());
        }

        #[test]
        fn test_try_parse_match_start_preserves_raw_bytes() {
            let body = match_start_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            assert_eq!(event.metadata().raw_bytes(), body.as_bytes());
        }

        #[test]
        fn test_try_parse_match_start_stores_timestamp() {
            let body = match_start_body();
            let entry = unity_entry(&body);
            let ts = Some(test_timestamp());
            let result = try_parse(&entry, ts);
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            assert_eq!(event.metadata().timestamp(), ts);
        }

        #[test]
        fn test_try_parse_match_start_includes_raw_match_state() {
            let body = match_start_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = match_state_payload(event);
            assert!(payload.get("raw_match_state").is_some());
            assert!(payload["raw_match_state"]["gameRoomInfo"].is_object());
        }
    }

    // -- Match completion parsing -------------------------------------------

    mod match_completed {
        use super::*;

        #[test]
        fn test_try_parse_match_completed_detected() {
            let body = match_completed_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
        }

        #[test]
        fn test_try_parse_match_completed_lifecycle_type() {
            let body = match_completed_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = match_state_payload(event);
            assert_eq!(payload["type"], "match_completed");
        }

        #[test]
        fn test_try_parse_match_completed_state_type() {
            let body = match_completed_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = match_state_payload(event);
            assert_eq!(
                payload["state_type"],
                "MatchGameRoomStateType_MatchCompleted"
            );
        }

        #[test]
        fn test_try_parse_match_completed_reason() {
            let body = match_completed_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = match_state_payload(event);
            assert_eq!(
                payload["match_completed_reason"],
                "MatchCompletedReasonType_Success"
            );
        }

        #[test]
        fn test_try_parse_match_completed_game_results() {
            let body = match_completed_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = match_state_payload(event);
            let results = payload["game_results"].as_array();
            assert!(results.is_some());
            let results = results.unwrap_or_else(|| unreachable!());
            assert_eq!(results.len(), 2);
        }

        #[test]
        fn test_try_parse_match_completed_game_result_fields() {
            let body = match_completed_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = match_state_payload(event);
            let results = payload["game_results"].as_array();
            assert!(results.is_some());
            let results = results.unwrap_or_else(|| unreachable!());

            // First entry: per-game result
            assert_eq!(results[0]["scope"], "MatchScope_Game");
            assert_eq!(results[0]["result"], "ResultType_WinLoss");
            assert_eq!(results[0]["winning_team_id"], 1);

            // Second entry: per-match result
            assert_eq!(results[1]["scope"], "MatchScope_Match");
            assert_eq!(results[1]["winning_team_id"], 1);
        }

        #[test]
        fn test_try_parse_match_completed_preserves_metadata() {
            let body = match_completed_body();
            let entry = unity_entry(&body);
            let ts = Some(test_timestamp());
            let result = try_parse(&entry, ts);
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            assert_eq!(event.metadata().timestamp(), ts);
            assert_eq!(event.metadata().raw_bytes(), body.as_bytes());
        }
    }

    // -- Bo3 match correlation -----------------------------------------------

    mod bo3_correlation {
        use super::*;

        #[test]
        fn test_try_parse_bo3_match_id() {
            let body = bo3_match_completed_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = match_state_payload(event);
            assert_eq!(payload["match_id"], "bo3-match-id");
        }

        #[test]
        fn test_try_parse_bo3_event_id() {
            let body = bo3_match_completed_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = match_state_payload(event);
            assert_eq!(payload["event_id"], "Traditional_Ladder");
        }

        #[test]
        fn test_try_parse_bo3_game_results_count() {
            let body = bo3_match_completed_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = match_state_payload(event);
            let results = payload["game_results"].as_array();
            assert!(results.is_some());
            let results = results.unwrap_or_else(|| unreachable!());
            // 3 game results + 1 match result = 4 total
            assert_eq!(results.len(), 4);
        }

        #[test]
        fn test_try_parse_bo3_individual_game_results() {
            let body = bo3_match_completed_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = match_state_payload(event);
            let results = payload["game_results"].as_array();
            assert!(results.is_some());
            let results = results.unwrap_or_else(|| unreachable!());

            // Game 1: team 1 wins
            assert_eq!(results[0]["scope"], "MatchScope_Game");
            assert_eq!(results[0]["winning_team_id"], 1);

            // Game 2: team 2 wins
            assert_eq!(results[1]["scope"], "MatchScope_Game");
            assert_eq!(results[1]["winning_team_id"], 2);

            // Game 3: team 1 wins (decisive)
            assert_eq!(results[2]["scope"], "MatchScope_Game");
            assert_eq!(results[2]["winning_team_id"], 1);

            // Match: team 1 wins overall
            assert_eq!(results[3]["scope"], "MatchScope_Match");
            assert_eq!(results[3]["winning_team_id"], 1);
        }
    }

    // -- Non-match entries (should return None) ----------------------------

    mod non_match_state {
        use super::*;

        #[test]
        fn test_try_parse_unrelated_entry_returns_none() {
            let body = "[UnityCrossThreadLogger]greToClientEvent\n{\"data\": 1}";
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
        fn test_try_parse_session_event_returns_none() {
            let body = "[UnityCrossThreadLogger]Updated account. \
                         DisplayName:Test, AccountID:abc123";
            let entry = unity_entry(body);
            assert!(try_parse(&entry, Some(test_timestamp())).is_none());
        }

        #[test]
        fn test_try_parse_no_json_body_returns_none() {
            let body = "[UnityCrossThreadLogger]matchGameRoomStateChangedEvent with no json";
            let entry = unity_entry(body);
            assert!(try_parse(&entry, Some(test_timestamp())).is_none());
        }

        #[test]
        fn test_try_parse_malformed_json_returns_none() {
            let body = "[UnityCrossThreadLogger]matchGameRoomStateChangedEvent\n{invalid json}";
            let entry = unity_entry(body);
            assert!(try_parse(&entry, Some(test_timestamp())).is_none());
        }

        #[test]
        fn test_try_parse_json_without_match_state_key_returns_none() {
            let body = "[UnityCrossThreadLogger]matchGameRoomStateChangedEvent\n\
                 {\"someOtherEvent\": {\"data\": 1}}";
            let entry = unity_entry(body);
            assert!(try_parse(&entry, Some(test_timestamp())).is_none());
        }

        #[test]
        fn test_try_parse_client_gre_entry_returns_none() {
            let entry = LogEntry {
                header: EntryHeader::ClientGre,
                body: "[Client GRE]matchGameRoomStateChangedEvent".to_owned(),
            };
            assert!(try_parse(&entry, Some(test_timestamp())).is_none());
        }
    }

    // -- Edge cases ---------------------------------------------------------

    mod edge_cases {
        use super::*;

        #[test]
        fn test_try_parse_missing_game_room_config() {
            let body = format!(
                "[UnityCrossThreadLogger]matchGameRoomStateChangedEvent\n{}",
                serde_json::json!({
                    "matchGameRoomStateChangedEvent": {
                        "gameRoomInfo": {
                            "stateType": "MatchGameRoomStateType_Playing"
                        }
                    }
                })
            );
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = match_state_payload(event);
            assert_eq!(payload["match_id"], "");
            assert_eq!(payload["event_id"], "");
            let players = payload["players"].as_array();
            assert!(players.is_some());
            let players = players.unwrap_or_else(|| unreachable!());
            assert!(players.is_empty());
        }

        #[test]
        fn test_try_parse_empty_reserved_players() {
            let body = format!(
                "[UnityCrossThreadLogger]matchGameRoomStateChangedEvent\n{}",
                serde_json::json!({
                    "matchGameRoomStateChangedEvent": {
                        "gameRoomInfo": {
                            "stateType": "MatchGameRoomStateType_Playing",
                            "gameRoomConfig": {
                                "matchId": "empty-players-match",
                                "eventId": "Ladder",
                                "reservedPlayers": []
                            }
                        }
                    }
                })
            );
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = match_state_payload(event);
            let players = payload["players"].as_array();
            assert!(players.is_some());
            let players = players.unwrap_or_else(|| unreachable!());
            assert!(players.is_empty());
        }

        #[test]
        fn test_try_parse_unknown_state_type() {
            let body = format!(
                "[UnityCrossThreadLogger]matchGameRoomStateChangedEvent\n{}",
                serde_json::json!({
                    "matchGameRoomStateChangedEvent": {
                        "gameRoomInfo": {
                            "stateType": "MatchGameRoomStateType_SomeNewState",
                            "gameRoomConfig": {
                                "matchId": "new-state-match",
                                "eventId": "Ladder"
                            }
                        }
                    }
                })
            );
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = match_state_payload(event);
            // Unknown state types get the generic "state_changed" lifecycle.
            assert_eq!(payload["type"], "state_changed");
            assert_eq!(payload["state_type"], "MatchGameRoomStateType_SomeNewState");
        }

        #[test]
        fn test_try_parse_with_timestamp_in_header() {
            let body = format!(
                "[UnityCrossThreadLogger]2/25/2026 12:00:00 PM matchGameRoomStateChangedEvent\n{}",
                serde_json::json!({
                    "matchGameRoomStateChangedEvent": {
                        "gameRoomInfo": {
                            "stateType": "MatchGameRoomStateType_Playing",
                            "gameRoomConfig": {
                                "matchId": "ts-match-id",
                                "eventId": "Ladder"
                            }
                        }
                    }
                })
            );
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = match_state_payload(event);
            assert_eq!(payload["match_id"], "ts-match-id");
        }

        #[test]
        fn test_try_parse_match_completed_disconnect_reason() {
            let body = format!(
                "[UnityCrossThreadLogger]matchGameRoomStateChangedEvent\n{}",
                serde_json::json!({
                    "matchGameRoomStateChangedEvent": {
                        "gameRoomInfo": {
                            "stateType": "MatchGameRoomStateType_MatchCompleted",
                            "gameRoomConfig": {
                                "matchId": "disconnect-match-id",
                                "eventId": "Ladder",
                                "reservedPlayers": []
                            },
                            "finalMatchResult": {
                                "matchId": "disconnect-match-id",
                                "matchCompletedReason": "MatchCompletedReasonType_PlayerDisconnectTimeout",
                                "resultList": [
                                    {
                                        "scope": "MatchScope_Match",
                                        "result": "ResultType_WinLoss",
                                        "winningTeamId": 2
                                    }
                                ]
                            }
                        }
                    }
                })
            );
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = match_state_payload(event);
            assert_eq!(payload["type"], "match_completed");
            assert_eq!(
                payload["match_completed_reason"],
                "MatchCompletedReasonType_PlayerDisconnectTimeout"
            );
        }

        #[test]
        fn test_try_parse_top_level_game_room_info() {
            // Some log formats may embed gameRoomInfo at top level
            // without the matchGameRoomStateChangedEvent wrapper.
            let body = format!(
                "[UnityCrossThreadLogger]matchGameRoomStateChangedEvent\n{}",
                serde_json::json!({
                    "gameRoomInfo": {
                        "stateType": "MatchGameRoomStateType_Playing",
                        "gameRoomConfig": {
                            "matchId": "top-level-match",
                            "eventId": "Ladder",
                            "reservedPlayers": []
                        }
                    }
                })
            );
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = match_state_payload(event);
            assert_eq!(payload["match_id"], "top-level-match");
        }

        #[test]
        fn test_try_parse_player_missing_optional_fields() {
            let body = format!(
                "[UnityCrossThreadLogger]matchGameRoomStateChangedEvent\n{}",
                serde_json::json!({
                    "matchGameRoomStateChangedEvent": {
                        "gameRoomInfo": {
                            "stateType": "MatchGameRoomStateType_Playing",
                            "gameRoomConfig": {
                                "matchId": "sparse-player-match",
                                "reservedPlayers": [
                                    {
                                        "systemSeatId": 1
                                    }
                                ]
                            }
                        }
                    }
                })
            );
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = match_state_payload(event);
            let players = payload["players"].as_array();
            assert!(players.is_some());
            let players = players.unwrap_or_else(|| unreachable!());
            assert_eq!(players.len(), 1);
            assert_eq!(players[0]["system_seat_id"], 1);
            assert_eq!(players[0]["user_id"], "");
            assert_eq!(players[0]["player_name"], "");
            assert_eq!(players[0]["team_id"], 0);
        }

        #[test]
        fn test_try_parse_final_result_empty_result_list() {
            let body = format!(
                "[UnityCrossThreadLogger]matchGameRoomStateChangedEvent\n{}",
                serde_json::json!({
                    "matchGameRoomStateChangedEvent": {
                        "gameRoomInfo": {
                            "stateType": "MatchGameRoomStateType_MatchCompleted",
                            "gameRoomConfig": {
                                "matchId": "empty-result-match",
                                "eventId": "Ladder"
                            },
                            "finalMatchResult": {
                                "matchId": "empty-result-match",
                                "matchCompletedReason": "MatchCompletedReasonType_Canceled",
                                "resultList": []
                            }
                        }
                    }
                })
            );
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = match_state_payload(event);
            assert_eq!(
                payload["match_completed_reason"],
                "MatchCompletedReasonType_Canceled"
            );
            let results = payload["game_results"].as_array();
            assert!(results.is_some());
            let results = results.unwrap_or_else(|| unreachable!());
            assert!(results.is_empty());
        }
    }

    // -- Performance class ---------------------------------------------------

    mod performance_class {
        use super::*;
        use crate::events::PerformanceClass;

        #[test]
        fn test_match_state_event_is_interactive_dispatch() {
            let body = match_start_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            assert_eq!(
                event.performance_class(),
                PerformanceClass::InteractiveDispatch
            );
        }
    }

    // -- Internal helpers ----------------------------------------------------

    mod helpers {
        use super::*;

        #[test]
        fn test_extract_players_none_config() {
            let result = extract_players(None);
            assert_eq!(result, serde_json::json!([]));
        }

        #[test]
        fn test_extract_players_no_reserved_players_key() {
            let config = serde_json::json!({"matchId": "test"});
            let result = extract_players(Some(&config));
            assert_eq!(result, serde_json::json!([]));
        }

        #[test]
        fn test_build_game_results_none() {
            let result = build_game_results(None);
            assert_eq!(result, serde_json::json!([]));
        }

        #[test]
        fn test_build_game_results_empty() {
            let empty_list: Vec<serde_json::Value> = vec![];
            let result = build_game_results(Some(&empty_list));
            assert_eq!(result, serde_json::json!([]));
        }

        #[test]
        fn test_build_game_results_single_entry() {
            let list = vec![serde_json::json!({
                "scope": "MatchScope_Game",
                "result": "ResultType_WinLoss",
                "winningTeamId": 1
            })];
            let result = build_game_results(Some(&list));
            let arr = result.as_array();
            assert!(arr.is_some());
            let arr = arr.unwrap_or_else(|| unreachable!());
            assert_eq!(arr.len(), 1);
            assert_eq!(arr[0]["scope"], "MatchScope_Game");
            assert_eq!(arr[0]["winning_team_id"], 1);
        }
    }
}
