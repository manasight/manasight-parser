//! Annotations extraction from `gameStateMessage.annotations`.
//!
//! Arena logs use a uniform `details` array of key-value pairs for
//! type-specific annotation data, e.g.:
//! ```json
//! {"key": "zone_src", "type": "KeyValuePairValueType_int32", "valueInt32": [31]}
//! ```
//!
//! The `type` field is an array of strings (e.g. `["AnnotationType_ZoneTransfer"]`).

/// Annotation type for zone transfers (draw, play, exile, etc.).
const ANNOTATION_TYPE_ZONE_TRANSFER: &str = "AnnotationType_ZoneTransfer";

/// Annotation type for object ID changes (new instance IDs after zone moves).
const ANNOTATION_TYPE_OBJECT_ID_CHANGED: &str = "AnnotationType_ObjectIdChanged";

/// Annotation type for damage dealt to a player or permanent.
const ANNOTATION_TYPE_DAMAGE_DEALT: &str = "AnnotationType_DamageDealt";

/// Annotation type for counters added to a permanent (e.g. +1/+1).
const ANNOTATION_TYPE_COUNTER_ADDED: &str = "AnnotationType_CounterAdded";

/// Annotation type for targeting relationships.
const ANNOTATION_TYPE_TARGET_SPEC: &str = "AnnotationType_TargetSpec";

/// Annotation type for life total modifications.
const ANNOTATION_TYPE_MODIFIED_LIFE: &str = "AnnotationType_ModifiedLife";

/// Extracts annotations from the `gameStateMessage.annotations` array.
///
/// Each annotation has at minimum `id`, `affectorId`, `affectedIds`, and
/// `type`. Special handling normalizes type-specific data:
///
/// - **`AnnotationType_ZoneTransfer`**: `zone_src`, `zone_dest`, `category`
/// - **`AnnotationType_ObjectIdChanged`**: `old_id`, `new_id`
/// - **`AnnotationType_DamageDealt`**: `damage`, `damage_type`
/// - **`AnnotationType_CounterAdded`**: `counter_type`, `amount`
/// - **`AnnotationType_TargetSpec`**: `ability_grp_id`, `target_index`
/// - **`AnnotationType_ModifiedLife`**: `life`
/// - **All other types**: passed through with base fields only.
///
/// Returns an empty `Vec` when `annotations` is absent or empty.
pub(super) fn extract_annotations(gsm: Option<&serde_json::Value>) -> Vec<serde_json::Value> {
    let Some(raw_annotations) = gsm
        .and_then(|g| g.get("annotations"))
        .and_then(serde_json::Value::as_array)
    else {
        return Vec::new();
    };

    raw_annotations
        .iter()
        .filter_map(extract_single_annotation)
        .collect()
}

/// Reads the annotation `type` field, which may be an array of strings
/// (current format) or a single string (legacy format).
/// Returns the first/only type string, or an empty string when absent.
fn read_annotation_type(annotation: &serde_json::Value) -> &str {
    let v = &annotation["type"];
    if let Some(s) = v.as_str() {
        return s;
    }
    v.as_array()
        .and_then(|arr| arr.first())
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
}

/// Reads all annotation type strings from the `type` field. Handles both
/// the array format (`["A", "B"]`) and the legacy single-string format.
fn read_annotation_types(annotation: &serde_json::Value) -> Vec<&str> {
    let v = &annotation["type"];
    if let Some(arr) = v.as_array() {
        return arr.iter().filter_map(serde_json::Value::as_str).collect();
    }
    v.as_str().into_iter().collect()
}

/// Looks up an `i64` value from a `details` key-value pair array by key name.
///
/// Each entry has `{"key": "<name>", "valueInt32": [<val>]}`.
fn detail_int(details: &[serde_json::Value], key: &str) -> Option<i64> {
    details
        .iter()
        .find(|d| d.get("key").and_then(serde_json::Value::as_str) == Some(key))
        .and_then(|d| {
            d.get("valueInt32")
                .and_then(serde_json::Value::as_array)
                .and_then(|arr| arr.first())
                .and_then(serde_json::Value::as_i64)
        })
}

