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

/// Extracts annotations from the `gameStateMessage.annotations` array.
///
/// Each annotation has at minimum `id`, `affectorId`, `affectedIds`, and
/// `type`. Special handling normalizes type-specific data:
///
/// - **`AnnotationType_ZoneTransfer`**: extracts `zone_src`, `zone_dest`,
///   `category` from the `details` key-value pairs.
/// - **`AnnotationType_ObjectIdChanged`**: extracts `old_id` / `new_id`
///   from the `details` key-value pairs.
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

/// Reads the annotation `type` field, which is an array of strings.
/// Returns the first type string found, or an empty string when absent.
fn read_annotation_type(annotation: &serde_json::Value) -> &str {
    annotation
        .get("type")
        .and_then(serde_json::Value::as_array)
        .and_then(|arr| arr.first())
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
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

    let details = annotation
        .get("details")
        .and_then(serde_json::Value::as_array);

    let mut result = serde_json::json!({
        "id": id,
        "affector_id": affector_id,
        "affected_ids": affected_ids,
        "type": annotation_type,
    });

    // Add type-specific data from `details`.
    if let Some(d) = details {
        match annotation_type {
            ANNOTATION_TYPE_ZONE_TRANSFER => {
                let zone_src = detail_int(d, "zone_src").unwrap_or(0);
                let zone_dest = detail_int(d, "zone_dest").unwrap_or(0);
                let category = detail_str(d, "category").unwrap_or("");

                if let Some(obj) = result.as_object_mut() {
                    obj.insert("zone_src".to_string(), serde_json::json!(zone_src));
                    obj.insert("zone_dest".to_string(), serde_json::json!(zone_dest));
                    obj.insert("category".to_string(), serde_json::json!(category));
                }
            }
            ANNOTATION_TYPE_OBJECT_ID_CHANGED => {
                let old_id = detail_int(d, "orig_id").unwrap_or(0);
                let new_id = detail_int(d, "new_id").unwrap_or(0);

                if let Some(obj) = result.as_object_mut() {
                    obj.insert("old_id".to_string(), serde_json::json!(old_id));
                    obj.insert("new_id".to_string(), serde_json::json!(new_id));
                }
            }
            _ => {}
        }
    }

    Some(result)
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
            let event = try_parse(&entry, Some(test_timestamp())).unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(&event);

            assert!(payload["annotations"].is_array());
        }

        #[test]
        fn test_annotations_count_three() {
            let body = game_state_message_with_annotations_body();
            let entry = unity_entry(&body);
            let event = try_parse(&entry, Some(test_timestamp())).unwrap_or_else(|| unreachable!());
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
            let event = try_parse(&entry, Some(test_timestamp())).unwrap_or_else(|| unreachable!());
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
            let event = try_parse(&entry, Some(test_timestamp())).unwrap_or_else(|| unreachable!());
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
            let event = try_parse(&entry, Some(test_timestamp())).unwrap_or_else(|| unreachable!());
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
            let event = try_parse(&entry, Some(test_timestamp())).unwrap_or_else(|| unreachable!());
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
            let event = try_parse(&entry, Some(test_timestamp())).unwrap_or_else(|| unreachable!());
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
            let event = try_parse(&entry, Some(test_timestamp())).unwrap_or_else(|| unreachable!());
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
            let event = try_parse(&entry, Some(test_timestamp())).unwrap_or_else(|| unreachable!());
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
            let event = try_parse(&entry, Some(test_timestamp())).unwrap_or_else(|| unreachable!());
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
            let event = try_parse(&entry, Some(test_timestamp())).unwrap_or_else(|| unreachable!());
            let payload = game_state_payload(&event);

            let ann = &payload["annotations"][0];
            assert_eq!(ann["type"], "AnnotationType_ModifiedToughness");
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
