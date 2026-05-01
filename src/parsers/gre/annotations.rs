//! Annotations extraction from `gameStateMessage.annotations` and
//! `gameStateMessage.persistentAnnotations`.
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

/// Annotation type for power/toughness modifications applied to a permanent.
const ANNOTATION_TYPE_POWER_TOUGHNESS_MOD_CREATED: &str = "AnnotationType_PowerToughnessModCreated";

/// Annotation type for triggered ability attribution.
const ANNOTATION_TYPE_TRIGGERING_OBJECT: &str = "AnnotationType_TriggeringObject";

/// Annotation type for mana payments linking sources to spells.
const ANNOTATION_TYPE_MANA_PAID: &str = "AnnotationType_ManaPaid";

/// Annotation type for player actions (cast, activate, play land).
const ANNOTATION_TYPE_USER_ACTION_TAKEN: &str = "AnnotationType_UserActionTaken";

/// Annotation type for scry decisions (top/bottom card placement).
const ANNOTATION_TYPE_SCRY: &str = "AnnotationType_Scry";

/// Annotation type for library shuffles (reissues every library instanceId).
const ANNOTATION_TYPE_SHUFFLE: &str = "AnnotationType_Shuffle";

/// Annotation type for persistent instance designation
/// (companion, conjured, drafted, plus uncharacterized state flags).
const ANNOTATION_TYPE_DESIGNATION: &str = "AnnotationType_Designation";

/// Extracts persistent annotations from the `gameStateMessage.persistentAnnotations` array.
///
/// Persistent annotations accumulate across game state updates (unlike
/// ephemeral `annotations` which appear only in the diff). They include
/// targeting (`TargetSpec`), trigger attribution (`TriggeringObject`),
/// and counter/layered-effect tracking.
///
/// Uses the same extraction pipeline as regular annotations.
///
/// Returns an empty `Vec` when `persistentAnnotations` is absent or empty.
pub(super) fn extract_persistent_annotations(
    gsm: Option<&serde_json::Value>,
) -> Vec<serde_json::Value> {
    let Some(raw_annotations) = gsm
        .and_then(|g| g.get("persistentAnnotations"))
        .and_then(serde_json::Value::as_array)
    else {
        return Vec::new();
    };

    raw_annotations
        .iter()
        .filter_map(extract_single_annotation)
        .collect()
}

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
/// - **`AnnotationType_PowerToughnessModCreated`**: `power`, `toughness`
/// - **`AnnotationType_TriggeringObject`**: `source_zone`
/// - **`AnnotationType_ManaPaid`**: `mana_payment_id`, `color`
/// - **`AnnotationType_UserActionTaken`**: `action_type`, `ability_grp_id`
/// - **`AnnotationType_Scry`**: `top_ids`, `bottom_ids`
/// - **`AnnotationType_Shuffle`**: `old_ids`, `new_ids`
/// - **`AnnotationType_Designation`**: `instance_id`, `designation_type`, plus
///   any of `conjuration_type`, `source_grpid`, `grpid`, `value`, `controller_id`
///   that are present in `details` (permissive — no branching on
///   `DesignationType` value, so undocumented variants pass through cleanly).
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

/// Looks up all `i64` values from a `details` key-value pair array by key name.
///
/// Returns the full `valueInt32` array (e.g. for Scry `topIds`/`bottomIds`).
/// Returns an empty `Vec` when the key is missing or has no values.
fn detail_int_array(details: &[serde_json::Value], key: &str) -> Vec<i64> {
    details
        .iter()
        .find(|d| d.get("key").and_then(serde_json::Value::as_str) == Some(key))
        .and_then(|d| d.get("valueInt32").and_then(serde_json::Value::as_array))
        .map(|arr| arr.iter().filter_map(serde_json::Value::as_i64).collect())
        .unwrap_or_default()
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
        ANNOTATION_TYPE_ZONE_TRANSFER => add_zone_transfer_fields(obj, details),
        ANNOTATION_TYPE_OBJECT_ID_CHANGED => add_object_id_changed_fields(obj, details),
        ANNOTATION_TYPE_DAMAGE_DEALT => add_damage_dealt_fields(obj, details),
        ANNOTATION_TYPE_COUNTER_ADDED => add_counter_added_fields(obj, details),
        ANNOTATION_TYPE_TARGET_SPEC => add_target_spec_fields(obj, details),
        ANNOTATION_TYPE_MODIFIED_LIFE => add_modified_life_fields(obj, details),
        ANNOTATION_TYPE_POWER_TOUGHNESS_MOD_CREATED => add_pt_mod_fields(obj, details),
        ANNOTATION_TYPE_TRIGGERING_OBJECT => add_triggering_object_fields(obj, details),
        ANNOTATION_TYPE_MANA_PAID => add_mana_paid_fields(obj, details),
        ANNOTATION_TYPE_USER_ACTION_TAKEN => add_user_action_fields(obj, details),
        ANNOTATION_TYPE_SCRY => add_scry_fields(obj, details),
        ANNOTATION_TYPE_SHUFFLE => add_shuffle_fields(obj, details),
        ANNOTATION_TYPE_DESIGNATION => add_designation_fields(obj, details),
        _ => {}
    }
}