/// Looks up a string value from a `details` key-value pair array by key name.
///
/// Each entry has `{"key": "<name>", "valueString": ["<val>"]}`.
fn detail_str<'a>(details: &'a [serde_json::Value], key: &str) -> Option<&'a str> {
    details
        .iter()
        .find(|d| d.get("key").and_then(serde_json::Value::as_str) == Some(key))
        .and_then(|d| {
            d.get("valueString")
                .and_then(serde_json::Value::as_array)
                .and_then(|arr| arr.first())
                .and_then(serde_json::Value::as_str)
        })
}

/// Extracts a single annotation, normalizing base fields and adding
/// type-specific data for known annotation types.
fn extract_single_annotation(annotation: &serde_json::Value) -> Option<serde_json::Value> {
    let id = annotation.get("id").and_then(serde_json::Value::as_i64)?;

    let affector_id = annotation
        .get("affectorId")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0);

    let affected_ids = annotation
        .get("affectedIds")
        .and_then(serde_json::Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(serde_json::Value::as_i64)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let annotation_type = read_annotation_type(annotation);
    let annotation_types = read_annotation_types(annotation);

    let details = annotation
        .get("details")
        .and_then(serde_json::Value::as_array);

    let mut result = serde_json::json!({
        "id": id,
        "affector_id": affector_id,
        "affected_ids": affected_ids,
        "type": annotation_type,
        "types": annotation_types,
    });

    // Add type-specific data from `details`.
    if let Some(d) = details {
        add_type_specific_fields(&mut result, annotation_type, d);
    }

    Some(result)
}

