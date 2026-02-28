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
//! `gameInfo.stage` equals `GameStage_GameOver` — these are emitted as
//! `GameEvent::GameResult` (Class 3, Post-Game Batch) to trigger batch
//! assembly in downstream consumers.
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
//! Annotations, turn info, and timers are deferred to B-7d.

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

/// Attempts to parse a [`LogEntry`] as a GRE event.
///
/// Dispatches to the appropriate sub-parser based on the GRE message type:
/// - `GREMessageType_ConnectResp` -> `connect_resp` payload
/// - `GREMessageType_GameStateMessage` -> `game_state_message` payload
///   (or `game_result` if `gameInfo.stage == GameStage_GameOver`)
/// - `GREMessageType_QueuedGameStateMessage` -> same as `GameStateMessage`
///
/// Returns `Some(GameEvent::GameState(_))` for normal game state updates,
/// `Some(GameEvent::GameResult(_))` for game-over messages, or `None` if
/// the entry does not match.
///
/// The `timestamp` is `None` when the log entry header did not contain a
/// parseable timestamp. It is passed through to [`EventMetadata`] so
/// downstream consumers can distinguish real vs missing timestamps.
pub fn try_parse(
    entry: &LogEntry,
    timestamp: Option<chrono::DateTime<chrono::Utc>>,
) -> Option<GameEvent> {
    let body = &entry.body;

    // Quick check: bail early if the GRE marker is not present.
    if !body.contains(GRE_TO_CLIENT_MARKER) {
        return None;
    }

    // Extract and parse the JSON payload from the body.
    let parsed = api_common::parse_json_from_body(body, "greToClientEvent")?;

    let messages = extract_gre_messages(&parsed)?;

    // Try ConnectResp first (highest priority, emitted once at game start).
    if let Some(connect_resp_msg) = find_message_by_type(messages, CONNECT_RESP_TYPE) {
        let payload = build_connect_resp_payload(connect_resp_msg, &parsed);
        let metadata = EventMetadata::new(timestamp, body.as_bytes().to_vec());
        return Some(GameEvent::GameState(GameStateEvent::new(metadata, payload)));
    }

    // Try GameStateMessage (most frequent during gameplay).
    if let Some(gsm) = find_message_by_type(messages, GAME_STATE_MESSAGE_TYPE) {
        let metadata = EventMetadata::new(timestamp, body.as_bytes().to_vec());
        if is_game_over(gsm) {
            let payload = build_game_result_payload(gsm);
            return Some(GameEvent::GameResult(GameResultEvent::new(
                metadata, payload,
            )));
        }
        let payload = build_game_state_message_payload(gsm);
        return Some(GameEvent::GameState(GameStateEvent::new(metadata, payload)));
    }

    // Try QueuedGameStateMessage (deferred game state, same structure).
    if let Some(qgsm) = find_message_by_type(messages, QUEUED_GAME_STATE_MESSAGE_TYPE) {
        let metadata = EventMetadata::new(timestamp, body.as_bytes().to_vec());
        if is_game_over(qgsm) {
            let payload = build_game_result_payload(qgsm);
            return Some(GameEvent::GameResult(GameResultEvent::new(
                metadata, payload,
            )));
        }
        let payload = build_game_state_message_payload(qgsm);
        return Some(GameEvent::GameState(GameStateEvent::new(metadata, payload)));
    }

    // Check for low-value noise types (UIMessage, TimerStateMessage,
    // SetSettingsResp). Claimed with a minimal payload so they don't inflate
    // the unclaimed-entry residual.
    for &noise_type in NOISE_MESSAGE_TYPES {
        if find_message_by_type(messages, noise_type).is_some() {
            ::log::trace!("greToClientEvent: claimed noise type {noise_type}");
            let payload = serde_json::json!({ "recognized_type": noise_type });
            let metadata = EventMetadata::new(timestamp, body.as_bytes().to_vec());
            return Some(GameEvent::GameState(GameStateEvent::new(metadata, payload)));
        }
    }

    // Unrecognized GRE message type — log and skip.
    ::log::debug!("greToClientEvent: no recognized message type found");
    None
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
// ConnectResp payload builder
// ---------------------------------------------------------------------------

/// Builds a structured payload from the `ConnectResp` message.
///
/// Extracts key fields from the nested JSON structure into a flat(ter)
/// payload for downstream consumers (deck tracker, overlay).
fn build_connect_resp_payload(
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

// ---------------------------------------------------------------------------
// GameStateMessage payload builder
// ---------------------------------------------------------------------------

/// Builds a structured payload from a `GameStateMessage` or
/// `QueuedGameStateMessage`.
///
/// Extracts zones, game objects, and game info from the
/// `gameStateMessage` sub-object. The output payload has the shape:
///
/// ```json
/// {
///   "type": "game_state_message",
///   "msg_id": 5,
///   "game_state_id": 42,
///   "zones": [ { "zone_id": 1, "zone_type": "ZoneType_Hand", ... }, ... ],
///   "game_objects": [ { "instance_id": 100, "grp_id": 68398, ... }, ... ],
///   "game_info": { ... }
/// }
/// ```
///
/// Incremental updates may include only a subset of zones and objects.
/// Missing fields default to empty arrays / null.
fn build_game_state_message_payload(gre_msg: &serde_json::Value) -> serde_json::Value {
    let gsm = gre_msg.get("gameStateMessage");

    // Message-level metadata.
    let msg_id = gre_msg
        .get("msgId")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0);

    let game_state_id = gre_msg
        .get("gameStateId")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0);

    // Determine the payload type based on the GRE message type.
    let gre_type = gre_msg
        .get("type")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let payload_type = if gre_type == QUEUED_GAME_STATE_MESSAGE_TYPE {
        "queued_game_state_message"
    } else {
        "game_state_message"
    };

    // Extract zones from gameStateMessage.zones[].
    let zones = extract_zones(gsm);

    // Extract game objects from gameStateMessage.gameObjects[].
    let game_objects = extract_game_objects(gsm);

    // Extract game info metadata.
    let game_info = gsm
        .and_then(|g| g.get("gameInfo"))
        .cloned()
        .unwrap_or(serde_json::Value::Null);

    serde_json::json!({
        "type": payload_type,
        "msg_id": msg_id,
        "game_state_id": game_state_id,
        "zones": zones,
        "game_objects": game_objects,
        "game_info": game_info,
    })
}

// ---------------------------------------------------------------------------
// GameStage_GameOver detection and payload builder
// ---------------------------------------------------------------------------

