//! GRE message parsers for `greToClientEvent` payloads.
//!
//! Covers `GameStateMessage` (zones, game objects, annotations, turn info),
//! `ConnectResp` (initial game configuration), and `QueuedGameStateMessage`.
//!
//! This module currently implements the `ConnectResp` parser. The
//! `GREMessageType_ConnectResp` message is the initial game configuration
//! message sent at the start of each game. It contains:
//!
//! | Field | Purpose |
//! |-------|---------|
//! | `connectResp.deckMessage.deckCards` | Player's decklist (card GRP IDs) |
//! | `connectResp.deckMessage.sideboardCards` | Sideboard cards (card GRP IDs) |
//! | `systemSeatIds` | All seat IDs in the game |
//! | `connectResp.settings` | Game configuration / settings |
//!
//! This is Class 1 (Interactive Dispatch) -- emitted once at game start
//! to initialize the deck tracker overlay.

use crate::events::{EventMetadata, GameEvent, GameStateEvent};
use crate::log::entry::LogEntry;

/// Marker that identifies a GRE-to-client event entry in the log.
const GRE_TO_CLIENT_MARKER: &str = "greToClientEvent";

/// GRE message type for the initial connection response.
const CONNECT_RESP_TYPE: &str = "GREMessageType_ConnectResp";

/// Attempts to parse a [`LogEntry`] as a GRE `ConnectResp` event.
///
/// Returns `Some(GameEvent::GameState(_))` if the entry body contains a
/// `greToClientEvent` with a `GREMessageType_ConnectResp` message, or
/// `None` if the entry does not match.
///
/// The `timestamp` is used to construct [`EventMetadata`] for the resulting
/// event. Callers are responsible for parsing the timestamp from the log
/// entry header before invoking this function.
pub fn try_parse(entry: &LogEntry, timestamp: chrono::DateTime<chrono::Utc>) -> Option<GameEvent> {
    let body = &entry.body;

    // Quick check: bail early if the GRE marker is not present.
    if !body.contains(GRE_TO_CLIENT_MARKER) {
        return None;
    }

    // Extract the JSON payload from the body.
    let json_str = extract_json_from_body(body)?;

    let parsed: serde_json::Value = match serde_json::from_str(json_str) {
        Ok(v) => v,
        Err(e) => {
            ::log::warn!("greToClientEvent: malformed JSON payload: {e}");
            return None;
        }
    };

    // Find the ConnectResp message within the greToClientMessages array.
    let connect_resp_msg = find_connect_resp_message(&parsed)?;

    let payload = build_payload(connect_resp_msg, &parsed);
    let metadata = EventMetadata::new(timestamp, body.as_bytes().to_vec());
    Some(GameEvent::GameState(GameStateEvent::new(metadata, payload)))
}

/// Searches the `greToClientMessages` array for a `GREMessageType_ConnectResp`
/// message and returns a reference to it.
///
/// The GRE event wraps messages in a `greToClientMessages` array. Each
/// message has a `type` field identifying its kind.
fn find_connect_resp_message(parsed: &serde_json::Value) -> Option<&serde_json::Value> {
    let messages = parsed
        .get("greToClientEvent")
        .and_then(|e| e.get("greToClientMessages"))
        .or_else(|| parsed.get("greToClientMessages"))
        .and_then(serde_json::Value::as_array)?;

    messages
        .iter()
        .find(|msg| msg.get("type").and_then(serde_json::Value::as_str) == Some(CONNECT_RESP_TYPE))
}