fn add_zone_transfer_fields(
    obj: &mut serde_json::Map<String, serde_json::Value>,
    details: &[serde_json::Value],
) {
    obj.insert(
        "zone_src".into(),
        serde_json::json!(detail_int(details, "zone_src").unwrap_or(0)),
    );
    obj.insert(
        "zone_dest".into(),
        serde_json::json!(detail_int(details, "zone_dest").unwrap_or(0)),
    );
    obj.insert(
        "category".into(),
        serde_json::json!(detail_str(details, "category").unwrap_or("")),
    );
}

fn add_object_id_changed_fields(
    obj: &mut serde_json::Map<String, serde_json::Value>,
    details: &[serde_json::Value],
) {
    obj.insert(
        "old_id".into(),
        serde_json::json!(detail_int(details, "orig_id").unwrap_or(0)),
    );
    obj.insert(
        "new_id".into(),
        serde_json::json!(detail_int(details, "new_id").unwrap_or(0)),
    );
}

fn add_damage_dealt_fields(
    obj: &mut serde_json::Map<String, serde_json::Value>,
    details: &[serde_json::Value],
) {
    obj.insert(
        "damage".into(),
        serde_json::json!(detail_int(details, "damage").unwrap_or(0)),
    );
    obj.insert(
        "damage_type".into(),
        serde_json::json!(detail_int(details, "type").unwrap_or(0)),
    );
}

fn add_counter_added_fields(
    obj: &mut serde_json::Map<String, serde_json::Value>,
    details: &[serde_json::Value],
) {
    obj.insert(
        "counter_type".into(),
        serde_json::json!(detail_int(details, "counter_type").unwrap_or(0)),
    );
    obj.insert(
        "amount".into(),
        serde_json::json!(detail_int(details, "transaction_amount").unwrap_or(0)),
    );
}

fn add_target_spec_fields(
    obj: &mut serde_json::Map<String, serde_json::Value>,
    details: &[serde_json::Value],
) {
    obj.insert(
        "ability_grp_id".into(),
        serde_json::json!(detail_int(details, "abilityGrpId").unwrap_or(0)),
    );
    obj.insert(
        "target_index".into(),
        serde_json::json!(detail_int(details, "index").unwrap_or(0)),
    );
}

fn add_modified_life_fields(
    obj: &mut serde_json::Map<String, serde_json::Value>,
    details: &[serde_json::Value],
) {
    obj.insert(
        "life".into(),
        serde_json::json!(detail_int(details, "life").unwrap_or(0)),
    );
}

fn add_pt_mod_fields(
    obj: &mut serde_json::Map<String, serde_json::Value>,
    details: &[serde_json::Value],
) {
    obj.insert(
        "power".into(),
        serde_json::json!(detail_int(details, "power").unwrap_or(0)),
    );
    obj.insert(
        "toughness".into(),
        serde_json::json!(detail_int(details, "toughness").unwrap_or(0)),
    );
}

fn add_triggering_object_fields(
    obj: &mut serde_json::Map<String, serde_json::Value>,
    details: &[serde_json::Value],
) {
    obj.insert(
        "source_zone".into(),
        serde_json::json!(detail_int(details, "source_zone").unwrap_or(0)),
    );
}

fn add_mana_paid_fields(
    obj: &mut serde_json::Map<String, serde_json::Value>,
    details: &[serde_json::Value],
) {
    obj.insert(
        "mana_payment_id".into(),
        serde_json::json!(detail_int(details, "id").unwrap_or(0)),
    );
    obj.insert(
        "color".into(),
        serde_json::json!(detail_int(details, "color").unwrap_or(0)),
    );
}

fn add_user_action_fields(
    obj: &mut serde_json::Map<String, serde_json::Value>,
    details: &[serde_json::Value],
) {
    obj.insert(
        "action_type".into(),
        serde_json::json!(detail_int(details, "actionType").unwrap_or(0)),
    );
    obj.insert(
        "ability_grp_id".into(),
        serde_json::json!(detail_int(details, "abilityGrpId").unwrap_or(0)),
    );
}

fn add_scry_fields(
    obj: &mut serde_json::Map<String, serde_json::Value>,
    details: &[serde_json::Value],
) {
    obj.insert(
        "top_ids".into(),
        serde_json::json!(detail_int_array(details, "topIds")),
    );
    obj.insert(
        "bottom_ids".into(),
        serde_json::json!(detail_int_array(details, "bottomIds")),
    );
}

