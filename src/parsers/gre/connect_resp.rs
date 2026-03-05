//! `ConnectResp` payload builder for GRE `GREMessageType_ConnectResp` messages.

/// Builds a structured payload from the `ConnectResp` message.
///
/// Extracts key fields from the nested JSON structure into a flat(ter)
/// payload for downstream consumers (deck tracker, overlay).
pub(super) fn build_connect_resp_payload(
    connect_resp_msg: &serde_json::Value,
    full_event: &serde_json::Value,
) -> serde_json::Value {
    let connect_resp = connect_resp_msg.get("connectResp");

    // Deck cards: array of card GRP IDs from the player's deck.
    let deck_cards = extract_card_ids(
        connect_resp
            .and_then(|cr| cr.get("deckMessage"))
            .and_then(|dm| dm.get("deckCards")),
    );

    // Sideboard cards: array of card GRP IDs from the sideboard.
    let sideboard_cards = extract_card_ids(
        connect_resp
            .and_then(|cr| cr.get("deckMessage"))
            .and_then(|dm| dm.get("sideboardCards")),
    );

    // System seat IDs: all seat IDs in the game (typically [1, 2]).
    let system_seat_ids = connect_resp_msg
        .get("systemSeatIds")
        .and_then(serde_json::Value::as_array)
        .map(|ids| {
            ids.iter()
                .filter_map(serde_json::Value::as_i64)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    // Message ID from the GRE message.
    let msg_id = connect_resp_msg
        .get("msgId")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0);

    // Game state ID from the GRE message.
    let game_state_id = connect_resp_msg
        .get("gameStateId")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0);

    // Settings from connectResp (game configuration metadata).
    let settings = connect_resp
        .and_then(|cr| cr.get("settings"))
        .cloned()
        .unwrap_or(serde_json::json!({}));

    serde_json::json!({
        "type": "connect_resp",
        "deck_cards": deck_cards,
        "sideboard_cards": sideboard_cards,
        "system_seat_ids": system_seat_ids,
        "msg_id": msg_id,
        "game_state_id": game_state_id,
        "settings": settings,
        "raw_connect_resp": full_event,
    })
}