/// Builds a structured payload from the `ConnectResp` message.
///
/// Extracts key fields from the nested JSON structure into a flat(ter)
/// payload for downstream consumers (deck tracker, overlay).
fn build_payload(
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

/// Extracts the first JSON object from a multi-line log body.
///
/// Scans for the first `{` character and finds the matching `}` using
/// brace-depth counting that respects string literals.
fn extract_json_from_body(body: &str) -> Option<&str> {
    let json_start = body.find('{')?;
    let candidate = &body[json_start..];

    let mut depth: i32 = 0;
    let mut in_string = false;
    let mut escape_next = false;
    let mut end_pos = None;

    for (i, ch) in candidate.char_indices() {
        if escape_next {
            escape_next = false;
            continue;
        }
        match ch {
            '\\' if in_string => {
                escape_next = true;
            }
            '"' => {
                in_string = !in_string;
            }
            '{' if !in_string => {
                depth += 1;
            }
            '}' if !in_string => {
                depth -= 1;
                if depth == 0 {
                    end_pos = Some(i + 1);
                    break;
                }
            }
            _ => {}
        }
    }

    end_pos.map(|end| &candidate[..end])
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log::entry::EntryHeader;
    use chrono::{TimeZone, Utc};

    /// Helper: build a UTC timestamp for tests.
    ///
    /// Uses `unwrap_or_default()` because `clippy::expect_used` is denied
    /// crate-wide. The epoch fallback would visibly fail timestamp assertions.
    fn test_timestamp() -> chrono::DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 2, 25, 12, 0, 0)
            .single()
            .unwrap_or_default()
    }

    /// Helper: build a `LogEntry` with `UnityCrossThreadLogger` header.
    fn unity_entry(body: &str) -> LogEntry {
        LogEntry {
            header: EntryHeader::UnityCrossThreadLogger,
            body: body.to_owned(),
        }
    }

    /// Helper: extract the JSON payload from a `GameEvent::GameState` variant.
    ///
    /// Returns a static null value if the variant is not `GameState`,
    /// which will cause assertion failures that clearly indicate the wrong
    /// variant was produced.
    fn game_state_payload(event: &GameEvent) -> &serde_json::Value {
        static EMPTY: std::sync::LazyLock<serde_json::Value> =
            std::sync::LazyLock::new(|| serde_json::json!(null));
        match event {
            GameEvent::GameState(e) => e.payload(),
            _ => &EMPTY,
        }
    }

    /// Helper: build a realistic `greToClientEvent` JSON body containing
    /// a `GREMessageType_ConnectResp` message with a full decklist.
    fn connect_resp_body() -> String {
        format!(
            "[UnityCrossThreadLogger]greToClientEvent\n{}",
            serde_json::json!({
                "greToClientEvent": {
                    "greToClientMessages": [
                        {
                            "type": "GREMessageType_ConnectResp",
                            "systemSeatIds": [1, 2],
                            "msgId": 1,
                            "gameStateId": 0,
                            "connectResp": {
                                "status": "ConnectionStatus_Success",
                                "deckMessage": {
                                    "deckCards": [68398, 68398, 68398, 68398, 70136, 70136, 70136, 70136, 71432, 71432, 71432],
                                    "sideboardCards": [73509, 73509, 73510]
                                },
                                "settings": {
                                    "stops": [
                                        {"stopType": "StopType_UpkeepStep", "appliesTo": "SettingScope_Team"},
                                        {"stopType": "StopType_DrawStep", "appliesTo": "SettingScope_Team"}
                                    ],
                                    "autoPassOption": "AutoPassOption_UnresolvedOnly",
                                    "graveyardOrder": "OrderType_OrderArbitrary",
                                    "manaSelectionType": "ManaSelectionType_Auto",
                                    "autoTapStopsSetting": "AutoTapStopsSetting_Enable",
                                    "autoOptionalPaymentCancellationSetting": "AutoOptionalPaymentCancellationSetting_AskMe",
                                    "transientStops": [],
                                    "cosmetics": []
                                }
                            }
                        }
                    ]
                }
            })
        )
    }

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

    /// Helper: build a `ConnectResp` body without the wrapper
    /// `greToClientEvent` key (flat format).
    fn flat_connect_resp_body() -> String {
        format!(
            "[UnityCrossThreadLogger]greToClientEvent\n{}",
            serde_json::json!({
                "greToClientMessages": [
                    {
                        "type": "GREMessageType_ConnectResp",
                        "systemSeatIds": [1, 2],
                        "msgId": 2,
                        "gameStateId": 1,
                        "connectResp": {
                            "deckMessage": {
                                "deckCards": [11111, 22222, 33333],
                                "sideboardCards": [44444]
                            },
                            "settings": {
                                "autoPassOption": "AutoPassOption_ResolveAll"
                            }
                        }
                    }
                ]
            })
        )
    }

    /// Helper: build a GRE event body with a non-ConnectResp message type.
    fn game_state_message_body() -> String {
        format!(
            "[UnityCrossThreadLogger]greToClientEvent\n{}",
            serde_json::json!({
                "greToClientEvent": {
                    "greToClientMessages": [
                        {
                            "type": "GREMessageType_GameStateMessage",
                            "gameStateMessage": {
                                "gameObjects": [],
                                "turnInfo": {}
                            }
                        }
                    ]
                }
            })
        )
    }

    // -- ConnectResp detection ------------------------------------------------

    mod connect_resp_detection {
        use super::*;

        #[test]
        fn test_try_parse_connect_resp_detected() {
            let body = connect_resp_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, test_timestamp());
            assert!(result.is_some());
        }

        #[test]
        fn test_try_parse_connect_resp_correct_variant() {
            let body = connect_resp_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, test_timestamp());
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            assert!(matches!(event, GameEvent::GameState(_)));
        }

        #[test]
        fn test_try_parse_connect_resp_type_field() {
            let body = connect_resp_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, test_timestamp());
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(event);
            assert_eq!(payload["type"], "connect_resp");
        }
    }

    // -- Decklist extraction --------------------------------------------------

    mod decklist {
        use super::*;

        #[test]
        fn test_try_parse_connect_resp_deck_cards() {
            let body = connect_resp_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, test_timestamp());
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
            let result = try_parse(&entry, test_timestamp());
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
            let result = try_parse(&entry, test_timestamp());
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
            let result = try_parse(&entry, test_timestamp());
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
            let result = try_parse(&entry, test_timestamp());
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
            let result = try_parse(&entry, test_timestamp());
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
            let result = try_parse(&entry, test_timestamp());
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
            let result = try_parse(&entry, test_timestamp());
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
            let result = try_parse(&entry, test_timestamp());
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
            let result = try_parse(&entry, test_timestamp());
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(event);
            assert_eq!(payload["msg_id"], 1);
        }

        #[test]
        fn test_try_parse_connect_resp_game_state_id() {
            let body = connect_resp_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, test_timestamp());
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(event);
            assert_eq!(payload["game_state_id"], 0);
        }
    }

    // -- Flat format (no greToClientEvent wrapper) ----------------------------

    mod flat_format {
        use super::*;

        #[test]
        fn test_try_parse_flat_format_detected() {
            let body = flat_connect_resp_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, test_timestamp());
            assert!(result.is_some());
        }

        #[test]
        fn test_try_parse_flat_format_deck_cards() {
            let body = flat_connect_resp_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, test_timestamp());
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(event);

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
            let result = try_parse(&entry, test_timestamp());
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(event);

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
            let result = try_parse(&entry, test_timestamp());
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(event);

            assert_eq!(
                payload["settings"]["autoPassOption"],
                "AutoPassOption_ResolveAll"
            );
        }

        #[test]
        fn test_try_parse_flat_format_msg_id() {
            let body = flat_connect_resp_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, test_timestamp());
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(event);
            assert_eq!(payload["msg_id"], 2);
        }
    }

    // -- Raw event preservation -----------------------------------------------

    mod raw_event {
        use super::*;

        #[test]
        fn test_try_parse_connect_resp_preserves_raw_connect_resp() {
            let body = connect_resp_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, test_timestamp());
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(event);

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
            let result = try_parse(&entry, test_timestamp());
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            assert_eq!(event.metadata().raw_bytes(), body.as_bytes());
        }

        #[test]
        fn test_try_parse_connect_resp_stores_timestamp() {
            let body = connect_resp_body();
            let entry = unity_entry(&body);
            let ts = test_timestamp();
            let result = try_parse(&entry, ts);
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            assert_eq!(event.metadata().timestamp(), ts);
        }
    }

    // -- Non-ConnectResp entries (should return None) --------------------------

    mod non_connect_resp {
        use super::*;

        #[test]
        fn test_try_parse_game_state_message_returns_none() {
            let body = game_state_message_body();
            let entry = unity_entry(&body);
            assert!(try_parse(&entry, test_timestamp()).is_none());
        }

        #[test]
        fn test_try_parse_unrelated_entry_returns_none() {
            let body = "[UnityCrossThreadLogger]Updated account. DisplayName:Test";
            let entry = unity_entry(body);
            assert!(try_parse(&entry, test_timestamp()).is_none());
        }

        #[test]
        fn test_try_parse_empty_body_returns_none() {
            let body = "[UnityCrossThreadLogger]";
            let entry = unity_entry(body);
            assert!(try_parse(&entry, test_timestamp()).is_none());
        }

        #[test]
        fn test_try_parse_match_state_event_returns_none() {
            let body = "[UnityCrossThreadLogger]matchGameRoomStateChangedEvent\n\
                         {\"matchGameRoomStateChangedEvent\": {\"gameRoomInfo\": {}}}";
            let entry = unity_entry(body);
            assert!(try_parse(&entry, test_timestamp()).is_none());
        }

        #[test]
        fn test_try_parse_no_json_body_returns_none() {
            let body = "[UnityCrossThreadLogger]greToClientEvent with no json";
            let entry = unity_entry(body);
            assert!(try_parse(&entry, test_timestamp()).is_none());
        }

        #[test]
        fn test_try_parse_malformed_json_returns_none() {
            let body = "[UnityCrossThreadLogger]greToClientEvent\n{invalid json}";
            let entry = unity_entry(body);
            assert!(try_parse(&entry, test_timestamp()).is_none());
        }

        #[test]
        fn test_try_parse_gre_event_without_messages_returns_none() {
            let body = format!(
                "[UnityCrossThreadLogger]greToClientEvent\n{}",
                serde_json::json!({
                    "greToClientEvent": {
                        "someOtherField": "value"
                    }
                })
            );
            let entry = unity_entry(&body);
            assert!(try_parse(&entry, test_timestamp()).is_none());
        }

        #[test]
        fn test_try_parse_gre_event_empty_messages_returns_none() {
            let body = format!(
                "[UnityCrossThreadLogger]greToClientEvent\n{}",
                serde_json::json!({
                    "greToClientEvent": {
                        "greToClientMessages": []
                    }
                })
            );
            let entry = unity_entry(&body);
            assert!(try_parse(&entry, test_timestamp()).is_none());
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
            // Note: This returns Some because the parser only checks the body
            // content, not the header type. The GRE marker is present in the
            // body. This is valid -- ConnectResp can appear under either header.
            let result = try_parse(&entry, test_timestamp());
            assert!(result.is_some());
        }
    }

    // -- Edge cases -----------------------------------------------------------

    mod edge_cases {
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
            let result = try_parse(&entry, test_timestamp());
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
            let result = try_parse(&entry, test_timestamp());
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
            let result = try_parse(&entry, test_timestamp());
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
            let result = try_parse(&entry, test_timestamp());
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
            let result = try_parse(&entry, test_timestamp());
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(event);
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
            let result = try_parse(&entry, test_timestamp());
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(event);
            assert_eq!(payload["type"], "connect_resp");
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
            let result = try_parse(&entry, test_timestamp());
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
        fn test_extract_json_from_body_simple() {
            let body = "header line\n{\"key\": \"value\"}";
            let json = extract_json_from_body(body);
            assert_eq!(json, Some("{\"key\": \"value\"}"));
        }

        #[test]
        fn test_extract_json_from_body_nested() {
            let body = "header\n{\"outer\": {\"inner\": 1}}";
            let json = extract_json_from_body(body);
            assert_eq!(json, Some("{\"outer\": {\"inner\": 1}}"));
        }

        #[test]
        fn test_extract_json_from_body_no_json() {
            let body = "no json here at all";
            assert!(extract_json_from_body(body).is_none());
        }

        #[test]
        fn test_extract_json_from_body_with_string_braces() {
            let body = "header\n{\"msg\": \"hello {world}\"}";
            let json = extract_json_from_body(body);
            assert_eq!(json, Some("{\"msg\": \"hello {world}\"}"));
        }

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

        #[test]
        fn test_find_connect_resp_message_found() {
            let parsed = serde_json::json!({
                "greToClientEvent": {
                    "greToClientMessages": [
                        {"type": "GREMessageType_ConnectResp", "connectResp": {}}
                    ]
                }
            });
            assert!(find_connect_resp_message(&parsed).is_some());
        }

        #[test]
        fn test_find_connect_resp_message_not_found() {
            let parsed = serde_json::json!({
                "greToClientEvent": {
                    "greToClientMessages": [
                        {"type": "GREMessageType_GameStateMessage"}
                    ]
                }
            });
            assert!(find_connect_resp_message(&parsed).is_none());
        }

        #[test]
        fn test_find_connect_resp_message_empty_messages() {
            let parsed = serde_json::json!({
                "greToClientEvent": {
                    "greToClientMessages": []
                }
            });
            assert!(find_connect_resp_message(&parsed).is_none());
        }

        #[test]
        fn test_find_connect_resp_message_no_messages_key() {
            let parsed = serde_json::json!({
                "greToClientEvent": {}
            });
            assert!(find_connect_resp_message(&parsed).is_none());
        }
    }
}