fn add_shuffle_fields(
    obj: &mut serde_json::Map<String, serde_json::Value>,
    details: &[serde_json::Value],
) {
    obj.insert(
        "old_ids".into(),
        serde_json::json!(detail_int_array(details, "OldIds")),
    );
    obj.insert(
        "new_ids".into(),
        serde_json::json!(detail_int_array(details, "NewIds")),
    );
}

/// Adds Designation fields to an annotation result.
///
/// Always emits `instance_id` (from the existing `affected_ids[0]` on the
/// result object) and `designation_type` (from `details.DesignationType`).
/// Then conditionally emits each of `conjuration_type`, `source_grpid`,
/// `grpid`, `value`, `controller_id` — but only when the corresponding
/// source key is present in `details`.
///
/// The implementation is intentionally permissive: every optional key is
/// looked up unconditionally via the existing `detail_int` / `detail_str`
/// helpers and inserted only when found. Never branches on the
/// `DesignationType` value — undocumented variants pass through cleanly
/// with whatever fields they happen to carry.
fn add_designation_fields(
    obj: &mut serde_json::Map<String, serde_json::Value>,
    details: &[serde_json::Value],
) {
    // instance_id mirrors affected_ids[0] for convenience. The
    // affected_ids array is already populated on the result object by
    // extract_single_annotation before this helper runs.
    let instance_id = obj
        .get("affected_ids")
        .and_then(serde_json::Value::as_array)
        .and_then(|arr| arr.first())
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0);
    obj.insert("instance_id".into(), serde_json::json!(instance_id));

    obj.insert(
        "designation_type".into(),
        serde_json::json!(detail_int(details, "DesignationType").unwrap_or(0)),
    );

    if let Some(ct) = detail_str(details, "conjuration_type") {
        obj.insert("conjuration_type".into(), serde_json::json!(ct));
    }
    if let Some(sg) = detail_int(details, "source_grpid") {
        obj.insert("source_grpid".into(), serde_json::json!(sg));
    }
    if let Some(g) = detail_int(details, "grpid") {
        obj.insert("grpid".into(), serde_json::json!(g));
    }
    if let Some(v) = detail_int(details, "value") {
        obj.insert("value".into(), serde_json::json!(v));
    }
    if let Some(c) = detail_int(details, "ControllerId") {
        obj.insert("controller_id".into(), serde_json::json!(c));
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

    mod power_toughness_mod_extraction {
        use super::*;

        fn pt_mod_body(power: i64, toughness: i64) -> String {
            format!(
                "[UnityCrossThreadLogger]greToClientEvent\n{}",
                serde_json::json!({
                    "greToClientEvent": {
                        "greToClientMessages": [{
                            "type": "GREMessageType_GameStateMessage",
                            "msgId": 26,
                            "gameStateId": 91,
                            "gameStateMessage": {
                                "annotations": [{
                                    "id": 145,
                                    "affectorId": 294,
                                    "affectedIds": [286],
                                    "type": ["AnnotationType_PowerToughnessModCreated"],
                                    "details": [
                                        { "key": "power", "type": "KeyValuePairValueType_int32", "valueInt32": [power] },
                                        { "key": "toughness", "type": "KeyValuePairValueType_int32", "valueInt32": [toughness] }
                                    ]
                                }]
                            }
                        }]
                    }
                })
            )
        }

        #[test]
        fn test_pt_mod_created_negative_values() {
            let body = pt_mod_body(-3, -3);
            let entry = unity_entry(&body);
            let event = try_parse(&entry, Some(test_timestamp()))
                .into_iter()
                .next()
                .unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(&event);

            let ann = &payload["annotations"][0];
            assert_eq!(ann["type"], "AnnotationType_PowerToughnessModCreated");
            assert_eq!(ann["power"], -3);
            assert_eq!(ann["toughness"], -3);
        }

        #[test]
        fn test_pt_mod_created_positive_values() {
            let body = pt_mod_body(1, 1);
            let entry = unity_entry(&body);
            let event = try_parse(&entry, Some(test_timestamp()))
                .into_iter()
                .next()
                .unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(&event);

            let ann = &payload["annotations"][0];
            assert_eq!(ann["power"], 1);
            assert_eq!(ann["toughness"], 1);
        }

        #[test]
        fn test_pt_mod_created_affector_and_affected() {
            let body = pt_mod_body(-3, -3);
            let entry = unity_entry(&body);
            let event = try_parse(&entry, Some(test_timestamp()))
                .into_iter()
                .next()
                .unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(&event);

            let ann = &payload["annotations"][0];
            assert_eq!(ann["affector_id"], 294);
            assert_eq!(ann["affected_ids"], serde_json::json!([286]));
        }
    }

    mod triggering_object_extraction {
        use super::*;

        fn triggering_object_body() -> String {
            format!(
                "[UnityCrossThreadLogger]greToClientEvent\n{}",
                serde_json::json!({
                    "greToClientEvent": {
                        "greToClientMessages": [{
                            "type": "GREMessageType_GameStateMessage",
                            "msgId": 27,
                            "gameStateId": 92,
                            "gameStateMessage": {
                                "persistentAnnotations": [{
                                    "id": 45,
                                    "affectorId": 286,
                                    "affectedIds": [284],
                                    "type": ["AnnotationType_TriggeringObject"],
                                    "details": [
                                        { "key": "source_zone", "type": "KeyValuePairValueType_int32", "valueInt32": [28] }
                                    ]
                                }]
                            }
                        }]
                    }
                })
            )
        }

        #[test]
        fn test_triggering_object_source_zone() {
            let body = triggering_object_body();
            let entry = unity_entry(&body);
            let event = try_parse(&entry, Some(test_timestamp()))
                .into_iter()
                .next()
                .unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(&event);

            let ann = &payload["persistent_annotations"][0];
            assert_eq!(ann["type"], "AnnotationType_TriggeringObject");
            assert_eq!(ann["source_zone"], 28);
        }

        #[test]
        fn test_triggering_object_affector_and_affected() {
            let body = triggering_object_body();
            let entry = unity_entry(&body);
            let event = try_parse(&entry, Some(test_timestamp()))
                .into_iter()
                .next()
                .unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(&event);

            let ann = &payload["persistent_annotations"][0];
            assert_eq!(ann["affector_id"], 286);
            assert_eq!(ann["affected_ids"], serde_json::json!([284]));
        }
    }

    mod mana_paid_extraction {
        use super::*;

        fn mana_paid_body() -> String {
            format!(
                "[UnityCrossThreadLogger]greToClientEvent\n{}",
                serde_json::json!({
                    "greToClientEvent": {
                        "greToClientMessages": [{
                            "type": "GREMessageType_GameStateMessage",
                            "msgId": 28,
                            "gameStateId": 93,
                            "gameStateMessage": {
                                "annotations": [{
                                    "id": 34,
                                    "affectorId": 283,
                                    "affectedIds": [284],
                                    "type": ["AnnotationType_ManaPaid"],
                                    "details": [
                                        { "key": "id", "type": "KeyValuePairValueType_int32", "valueInt32": [3] },
                                        { "key": "color", "type": "KeyValuePairValueType_int32", "valueInt32": [2] }
                                    ]
                                }]
                            }
                        }]
                    }
                })
            )
        }

        #[test]
        fn test_mana_paid_payment_id_and_color() {
            let body = mana_paid_body();
            let entry = unity_entry(&body);
            let event = try_parse(&entry, Some(test_timestamp()))
                .into_iter()
                .next()
                .unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(&event);

            let ann = &payload["annotations"][0];
            assert_eq!(ann["type"], "AnnotationType_ManaPaid");
            assert_eq!(ann["mana_payment_id"], 3);
            assert_eq!(ann["color"], 2);
        }

        #[test]
        fn test_mana_paid_affector_is_source() {
            let body = mana_paid_body();
            let entry = unity_entry(&body);
            let event = try_parse(&entry, Some(test_timestamp()))
                .into_iter()
                .next()
                .unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(&event);

            let ann = &payload["annotations"][0];
            assert_eq!(ann["affector_id"], 283);
            assert_eq!(ann["affected_ids"], serde_json::json!([284]));
        }
    }

    mod user_action_taken_extraction {
        use super::*;

        fn user_action_body(action_type: i64, ability_grp_id: i64) -> String {
            format!(
                "[UnityCrossThreadLogger]greToClientEvent\n{}",
                serde_json::json!({
                    "greToClientEvent": {
                        "greToClientMessages": [{
                            "type": "GREMessageType_GameStateMessage",
                            "msgId": 29,
                            "gameStateId": 94,
                            "gameStateMessage": {
                                "annotations": [{
                                    "id": 393,
                                    "affectorId": 1,
                                    "affectedIds": [327],
                                    "type": ["AnnotationType_UserActionTaken"],
                                    "details": [
                                        { "key": "actionType", "type": "KeyValuePairValueType_int32", "valueInt32": [action_type] },
                                        { "key": "abilityGrpId", "type": "KeyValuePairValueType_int32", "valueInt32": [ability_grp_id] }
                                    ]
                                }]
                            }
                        }]
                    }
                })
            )
        }

        #[test]
        fn test_user_action_activate_ability() {
            let body = user_action_body(2, 152_795);
            let entry = unity_entry(&body);
            let event = try_parse(&entry, Some(test_timestamp()))
                .into_iter()
                .next()
                .unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(&event);

            let ann = &payload["annotations"][0];
            assert_eq!(ann["type"], "AnnotationType_UserActionTaken");
            assert_eq!(ann["action_type"], 2);
            assert_eq!(ann["ability_grp_id"], 152_795);
        }

        #[test]
        fn test_user_action_play_land() {
            let body = user_action_body(3, 75478);
            let entry = unity_entry(&body);
            let event = try_parse(&entry, Some(test_timestamp()))
                .into_iter()
                .next()
                .unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(&event);

            let ann = &payload["annotations"][0];
            assert_eq!(ann["action_type"], 3);
            assert_eq!(ann["ability_grp_id"], 75478);
        }

        #[test]
        fn test_user_action_affector_is_player_seat() {
            let body = user_action_body(1, 0);
            let entry = unity_entry(&body);
            let event = try_parse(&entry, Some(test_timestamp()))
                .into_iter()
                .next()
                .unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(&event);

            let ann = &payload["annotations"][0];
            assert_eq!(ann["affector_id"], 1);
        }
    }

    mod scry_extraction {
        use super::*;

        #[test]
        fn test_scry_top_and_bottom_ids() {
            let body = format!(
                "[UnityCrossThreadLogger]greToClientEvent\n{}",
                serde_json::json!({
                    "greToClientEvent": {
                        "greToClientMessages": [{
                            "type": "GREMessageType_GameStateMessage",
                            "msgId": 30,
                            "gameStateId": 95,
                            "gameStateMessage": {
                                "annotations": [{
                                    "id": 48,
                                    "affectorId": 286,
                                    "affectedIds": [230],
                                    "type": ["AnnotationType_Scry"],
                                    "details": [
                                        { "key": "topIds", "type": "KeyValuePairValueType_int32", "valueInt32": [230, 231] },
                                        { "key": "bottomIds", "type": "KeyValuePairValueType_int32", "valueInt32": [232] }
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
            assert_eq!(ann["type"], "AnnotationType_Scry");
            assert_eq!(ann["top_ids"], serde_json::json!([230, 231]));
            assert_eq!(ann["bottom_ids"], serde_json::json!([232]));
        }

        #[test]
        fn test_scry_empty_bottom_ids() {
            let body = format!(
                "[UnityCrossThreadLogger]greToClientEvent\n{}",
                serde_json::json!({
                    "greToClientEvent": {
                        "greToClientMessages": [{
                            "type": "GREMessageType_GameStateMessage",
                            "msgId": 31,
                            "gameStateId": 96,
                            "gameStateMessage": {
                                "annotations": [{
                                    "id": 49,
                                    "affectorId": 286,
                                    "affectedIds": [230],
                                    "type": ["AnnotationType_Scry"],
                                    "details": [
                                        { "key": "topIds", "type": "KeyValuePairValueType_int32", "valueInt32": [230] },
                                        { "key": "bottomIds" }
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
            assert_eq!(ann["top_ids"], serde_json::json!([230]));
            assert_eq!(ann["bottom_ids"], serde_json::json!([]));
        }
    }

    mod shuffle_extraction {
        use super::*;

        /// Standalone library shuffle: 32-element `OldIds` and `NewIds` arrays
        /// (sourced from `session_2026-02-22_0000_ecl-premier-bg-elves` line 5828).
        /// Every library card receives a fresh `instanceId`; arrays are equal length.
        #[test]
        fn test_shuffle_old_and_new_ids_full_library() {
            let old_ids: Vec<i64> = vec![
                166, 167, 168, 169, 170, 171, 172, 173, 174, 175, 176, 177, 178, 179, 180, 181,
                182, 183, 184, 186, 187, 188, 189, 190, 191, 192, 193, 194, 195, 196, 197, 198,
            ];
            let new_ids: Vec<i64> = vec![
                205, 206, 207, 208, 209, 210, 211, 212, 213, 214, 215, 216, 217, 218, 219, 220,
                221, 222, 223, 224, 225, 226, 227, 228, 229, 230, 231, 232, 233, 234, 235, 236,
            ];
            let body = format!(
                "[UnityCrossThreadLogger]greToClientEvent\n{}",
                serde_json::json!({
                    "greToClientEvent": {
                        "greToClientMessages": [{
                            "type": "GREMessageType_GameStateMessage",
                            "msgId": 40,
                            "gameStateId": 100,
                            "gameStateMessage": {
                                "annotations": [{
                                    "id": 121,
                                    "affectorId": 202,
                                    "affectedIds": [2],
                                    "type": ["AnnotationType_Shuffle"],
                                    "details": [
                                        { "key": "OldIds", "type": "KeyValuePairValueType_int32", "valueInt32": old_ids },
                                        { "key": "NewIds", "type": "KeyValuePairValueType_int32", "valueInt32": new_ids }
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
            assert_eq!(ann["type"], "AnnotationType_Shuffle");

            let old = ann["old_ids"].as_array().unwrap_or_else(|| unreachable!());
            let new = ann["new_ids"].as_array().unwrap_or_else(|| unreachable!());
            assert_eq!(old.len(), 32);
            assert_eq!(new.len(), 32);
            assert_eq!(old.len(), new.len());
            assert_eq!(old[0], 166);
            assert_eq!(new[0], 205);
            assert_eq!(old[31], 198);
            assert_eq!(new[31], 236);
        }

        /// Scry annotation followed by Shuffle annotation in the same
        /// `annotations` array. Both must be extracted independently with
        /// their own type-specific fields.
        #[test]
        fn test_scry_then_shuffle_co_occurrence() {
            let body = format!(
                "[UnityCrossThreadLogger]greToClientEvent\n{}",
                serde_json::json!({
                    "greToClientEvent": {
                        "greToClientMessages": [{
                            "type": "GREMessageType_GameStateMessage",
                            "msgId": 41,
                            "gameStateId": 101,
                            "gameStateMessage": {
                                "annotations": [
                                    {
                                        "id": 50,
                                        "affectorId": 286,
                                        "affectedIds": [240],
                                        "type": ["AnnotationType_Scry"],
                                        "details": [
                                            { "key": "topIds", "type": "KeyValuePairValueType_int32", "valueInt32": [240, 241] },
                                            { "key": "bottomIds", "type": "KeyValuePairValueType_int32", "valueInt32": [242] }
                                        ]
                                    },
                                    {
                                        "id": 51,
                                        "affectorId": 286,
                                        "affectedIds": [2],
                                        "type": ["AnnotationType_Shuffle"],
                                        "details": [
                                            { "key": "OldIds", "type": "KeyValuePairValueType_int32", "valueInt32": [240, 241, 242] },
                                            { "key": "NewIds", "type": "KeyValuePairValueType_int32", "valueInt32": [300, 301, 302] }
                                        ]
                                    }
                                ]
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

            let scry = &payload["annotations"][0];
            assert_eq!(scry["type"], "AnnotationType_Scry");
            assert_eq!(scry["top_ids"], serde_json::json!([240, 241]));
            assert_eq!(scry["bottom_ids"], serde_json::json!([242]));
            // Scry must not pick up shuffle fields.
            assert!(scry.get("old_ids").is_none());
            assert!(scry.get("new_ids").is_none());

            let shuffle = &payload["annotations"][1];
            assert_eq!(shuffle["type"], "AnnotationType_Shuffle");
            assert_eq!(shuffle["old_ids"], serde_json::json!([240, 241, 242]));
            assert_eq!(shuffle["new_ids"], serde_json::json!([300, 301, 302]));
            // Shuffle must not pick up scry fields.
            assert!(shuffle.get("top_ids").is_none());
            assert!(shuffle.get("bottom_ids").is_none());
        }
    }

    mod designation_extraction {
        use super::*;

        /// Type=7 companion (Yorion) — observed in `session_2026-04-30_0000`.
        /// `details` carries `DesignationType=7` and `grpid` (companion grpId).
        /// No `conjuration_type` or `source_grpid`.
        #[test]
        fn test_designation_type_7_companion() {
            let body = format!(
                "[UnityCrossThreadLogger]greToClientEvent\n{}",
                serde_json::json!({
                    "greToClientEvent": {
                        "greToClientMessages": [{
                            "type": "GREMessageType_GameStateMessage",
                            "msgId": 42,
                            "gameStateId": 102,
                            "gameStateMessage": {
                                "persistentAnnotations": [{
                                    "id": 12,
                                    "affectorId": 1,
                                    "affectedIds": [55],
                                    "type": ["AnnotationType_Designation"],
                                    "details": [
                                        { "key": "DesignationType", "type": "KeyValuePairValueType_int32", "valueInt32": [7] },
                                        { "key": "grpid", "type": "KeyValuePairValueType_int32", "valueInt32": [85582] }
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

            let ann = &payload["persistent_annotations"][0];
            assert_eq!(ann["type"], "AnnotationType_Designation");
            assert_eq!(ann["instance_id"], 55);
            assert_eq!(ann["designation_type"], 7);
            assert_eq!(ann["grpid"], 85582);
            // type=7 has no conjuration_type or source_grpid.
            assert!(ann.get("conjuration_type").is_none());
            assert!(ann.get("source_grpid").is_none());
            assert!(ann.get("value").is_none());
            assert!(ann.get("controller_id").is_none());
        }

        /// Type=26 conjured — observed in `session_2026-04-29_2128`.
        /// `details` carries `DesignationType=26`, `source_grpid=94195`,
        /// `conjuration_type="conjured"`.
        #[test]
        fn test_designation_type_26_conjured() {
            let body = format!(
                "[UnityCrossThreadLogger]greToClientEvent\n{}",
                serde_json::json!({
                    "greToClientEvent": {
                        "greToClientMessages": [{
                            "type": "GREMessageType_GameStateMessage",
                            "msgId": 43,
                            "gameStateId": 103,
                            "gameStateMessage": {
                                "persistentAnnotations": [{
                                    "id": 600,
                                    "affectorId": 335,
                                    "affectedIds": [351],
                                    "type": ["AnnotationType_Designation"],
                                    "details": [
                                        { "key": "DesignationType", "type": "KeyValuePairValueType_int32", "valueInt32": [26] },
                                        { "key": "source_grpid", "type": "KeyValuePairValueType_int32", "valueInt32": [94195] },
                                        { "key": "conjuration_type", "type": "KeyValuePairValueType_string", "valueString": ["conjured"] }
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

            let ann = &payload["persistent_annotations"][0];
            assert_eq!(ann["type"], "AnnotationType_Designation");
            assert_eq!(ann["instance_id"], 351);
            assert_eq!(ann["designation_type"], 26);
            assert_eq!(ann["source_grpid"], 94195);
            assert_eq!(ann["conjuration_type"], "conjured");
            // type=26 has no grpid or value.
            assert!(ann.get("grpid").is_none());
            assert!(ann.get("value").is_none());
            assert!(ann.get("controller_id").is_none());
        }

        /// Type=26 drafted — observed in `session_2026-04-29_2201`.
        /// Same shape as `conjured` but with `conjuration_type="drafted"`
        /// and a different `source_grpid`.
        #[test]
        fn test_designation_type_26_drafted() {
            let body = format!(
                "[UnityCrossThreadLogger]greToClientEvent\n{}",
                serde_json::json!({
                    "greToClientEvent": {
                        "greToClientMessages": [{
                            "type": "GREMessageType_GameStateMessage",
                            "msgId": 44,
                            "gameStateId": 104,
                            "gameStateMessage": {
                                "persistentAnnotations": [{
                                    "id": 700,
                                    "affectorId": 335,
                                    "affectedIds": [400],
                                    "type": ["AnnotationType_Designation"],
                                    "details": [
                                        { "key": "DesignationType", "type": "KeyValuePairValueType_int32", "valueInt32": [26] },
                                        { "key": "source_grpid", "type": "KeyValuePairValueType_int32", "valueInt32": [98812] },
                                        { "key": "conjuration_type", "type": "KeyValuePairValueType_string", "valueString": ["drafted"] }
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

            let ann = &payload["persistent_annotations"][0];
            assert_eq!(ann["type"], "AnnotationType_Designation");
            assert_eq!(ann["instance_id"], 400);
            assert_eq!(ann["designation_type"], 26);
            assert_eq!(ann["source_grpid"], 98812);
            assert_eq!(ann["conjuration_type"], "drafted");
        }

        /// Type=12 persistent variant — observed in `session_2026-04-29_2128`.
        /// `details` carries `DesignationType=12` plus `value` (int32; the
        /// same persistent annotation `id` reappears with an updated `value`
        /// across successive diffs). Doubles as the persistent-annotation
        /// regression test.
        #[test]
        fn test_designation_type_12_persistent_value() {
            let body = format!(
                "[UnityCrossThreadLogger]greToClientEvent\n{}",
                serde_json::json!({
                    "greToClientEvent": {
                        "greToClientMessages": [{
                            "type": "GREMessageType_GameStateMessage",
                            "msgId": 45,
                            "gameStateId": 105,
                            "gameStateMessage": {
                                "persistentAnnotations": [{
                                    "id": 472,
                                    "affectorId": 1,
                                    "affectedIds": [160],
                                    "type": ["AnnotationType_Designation"],
                                    "details": [
                                        { "key": "DesignationType", "type": "KeyValuePairValueType_int32", "valueInt32": [12] },
                                        { "key": "value", "type": "KeyValuePairValueType_int32", "valueInt32": [1] }
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

            let ann = &payload["persistent_annotations"][0];
            assert_eq!(ann["type"], "AnnotationType_Designation");
            assert_eq!(ann["instance_id"], 160);
            assert_eq!(ann["designation_type"], 12);
            assert_eq!(ann["value"], 1);
            // type=12 has no grpid, source_grpid, conjuration_type, or controller_id.
            assert!(ann.get("grpid").is_none());
            assert!(ann.get("source_grpid").is_none());
            assert!(ann.get("conjuration_type").is_none());
            assert!(ann.get("controller_id").is_none());
        }

        /// Type=23 unknown variant pass-through — observed across multiple
        /// sessions (e.g. `session_2026-02-23_0000_standard-bo1`).
        /// `details` carries `DesignationType=23` and `ControllerId` (seat ID).
        ///
        /// Locks in the regression-proof permissive contract: the extractor
        /// must emit `instance_id`, `designation_type`, and `controller_id`
        /// without erroring on the undocumented `DesignationType` value, and
        /// without branching on the value.
        #[test]
        fn test_designation_type_23_controller_pass_through() {
            let body = format!(
                "[UnityCrossThreadLogger]greToClientEvent\n{}",
                serde_json::json!({
                    "greToClientEvent": {
                        "greToClientMessages": [{
                            "type": "GREMessageType_GameStateMessage",
                            "msgId": 46,
                            "gameStateId": 106,
                            "gameStateMessage": {
                                "persistentAnnotations": [{
                                    "id": 281,
                                    "affectorId": 292,
                                    "affectedIds": [292],
                                    "type": ["AnnotationType_Designation"],
                                    "details": [
                                        { "key": "DesignationType", "type": "KeyValuePairValueType_int32", "valueInt32": [23] },
                                        { "key": "ControllerId", "type": "KeyValuePairValueType_int32", "valueInt32": [2] }
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

            let ann = &payload["persistent_annotations"][0];
            assert_eq!(ann["type"], "AnnotationType_Designation");
            assert_eq!(ann["instance_id"], 292);
            assert_eq!(ann["designation_type"], 23);
            assert_eq!(ann["controller_id"], 2);
            // type=23 has no grpid/source_grpid/conjuration_type/value.
            assert!(ann.get("grpid").is_none());
            assert!(ann.get("source_grpid").is_none());
            assert!(ann.get("conjuration_type").is_none());
            assert!(ann.get("value").is_none());
        }
    }

    mod persistent_annotations_extraction {
        use super::*;

        #[test]
        fn test_persistent_annotations_present_is_array() {
            let body = format!(
                "[UnityCrossThreadLogger]greToClientEvent\n{}",
                serde_json::json!({
                    "greToClientEvent": {
                        "greToClientMessages": [{
                            "type": "GREMessageType_GameStateMessage",
                            "msgId": 32,
                            "gameStateId": 97,
                            "gameStateMessage": {
                                "persistentAnnotations": [
                                    {
                                        "id": 5,
                                        "affectorId": 28,
                                        "affectedIds": [283],
                                        "type": ["AnnotationType_EnteredZoneThisTurn"]
                                    },
                                    {
                                        "id": 140,
                                        "affectorId": 294,
                                        "affectedIds": [286],
                                        "type": ["AnnotationType_TargetSpec"],
                                        "details": [
                                            { "key": "abilityGrpId", "type": "KeyValuePairValueType_int32", "valueInt32": [174_395] },
                                            { "key": "index", "type": "KeyValuePairValueType_int32", "valueInt32": [1] }
                                        ]
                                    }
                                ]
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

            assert!(payload["persistent_annotations"].is_array());
            let pa = payload["persistent_annotations"]
                .as_array()
                .unwrap_or_else(|| unreachable!());
            assert_eq!(pa.len(), 2);
        }

        #[test]
        fn test_persistent_target_spec_has_structured_fields() {
            let body = format!(
                "[UnityCrossThreadLogger]greToClientEvent\n{}",
                serde_json::json!({
                    "greToClientEvent": {
                        "greToClientMessages": [{
                            "type": "GREMessageType_GameStateMessage",
                            "msgId": 33,
                            "gameStateId": 98,
                            "gameStateMessage": {
                                "persistentAnnotations": [{
                                    "id": 140,
                                    "affectorId": 294,
                                    "affectedIds": [286],
                                    "type": ["AnnotationType_TargetSpec"],
                                    "details": [
                                        { "key": "abilityGrpId", "type": "KeyValuePairValueType_int32", "valueInt32": [174_395] },
                                        { "key": "index", "type": "KeyValuePairValueType_int32", "valueInt32": [1] }
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

            let ann = &payload["persistent_annotations"][0];
            assert_eq!(ann["type"], "AnnotationType_TargetSpec");
            assert_eq!(ann["ability_grp_id"], 174_395);
            assert_eq!(ann["target_index"], 1);
        }

        #[test]
        fn test_missing_persistent_annotations_returns_empty_array() {
            let body = game_state_message_body();
            let entry = unity_entry(&body);
            let event = try_parse(&entry, Some(test_timestamp()))
                .into_iter()
                .next()
                .unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(&event);

            let pa = payload["persistent_annotations"]
                .as_array()
                .unwrap_or_else(|| unreachable!());
            assert!(pa.is_empty());
        }
    }

    mod detail_helpers {
        use super::super::{detail_int, detail_int_array, detail_str};

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

        #[test]
        fn test_detail_int_array_found() {
            let details = vec![serde_json::json!({
                "key": "topIds",
                "type": "KeyValuePairValueType_int32",
                "valueInt32": [230, 231, 232]
            })];
            assert_eq!(detail_int_array(&details, "topIds"), vec![230, 231, 232]);
        }

        #[test]
        fn test_detail_int_array_missing_key() {
            let details = vec![serde_json::json!({
                "key": "topIds",
                "type": "KeyValuePairValueType_int32",
                "valueInt32": [230]
            })];
            assert!(detail_int_array(&details, "bottomIds").is_empty());
        }

        #[test]
        fn test_detail_int_array_empty_value() {
            let details = vec![serde_json::json!({
                "key": "bottomIds"
            })];
            assert!(detail_int_array(&details, "bottomIds").is_empty());
        }

        #[test]
        fn test_detail_int_array_single_element() {
            let details = vec![serde_json::json!({
                "key": "topIds",
                "type": "KeyValuePairValueType_int32",
                "valueInt32": [42]
            })];
            assert_eq!(detail_int_array(&details, "topIds"), vec![42]);
        }
    }
}
