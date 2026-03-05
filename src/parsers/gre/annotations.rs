//! Annotations extraction from `gameStateMessage.annotations`.

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
///   `category` from `ZoneTransferData[0]`.
/// - **`AnnotationType_ObjectIdChanged`**: extracts `old_id` / `new_id`
///   from `ObjectIdChangedData[0]`.
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

    let annotation_type = annotation
        .get("type")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");

    let mut result = serde_json::json!({
        "id": id,
        "affector_id": affector_id,
        "affected_ids": affected_ids,
        "type": annotation_type,
    });

    // Add type-specific data.
    match annotation_type {
        ANNOTATION_TYPE_ZONE_TRANSFER => {
            if let Some(ztd) = annotation
                .get("ZoneTransferData")
                .and_then(serde_json::Value::as_array)
                .and_then(|arr| arr.first())
            {
                // Arena logs may use either snake_case or camelCase for
                // ZoneTransferData fields depending on the client version.
                let zone_src = ztd
                    .get("zone_src")
                    .or_else(|| ztd.get("zoneSrc"))
                    .and_then(serde_json::Value::as_i64)
                    .unwrap_or(0);
                let zone_dest = ztd
                    .get("zone_dest")
                    .or_else(|| ztd.get("zoneDest"))
                    .and_then(serde_json::Value::as_i64)
                    .unwrap_or(0);
                let category = ztd
                    .get("category")
                    .or_else(|| ztd.get("Category"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("");

                if let Some(obj) = result.as_object_mut() {
                    obj.insert("zone_src".to_string(), serde_json::json!(zone_src));
                    obj.insert("zone_dest".to_string(), serde_json::json!(zone_dest));
                    obj.insert("category".to_string(), serde_json::json!(category));
                }
            }
        }
        ANNOTATION_TYPE_OBJECT_ID_CHANGED => {
            if let Some(oid) = annotation
                .get("ObjectIdChangedData")
                .and_then(serde_json::Value::as_array)
                .and_then(|arr| arr.first())
            {
                let old_id = oid
                    .get("oldId")
                    .and_then(serde_json::Value::as_i64)
                    .unwrap_or(0);
                let new_id = oid
                    .get("newId")
                    .and_then(serde_json::Value::as_i64)
                    .unwrap_or(0);

                if let Some(obj) = result.as_object_mut() {
                    obj.insert("old_id".to_string(), serde_json::json!(old_id));
                    obj.insert("new_id".to_string(), serde_json::json!(new_id));
                }
            }
        }
        _ => {}
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

    /// Helper: build a `GameStateMessage` body with annotations.
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
                                    "type": "AnnotationType_ZoneTransfer",
                                    "ZoneTransferData": [{
                                        "zone_src": 29,
                                        "zone_dest": 31,
                                        "category": "Draw"
                                    }]
                                },
                                {
                                    "id": 146,
                                    "affectorId": 300,
                                    "affectedIds": [410, 411],
                                    "type": "AnnotationType_ObjectIdChanged",
                                    "ObjectIdChangedData": [{
                                        "oldId": 410,
                                        "newId": 500
                                    }]
                                },
                                {
                                    "id": 147,
                                    "affectorId": 305,
                                    "affectedIds": [412],
                                    "type": "AnnotationType_ResolutionStart"
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
                                    "type": "AnnotationType_ZoneTransfer",
                                    "ZoneTransferData": [{
                                        "zone_src": 30,
                                        "zone_dest": 34,
                                        "category": "CastSpell"
                                    }]
                                }
                            ]
                        }
                    }]
                }
            })
        )
    }

    /// Helper: build a `GameStateMessage` body with a `ZoneTransfer` annotation
    /// that has no `ZoneTransferData` array (edge case).
    fn game_state_message_with_missing_ztd_body() -> String {
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
                                    "type": "AnnotationType_ZoneTransfer"
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
        fn test_zone_transfer_missing_ztd_still_has_base_fields() {
            let body = game_state_message_with_missing_ztd_body();
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
            // No zone_src/zone_dest/category when ZoneTransferData is missing.
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
        fn test_zone_transfer_camel_case_field_names() {
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
                                    "type": "AnnotationType_ZoneTransfer",
                                    "ZoneTransferData": [{
                                        "zoneSrc": 29,
                                        "zoneDest": 31,
                                        "category": "Draw"
                                    }]
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
            assert_eq!(ann["zone_src"], 29);
            assert_eq!(ann["zone_dest"], 31);
            assert_eq!(ann["category"], "Draw");
        }
    }
}