/// Extracts an array of card GRP IDs from a JSON array value.
///
/// Card IDs in the MTGA log are integers representing the card's
/// group/print ID. This function collects all integer values from the
/// array, silently skipping any non-integer entries.
fn extract_card_ids(cards_value: Option<&serde_json::Value>) -> Vec<i64> {
    cards_value
        .and_then(serde_json::Value::as_array)
        .map(|arr| arr.iter().filter_map(serde_json::Value::as_i64).collect())
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::super::test_fixtures::*;
    use super::super::try_parse;
    use super::*;
    use crate::parsers::test_helpers::{game_state_payload, test_timestamp, unity_entry};

    /// Helper: build a `ConnectResp` body with minimal fields (no sideboard,
    /// no settings).
    fn minimal_connect_resp_body() -> String {
        format!(
            "[UnityCrossThreadLogger]greToClientEvent\n{}",
            serde_json::json!({
                "greToClientEvent": {
                    "greToClientMessages": [
                        {
                            "type": "GREMessageType_ConnectResp",
                            "systemSeatIds": [1, 2],
                            "msgId": 1,
                            "connectResp": {
                                "deckMessage": {
                                    "deckCards": [12345, 67890]
                                }
                            }
                        }
                    ]
                }
            })
        )
    }

    // -- Decklist extraction --------------------------------------------------

    mod decklist {
        use super::*;

        #[test]
        fn test_try_parse_connect_resp_deck_cards() {
            let body = connect_resp_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(event);

            let deck_cards = payload["deck_cards"].as_array();
            assert!(deck_cards.is_some());
            let deck_cards = deck_cards.unwrap_or_else(|| unreachable!());
            assert_eq!(deck_cards.len(), 11);
            // First four cards should be the same (4x 68398)
            assert_eq!(deck_cards[0], 68398);
            assert_eq!(deck_cards[1], 68398);
            assert_eq!(deck_cards[2], 68398);
            assert_eq!(deck_cards[3], 68398);
        }

        #[test]
        fn test_try_parse_connect_resp_sideboard_cards() {
            let body = connect_resp_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(event);

            let sideboard = payload["sideboard_cards"].as_array();
            assert!(sideboard.is_some());
            let sideboard = sideboard.unwrap_or_else(|| unreachable!());
            assert_eq!(sideboard.len(), 3);
            assert_eq!(sideboard[0], 73509);
            assert_eq!(sideboard[1], 73509);
            assert_eq!(sideboard[2], 73510);
        }

        #[test]
        fn test_try_parse_connect_resp_no_sideboard_returns_empty() {
            let body = minimal_connect_resp_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(event);

            let sideboard = payload["sideboard_cards"].as_array();
            assert!(sideboard.is_some());
            let sideboard = sideboard.unwrap_or_else(|| unreachable!());
            assert!(sideboard.is_empty());
        }

        #[test]
        fn test_try_parse_connect_resp_deck_cards_minimal() {
            let body = minimal_connect_resp_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(event);

            let deck_cards = payload["deck_cards"].as_array();
            assert!(deck_cards.is_some());
            let deck_cards = deck_cards.unwrap_or_else(|| unreachable!());
            assert_eq!(deck_cards.len(), 2);
            assert_eq!(deck_cards[0], 12345);
            assert_eq!(deck_cards[1], 67890);
        }
    }

    // -- Seat ID extraction ---------------------------------------------------

    mod seat_ids {
        use super::*;

        #[test]
        fn test_try_parse_connect_resp_system_seat_ids() {
            let body = connect_resp_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(event);

            let seat_ids = payload["system_seat_ids"].as_array();
            assert!(seat_ids.is_some());
            let seat_ids = seat_ids.unwrap_or_else(|| unreachable!());
            assert_eq!(seat_ids.len(), 2);
            assert_eq!(seat_ids[0], 1);
            assert_eq!(seat_ids[1], 2);
        }

        #[test]
        fn test_try_parse_connect_resp_seat_ids_flat_format() {
            let body = flat_connect_resp_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(event);

            let seat_ids = payload["system_seat_ids"].as_array();
            assert!(seat_ids.is_some());
            let seat_ids = seat_ids.unwrap_or_else(|| unreachable!());
            assert_eq!(seat_ids.len(), 2);
        }
    }

    // -- Game configuration metadata ------------------------------------------

    mod game_config {
        use super::*;

        #[test]
        fn test_try_parse_connect_resp_settings() {
            let body = connect_resp_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(event);

            assert!(payload["settings"].is_object());
            let stops = payload["settings"]["stops"].as_array();
            assert!(stops.is_some());
            let stops = stops.unwrap_or_else(|| unreachable!());
            assert_eq!(stops.len(), 2);
        }

        #[test]
        fn test_try_parse_connect_resp_auto_pass_option() {
            let body = connect_resp_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(event);

            assert_eq!(
                payload["settings"]["autoPassOption"],
                "AutoPassOption_UnresolvedOnly"
            );
        }

        #[test]
        fn test_try_parse_connect_resp_missing_settings_returns_empty_object() {
            let body = minimal_connect_resp_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(event);

            assert!(payload["settings"].is_object());
            assert!(payload["settings"]
                .as_object()
                .unwrap_or_else(|| unreachable!())
                .is_empty());
        }

        #[test]
        fn test_try_parse_connect_resp_msg_id() {
            let body = connect_resp_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(event);
            assert_eq!(payload["msg_id"], 1);
        }

        #[test]
        fn test_try_parse_connect_resp_game_state_id() {
            let body = connect_resp_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(event);
            assert_eq!(payload["game_state_id"], 0);
        }
    }

    // -- ConnectResp edge cases -----------------------------------------------

    mod connect_resp_edge_cases {
        use super::*;

        #[test]
        fn test_try_parse_connect_resp_missing_deck_message() {
            let body = format!(
                "[UnityCrossThreadLogger]greToClientEvent\n{}",
                serde_json::json!({
                    "greToClientEvent": {
                        "greToClientMessages": [
                            {
                                "type": "GREMessageType_ConnectResp",
                                "systemSeatIds": [1, 2],
                                "connectResp": {}
                            }
                        ]
                    }
                })
            );
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(event);
            // Deck cards should be empty when deckMessage is missing.
            let deck_cards = payload["deck_cards"].as_array();
            assert!(deck_cards.is_some());
            let deck_cards = deck_cards.unwrap_or_else(|| unreachable!());
            assert!(deck_cards.is_empty());
        }

        #[test]
        fn test_try_parse_connect_resp_missing_connect_resp_key() {
            let body = format!(
                "[UnityCrossThreadLogger]greToClientEvent\n{}",
                serde_json::json!({
                    "greToClientEvent": {
                        "greToClientMessages": [
                            {
                                "type": "GREMessageType_ConnectResp",
                                "systemSeatIds": [1, 2]
                            }
                        ]
                    }
                })
            );
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(event);
            // Should still parse but with empty deck/sideboard.
            let deck_cards = payload["deck_cards"].as_array();
            assert!(deck_cards.is_some());
            let deck_cards = deck_cards.unwrap_or_else(|| unreachable!());
            assert!(deck_cards.is_empty());
        }

        #[test]
        fn test_try_parse_connect_resp_empty_deck_cards() {
            let body = format!(
                "[UnityCrossThreadLogger]greToClientEvent\n{}",
                serde_json::json!({
                    "greToClientEvent": {
                        "greToClientMessages": [
                            {
                                "type": "GREMessageType_ConnectResp",
                                "systemSeatIds": [1, 2],
                                "connectResp": {
                                    "deckMessage": {
                                        "deckCards": [],
                                        "sideboardCards": []
                                    }
                                }
                            }
                        ]
                    }
                })
            );
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(event);
            let deck_cards = payload["deck_cards"].as_array();
            assert!(deck_cards.is_some());
            let deck_cards = deck_cards.unwrap_or_else(|| unreachable!());
            assert!(deck_cards.is_empty());
        }

        #[test]
        fn test_try_parse_connect_resp_missing_system_seat_ids() {
            let body = format!(
                "[UnityCrossThreadLogger]greToClientEvent\n{}",
                serde_json::json!({
                    "greToClientEvent": {
                        "greToClientMessages": [
                            {
                                "type": "GREMessageType_ConnectResp",
                                "connectResp": {
                                    "deckMessage": {
                                        "deckCards": [99999]
                                    }
                                }
                            }
                        ]
                    }
                })
            );
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(event);
            let seat_ids = payload["system_seat_ids"].as_array();
            assert!(seat_ids.is_some());
            let seat_ids = seat_ids.unwrap_or_else(|| unreachable!());
            assert!(seat_ids.is_empty());
        }

        #[test]
        fn test_try_parse_multiple_messages_finds_connect_resp() {
            let body = format!(
                "[UnityCrossThreadLogger]greToClientEvent\n{}",
                serde_json::json!({
                    "greToClientEvent": {
                        "greToClientMessages": [
                            {
                                "type": "GREMessageType_GameStateMessage",
                                "gameStateMessage": {"gameObjects": []}
                            },
                            {
                                "type": "GREMessageType_ConnectResp",
                                "systemSeatIds": [1, 2],
                                "connectResp": {
                                    "deckMessage": {
                                        "deckCards": [55555, 66666]
                                    }
                                }
                            }
                        ]
                    }
                })
            );
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(event);
            // ConnectResp has priority over GameStateMessage.
            assert_eq!(payload["type"], "connect_resp");
            let deck_cards = payload["deck_cards"].as_array();
            assert!(deck_cards.is_some());
            let deck_cards = deck_cards.unwrap_or_else(|| unreachable!());
            assert_eq!(deck_cards.len(), 2);
            assert_eq!(deck_cards[0], 55555);
        }

        #[test]
        fn test_try_parse_connect_resp_with_timestamp_in_header() {
            let body = format!(
                "[UnityCrossThreadLogger]2/25/2026 12:00:00 PM greToClientEvent\n{}",
                serde_json::json!({
                    "greToClientEvent": {
                        "greToClientMessages": [
                            {
                                "type": "GREMessageType_ConnectResp",
                                "systemSeatIds": [1, 2],
                                "connectResp": {
                                    "deckMessage": {
                                        "deckCards": [77777]
                                    }
                                }
                            }
                        ]
                    }
                })
            );
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(event);
            assert_eq!(payload["type"], "connect_resp");
        }
    }

    // -- Internal helpers ----------------------------------------------------

    mod card_id_helpers {
        use super::*;

        #[test]
        fn test_extract_card_ids_normal() {
            let value = serde_json::json!([1, 2, 3, 4, 5]);
            let ids = extract_card_ids(Some(&value));
            assert_eq!(ids, vec![1, 2, 3, 4, 5]);
        }

        #[test]
        fn test_extract_card_ids_empty_array() {
            let value = serde_json::json!([]);
            let ids = extract_card_ids(Some(&value));
            assert!(ids.is_empty());
        }

        #[test]
        fn test_extract_card_ids_none() {
            let ids = extract_card_ids(None);
            assert!(ids.is_empty());
        }

        #[test]
        fn test_extract_card_ids_non_array() {
            let value = serde_json::json!("not an array");
            let ids = extract_card_ids(Some(&value));
            assert!(ids.is_empty());
        }
    }
}
