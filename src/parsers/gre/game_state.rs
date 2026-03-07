//! `GameStateMessage` and `QueuedGameStateMessage` payload builder.

use super::annotations::extract_annotations;
use super::helpers::{extract_nested_value, extract_string_array};
use super::turn_info::extract_turn_info;

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
///   "game_info": { ... },
///   "turn_info": { "turn_number": 3, "phase": "Phase_Main1", ... },
///   "annotations": [ { "id": 145, "affector_id": 296, ... }, ... ],
///   "timers": [ { "timer_id": 9, "type": "TimerType_ActivePlayer", ... }, ... ],
///   "diff_deleted_instance_ids": [279, 282, 284]
/// }
/// ```
///
/// Incremental updates may include only a subset of zones and objects.
/// Missing fields default to empty arrays / null. `turn_info` is `null`
/// when `gameInfo.turnInfo` is absent. `annotations`, `timers`, and
/// `diff_deleted_instance_ids` are empty arrays when their respective
/// source arrays are absent.
pub(super) fn build_game_state_message_payload(gre_msg: &serde_json::Value) -> serde_json::Value {
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
    let payload_type = if gre_type == super::QUEUED_GAME_STATE_MESSAGE_TYPE {
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

    // Extract structured turn info from gameInfo.turnInfo.
    let turn_info = extract_turn_info(gsm);

    // Extract annotations from gameStateMessage.annotations[].
    let annotations = extract_annotations(gsm);

    // Extract inline timers from gameStateMessage.timers[].
    let timers = extract_timers(gsm);

    // Extract diffDeletedInstanceIds (instance IDs removed from game state).
    let diff_deleted_instance_ids = gsm
        .and_then(|g| g.get("diffDeletedInstanceIds"))
        .and_then(serde_json::Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(serde_json::Value::as_i64)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    serde_json::json!({
        "type": payload_type,
        "msg_id": msg_id,
        "game_state_id": game_state_id,
        "zones": zones,
        "game_objects": game_objects,
        "game_info": game_info,
        "turn_info": turn_info,
        "annotations": annotations,
        "timers": timers,
        "diff_deleted_instance_ids": diff_deleted_instance_ids,
    })
}

/// Extracts timer data from the `gameStateMessage.timers` array.
///
/// Each timer has fields like `timerId`, `type`, `durationSec`, `elapsedSec`,
/// `elapsedMs`, `running`, `behavior`, and `warningThresholdSec`. Field names
/// are normalized to `snake_case`.
///
/// Returns an empty `Vec` when `timers` is absent or empty.
fn extract_timers(gsm: Option<&serde_json::Value>) -> Vec<serde_json::Value> {
    let Some(raw_timers) = gsm
        .and_then(|g| g.get("timers"))
        .and_then(serde_json::Value::as_array)
    else {
        return Vec::new();
    };

    raw_timers.iter().filter_map(extract_single_timer).collect()
}

/// Extracts a single timer, normalizing field names to `snake_case`.
fn extract_single_timer(timer: &serde_json::Value) -> Option<serde_json::Value> {
    let timer_id = timer.get("timerId").and_then(serde_json::Value::as_i64)?;

    let timer_type = timer
        .get("type")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");

    let duration_sec = timer
        .get("durationSec")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0);

    let behavior = timer
        .get("behavior")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");

    let mut result = serde_json::json!({
        "timer_id": timer_id,
        "type": timer_type,
        "duration_sec": duration_sec,
        "behavior": behavior,
    });

    if let Some(obj) = result.as_object_mut() {
        if let Some(elapsed_sec) = timer.get("elapsedSec").and_then(serde_json::Value::as_i64) {
            obj.insert("elapsed_sec".to_string(), serde_json::json!(elapsed_sec));
        }
        if let Some(elapsed_ms) = timer.get("elapsedMs").and_then(serde_json::Value::as_i64) {
            obj.insert("elapsed_ms".to_string(), serde_json::json!(elapsed_ms));
        }
        if let Some(running) = timer.get("running").and_then(serde_json::Value::as_bool) {
            obj.insert("running".to_string(), serde_json::json!(running));
        }
        if let Some(warning) = timer
            .get("warningThresholdSec")
            .and_then(serde_json::Value::as_i64)
        {
            obj.insert(
                "warning_threshold_sec".to_string(),
                serde_json::json!(warning),
            );
        }
    }

    Some(result)
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
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::super::test_fixtures::*;
    use super::super::try_parse;
    use super::*;
    use crate::parsers::test_helpers::{game_state_payload, test_timestamp, unity_entry};

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

    // -- GameStateMessage detection -------------------------------------------

    mod game_state_detection {
        use super::*;
        use crate::events::GameEvent;

        #[test]
        fn test_try_parse_game_state_message_detected() {
            let body = game_state_message_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(!result.is_empty());
        }

        #[test]
        fn test_try_parse_game_state_message_correct_variant() {
            let body = game_state_message_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(!result.is_empty());
            let event = &result[0];
            assert!(matches!(event, GameEvent::GameState(_)));
        }

        #[test]
        fn test_try_parse_game_state_message_type_field() {
            let body = game_state_message_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(!result.is_empty());
            let event = &result[0];
            let payload = game_state_payload(event);
            assert_eq!(payload["type"], "game_state_message");
        }

        #[test]
        fn test_try_parse_game_state_message_msg_id() {
            let body = game_state_message_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(!result.is_empty());
            let event = &result[0];
            let payload = game_state_payload(event);
            assert_eq!(payload["msg_id"], 5);
        }

        #[test]
        fn test_try_parse_game_state_message_game_state_id() {
            let body = game_state_message_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(!result.is_empty());
            let event = &result[0];
            let payload = game_state_payload(event);
            assert_eq!(payload["game_state_id"], 42);
        }

        #[test]
        fn test_try_parse_game_state_message_preserves_raw_bytes() {
            let body = game_state_message_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(!result.is_empty());
            let event = &result[0];
            assert_eq!(event.metadata().raw_bytes(), body.as_bytes());
        }

        #[test]
        fn test_try_parse_game_state_message_stores_timestamp() {
            let body = game_state_message_body();
            let entry = unity_entry(&body);
            let ts = Some(test_timestamp());
            let result = try_parse(&entry, ts);
            assert!(!result.is_empty());
            let event = &result[0];
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
            assert!(!result.is_empty());
            let event = &result[0];
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
            assert!(!result.is_empty());
            let event = &result[0];
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
            assert!(!result.is_empty());
            let event = &result[0];
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
            assert!(!result.is_empty());
            let event = &result[0];
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
            assert!(!result.is_empty());
            let event = &result[0];
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
            assert!(!result.is_empty());
            let event = &result[0];
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
            assert!(!result.is_empty());
            let event = &result[0];
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
            assert!(!result.is_empty());
            let event = &result[0];
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
            assert!(!result.is_empty());
            let event = &result[0];
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
            assert!(!result.is_empty());
            let event = &result[0];
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
            assert!(!result.is_empty());
            let event = &result[0];
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
            assert!(!result.is_empty());
            let event = &result[0];
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
            assert!(!result.is_empty());
            let event = &result[0];
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
            assert!(!result.is_empty());
            let event = &result[0];
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
            assert!(!result.is_empty());
            let event = &result[0];
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
            assert!(!result.is_empty());
            let event = &result[0];
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
            assert!(!result.is_empty());
            let event = &result[0];
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
            assert!(!result.is_empty());
            let event = &result[0];
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
            assert!(!result.is_empty());
            let event = &result[0];
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
            assert!(!result.is_empty());
            let event = &result[0];
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
            assert!(!result.is_empty());
            let event = &result[0];
            let payload = game_state_payload(event);

            assert!(payload["game_info"].is_object());
        }

        #[test]
        fn test_try_parse_game_state_message_game_info_match_id() {
            let body = game_state_message_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(!result.is_empty());
            let event = &result[0];
            let payload = game_state_payload(event);

            assert_eq!(payload["game_info"]["matchID"], "match-id-12345");
        }

        #[test]
        fn test_try_parse_game_state_message_game_info_stage() {
            let body = game_state_message_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(!result.is_empty());
            let event = &result[0];
            let payload = game_state_payload(event);

            assert_eq!(payload["game_info"]["stage"], "GameStage_Play");
        }

        #[test]
        fn test_try_parse_game_state_message_game_info_mulligan_type() {
            let body = game_state_message_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(!result.is_empty());
            let event = &result[0];
            let payload = game_state_payload(event);

            assert_eq!(payload["game_info"]["mulliganType"], "MulliganType_London");
        }

        #[test]
        fn test_try_parse_game_state_message_missing_game_info_returns_null() {
            let body = empty_game_state_message_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(!result.is_empty());
            let event = &result[0];
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
            assert!(!result.is_empty());
        }

        #[test]
        fn test_try_parse_queued_game_state_message_type_field() {
            let body = queued_game_state_message_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(!result.is_empty());
            let event = &result[0];
            let payload = game_state_payload(event);
            assert_eq!(payload["type"], "queued_game_state_message");
        }

        #[test]
        fn test_try_parse_queued_game_state_message_msg_id() {
            let body = queued_game_state_message_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(!result.is_empty());
            let event = &result[0];
            let payload = game_state_payload(event);
            assert_eq!(payload["msg_id"], 7);
        }

        #[test]
        fn test_try_parse_queued_game_state_message_zones() {
            let body = queued_game_state_message_body();
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            assert!(!result.is_empty());
            let event = &result[0];
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
            assert!(!result.is_empty());
            let event = &result[0];
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
            assert!(!result.is_empty());
            let event = &result[0];
            assert_eq!(event.metadata().raw_bytes(), body.as_bytes());
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
            assert!(!result.is_empty());
            let event = &result[0];
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
            assert!(!result.is_empty());
            let event = &result[0];
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
            assert!(!result.is_empty());
            let event = &result[0];
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
            assert!(!result.is_empty());
            let event = &result[0];
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
            assert!(!result.is_empty());
            let event = &result[0];
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
            assert!(!result.is_empty());
            let event = &result[0];
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

    // -- Internal helpers ----------------------------------------------------

    mod internal_helpers {
        use super::*;

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

    // -- Timer extraction ----------------------------------------------------

    mod timer_extraction {
        use super::*;

        fn game_state_with_timers_body() -> String {
            format!(
                "[UnityCrossThreadLogger]greToClientEvent\n{}",
                serde_json::json!({
                    "greToClientEvent": {
                        "greToClientMessages": [{
                            "type": "GREMessageType_GameStateMessage",
                            "msgId": 30,
                            "gameStateId": 95,
                            "gameStateMessage": {
                                "timers": [
                                    {
                                        "timerId": 9,
                                        "type": "TimerType_ActivePlayer",
                                        "durationSec": 116,
                                        "elapsedSec": 16,
                                        "running": true,
                                        "behavior": "TimerBehavior_TakeControl",
                                        "warningThresholdSec": 30,
                                        "elapsedMs": 16889
                                    },
                                    {
                                        "timerId": 12,
                                        "type": "TimerType_Inactivity",
                                        "durationSec": 150,
                                        "behavior": "TimerBehavior_Timeout",
                                        "warningThresholdSec": 30
                                    }
                                ]
                            }
                        }]
                    }
                })
            )
        }

        #[test]
        fn test_timers_present_is_array() {
            let body = game_state_with_timers_body();
            let entry = unity_entry(&body);
            let event = try_parse(&entry, Some(test_timestamp()))
                .into_iter()
                .next()
                .unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(&event);

            assert!(payload["timers"].is_array());
        }

        #[test]
        fn test_timers_count() {
            let body = game_state_with_timers_body();
            let entry = unity_entry(&body);
            let event = try_parse(&entry, Some(test_timestamp()))
                .into_iter()
                .next()
                .unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(&event);

            let timers = payload["timers"]
                .as_array()
                .unwrap_or_else(|| unreachable!());
            assert_eq!(timers.len(), 2);
        }

        #[test]
        fn test_timer_base_fields() {
            let body = game_state_with_timers_body();
            let entry = unity_entry(&body);
            let event = try_parse(&entry, Some(test_timestamp()))
                .into_iter()
                .next()
                .unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(&event);

            let timer = &payload["timers"][0];
            assert_eq!(timer["timer_id"], 9);
            assert_eq!(timer["type"], "TimerType_ActivePlayer");
            assert_eq!(timer["duration_sec"], 116);
            assert_eq!(timer["behavior"], "TimerBehavior_TakeControl");
        }

        #[test]
        fn test_timer_optional_fields_present() {
            let body = game_state_with_timers_body();
            let entry = unity_entry(&body);
            let event = try_parse(&entry, Some(test_timestamp()))
                .into_iter()
                .next()
                .unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(&event);

            let timer = &payload["timers"][0];
            assert_eq!(timer["elapsed_sec"], 16);
            assert_eq!(timer["elapsed_ms"], 16889);
            assert_eq!(timer["running"], true);
            assert_eq!(timer["warning_threshold_sec"], 30);
        }

        #[test]
        fn test_timer_optional_fields_absent() {
            let body = game_state_with_timers_body();
            let entry = unity_entry(&body);
            let event = try_parse(&entry, Some(test_timestamp()))
                .into_iter()
                .next()
                .unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(&event);

            let timer = &payload["timers"][1];
            assert_eq!(timer["timer_id"], 12);
            assert!(timer.get("elapsed_sec").is_none());
            assert!(timer.get("elapsed_ms").is_none());
            assert!(timer.get("running").is_none());
        }

        #[test]
        fn test_missing_timers_returns_empty_array() {
            let body = game_state_message_body();
            let entry = unity_entry(&body);
            let event = try_parse(&entry, Some(test_timestamp()))
                .into_iter()
                .next()
                .unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(&event);

            let timers = payload["timers"]
                .as_array()
                .unwrap_or_else(|| unreachable!());
            assert!(timers.is_empty());
        }
    }

    mod diff_deleted_instance_ids_extraction {
        use super::*;

        /// Helper: build a `GameStateMessage` body with `diffDeletedInstanceIds`.
        fn game_state_with_diff_deleted_body() -> String {
            format!(
                "[UnityCrossThreadLogger]greToClientEvent\n{}",
                serde_json::json!({
                    "greToClientEvent": {
                        "greToClientMessages": [{
                            "type": "GREMessageType_GameStateMessage",
                            "msgId": 30,
                            "gameStateId": 100,
                            "gameStateMessage": {
                                "gameObjects": [],
                                "diffDeletedInstanceIds": [279, 282, 284]
                            }
                        }]
                    }
                })
            )
        }

        #[test]
        fn test_diff_deleted_instance_ids_present() {
            let body = game_state_with_diff_deleted_body();
            let entry = unity_entry(&body);
            let event = try_parse(&entry, Some(test_timestamp()))
                .into_iter()
                .next()
                .unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(&event);

            let ids = payload["diff_deleted_instance_ids"]
                .as_array()
                .unwrap_or_else(|| unreachable!());
            assert_eq!(ids.len(), 3);
            assert_eq!(ids[0], 279);
            assert_eq!(ids[1], 282);
            assert_eq!(ids[2], 284);
        }

        #[test]
        fn test_diff_deleted_instance_ids_empty_array() {
            let body = format!(
                "[UnityCrossThreadLogger]greToClientEvent\n{}",
                serde_json::json!({
                    "greToClientEvent": {
                        "greToClientMessages": [{
                            "type": "GREMessageType_GameStateMessage",
                            "msgId": 31,
                            "gameStateId": 101,
                            "gameStateMessage": {
                                "diffDeletedInstanceIds": []
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
            let payload = game_state_payload(&event);

            let ids = payload["diff_deleted_instance_ids"]
                .as_array()
                .unwrap_or_else(|| unreachable!());
            assert!(ids.is_empty());
        }

        #[test]
        fn test_diff_deleted_instance_ids_absent_returns_empty() {
            let body = game_state_message_body();
            let entry = unity_entry(&body);
            let event = try_parse(&entry, Some(test_timestamp()))
                .into_iter()
                .next()
                .unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(&event);

            let ids = payload["diff_deleted_instance_ids"]
                .as_array()
                .unwrap_or_else(|| unreachable!());
            assert!(ids.is_empty());
        }

        #[test]
        fn test_diff_deleted_single_id() {
            let body = format!(
                "[UnityCrossThreadLogger]greToClientEvent\n{}",
                serde_json::json!({
                    "greToClientEvent": {
                        "greToClientMessages": [{
                            "type": "GREMessageType_GameStateMessage",
                            "msgId": 32,
                            "gameStateId": 102,
                            "gameStateMessage": {
                                "diffDeletedInstanceIds": [500]
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
            let payload = game_state_payload(&event);

            let ids = payload["diff_deleted_instance_ids"]
                .as_array()
                .unwrap_or_else(|| unreachable!());
            assert_eq!(ids.len(), 1);
            assert_eq!(ids[0], 500);
        }
    }
}