/// Returns `true` if the GRE message contains `GameStage_GameOver` in
/// `gameStateMessage.gameInfo.stage`.
fn is_game_over(gre_msg: &serde_json::Value) -> bool {
    gre_msg
        .get("gameStateMessage")
        .and_then(|gsm| gsm.get("gameInfo"))
        .and_then(|gi| gi.get("stage"))
        .and_then(serde_json::Value::as_str)
        == Some(GAME_STAGE_GAME_OVER)
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
fn build_game_result_payload(gre_msg: &serde_json::Value) -> serde_json::Value {
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

    // Find the first MatchScope_Game result for top-level convenience fields.
    let game_scope_result = game_info
        .and_then(|gi| gi.get("results"))
        .and_then(serde_json::Value::as_array)
        .and_then(|arr| {
            arr.iter().find(|r| {
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

/// Extracts zone descriptors from the `gameStateMessage.zones` array.
///
/// Each zone in the MTGA log has the structure:
/// ```json
/// {
///   "zoneId": 30,
///   "type": "ZoneType_Hand",
///   "visibility": "Visibility_Public",
///   "ownerSeatId": 1,
///   "objectInstanceIds": [101, 102, 103]
/// }
/// ```
///
/// The output normalizes field names to `snake_case` for consistency
/// with the rest of the parser output.
fn extract_zones(gsm: Option<&serde_json::Value>) -> Vec<serde_json::Value> {
    let Some(raw_zones) = gsm
        .and_then(|g| g.get("zones"))
        .and_then(serde_json::Value::as_array)
    else {
        return Vec::new();
    };

    raw_zones.iter().filter_map(extract_single_zone).collect()
}

/// Extracts a single zone descriptor, normalizing field names to `snake_case`.
fn extract_single_zone(zone: &serde_json::Value) -> Option<serde_json::Value> {
    let zone_id = zone.get("zoneId").and_then(serde_json::Value::as_i64)?;
    let zone_type = zone
        .get("type")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("Unknown");

    let owner_seat_id = zone
        .get("ownerSeatId")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0);

    let visibility = zone
        .get("visibility")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");

    let object_instance_ids = zone
        .get("objectInstanceIds")
        .and_then(serde_json::Value::as_array)
        .map(|ids| {
            ids.iter()
                .filter_map(serde_json::Value::as_i64)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    Some(serde_json::json!({
        "zone_id": zone_id,
        "zone_type": zone_type,
        "owner_seat_id": owner_seat_id,
        "visibility": visibility,
        "object_instance_ids": object_instance_ids,
    }))
}

/// Extracts game object descriptors from the `gameStateMessage.gameObjects` array.
///
/// Each game object in the MTGA log has the structure:
/// ```json
/// {
///   "instanceId": 101,
///   "grpId": 68398,
///   "type": "GameObjectType_Card",
///   "zoneId": 30,
///   "visibility": "Visibility_Public",
///   "ownerSeatId": 1,
///   "controllerSeatId": 1,
///   "cardTypes": ["CardType_Creature"],
///   "subtypes": ["SubType_Human", "SubType_Soldier"],
///   "name": 68398,
///   "power": { "value": 3 },
///   "toughness": { "value": 2 }
/// }
/// ```
///
/// The output normalizes field names to `snake_case`. Not all fields are
/// present in every object (incremental updates, non-card types, etc.).
fn extract_game_objects(gsm: Option<&serde_json::Value>) -> Vec<serde_json::Value> {
    let Some(raw_objects) = gsm
        .and_then(|g| g.get("gameObjects"))
        .and_then(serde_json::Value::as_array)
    else {
        return Vec::new();
    };

    raw_objects
        .iter()
        .filter_map(extract_single_game_object)
        .collect()
}

/// Extracts a single game object, normalizing field names to `snake_case`.
fn extract_single_game_object(obj: &serde_json::Value) -> Option<serde_json::Value> {
    let instance_id = obj.get("instanceId").and_then(serde_json::Value::as_i64)?;

    let grp_id = obj
        .get("grpId")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0);

    let object_type = obj
        .get("type")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("Unknown");

    let zone_id = obj
        .get("zoneId")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0);

    let visibility = obj
        .get("visibility")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");

    let owner_seat_id = obj
        .get("ownerSeatId")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0);

    let controller_seat_id = obj
        .get("controllerSeatId")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0);

    let card_types = extract_string_array(obj.get("cardTypes"));
    let subtypes = extract_string_array(obj.get("subtypes"));
    let abilities = extract_string_array(obj.get("abilities"));

    // name can be an integer (grpId reference) or string.
    let name = obj.get("name").cloned().unwrap_or(serde_json::Value::Null);

    // Power/toughness are nested objects with a "value" field.
    let power = extract_nested_value(obj.get("power"));
    let toughness = extract_nested_value(obj.get("toughness"));

    Some(serde_json::json!({
        "instance_id": instance_id,
        "grp_id": grp_id,
        "object_type": object_type,
        "zone_id": zone_id,
        "visibility": visibility,
        "owner_seat_id": owner_seat_id,
        "controller_seat_id": controller_seat_id,
        "card_types": card_types,
        "subtypes": subtypes,
        "abilities": abilities,
        "name": name,
        "power": power,
        "toughness": toughness,
    }))
}

// ---------------------------------------------------------------------------
// Shared extraction helpers
// ---------------------------------------------------------------------------

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

/// Extracts an array of strings from a JSON array value.
///
/// Collects all string values, silently skipping non-string entries.
fn extract_string_array(value: Option<&serde_json::Value>) -> Vec<String> {
    value
        .and_then(serde_json::Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(serde_json::Value::as_str)
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
}

/// Extracts a numeric value from a nested `{ "value": N }` object.
///
/// Power and toughness in MTGA logs are represented as objects with
/// a `value` field (e.g., `{ "value": 3 }`). Returns `null` if the
/// structure is missing or malformed.
fn extract_nested_value(obj: Option<&serde_json::Value>) -> serde_json::Value {
    obj.and_then(|o| o.get("value"))
        .cloned()
        .unwrap_or(serde_json::Value::Null)
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

    /// Helper: build sample zone fixtures for a full game state snapshot.
    fn sample_zones() -> serde_json::Value {
        serde_json::json!([
            {"zoneId": 30, "type": "ZoneType_Hand", "visibility": "Visibility_Public",
             "ownerSeatId": 1, "objectInstanceIds": [101, 102, 103]},
            {"zoneId": 31, "type": "ZoneType_Library", "visibility": "Visibility_Hidden",
             "ownerSeatId": 1, "objectInstanceIds": [201, 202, 203, 204, 205]},
            {"zoneId": 32, "type": "ZoneType_Battlefield", "visibility": "Visibility_Public",
             "ownerSeatId": 0, "objectInstanceIds": [301, 302]},
            {"zoneId": 33, "type": "ZoneType_Graveyard", "visibility": "Visibility_Public",
             "ownerSeatId": 1, "objectInstanceIds": [401]},
            {"zoneId": 34, "type": "ZoneType_Exile", "visibility": "Visibility_Public",
             "ownerSeatId": 0, "objectInstanceIds": []},
            {"zoneId": 35, "type": "ZoneType_Stack", "visibility": "Visibility_Public",
             "ownerSeatId": 0, "objectInstanceIds": [501]}
        ])
    }

    /// Helper: build sample game object fixtures.
    fn sample_game_objects() -> serde_json::Value {
        serde_json::json!([
            {"instanceId": 101, "grpId": 68398, "type": "GameObjectType_Card",
             "zoneId": 30, "visibility": "Visibility_Public",
             "ownerSeatId": 1, "controllerSeatId": 1,
             "cardTypes": ["CardType_Creature"],
             "subtypes": ["SubType_Human", "SubType_Soldier"],
             "abilities": ["AbilityType_Lifelink"],
             "name": 68398, "power": {"value": 3}, "toughness": {"value": 2}},
            {"instanceId": 301, "grpId": 70136, "type": "GameObjectType_Card",
             "zoneId": 32, "visibility": "Visibility_Public",
             "ownerSeatId": 1, "controllerSeatId": 1,
             "cardTypes": ["CardType_Land"], "subtypes": ["SubType_Plains"],
             "abilities": [], "name": 70136},
            {"instanceId": 501, "grpId": 71432, "type": "GameObjectType_Card",
             "zoneId": 35, "visibility": "Visibility_Public",
             "ownerSeatId": 1, "controllerSeatId": 1,
             "cardTypes": ["CardType_Instant"], "subtypes": [],
             "abilities": [], "name": 71432}
        ])
    }

    /// Helper: build a GRE event body with a `GameStateMessage` containing
    /// zones and game objects.
    fn game_state_message_body() -> String {
        let zones = sample_zones();
        let objects = sample_game_objects();
        format!(
            "[UnityCrossThreadLogger]greToClientEvent\n{}",
            serde_json::json!({
                "greToClientEvent": {
                    "greToClientMessages": [{
                        "type": "GREMessageType_GameStateMessage",
                        "msgId": 5,
                        "gameStateId": 42,
                        "gameStateMessage": {
                            "zones": zones,
                            "gameObjects": objects,
                            "gameInfo": {
                                "matchID": "match-id-12345",
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

    /// Helper: build a minimal `GameStateMessage` body with just one zone
    /// and no game objects (incremental update pattern).
    fn minimal_game_state_message_body() -> String {
        format!(
            "[UnityCrossThreadLogger]greToClientEvent\n{}",
            serde_json::json!({
                "greToClientEvent": {
                    "greToClientMessages": [
                        {
                            "type": "GREMessageType_GameStateMessage",
                            "msgId": 10,
                            "gameStateId": 50,
                            "gameStateMessage": {
                                "zones": [
                                    {
                                        "zoneId": 30,
                                        "type": "ZoneType_Hand",
                                        "ownerSeatId": 1,
                                        "objectInstanceIds": [101, 102]
                                    }
                                ]
                            }
                        }
                    ]
                }
            })
        )
    }

    /// Helper: build a `QueuedGameStateMessage` body.
    fn queued_game_state_message_body() -> String {
        format!(
            "[UnityCrossThreadLogger]greToClientEvent\n{}",
            serde_json::json!({
                "greToClientEvent": {
                    "greToClientMessages": [
                        {
                            "type": "GREMessageType_QueuedGameStateMessage",
                            "msgId": 7,
                            "gameStateId": 60,
                            "gameStateMessage": {
                                "zones": [
                                    {
                                        "zoneId": 32,
                                        "type": "ZoneType_Battlefield",
                                        "visibility": "Visibility_Public",
                                        "ownerSeatId": 0,
                                        "objectInstanceIds": [301, 302, 303]
                                    }
                                ],
                                "gameObjects": [
                                    {
                                        "instanceId": 303,
                                        "grpId": 72000,
                                        "type": "GameObjectType_Card",
                                        "zoneId": 32,
                                        "visibility": "Visibility_Public",
                                        "ownerSeatId": 2,
                                        "controllerSeatId": 2,
                                        "cardTypes": ["CardType_Creature"],
                                        "subtypes": [],
                                        "abilities": [],
                                        "name": 72000,
                                        "power": { "value": 5 },
                                        "toughness": { "value": 5 }
                                    }
                                ]
                            }
                        }
                    ]
                }
            })
        )
    }

    /// Helper: build a `GameStateMessage` body in flat format (no wrapper).
    fn flat_game_state_message_body() -> String {
        format!(
            "[UnityCrossThreadLogger]greToClientEvent\n{}",
            serde_json::json!({
                "greToClientMessages": [
                    {
                        "type": "GREMessageType_GameStateMessage",
                        "msgId": 3,
                        "gameStateId": 15,
                        "gameStateMessage": {
                            "zones": [
                                {
                                    "zoneId": 30,
                                    "type": "ZoneType_Hand",
                                    "ownerSeatId": 2,
                                    "objectInstanceIds": [601, 602]
                                }
                            ],
                            "gameObjects": [
                                {
                                    "instanceId": 601,
                                    "grpId": 80000,
                                    "type": "GameObjectType_Card",
                                    "zoneId": 30,
                                    "ownerSeatId": 2,
                                    "controllerSeatId": 2
                                }
                            ]
                        }
                    }
                ]
            })
        )
    }

    /// Helper: build a `GameStateMessage` with an empty `gameStateMessage`
    /// (no zones, no objects, no game info).
    fn empty_game_state_message_body() -> String {
        format!(
            "[UnityCrossThreadLogger]greToClientEvent\n{}",
            serde_json::json!({
                "greToClientEvent": {
                    "greToClientMessages": [
                        {
                            "type": "GREMessageType_GameStateMessage",
                            "msgId": 1,
                            "gameStateMessage": {}
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
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
        }

        #[test]
        fn test_try_parse_connect_resp_correct_variant() {
            let body = connect_resp_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            assert!(matches!(event, GameEvent::GameState(_)));
        }

        #[test]
        fn test_try_parse_connect_resp_type_field() {
            let body = connect_resp_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
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

    // -- Flat format (no greToClientEvent wrapper) ----------------------------

    mod flat_format {
        use super::*;

        #[test]
        fn test_try_parse_flat_format_detected() {
            let body = flat_connect_resp_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
        }

        #[test]
        fn test_try_parse_flat_format_deck_cards() {
            let body = flat_connect_resp_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
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
            let result = try_parse(&entry, Some(test_timestamp()));
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
            let result = try_parse(&entry, Some(test_timestamp()));
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
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(event);
            assert_eq!(payload["msg_id"], 2);
        }

        #[test]
        fn test_try_parse_flat_format_game_state_message() {
            let body = flat_game_state_message_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(event);
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
            let result = try_parse(&entry, Some(test_timestamp()));
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
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            assert_eq!(event.metadata().raw_bytes(), body.as_bytes());
        }

        #[test]
        fn test_try_parse_connect_resp_stores_timestamp() {
            let body = connect_resp_body();
            let entry = unity_entry(&body);
            let ts = Some(test_timestamp());
            let result = try_parse(&entry, ts);
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            assert_eq!(event.metadata().timestamp(), ts);
        }
    }

    // -- Non-GRE entries (should return None) ---------------------------------

    mod non_gre_entries {
        use super::*;

        #[test]
        fn test_try_parse_unrelated_entry_returns_none() {
            let body = "[UnityCrossThreadLogger]Updated account. DisplayName:Test";
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
        fn test_try_parse_match_state_event_returns_none() {
            let body = "[UnityCrossThreadLogger]matchGameRoomStateChangedEvent\n\
                         {\"matchGameRoomStateChangedEvent\": {\"gameRoomInfo\": {}}}";
            let entry = unity_entry(body);
            assert!(try_parse(&entry, Some(test_timestamp())).is_none());
        }

        #[test]
        fn test_try_parse_no_json_body_returns_none() {
            let body = "[UnityCrossThreadLogger]greToClientEvent with no json";
            let entry = unity_entry(body);
            assert!(try_parse(&entry, Some(test_timestamp())).is_none());
        }

        #[test]
        fn test_try_parse_malformed_json_returns_none() {
            let body = "[UnityCrossThreadLogger]greToClientEvent\n{invalid json}";
            let entry = unity_entry(body);
            assert!(try_parse(&entry, Some(test_timestamp())).is_none());
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
            assert!(try_parse(&entry, Some(test_timestamp())).is_none());
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
            assert!(try_parse(&entry, Some(test_timestamp())).is_none());
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
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
        }

        #[test]
        fn test_try_parse_unknown_gre_message_type_returns_none() {
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
            assert!(try_parse(&entry, Some(test_timestamp())).is_none());
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

    // -- GameStateMessage detection -------------------------------------------

    mod game_state_detection {
        use super::*;

        #[test]
        fn test_try_parse_game_state_message_detected() {
            let body = game_state_message_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
        }

        #[test]
        fn test_try_parse_game_state_message_correct_variant() {
            let body = game_state_message_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            assert!(matches!(event, GameEvent::GameState(_)));
        }

        #[test]
        fn test_try_parse_game_state_message_type_field() {
            let body = game_state_message_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(event);
            assert_eq!(payload["type"], "game_state_message");
        }

        #[test]
        fn test_try_parse_game_state_message_msg_id() {
            let body = game_state_message_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(event);
            assert_eq!(payload["msg_id"], 5);
        }

        #[test]
        fn test_try_parse_game_state_message_game_state_id() {
            let body = game_state_message_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(event);
            assert_eq!(payload["game_state_id"], 42);
        }

        #[test]
        fn test_try_parse_game_state_message_preserves_raw_bytes() {
            let body = game_state_message_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            assert_eq!(event.metadata().raw_bytes(), body.as_bytes());
        }

        #[test]
        fn test_try_parse_game_state_message_stores_timestamp() {
            let body = game_state_message_body();
            let entry = unity_entry(&body);
            let ts = Some(test_timestamp());
            let result = try_parse(&entry, ts);
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            assert_eq!(event.metadata().timestamp(), ts);
        }
    }

    // -- Zone extraction ------------------------------------------------------

    mod zone_extraction {
        use super::*;

        #[test]
        fn test_try_parse_game_state_message_zone_count() {
            let body = game_state_message_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(event);

            let zones = payload["zones"].as_array();
            assert!(zones.is_some());
            let zones = zones.unwrap_or_else(|| unreachable!());
            assert_eq!(zones.len(), 6);
        }

        #[test]
        fn test_try_parse_game_state_message_hand_zone() {
            let body = game_state_message_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(event);

            let zones = payload["zones"]
                .as_array()
                .unwrap_or_else(|| unreachable!());
            let hand = &zones[0];
            assert_eq!(hand["zone_id"], 30);
            assert_eq!(hand["zone_type"], "ZoneType_Hand");
            assert_eq!(hand["owner_seat_id"], 1);
            assert_eq!(hand["visibility"], "Visibility_Public");
            let obj_ids = hand["object_instance_ids"]
                .as_array()
                .unwrap_or_else(|| unreachable!());
            assert_eq!(obj_ids.len(), 3);
            assert_eq!(obj_ids[0], 101);
            assert_eq!(obj_ids[1], 102);
            assert_eq!(obj_ids[2], 103);
        }

        #[test]
        fn test_try_parse_game_state_message_library_zone() {
            let body = game_state_message_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(event);

            let zones = payload["zones"]
                .as_array()
                .unwrap_or_else(|| unreachable!());
            let library = &zones[1];
            assert_eq!(library["zone_type"], "ZoneType_Library");
            assert_eq!(library["visibility"], "Visibility_Hidden");
            let obj_ids = library["object_instance_ids"]
                .as_array()
                .unwrap_or_else(|| unreachable!());
            assert_eq!(obj_ids.len(), 5);
        }

        #[test]
        fn test_try_parse_game_state_message_battlefield_zone() {
            let body = game_state_message_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(event);

            let zones = payload["zones"]
                .as_array()
                .unwrap_or_else(|| unreachable!());
            let battlefield = &zones[2];
            assert_eq!(battlefield["zone_type"], "ZoneType_Battlefield");
            assert_eq!(battlefield["owner_seat_id"], 0);
            let obj_ids = battlefield["object_instance_ids"]
                .as_array()
                .unwrap_or_else(|| unreachable!());
            assert_eq!(obj_ids.len(), 2);
        }

        #[test]
        fn test_try_parse_game_state_message_graveyard_zone() {
            let body = game_state_message_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(event);

            let zones = payload["zones"]
                .as_array()
                .unwrap_or_else(|| unreachable!());
            let graveyard = &zones[3];
            assert_eq!(graveyard["zone_type"], "ZoneType_Graveyard");
            let obj_ids = graveyard["object_instance_ids"]
                .as_array()
                .unwrap_or_else(|| unreachable!());
            assert_eq!(obj_ids.len(), 1);
        }

        #[test]
        fn test_try_parse_game_state_message_exile_zone_empty() {
            let body = game_state_message_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(event);

            let zones = payload["zones"]
                .as_array()
                .unwrap_or_else(|| unreachable!());
            let exile = &zones[4];
            assert_eq!(exile["zone_type"], "ZoneType_Exile");
            let obj_ids = exile["object_instance_ids"]
                .as_array()
                .unwrap_or_else(|| unreachable!());
            assert!(obj_ids.is_empty());
        }

        #[test]
        fn test_try_parse_game_state_message_stack_zone() {
            let body = game_state_message_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(event);

            let zones = payload["zones"]
                .as_array()
                .unwrap_or_else(|| unreachable!());
            let stack = &zones[5];
            assert_eq!(stack["zone_type"], "ZoneType_Stack");
            let obj_ids = stack["object_instance_ids"]
                .as_array()
                .unwrap_or_else(|| unreachable!());
            assert_eq!(obj_ids.len(), 1);
            assert_eq!(obj_ids[0], 501);
        }

        #[test]
        fn test_try_parse_game_state_message_no_zones_returns_empty() {
            let body = empty_game_state_message_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(event);

            let zones = payload["zones"].as_array();
            assert!(zones.is_some());
            let zones = zones.unwrap_or_else(|| unreachable!());
            assert!(zones.is_empty());
        }

        #[test]
        fn test_try_parse_game_state_message_incremental_single_zone() {
            let body = minimal_game_state_message_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(event);

            let zones = payload["zones"]
                .as_array()
                .unwrap_or_else(|| unreachable!());
            assert_eq!(zones.len(), 1);
            assert_eq!(zones[0]["zone_type"], "ZoneType_Hand");
            let obj_ids = zones[0]["object_instance_ids"]
                .as_array()
                .unwrap_or_else(|| unreachable!());
            assert_eq!(obj_ids.len(), 2);
        }
    }

    // -- Game object extraction -----------------------------------------------

    mod game_object_extraction {
        use super::*;

        #[test]
        fn test_try_parse_game_state_message_object_count() {
            let body = game_state_message_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(event);

            let objects = payload["game_objects"].as_array();
            assert!(objects.is_some());
            let objects = objects.unwrap_or_else(|| unreachable!());
            assert_eq!(objects.len(), 3);
        }

        #[test]
        fn test_try_parse_game_state_message_creature_object() {
            let body = game_state_message_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(event);

            let objects = payload["game_objects"]
                .as_array()
                .unwrap_or_else(|| unreachable!());
            let creature = &objects[0];
            assert_eq!(creature["instance_id"], 101);
            assert_eq!(creature["grp_id"], 68398);
            assert_eq!(creature["object_type"], "GameObjectType_Card");
            assert_eq!(creature["zone_id"], 30);
            assert_eq!(creature["visibility"], "Visibility_Public");
            assert_eq!(creature["owner_seat_id"], 1);
            assert_eq!(creature["controller_seat_id"], 1);
        }

        #[test]
        fn test_try_parse_game_state_message_creature_card_types() {
            let body = game_state_message_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(event);

            let objects = payload["game_objects"]
                .as_array()
                .unwrap_or_else(|| unreachable!());
            let creature = &objects[0];
            let card_types = creature["card_types"]
                .as_array()
                .unwrap_or_else(|| unreachable!());
            assert_eq!(card_types.len(), 1);
            assert_eq!(card_types[0], "CardType_Creature");
        }

        #[test]
        fn test_try_parse_game_state_message_creature_subtypes() {
            let body = game_state_message_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(event);

            let objects = payload["game_objects"]
                .as_array()
                .unwrap_or_else(|| unreachable!());
            let creature = &objects[0];
            let subtypes = creature["subtypes"]
                .as_array()
                .unwrap_or_else(|| unreachable!());
            assert_eq!(subtypes.len(), 2);
            assert_eq!(subtypes[0], "SubType_Human");
            assert_eq!(subtypes[1], "SubType_Soldier");
        }

        #[test]
        fn test_try_parse_game_state_message_creature_abilities() {
            let body = game_state_message_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(event);

            let objects = payload["game_objects"]
                .as_array()
                .unwrap_or_else(|| unreachable!());
            let creature = &objects[0];
            let abilities = creature["abilities"]
                .as_array()
                .unwrap_or_else(|| unreachable!());
            assert_eq!(abilities.len(), 1);
            assert_eq!(abilities[0], "AbilityType_Lifelink");
        }

        #[test]
        fn test_try_parse_game_state_message_creature_power_toughness() {
            let body = game_state_message_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(event);

            let objects = payload["game_objects"]
                .as_array()
                .unwrap_or_else(|| unreachable!());
            let creature = &objects[0];
            assert_eq!(creature["power"], 3);
            assert_eq!(creature["toughness"], 2);
        }

        #[test]
        fn test_try_parse_game_state_message_land_object() {
            let body = game_state_message_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(event);

            let objects = payload["game_objects"]
                .as_array()
                .unwrap_or_else(|| unreachable!());
            let land = &objects[1];
            assert_eq!(land["instance_id"], 301);
            assert_eq!(land["grp_id"], 70136);
            assert_eq!(land["zone_id"], 32);
            let card_types = land["card_types"]
                .as_array()
                .unwrap_or_else(|| unreachable!());
            assert_eq!(card_types[0], "CardType_Land");
            // Land has no power/toughness.
            assert!(land["power"].is_null());
            assert!(land["toughness"].is_null());
        }

        #[test]
        fn test_try_parse_game_state_message_instant_on_stack() {
            let body = game_state_message_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(event);

            let objects = payload["game_objects"]
                .as_array()
                .unwrap_or_else(|| unreachable!());
            let instant = &objects[2];
            assert_eq!(instant["instance_id"], 501);
            assert_eq!(instant["zone_id"], 35);
            let card_types = instant["card_types"]
                .as_array()
                .unwrap_or_else(|| unreachable!());
            assert_eq!(card_types[0], "CardType_Instant");
        }

        #[test]
        fn test_try_parse_game_state_message_no_objects_returns_empty() {
            let body = empty_game_state_message_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(event);

            let objects = payload["game_objects"].as_array();
            assert!(objects.is_some());
            let objects = objects.unwrap_or_else(|| unreachable!());
            assert!(objects.is_empty());
        }

        #[test]
        fn test_try_parse_game_state_message_object_name_integer() {
            let body = game_state_message_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(event);

            let objects = payload["game_objects"]
                .as_array()
                .unwrap_or_else(|| unreachable!());
            // name is the grpId integer in this fixture.
            assert_eq!(objects[0]["name"], 68398);
        }

        #[test]
        fn test_try_parse_game_state_message_minimal_object() {
            let body = flat_game_state_message_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(event);

            let objects = payload["game_objects"]
                .as_array()
                .unwrap_or_else(|| unreachable!());
            assert_eq!(objects.len(), 1);
            let obj = &objects[0];
            assert_eq!(obj["instance_id"], 601);
            assert_eq!(obj["grp_id"], 80000);
            // Missing fields should have defaults.
            assert_eq!(obj["visibility"], "");
            assert!(obj["power"].is_null());
            assert!(obj["toughness"].is_null());
        }
    }

    // -- Game info extraction -------------------------------------------------

    mod game_info_extraction {
        use super::*;

        #[test]
        fn test_try_parse_game_state_message_game_info_present() {
            let body = game_state_message_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(event);

            assert!(payload["game_info"].is_object());
        }

        #[test]
        fn test_try_parse_game_state_message_game_info_match_id() {
            let body = game_state_message_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(event);

            assert_eq!(payload["game_info"]["matchID"], "match-id-12345");
        }

        #[test]
        fn test_try_parse_game_state_message_game_info_stage() {
            let body = game_state_message_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(event);

            assert_eq!(payload["game_info"]["stage"], "GameStage_Play");
        }

        #[test]
        fn test_try_parse_game_state_message_game_info_mulligan_type() {
            let body = game_state_message_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(event);

            assert_eq!(payload["game_info"]["mulliganType"], "MulliganType_London");
        }

        #[test]
        fn test_try_parse_game_state_message_missing_game_info_returns_null() {
            let body = empty_game_state_message_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(event);

            assert!(payload["game_info"].is_null());
        }
    }

    // -- QueuedGameStateMessage -----------------------------------------------

    mod queued_game_state {
        use super::*;

        #[test]
        fn test_try_parse_queued_game_state_message_detected() {
            let body = queued_game_state_message_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
        }

        #[test]
        fn test_try_parse_queued_game_state_message_type_field() {
            let body = queued_game_state_message_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(event);
            assert_eq!(payload["type"], "queued_game_state_message");
        }

        #[test]
        fn test_try_parse_queued_game_state_message_msg_id() {
            let body = queued_game_state_message_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(event);
            assert_eq!(payload["msg_id"], 7);
        }

        #[test]
        fn test_try_parse_queued_game_state_message_zones() {
            let body = queued_game_state_message_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(event);

            let zones = payload["zones"]
                .as_array()
                .unwrap_or_else(|| unreachable!());
            assert_eq!(zones.len(), 1);
            assert_eq!(zones[0]["zone_type"], "ZoneType_Battlefield");
            let obj_ids = zones[0]["object_instance_ids"]
                .as_array()
                .unwrap_or_else(|| unreachable!());
            assert_eq!(obj_ids.len(), 3);
        }

        #[test]
        fn test_try_parse_queued_game_state_message_objects() {
            let body = queued_game_state_message_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(event);

            let objects = payload["game_objects"]
                .as_array()
                .unwrap_or_else(|| unreachable!());
            assert_eq!(objects.len(), 1);
            assert_eq!(objects[0]["instance_id"], 303);
            assert_eq!(objects[0]["grp_id"], 72000);
            assert_eq!(objects[0]["owner_seat_id"], 2);
            assert_eq!(objects[0]["power"], 5);
            assert_eq!(objects[0]["toughness"], 5);
        }

        #[test]
        fn test_try_parse_queued_game_state_message_preserves_raw_bytes() {
            let body = queued_game_state_message_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            assert_eq!(event.metadata().raw_bytes(), body.as_bytes());
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
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            assert_eq!(
                event.performance_class(),
                PerformanceClass::InteractiveDispatch
            );
        }

        #[test]
        fn test_try_parse_game_state_message_performance_class_interactive_dispatch() {
            let body = game_state_message_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            assert_eq!(
                event.performance_class(),
                PerformanceClass::InteractiveDispatch
            );
        }

        #[test]
        fn test_try_parse_queued_game_state_message_performance_class_interactive_dispatch() {
            let body = queued_game_state_message_body();
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
        fn test_extract_string_array_normal() {
            let value = serde_json::json!(["CardType_Creature", "CardType_Artifact"]);
            let strings = extract_string_array(Some(&value));
            assert_eq!(strings, vec!["CardType_Creature", "CardType_Artifact"]);
        }

        #[test]
        fn test_extract_string_array_empty() {
            let value = serde_json::json!([]);
            let strings = extract_string_array(Some(&value));
            assert!(strings.is_empty());
        }

        #[test]
        fn test_extract_string_array_none() {
            let strings = extract_string_array(None);
            assert!(strings.is_empty());
        }

        #[test]
        fn test_extract_string_array_mixed_types_skips_non_strings() {
            let value = serde_json::json!(["valid", 42, "also_valid", null]);
            let strings = extract_string_array(Some(&value));
            assert_eq!(strings, vec!["valid", "also_valid"]);
        }

        #[test]
        fn test_extract_nested_value_present() {
            let value = serde_json::json!({"value": 3});
            let result = extract_nested_value(Some(&value));
            assert_eq!(result, serde_json::json!(3));
        }

        #[test]
        fn test_extract_nested_value_missing_value_key() {
            let value = serde_json::json!({"other": 5});
            let result = extract_nested_value(Some(&value));
            assert!(result.is_null());
        }

        #[test]
        fn test_extract_nested_value_none() {
            let result = extract_nested_value(None);
            assert!(result.is_null());
        }

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

        #[test]
        fn test_extract_single_zone_valid() {
            let zone = serde_json::json!({
                "zoneId": 30,
                "type": "ZoneType_Hand",
                "ownerSeatId": 1,
                "visibility": "Visibility_Public",
                "objectInstanceIds": [101, 102]
            });
            let result = extract_single_zone(&zone);
            assert!(result.is_some());
            let z = result.unwrap_or_else(|| unreachable!());
            assert_eq!(z["zone_id"], 30);
            assert_eq!(z["zone_type"], "ZoneType_Hand");
        }

        #[test]
        fn test_extract_single_zone_missing_zone_id_returns_none() {
            let zone = serde_json::json!({
                "type": "ZoneType_Hand",
                "ownerSeatId": 1
            });
            assert!(extract_single_zone(&zone).is_none());
        }

        #[test]
        fn test_extract_single_game_object_valid() {
            let obj = serde_json::json!({
                "instanceId": 101,
                "grpId": 68398,
                "type": "GameObjectType_Card",
                "zoneId": 30,
                "ownerSeatId": 1,
                "controllerSeatId": 1
            });
            let result = extract_single_game_object(&obj);
            assert!(result.is_some());
            let o = result.unwrap_or_else(|| unreachable!());
            assert_eq!(o["instance_id"], 101);
            assert_eq!(o["grp_id"], 68398);
        }

        #[test]
        fn test_extract_single_game_object_missing_instance_id_returns_none() {
            let obj = serde_json::json!({
                "grpId": 68398,
                "type": "GameObjectType_Card"
            });
            assert!(extract_single_game_object(&obj).is_none());
        }
    }

    // -- GameStateMessage edge cases ------------------------------------------

    mod game_state_edge_cases {
        use super::*;

        #[test]
        fn test_try_parse_game_state_message_zone_missing_object_ids() {
            let body = format!(
                "[UnityCrossThreadLogger]greToClientEvent\n{}",
                serde_json::json!({
                    "greToClientEvent": {
                        "greToClientMessages": [
                            {
                                "type": "GREMessageType_GameStateMessage",
                                "msgId": 1,
                                "gameStateMessage": {
                                    "zones": [
                                        {
                                            "zoneId": 30,
                                            "type": "ZoneType_Hand",
                                            "ownerSeatId": 1
                                        }
                                    ]
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

            let zones = payload["zones"]
                .as_array()
                .unwrap_or_else(|| unreachable!());
            assert_eq!(zones.len(), 1);
            let obj_ids = zones[0]["object_instance_ids"]
                .as_array()
                .unwrap_or_else(|| unreachable!());
            assert!(obj_ids.is_empty());
        }

        #[test]
        fn test_try_parse_game_state_message_object_missing_optional_fields() {
            let body = format!(
                "[UnityCrossThreadLogger]greToClientEvent\n{}",
                serde_json::json!({
                    "greToClientEvent": {
                        "greToClientMessages": [
                            {
                                "type": "GREMessageType_GameStateMessage",
                                "msgId": 1,
                                "gameStateMessage": {
                                    "gameObjects": [
                                        {
                                            "instanceId": 999,
                                            "grpId": 12345
                                        }
                                    ]
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

            let objects = payload["game_objects"]
                .as_array()
                .unwrap_or_else(|| unreachable!());
            assert_eq!(objects.len(), 1);
            let obj = &objects[0];
            assert_eq!(obj["instance_id"], 999);
            assert_eq!(obj["grp_id"], 12345);
            assert_eq!(obj["object_type"], "Unknown");
            assert_eq!(obj["zone_id"], 0);
            assert_eq!(obj["visibility"], "");
            assert_eq!(obj["owner_seat_id"], 0);
            assert_eq!(obj["controller_seat_id"], 0);
            assert!(obj["card_types"]
                .as_array()
                .unwrap_or_else(|| unreachable!())
                .is_empty());
            assert!(obj["subtypes"]
                .as_array()
                .unwrap_or_else(|| unreachable!())
                .is_empty());
            assert!(obj["power"].is_null());
            assert!(obj["toughness"].is_null());
        }

        #[test]
        fn test_try_parse_game_state_message_missing_game_state_message_key() {
            let body = format!(
                "[UnityCrossThreadLogger]greToClientEvent\n{}",
                serde_json::json!({
                    "greToClientEvent": {
                        "greToClientMessages": [
                            {
                                "type": "GREMessageType_GameStateMessage",
                                "msgId": 1
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

            // Should parse with empty zones and objects.
            let zones = payload["zones"]
                .as_array()
                .unwrap_or_else(|| unreachable!());
            assert!(zones.is_empty());
            let objects = payload["game_objects"]
                .as_array()
                .unwrap_or_else(|| unreachable!());
            assert!(objects.is_empty());
            assert!(payload["game_info"].is_null());
        }

        #[test]
        fn test_try_parse_game_state_message_with_timestamp_in_header() {
            let body = format!(
                "[UnityCrossThreadLogger]2/25/2026 12:00:00 PM greToClientEvent\n{}",
                serde_json::json!({
                    "greToClientEvent": {
                        "greToClientMessages": [
                            {
                                "type": "GREMessageType_GameStateMessage",
                                "msgId": 1,
                                "gameStateMessage": {
                                    "zones": [],
                                    "gameObjects": []
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
            assert_eq!(payload["type"], "game_state_message");
        }

        #[test]
        fn test_try_parse_game_state_message_zone_invalid_zone_skipped() {
            let body = format!(
                "[UnityCrossThreadLogger]greToClientEvent\n{}",
                serde_json::json!({
                    "greToClientEvent": {
                        "greToClientMessages": [
                            {
                                "type": "GREMessageType_GameStateMessage",
                                "msgId": 1,
                                "gameStateMessage": {
                                    "zones": [
                                        {
                                            "zoneId": 30,
                                            "type": "ZoneType_Hand",
                                            "ownerSeatId": 1,
                                            "objectInstanceIds": [101]
                                        },
                                        {
                                            "type": "ZoneType_Invalid"
                                        },
                                        {
                                            "zoneId": 32,
                                            "type": "ZoneType_Battlefield",
                                            "ownerSeatId": 0,
                                            "objectInstanceIds": [301]
                                        }
                                    ]
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

            // Zone without zoneId should be skipped.
            let zones = payload["zones"]
                .as_array()
                .unwrap_or_else(|| unreachable!());
            assert_eq!(zones.len(), 2);
            assert_eq!(zones[0]["zone_id"], 30);
            assert_eq!(zones[1]["zone_id"], 32);
        }

        #[test]
        fn test_try_parse_game_state_message_object_invalid_object_skipped() {
            let body = format!(
                "[UnityCrossThreadLogger]greToClientEvent\n{}",
                serde_json::json!({
                    "greToClientEvent": {
                        "greToClientMessages": [
                            {
                                "type": "GREMessageType_GameStateMessage",
                                "msgId": 1,
                                "gameStateMessage": {
                                    "gameObjects": [
                                        {
                                            "instanceId": 101,
                                            "grpId": 68398,
                                            "type": "GameObjectType_Card"
                                        },
                                        {
                                            "grpId": 99999
                                        },
                                        {
                                            "instanceId": 102,
                                            "grpId": 70136,
                                            "type": "GameObjectType_Card"
                                        }
                                    ]
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

            // Object without instanceId should be skipped.
            let objects = payload["game_objects"]
                .as_array()
                .unwrap_or_else(|| unreachable!());
            assert_eq!(objects.len(), 2);
            assert_eq!(objects[0]["instance_id"], 101);
            assert_eq!(objects[1]["instance_id"], 102);
        }
    }

    // -----------------------------------------------------------------------
    // GameStage_GameOver → GameResult detection
    // -----------------------------------------------------------------------

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
        fn test_try_parse_ui_message_returns_some() {
            let entry = gre_entry_with_type("GREMessageType_UIMessage");
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            assert!(matches!(event, GameEvent::GameState(_)));
            let payload = game_state_payload(event);
            assert_eq!(payload["recognized_type"], "GREMessageType_UIMessage");
        }

        #[test]
        fn test_try_parse_timer_state_message_returns_some() {
            let entry = gre_entry_with_type("GREMessageType_TimerStateMessage");
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            assert!(matches!(event, GameEvent::GameState(_)));
            let payload = game_state_payload(event);
            assert_eq!(
                payload["recognized_type"],
                "GREMessageType_TimerStateMessage"
            );
        }

        #[test]
        fn test_try_parse_set_settings_resp_returns_some() {
            let entry = gre_entry_with_type("GREMessageType_SetSettingsResp");
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            assert!(matches!(event, GameEvent::GameState(_)));
            let payload = game_state_payload(event);
            assert_eq!(payload["recognized_type"], "GREMessageType_SetSettingsResp");
        }

        #[test]
        fn test_noise_types_preserve_metadata_raw_bytes() {
            let entry = gre_entry_with_type("GREMessageType_UIMessage");
            let result = try_parse(&entry, Some(test_timestamp()));
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            assert!(!event.metadata().raw_bytes().is_empty());
        }

        #[test]
        fn test_noise_types_prioritize_real_events() {
            // If a message array has both a GameStateMessage and a UIMessage,
            // the GameStateMessage should be returned (it comes first in dispatch).
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
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            // Should be GameState from GameStateMessage, not from UIMessage noise
            let payload = game_state_payload(event);
            assert!(payload.get("recognized_type").is_none());
        }

        #[test]
        fn test_truly_unknown_type_still_returns_none() {
            let entry = gre_entry_with_type("GREMessageType_SomeFutureType");
            assert!(try_parse(&entry, Some(test_timestamp())).is_none());
        }
    }

    mod game_result_detection {
        use super::*;
        use crate::events::PerformanceClass;

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

        #[test]
        fn test_try_parse_game_state_message_game_over_emits_game_result() {
            let entry = unity_entry(&game_over_body());
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            assert!(matches!(event, GameEvent::GameResult(_)));
        }

        #[test]
        fn test_try_parse_game_over_performance_class_post_game_batch() {
            let entry = unity_entry(&game_over_body());
            let event = try_parse(&entry, Some(test_timestamp())).unwrap_or_else(|| unreachable!());
            assert_eq!(event.performance_class(), PerformanceClass::PostGameBatch);
        }

        #[test]
        fn test_try_parse_game_over_payload_type_and_source() {
            let entry = unity_entry(&game_over_body());
            let event = try_parse(&entry, Some(test_timestamp())).unwrap_or_else(|| unreachable!());
            let payload = game_result_payload(&event);
            assert_eq!(payload["type"], "game_result");
            assert_eq!(payload["source"], "gre_game_state");
        }

        #[test]
        fn test_try_parse_game_over_extracts_stage_and_match_state() {
            let entry = unity_entry(&game_over_body());
            let event = try_parse(&entry, Some(test_timestamp())).unwrap_or_else(|| unreachable!());
            let payload = game_result_payload(&event);
            assert_eq!(payload["stage"], "GameStage_GameOver");
            assert_eq!(payload["match_state"], "MatchState_GameComplete");
        }

        #[test]
        fn test_try_parse_game_over_extracts_winning_team_id() {
            let entry = unity_entry(&game_over_body());
            let event = try_parse(&entry, Some(test_timestamp())).unwrap_or_else(|| unreachable!());
            let payload = game_result_payload(&event);
            assert_eq!(payload["winning_team_id"], 1);
        }

        #[test]
        fn test_try_parse_game_over_extracts_result_type() {
            let entry = unity_entry(&game_over_body());
            let event = try_parse(&entry, Some(test_timestamp())).unwrap_or_else(|| unreachable!());
            let payload = game_result_payload(&event);
            assert_eq!(payload["result_type"], "ResultType_WinLoss");
        }

        #[test]
        fn test_try_parse_game_over_extracts_reason() {
            let entry = unity_entry(&game_over_body());
            let event = try_parse(&entry, Some(test_timestamp())).unwrap_or_else(|| unreachable!());
            let payload = game_result_payload(&event);
            assert_eq!(payload["reason"], "ResultReason_Game");
        }

        #[test]
        fn test_try_parse_game_over_preserves_results_array() {
            let entry = unity_entry(&game_over_body());
            let event = try_parse(&entry, Some(test_timestamp())).unwrap_or_else(|| unreachable!());
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
            let event = try_parse(&entry, Some(test_timestamp())).unwrap_or_else(|| unreachable!());
            let payload = game_result_payload(&event);
            let gi = &payload["game_info"];
            assert_eq!(gi["matchID"], "match-abc-123");
            assert_eq!(gi["gameNumber"], 1);
            assert_eq!(gi["mulliganType"], "MulliganType_London");
        }

        #[test]
        fn test_try_parse_game_over_preserves_timestamp() {
            let entry = unity_entry(&game_over_body());
            let event = try_parse(&entry, Some(test_timestamp())).unwrap_or_else(|| unreachable!());
            assert_eq!(event.metadata().timestamp(), Some(test_timestamp()));
        }

        #[test]
        fn test_try_parse_game_over_preserves_raw_bytes() {
            let body = game_over_body();
            let entry = unity_entry(&body);
            let event = try_parse(&entry, Some(test_timestamp())).unwrap_or_else(|| unreachable!());
            assert_eq!(event.metadata().raw_bytes(), body.as_bytes());
        }

        #[test]
        fn test_try_parse_queued_game_state_message_game_over_emits_game_result() {
            let entry = unity_entry(&queued_game_over_body());
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
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
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
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
            let event = try_parse(&entry, Some(test_timestamp())).unwrap_or_else(|| unreachable!());
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
            let event = try_parse(&entry, Some(test_timestamp())).unwrap_or_else(|| unreachable!());
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
            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            assert!(matches!(event, GameEvent::GameState(_)));
        }
    }
}
