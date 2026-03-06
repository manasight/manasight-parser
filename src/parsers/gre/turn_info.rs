//! Turn info extraction from `gameStateMessage.turnInfo`.

/// Extracts structured turn info from `gameStateMessage.turnInfo`.
///
/// The `turnInfo` sub-object in the MTGA log has the structure:
/// ```json
/// {
///   "turnNumber": 3,
///   "phase": "Phase_Main1",
///   "step": "Step_Upkeep",
///   "activePlayer": 1,
///   "decisionPlayer": 1
/// }
/// ```
///
/// The output normalizes field names to `snake_case`. Returns `null` when
/// `turnInfo` is absent. Partial `turnInfo` objects are handled
/// gracefully — missing fields get default values (`0` for integers,
/// empty string for strings).
pub(super) fn extract_turn_info(gsm: Option<&serde_json::Value>) -> serde_json::Value {
    let Some(turn_info) = gsm.and_then(|g| g.get("turnInfo")) else {
        return serde_json::Value::Null;
    };

    // If turnInfo exists but is not an object, return null.
    if !turn_info.is_object() {
        return serde_json::Value::Null;
    }

    let turn_number = turn_info
        .get("turnNumber")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0);

    let phase = turn_info
        .get("phase")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");

    let step = turn_info
        .get("step")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");

    let active_player = turn_info
        .get("activePlayer")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0);

    let decision_player = turn_info
        .get("decisionPlayer")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0);

    serde_json::json!({
        "turn_number": turn_number,
        "phase": phase,
        "step": step,
        "active_player": active_player,
        "decision_player": decision_player,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::super::test_fixtures::*;
    use super::super::try_parse;
    use crate::parsers::test_helpers::{game_state_payload, test_timestamp, unity_entry};

    /// Helper: build a `GameStateMessage` containing `turnInfo` as a
    /// direct child of `gameStateMessage` (sibling of `gameInfo`).
    fn game_state_message_with_turn_info_body() -> String {
        format!(
            "[UnityCrossThreadLogger]greToClientEvent\n{}",
            serde_json::json!({
                "greToClientEvent": {
                    "greToClientMessages": [{
                        "type": "GREMessageType_GameStateMessage",
                        "msgId": 8,
                        "gameStateId": 55,
                        "gameStateMessage": {
                            "zones": [],
                            "gameObjects": [],
                            "turnInfo": {
                                "turnNumber": 3,
                                "phase": "Phase_Main1",
                                "step": "Step_Upkeep",
                                "activePlayer": 1,
                                "decisionPlayer": 2
                            },
                            "gameInfo": {
                                "matchID": "match-id-99999",
                                "gameNumber": 1,
                                "stage": "GameStage_Play",
                                "type": "GameType_Standard",
                                "variant": "GameVariant_Normal",
                                "mulliganType": "MulliganType_London"
                            }
                        }
                    }]
                }
            })
        )
    }

    /// Helper: build a `GameStateMessage` with a partial `turnInfo`
    /// (only `turnNumber` present).
    fn game_state_message_with_partial_turn_info_body() -> String {
        format!(
            "[UnityCrossThreadLogger]greToClientEvent\n{}",
            serde_json::json!({
                "greToClientEvent": {
                    "greToClientMessages": [{
                        "type": "GREMessageType_GameStateMessage",
                        "msgId": 9,
                        "gameStateId": 56,
                        "gameStateMessage": {
                            "zones": [],
                            "gameObjects": [],
                            "turnInfo": {
                                "turnNumber": 5
                            },
                            "gameInfo": {
                                "matchID": "match-id-partial",
                                "stage": "GameStage_Play"
                            }
                        }
                    }]
                }
            })
        )
    }

    mod turn_info_extraction {
        use super::*;

        #[test]
        fn test_turn_info_present_is_object() {
            let body = game_state_message_with_turn_info_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(event);

            assert!(payload["turn_info"].is_object());
        }

        #[test]
        fn test_turn_info_turn_number() {
            let body = game_state_message_with_turn_info_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(event);

            assert_eq!(payload["turn_info"]["turn_number"], 3);
        }

        #[test]
        fn test_turn_info_phase() {
            let body = game_state_message_with_turn_info_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(event);

            assert_eq!(payload["turn_info"]["phase"], "Phase_Main1");
        }

        #[test]
        fn test_turn_info_step() {
            let body = game_state_message_with_turn_info_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(event);

            assert_eq!(payload["turn_info"]["step"], "Step_Upkeep");
        }

        #[test]
        fn test_turn_info_active_player() {
            let body = game_state_message_with_turn_info_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(event);

            assert_eq!(payload["turn_info"]["active_player"], 1);
        }

        #[test]
        fn test_turn_info_decision_player() {
            let body = game_state_message_with_turn_info_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(event);

            assert_eq!(payload["turn_info"]["decision_player"], 2);
        }

        #[test]
        fn test_turn_info_missing_returns_null() {
            let body = game_state_message_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(event);

            assert!(payload["turn_info"].is_null());
        }

        #[test]
        fn test_turn_info_missing_when_gsm_empty() {
            let body = empty_game_state_message_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(event);

            assert!(payload["turn_info"].is_null());
        }

        #[test]
        fn test_turn_info_partial_defaults_missing_fields() {
            let body = game_state_message_with_partial_turn_info_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(event);

            let turn_info = &payload["turn_info"];
            assert!(turn_info.is_object());
            assert_eq!(turn_info["turn_number"], 5);
            assert_eq!(turn_info["phase"], "");
            assert_eq!(turn_info["step"], "");
            assert_eq!(turn_info["active_player"], 0);
            assert_eq!(turn_info["decision_player"], 0);
        }
    }
}