/// Adds type-specific fields to an annotation result based on its primary
/// type and the `details` key-value pairs.
fn add_type_specific_fields(
    result: &mut serde_json::Value,
    annotation_type: &str,
    details: &[serde_json::Value],
) {
    let Some(obj) = result.as_object_mut() else {
        return;
    };

    match annotation_type {
        ANNOTATION_TYPE_ZONE_TRANSFER => {
            obj.insert(
                "zone_src".to_string(),
                serde_json::json!(detail_int(details, "zone_src").unwrap_or(0)),
            );
            obj.insert(
                "zone_dest".to_string(),
                serde_json::json!(detail_int(details, "zone_dest").unwrap_or(0)),
            );
            obj.insert(
                "category".to_string(),
                serde_json::json!(detail_str(details, "category").unwrap_or("")),
            );
        }
        ANNOTATION_TYPE_OBJECT_ID_CHANGED => {
            obj.insert(
                "old_id".to_string(),
                serde_json::json!(detail_int(details, "orig_id").unwrap_or(0)),
            );
            obj.insert(
                "new_id".to_string(),
                serde_json::json!(detail_int(details, "new_id").unwrap_or(0)),
            );
        }
        ANNOTATION_TYPE_DAMAGE_DEALT => {
            obj.insert(
                "damage".to_string(),
                serde_json::json!(detail_int(details, "damage").unwrap_or(0)),
            );
            obj.insert(
                "damage_type".to_string(),
                serde_json::json!(detail_int(details, "type").unwrap_or(0)),
            );
        }
        ANNOTATION_TYPE_COUNTER_ADDED => {
            obj.insert(
                "counter_type".to_string(),
                serde_json::json!(detail_int(details, "counter_type").unwrap_or(0)),
            );
            obj.insert(
                "amount".to_string(),
                serde_json::json!(detail_int(details, "transaction_amount").unwrap_or(0)),
            );
        }
        ANNOTATION_TYPE_TARGET_SPEC => {
            obj.insert(
                "ability_grp_id".to_string(),
                serde_json::json!(detail_int(details, "abilityGrpId").unwrap_or(0)),
            );
            obj.insert(
                "target_index".to_string(),
                serde_json::json!(detail_int(details, "index").unwrap_or(0)),
            );
        }
        ANNOTATION_TYPE_MODIFIED_LIFE => {
            obj.insert(
                "life".to_string(),
                serde_json::json!(detail_int(details, "life").unwrap_or(0)),
            );
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::super::test_fixtures::*;
    use super::super::try_parse;
    use crate::parsers::test_helpers::{game_state_payload, test_timestamp, unity_entry};

    /// Helper: build a `GameStateMessage` body with annotations using the
    /// current Arena `details` key-value pair format.
    fn game_state_message_with_annotations_body() -> String {
        format!(
            "[UnityCrossThreadLogger]greToClientEvent\n{}",
            serde_json::json!({
                "greToClientEvent": {
                    "greToClientMessages": [{
                        "type": "GREMessageType_GameStateMessage",
                        "msgId": 15,
                        "gameStateId": 80,
                        "gameStateMessage": {
                            "zones": [],
                            "gameObjects": [],
                            "gameInfo": {
                                "matchID": "match-annot-001",
                                "gameNumber": 1,
                                "stage": "GameStage_Play"
                            },
                            "annotations": [
                                {
                                    "id": 145,
                                    "affectorId": 296,
                                    "affectedIds": [409],
                                    "type": ["AnnotationType_ZoneTransfer"],
                                    "details": [
                                        { "key": "zone_src", "type": "KeyValuePairValueType_int32", "valueInt32": [29] },
                                        { "key": "zone_dest", "type": "KeyValuePairValueType_int32", "valueInt32": [31] },
                                        { "key": "category", "type": "KeyValuePairValueType_string", "valueString": ["Draw"] }
                                    ]
                                },
                                {
                                    "id": 146,
                                    "affectorId": 300,
                                    "affectedIds": [410, 411],
                                    "type": ["AnnotationType_ObjectIdChanged"],
                                    "details": [
                                        { "key": "orig_id", "type": "KeyValuePairValueType_int32", "valueInt32": [410] },
                                        { "key": "new_id", "type": "KeyValuePairValueType_int32", "valueInt32": [500] }
                                    ]
                                },
                                {
                                    "id": 147,
                                    "affectorId": 305,
                                    "affectedIds": [412],
                                    "type": ["AnnotationType_ResolutionStart"]
                                }
                            ]
                        }
                    }]
                }
            })
        )
    }

    /// Helper: build a `GameStateMessage` body with a single `ZoneTransfer`
    /// annotation (`CastSpell` category).
    fn game_state_message_with_single_annotation_body() -> String {
        format!(
            "[UnityCrossThreadLogger]greToClientEvent\n{}",
            serde_json::json!({
                "greToClientEvent": {
                    "greToClientMessages": [{
                        "type": "GREMessageType_GameStateMessage",
                        "msgId": 16,
                        "gameStateId": 81,
                        "gameStateMessage": {
                            "annotations": [
                                {
                                    "id": 200,
                                    "affectorId": 350,
                                    "affectedIds": [420],
                                    "type": ["AnnotationType_ZoneTransfer"],
                                    "details": [
                                        { "key": "zone_src", "type": "KeyValuePairValueType_int32", "valueInt32": [30] },
                                        { "key": "zone_dest", "type": "KeyValuePairValueType_int32", "valueInt32": [34] },
                                        { "key": "category", "type": "KeyValuePairValueType_string", "valueString": ["CastSpell"] }
                                    ]
                                }
                            ]
                        }
                    }]
                }
            })
        )
    }

    /// Helper: build a `GameStateMessage` body with a `ZoneTransfer` annotation
    /// that has no `details` array (edge case).
    fn game_state_message_with_missing_details_body() -> String {
        format!(
            "[UnityCrossThreadLogger]greToClientEvent\n{}",
            serde_json::json!({
                "greToClientEvent": {
                    "greToClientMessages": [{
                        "type": "GREMessageType_GameStateMessage",
                        "msgId": 17,
                        "gameStateId": 82,
                        "gameStateMessage": {
                            "annotations": [
                                {
                                    "id": 201,
                                    "affectorId": 355,
                                    "affectedIds": [430],
                                    "type": ["AnnotationType_ZoneTransfer"]
                                }
                            ]
                        }
                    }]
                }
            })
        )
    }

    mod annotations_extraction {
        use super::*;

        #[test]
        fn test_annotations_present_is_array() {
            let body = game_state_message_with_annotations_body();
            let entry = unity_entry(&body);
            let event = try_parse(&entry, Some(test_timestamp()))
                .into_iter()
                .next()
                .unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(&event);

            assert!(payload["annotations"].is_array());
        }

        #[test]
        fn test_annotations_count_three() {
            let body = game_state_message_with_annotations_body();
            let entry = unity_entry(&body);
            let event = try_parse(&entry, Some(test_timestamp()))
                .into_iter()
                .next()
                .unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(&event);

            let annotations = payload["annotations"]
                .as_array()
                .unwrap_or_else(|| unreachable!());
            assert_eq!(annotations.len(), 3);
        }

        #[test]
        fn test_single_annotation_base_fields() {
            let body = game_state_message_with_single_annotation_body();
            let entry = unity_entry(&body);
            let event = try_parse(&entry, Some(test_timestamp()))
                .into_iter()
                .next()
                .unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(&event);

            let annotations = payload["annotations"]
                .as_array()
                .unwrap_or_else(|| unreachable!());
            assert_eq!(annotations.len(), 1);
            let ann = &annotations[0];
            assert_eq!(ann["id"], 200);
            assert_eq!(ann["affector_id"], 350);
            assert_eq!(ann["affected_ids"], serde_json::json!([420]));
            assert_eq!(ann["type"], "AnnotationType_ZoneTransfer");
        }

        #[test]
        fn test_zone_transfer_fields() {
            let body = game_state_message_with_annotations_body();
            let entry = unity_entry(&body);
            let event = try_parse(&entry, Some(test_timestamp()))
                .into_iter()
                .next()
                .unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(&event);

            let ann = &payload["annotations"][0];
            assert_eq!(ann["type"], "AnnotationType_ZoneTransfer");
            assert_eq!(ann["zone_src"], 29);
            assert_eq!(ann["zone_dest"], 31);
            assert_eq!(ann["category"], "Draw");
        }

        #[test]
        fn test_object_id_changed_fields() {
            let body = game_state_message_with_annotations_body();
            let entry = unity_entry(&body);
            let event = try_parse(&entry, Some(test_timestamp()))
                .into_iter()
                .next()
                .unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(&event);

            let ann = &payload["annotations"][1];
            assert_eq!(ann["type"], "AnnotationType_ObjectIdChanged");
            assert_eq!(ann["old_id"], 410);
            assert_eq!(ann["new_id"], 500);
        }

        #[test]
        fn test_unknown_annotation_type_passed_through() {
            let body = game_state_message_with_annotations_body();
            let entry = unity_entry(&body);
            let event = try_parse(&entry, Some(test_timestamp()))
                .into_iter()
                .next()
                .unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(&event);

            let ann = &payload["annotations"][2];
            assert_eq!(ann["type"], "AnnotationType_ResolutionStart");
            assert_eq!(ann["id"], 147);
            assert_eq!(ann["affector_id"], 305);
            assert_eq!(ann["affected_ids"], serde_json::json!([412]));
            // No extra type-specific fields for unknown types.
            assert!(ann.get("zone_src").is_none());
            assert!(ann.get("old_id").is_none());
        }

        #[test]
        fn test_empty_annotations_array() {
            let body = format!(
                "[UnityCrossThreadLogger]greToClientEvent\n{}",
                serde_json::json!({
                    "greToClientEvent": {
                        "greToClientMessages": [{
                            "type": "GREMessageType_GameStateMessage",
                            "msgId": 18,
                            "gameStateId": 83,
                            "gameStateMessage": {
                                "annotations": []
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

            let annotations = payload["annotations"]
                .as_array()
                .unwrap_or_else(|| unreachable!());
            assert!(annotations.is_empty());
        }

        #[test]
        fn test_missing_annotations_returns_empty_array() {
            let body = game_state_message_body();
            let entry = unity_entry(&body);
            let event = try_parse(&entry, Some(test_timestamp()))
                .into_iter()
                .next()
                .unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(&event);

            let annotations = payload["annotations"]
                .as_array()
                .unwrap_or_else(|| unreachable!());
            assert!(annotations.is_empty());
        }

        #[test]
        fn test_zone_transfer_missing_details_still_has_base_fields() {
            let body = game_state_message_with_missing_details_body();
            let entry = unity_entry(&body);
            let event = try_parse(&entry, Some(test_timestamp()))
                .into_iter()
                .next()
                .unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(&event);

            let annotations = payload["annotations"]
                .as_array()
                .unwrap_or_else(|| unreachable!());
            assert_eq!(annotations.len(), 1);
            let ann = &annotations[0];
            assert_eq!(ann["id"], 201);
            assert_eq!(ann["type"], "AnnotationType_ZoneTransfer");
            // No zone_src/zone_dest/category when details is missing.
            assert!(ann.get("zone_src").is_none());
        }

        #[test]
        fn test_multiple_affected_ids() {
            let body = game_state_message_with_annotations_body();
            let entry = unity_entry(&body);
            let event = try_parse(&entry, Some(test_timestamp()))
                .into_iter()
                .next()
                .unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(&event);

            // Second annotation has two affected IDs.
            let ann = &payload["annotations"][1];
            assert_eq!(ann["affected_ids"], serde_json::json!([410, 411]));
        }

        #[test]
        fn test_multi_type_annotation_uses_first_type() {
            let body = format!(
                "[UnityCrossThreadLogger]greToClientEvent\n{}",
                serde_json::json!({
                    "greToClientEvent": {
                        "greToClientMessages": [{
                            "type": "GREMessageType_GameStateMessage",
                            "msgId": 19,
                            "gameStateId": 84,
                            "gameStateMessage": {
                                "annotations": [{
                                    "id": 300,
                                    "affectorId": 400,
                                    "affectedIds": [500],
                                    "type": [
                                        "AnnotationType_ModifiedToughness",
                                        "AnnotationType_ModifiedPower",
                                        "AnnotationType_Counter"
                                    ]
                                }]
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

            let ann = &payload["annotations"][0];
            assert_eq!(ann["type"], "AnnotationType_ModifiedToughness");
        }

        #[test]
        fn test_multi_type_annotation_includes_types_array() {
            let body = format!(
                "[UnityCrossThreadLogger]greToClientEvent\n{}",
                serde_json::json!({
                    "greToClientEvent": {
                        "greToClientMessages": [{
                            "type": "GREMessageType_GameStateMessage",
                            "msgId": 19,
                            "gameStateId": 84,
                            "gameStateMessage": {
                                "annotations": [{
                                    "id": 300,
                                    "affectorId": 400,
                                    "affectedIds": [500],
                                    "type": [
                                        "AnnotationType_ModifiedToughness",
                                        "AnnotationType_ModifiedPower",
                                        "AnnotationType_Counter"
                                    ]
                                }]
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

            let ann = &payload["annotations"][0];
            assert_eq!(
                ann["types"],
                serde_json::json!([
                    "AnnotationType_ModifiedToughness",
                    "AnnotationType_ModifiedPower",
                    "AnnotationType_Counter"
                ])
            );
        }

        #[test]
        fn test_single_type_annotation_has_types_array() {
            let body = game_state_message_with_single_annotation_body();
            let entry = unity_entry(&body);
            let event = try_parse(&entry, Some(test_timestamp()))
                .into_iter()
                .next()
                .unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(&event);

            let ann = &payload["annotations"][0];
            assert_eq!(
                ann["types"],
                serde_json::json!(["AnnotationType_ZoneTransfer"])
            );
        }
    }

    mod damage_dealt_extraction {
        use super::*;

        fn damage_dealt_body() -> String {
            format!(
                "[UnityCrossThreadLogger]greToClientEvent\n{}",
                serde_json::json!({
                    "greToClientEvent": {
                        "greToClientMessages": [{
                            "type": "GREMessageType_GameStateMessage",
                            "msgId": 20,
                            "gameStateId": 85,
                            "gameStateMessage": {
                                "annotations": [{
                                    "id": 214,
                                    "affectorId": 286,
                                    "affectedIds": [1],
                                    "type": ["AnnotationType_DamageDealt"],
                                    "details": [
                                        { "key": "damage", "type": "KeyValuePairValueType_int32", "valueInt32": [3] },
                                        { "key": "type", "type": "KeyValuePairValueType_int32", "valueInt32": [1] },
                                        { "key": "markDamage", "type": "KeyValuePairValueType_int32", "valueInt32": [1] }
                                    ]
                                }]
                            }
                        }]
                    }
                })
            )
        }

        #[test]
        fn test_damage_dealt_damage_amount() {
            let body = damage_dealt_body();
            let entry = unity_entry(&body);
            let event = try_parse(&entry, Some(test_timestamp()))
                .into_iter()
                .next()
                .unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(&event);

            let ann = &payload["annotations"][0];
            assert_eq!(ann["type"], "AnnotationType_DamageDealt");
            assert_eq!(ann["damage"], 3);
        }

        #[test]
        fn test_damage_dealt_damage_type() {
            let body = damage_dealt_body();
            let entry = unity_entry(&body);
            let event = try_parse(&entry, Some(test_timestamp()))
                .into_iter()
                .next()
                .unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(&event);

            let ann = &payload["annotations"][0];
            assert_eq!(ann["damage_type"], 1);
        }

        #[test]
        fn test_damage_dealt_affector_and_affected() {
            let body = damage_dealt_body();
            let entry = unity_entry(&body);
            let event = try_parse(&entry, Some(test_timestamp()))
                .into_iter()
                .next()
                .unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(&event);

            let ann = &payload["annotations"][0];
            assert_eq!(ann["affector_id"], 286);
            assert_eq!(ann["affected_ids"], serde_json::json!([1]));
        }

        #[test]
        fn test_damage_dealt_without_mark_damage() {
            let body = format!(
                "[UnityCrossThreadLogger]greToClientEvent\n{}",
                serde_json::json!({
                    "greToClientEvent": {
                        "greToClientMessages": [{
                            "type": "GREMessageType_GameStateMessage",
                            "msgId": 21,
                            "gameStateId": 86,
                            "gameStateMessage": {
                                "annotations": [{
                                    "id": 215,
                                    "affectorId": 300,
                                    "affectedIds": [2],
                                    "type": ["AnnotationType_DamageDealt"],
                                    "details": [
                                        { "key": "damage", "type": "KeyValuePairValueType_int32", "valueInt32": [5] },
                                        { "key": "type", "type": "KeyValuePairValueType_int32", "valueInt32": [0] }
                                    ]
                                }]
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

            let ann = &payload["annotations"][0];
            assert_eq!(ann["damage"], 5);
            assert_eq!(ann["damage_type"], 0);
        }
    }

    mod counter_added_extraction {
        use super::*;

        fn counter_added_body() -> String {
            format!(
                "[UnityCrossThreadLogger]greToClientEvent\n{}",
                serde_json::json!({
                    "greToClientEvent": {
                        "greToClientMessages": [{
                            "type": "GREMessageType_GameStateMessage",
                            "msgId": 22,
                            "gameStateId": 87,
                            "gameStateMessage": {
                                "annotations": [{
                                    "id": 196,
                                    "affectorId": 298,
                                    "affectedIds": [286],
                                    "type": ["AnnotationType_CounterAdded"],
                                    "details": [
                                        { "key": "counter_type", "type": "KeyValuePairValueType_int32", "valueInt32": [1] },
                                        { "key": "transaction_amount", "type": "KeyValuePairValueType_int32", "valueInt32": [1] }
                                    ]
                                }]
                            }
                        }]
                    }
                })
            )
        }

        #[test]
        fn test_counter_added_type() {
            let body = counter_added_body();
            let entry = unity_entry(&body);
            let event = try_parse(&entry, Some(test_timestamp()))
                .into_iter()
                .next()
                .unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(&event);

            let ann = &payload["annotations"][0];
            assert_eq!(ann["type"], "AnnotationType_CounterAdded");
            assert_eq!(ann["counter_type"], 1);
        }

        #[test]
        fn test_counter_added_amount() {
            let body = counter_added_body();
            let entry = unity_entry(&body);
            let event = try_parse(&entry, Some(test_timestamp()))
                .into_iter()
                .next()
                .unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(&event);

            let ann = &payload["annotations"][0];
            assert_eq!(ann["amount"], 1);
        }

        #[test]
        fn test_counter_added_affector_and_affected() {
            let body = counter_added_body();
            let entry = unity_entry(&body);
            let event = try_parse(&entry, Some(test_timestamp()))
                .into_iter()
                .next()
                .unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(&event);

            let ann = &payload["annotations"][0];
            assert_eq!(ann["affector_id"], 298);
            assert_eq!(ann["affected_ids"], serde_json::json!([286]));
        }
    }

    mod target_spec_extraction {
        use super::*;

        fn target_spec_body() -> String {
            format!(
                "[UnityCrossThreadLogger]greToClientEvent\n{}",
                serde_json::json!({
                    "greToClientEvent": {
                        "greToClientMessages": [{
                            "type": "GREMessageType_GameStateMessage",
                            "msgId": 23,
                            "gameStateId": 88,
                            "gameStateMessage": {
                                "annotations": [{
                                    "id": 159,
                                    "affectorId": 295,
                                    "affectedIds": [286],
                                    "type": ["AnnotationType_TargetSpec"],
                                    "details": [
                                        { "key": "abilityGrpId", "type": "KeyValuePairValueType_int32", "valueInt32": [1886] },
                                        { "key": "index", "type": "KeyValuePairValueType_int32", "valueInt32": [1] },
                                        { "key": "promptParameters", "type": "KeyValuePairValueType_int32", "valueInt32": [295] },
                                        { "key": "promptId", "type": "KeyValuePairValueType_int32", "valueInt32": [152] }
                                    ]
                                }]
                            }
                        }]
                    }
                })
            )
        }

        #[test]
        fn test_target_spec_ability_grp_id() {
            let body = target_spec_body();
            let entry = unity_entry(&body);
            let event = try_parse(&entry, Some(test_timestamp()))
                .into_iter()
                .next()
                .unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(&event);

            let ann = &payload["annotations"][0];
            assert_eq!(ann["type"], "AnnotationType_TargetSpec");
            assert_eq!(ann["ability_grp_id"], 1886);
        }

        #[test]
        fn test_target_spec_index() {
            let body = target_spec_body();
            let entry = unity_entry(&body);
            let event = try_parse(&entry, Some(test_timestamp()))
                .into_iter()
                .next()
                .unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(&event);

            let ann = &payload["annotations"][0];
            assert_eq!(ann["target_index"], 1);
        }

        #[test]
        fn test_target_spec_affector_and_affected() {
            let body = target_spec_body();
            let entry = unity_entry(&body);
            let event = try_parse(&entry, Some(test_timestamp()))
                .into_iter()
                .next()
                .unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(&event);

            let ann = &payload["annotations"][0];
            assert_eq!(ann["affector_id"], 295);
            assert_eq!(ann["affected_ids"], serde_json::json!([286]));
        }
    }

    mod modified_life_extraction {
        use super::*;

        #[test]
        fn test_modified_life_negative() {
            let body = format!(
                "[UnityCrossThreadLogger]greToClientEvent\n{}",
                serde_json::json!({
                    "greToClientEvent": {
                        "greToClientMessages": [{
                            "type": "GREMessageType_GameStateMessage",
                            "msgId": 24,
                            "gameStateId": 89,
                            "gameStateMessage": {
                                "annotations": [{
                                    "id": 218,
                                    "affectorId": 286,
                                    "affectedIds": [1],
                                    "type": ["AnnotationType_ModifiedLife"],
                                    "details": [
                                        { "key": "life", "type": "KeyValuePairValueType_int32", "valueInt32": [-3] }
                                    ]
                                }]
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

            let ann = &payload["annotations"][0];
            assert_eq!(ann["type"], "AnnotationType_ModifiedLife");
            assert_eq!(ann["life"], -3);
        }

        #[test]
        fn test_modified_life_positive() {
            let body = format!(
                "[UnityCrossThreadLogger]greToClientEvent\n{}",
                serde_json::json!({
                    "greToClientEvent": {
                        "greToClientMessages": [{
                            "type": "GREMessageType_GameStateMessage",
                            "msgId": 25,
                            "gameStateId": 90,
                            "gameStateMessage": {
                                "annotations": [{
                                    "id": 219,
                                    "affectorId": 286,
                                    "affectedIds": [2],
                                    "type": ["AnnotationType_ModifiedLife"],
                                    "details": [
                                        { "key": "life", "type": "KeyValuePairValueType_int32", "valueInt32": [3] }
                                    ]
                                }]
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

            let ann = &payload["annotations"][0];
            assert_eq!(ann["life"], 3);
        }
    }

    mod detail_helpers {
        use super::super::{detail_int, detail_str};

        #[test]
        fn test_detail_int_found() {
            let details = vec![serde_json::json!({
                "key": "zone_src",
                "type": "KeyValuePairValueType_int32",
                "valueInt32": [31]
            })];
            assert_eq!(detail_int(&details, "zone_src"), Some(31));
        }

        #[test]
        fn test_detail_int_missing_key() {
            let details = vec![serde_json::json!({
                "key": "zone_src",
                "type": "KeyValuePairValueType_int32",
                "valueInt32": [31]
            })];
            assert_eq!(detail_int(&details, "zone_dest"), None);
        }

        #[test]
        fn test_detail_str_found() {
            let details = vec![serde_json::json!({
                "key": "category",
                "type": "KeyValuePairValueType_string",
                "valueString": ["PlayLand"]
            })];
            assert_eq!(detail_str(&details, "category"), Some("PlayLand"));
        }

        #[test]
        fn test_detail_str_missing_key() {
            let details = vec![serde_json::json!({
                "key": "category",
                "type": "KeyValuePairValueType_string",
                "valueString": ["PlayLand"]
            })];
            assert_eq!(detail_str(&details, "other"), None);
        }

        #[test]
        fn test_detail_int_empty_array() {
            let details: Vec<serde_json::Value> = vec![];
            assert_eq!(detail_int(&details, "zone_src"), None);
        }
    }
}
