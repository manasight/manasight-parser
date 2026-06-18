//! Deck-submission parser: `EventSetDeckV2`, `EventSetDeckV3`, and future `Vn` variants.
//!
//! Recognises the `==>` **request** for the `EventSetDeck` family using a bare
//! prefix check (`body.contains("==> EventSetDeck")`), which covers V2, V3, and
//! any future version without panicking on an unrecognised variant.
//!
//! The `<==` response is just an ack and is intentionally **not** parsed — each
//! submission emits at most one `GameEvent::DeckSubmission` from the request line.
//!
//! # Real log format
//!
//! ```text
//! [UnityCrossThreadLogger]==> EventSetDeckV2 {"id":"<uuid>","request":"{\"EventName\":\"Constructed_BestOf3\",\"Summary\":{\"DeckId\":\"<deck-uuid>\",\"Attributes\":[...,{\"name\":\"Format\",\"value\":\"TraditionalStandard\"}],...},\"Deck\":{\"MainDeck\":[...],...,\"CommandZone\":[],...}}"}
//! ```
//!
//! The outer JSON has an `"id"` field and a string-escaped `"request"` field.
//! The `request` object carries:
//! - `EventName` — the queue/event string (e.g. `Ladder`, `Constructed_BestOf3`)
//! - `Summary.DeckId` — the deck's UUID
//! - `Summary.Attributes[]` — an array of `{name, value}` pairs; `Format` is one entry
//! - `Deck.CommandZone` — non-empty for Brawl/Commander (singleton) decks
//!
//! # Extraction contract
//!
//! | Payload field | Source path |
//! |---|---|
//! | `deck_id` | `request.Summary.DeckId` via [`api_common::extract_deck_id`] |
//! | `deck_format` | `value` of `request.Summary.Attributes[name == "Format"]` |
//! | `event_name` | [`api_common::extract_event_name`] |
//! | `is_singleton` | `request.Deck.CommandZone` is a non-empty array |

use crate::events::{DeckSubmissionEvent, EventMetadata, GameEvent};
use crate::log::entry::LogEntry;
use crate::parsers::api_common;

/// Bare prefix that matches the entire `EventSetDeck` family.
///
/// Using a bare prefix rather than `api_common::is_api_request("EventSetDeck")`
/// because that helper appends a trailing space and builds the marker
/// `"==> EventSetDeck "`, which would match nothing — real log lines read
/// `==> EventSetDeckV2 ` / `==> EventSetDeckV3 ` (no bare `EventSetDeck `).
const FAMILY_PREFIX: &str = "==> EventSetDeck";

/// Attempts to parse a [`LogEntry`] as a deck-submission event.
///
/// Returns `Some(GameEvent::DeckSubmission(_))` when the entry contains an
/// `EventSetDeck` request (`==> EventSetDeckV2`, `==> EventSetDeckV3`, or any
/// future `Vn`), or `None` for all other entries including `<==` responses.
///
/// The `timestamp` is `None` when the log-entry header did not contain a
/// parseable timestamp. It is passed through to [`EventMetadata`] so
/// downstream consumers can distinguish real vs missing timestamps.
pub fn try_parse(
    entry: &LogEntry,
    timestamp: Option<chrono::DateTime<chrono::Utc>>,
) -> Option<GameEvent> {
    let body = &entry.body;
    let payload = try_parse_request(body)?;
    let metadata = EventMetadata::new(timestamp, body.as_bytes().to_vec());
    Some(GameEvent::DeckSubmission(DeckSubmissionEvent::new(
        metadata, payload,
    )))
}

/// Extracts the deck-submission payload from an `EventSetDeck` request body.
///
/// Returns `None` when the body does not contain the `EventSetDeck` prefix or
/// when the JSON cannot be parsed.
fn try_parse_request(body: &str) -> Option<serde_json::Value> {
    if !body.contains(FAMILY_PREFIX) {
        return None;
    }

    let parsed = api_common::parse_json_from_body(body, "EventSetDeck request")?;

    // The `request` field is a double-encoded JSON string.
    let request =
        api_common::parse_nested_json(&parsed, "request", Some("EventSetDeck request.request"))?;

    let event_name = api_common::extract_event_name(&parsed);

    let summary = request.get("Summary");
    let deck = request.get("Deck");

    let deck_id = summary.and_then(api_common::extract_deck_id);
    let deck_format = summary.and_then(extract_format_attribute);
    let is_singleton = deck
        .and_then(|d| d.get("CommandZone"))
        .and_then(|cz| cz.as_array().map(|arr| !arr.is_empty()));

    Some(serde_json::json!({
        "type": "deck_submission",
        "deck_id": deck_id,
        "deck_format": deck_format,
        "event_name": event_name,
        "is_singleton": is_singleton.unwrap_or(false),
    }))
}

/// Finds the `"Format"` entry in a `Summary.Attributes` array and returns its value.
///
/// The `Attributes` array contains objects of the form `{"name": "...", "value": "..."}`.
/// Returns `None` when the `Attributes` field is absent or no entry has `name == "Format"`.
fn extract_format_attribute(summary: &serde_json::Value) -> Option<String> {
    summary
        .get("Attributes")?
        .as_array()?
        .iter()
        .find(|attr| attr.get("name").and_then(serde_json::Value::as_str) == Some("Format"))
        .and_then(|attr| attr.get("value"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parsers::test_helpers::{deck_submission_payload, test_timestamp, unity_entry};

    /// Builds a minimal double-encoded request JSON string suitable for use in
    /// the outer `{"id": ..., "request": "..."}` payload.
    fn make_request_body(
        event_name: &str,
        deck_id: &str,
        format: Option<&str>,
        command_zone: &[u32],
    ) -> String {
        let attributes = match format {
            Some(fmt) => serde_json::json!([{"name":"Format","value":fmt}]),
            None => serde_json::json!([]),
        };
        let command_zone_json = serde_json::json!(command_zone);
        let inner = serde_json::json!({
            "EventName": event_name,
            "Summary": {
                "DeckId": deck_id,
                "Attributes": attributes
            },
            "Deck": {
                "MainDeck": [],
                "Sideboard": [],
                "CommandZone": command_zone_json,
                "Companions": []
            }
        });
        let inner_str = inner.to_string();
        let outer = serde_json::json!({"id": "test-uuid", "request": inner_str});
        outer.to_string()
    }

    fn make_v2_body(
        event_name: &str,
        deck_id: &str,
        format: Option<&str>,
        command_zone: &[u32],
    ) -> String {
        format!(
            "[UnityCrossThreadLogger]4/12/2026 8:44:00 AM ==> EventSetDeckV2 {}",
            make_request_body(event_name, deck_id, format, command_zone)
        )
    }

    fn make_v3_body(
        event_name: &str,
        deck_id: &str,
        format: Option<&str>,
        command_zone: &[u32],
    ) -> String {
        format!(
            "[UnityCrossThreadLogger]4/12/2026 8:44:00 AM ==> EventSetDeckV3 {}",
            make_request_body(event_name, deck_id, format, command_zone)
        )
    }

    // -- V2 request parsing ---------------------------------------------------

    mod v2_parsing {
        use super::*;

        #[test]
        fn test_try_parse_v2_request_emits_deck_submission() {
            let body = make_v2_body(
                "Constructed_BestOf3",
                "deck-uuid-1",
                Some("TraditionalStandard"),
                &[],
            );
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            assert!(matches!(event, GameEvent::DeckSubmission(_)));
        }

        #[test]
        fn test_try_parse_v2_extracts_deck_id() {
            let body = make_v2_body("Ladder", "abc-deck-id", Some("Standard"), &[]);
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));

            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = deck_submission_payload(event);
            assert_eq!(payload["deck_id"], "abc-deck-id");
        }

        #[test]
        fn test_try_parse_v2_extracts_deck_format() {
            let body = make_v2_body(
                "Constructed_BestOf3",
                "deck-1",
                Some("TraditionalStandard"),
                &[],
            );
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));

            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = deck_submission_payload(event);
            assert_eq!(payload["deck_format"], "TraditionalStandard");
        }

        #[test]
        fn test_try_parse_v2_extracts_event_name() {
            let body = make_v2_body(
                "Constructed_BestOf3",
                "deck-1",
                Some("TraditionalStandard"),
                &[],
            );
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));

            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = deck_submission_payload(event);
            assert_eq!(payload["event_name"], "Constructed_BestOf3");
        }

        #[test]
        fn test_try_parse_v2_payload_type_is_deck_submission() {
            let body = make_v2_body("Ladder", "d1", Some("Standard"), &[]);
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));

            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = deck_submission_payload(event);
            assert_eq!(payload["type"], "deck_submission");
        }
    }

    // -- V3 request parsing ---------------------------------------------------

    mod v3_parsing {
        use super::*;

        #[test]
        fn test_try_parse_v3_request_emits_deck_submission() {
            let body = make_v3_body("Ladder", "v3-deck", Some("Standard"), &[]);
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            assert!(matches!(
                result.as_ref().unwrap_or_else(|| unreachable!()),
                GameEvent::DeckSubmission(_)
            ));
        }

        #[test]
        fn test_try_parse_v3_extracts_format() {
            let body = make_v3_body("Play", "v3-deck-2", Some("Alchemy"), &[]);
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));

            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = deck_submission_payload(event);
            assert_eq!(payload["deck_format"], "Alchemy");
        }
    }

    // -- Response is NOT claimed ----------------------------------------------

    mod response_not_claimed {
        use super::*;

        #[test]
        fn test_try_parse_response_arrow_returns_none() {
            let body = "[UnityCrossThreadLogger]4/12/2026 8:44:00 AM\n\
                         <== EventSetDeckV2(some-uuid)\n\
                         {\"result\":\"ok\"}";
            let entry = unity_entry(body);
            assert!(try_parse(&entry, Some(test_timestamp())).is_none());
        }
    }

    // -- Unknown version degrades gracefully ----------------------------------

    mod unknown_version {
        use super::*;

        #[test]
        fn test_try_parse_v99_does_not_panic() {
            let body = format!(
                "[UnityCrossThreadLogger]4/12/2026 8:44:00 AM ==> EventSetDeckV99 {}",
                make_request_body("Ladder", "future-deck", Some("Standard"), &[])
            );
            let entry = unity_entry(&body);
            // Must not panic; may return Some or None — both are acceptable.
            let _ = try_parse(&entry, Some(test_timestamp()));
        }

        #[test]
        fn test_try_parse_v99_with_valid_payload_returns_event() {
            let body = format!(
                "[UnityCrossThreadLogger]4/12/2026 8:44:00 AM ==> EventSetDeckV99 {}",
                make_request_body("Ladder", "future-deck", Some("Standard"), &[])
            );
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));
            // The bare-prefix check matches V99, so it should parse.
            assert!(result.is_some());
        }
    }

    // -- is_singleton ---------------------------------------------------------

    mod singleton {
        use super::*;

        #[test]
        fn test_try_parse_is_singleton_false_for_empty_command_zone() {
            let body = make_v2_body("Ladder", "deck-1", Some("Standard"), &[]);
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));

            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = deck_submission_payload(event);
            assert_eq!(payload["is_singleton"], false);
        }

        #[test]
        fn test_try_parse_is_singleton_true_for_nonempty_command_zone() {
            let body = make_v2_body("Brawl", "brawl-deck", Some("Brawl"), &[12345]);
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));

            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = deck_submission_payload(event);
            assert_eq!(payload["is_singleton"], true);
        }

        #[test]
        fn test_try_parse_is_singleton_true_for_multiple_command_zone_cards() {
            let body = make_v2_body("Brawl_Best3", "partner-brawl", Some("Brawl"), &[111, 222]);
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));

            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = deck_submission_payload(event);
            assert_eq!(payload["is_singleton"], true);
        }
    }

    // -- Format attribute extraction ------------------------------------------

    mod format_attribute {
        use super::*;

        #[test]
        fn test_extract_format_attribute_finds_format_among_multiple_attributes() {
            let body = format!("[UnityCrossThreadLogger]==> EventSetDeckV2 {}", {
                // Build a request with multiple attributes including Format
                let inner = serde_json::json!({
                    "EventName": "Ladder",
                    "Summary": {
                        "DeckId": "d1",
                        "Attributes": [
                            {"name": "Version", "value": "5"},
                            {"name": "TileID", "value": "12345"},
                            {"name": "Format", "value": "Pioneer"},
                            {"name": "IsFavorite", "value": "false"}
                        ]
                    },
                    "Deck": {"MainDeck": [], "CommandZone": [], "Sideboard": [], "Companions": []}
                });
                let outer = serde_json::json!({"id": "u", "request": inner.to_string()});
                outer.to_string()
            });
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));

            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = deck_submission_payload(event);
            assert_eq!(payload["deck_format"], "Pioneer");
        }

        #[test]
        fn test_try_parse_missing_format_attribute_returns_null_deck_format() {
            let body = make_v2_body("Ladder", "deck-no-fmt", None, &[]);
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));

            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = deck_submission_payload(event);
            // deck_format should be JSON null when absent
            assert!(payload["deck_format"].is_null());
        }
    }

    // -- Metadata preservation ------------------------------------------------

    mod metadata {
        use super::*;

        #[test]
        fn test_try_parse_preserves_raw_bytes() {
            let body = make_v2_body("Ladder", "raw-d", Some("Standard"), &[]);
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));

            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            assert_eq!(event.metadata().raw_bytes(), body.as_bytes());
        }

        #[test]
        fn test_try_parse_stores_timestamp() {
            let body = make_v2_body("Ladder", "ts-d", Some("Standard"), &[]);
            let entry = unity_entry(&body);
            let ts = Some(test_timestamp());
            let result = try_parse(&entry, ts);

            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            assert_eq!(event.metadata().timestamp(), ts);
        }
    }

    // -- Non-matching entries -------------------------------------------------

    mod non_matching {
        use super::*;

        #[test]
        fn test_try_parse_unrelated_entry_returns_none() {
            let body = "[UnityCrossThreadLogger]greToClientEvent\n{\"data\": 1}";
            let entry = unity_entry(body);
            assert!(try_parse(&entry, Some(test_timestamp())).is_none());
        }

        #[test]
        fn test_try_parse_event_join_returns_none() {
            let body = r#"[UnityCrossThreadLogger]==> EventJoin {"id":"x","request":"{\"EventName\":\"Test\"}"}"#;
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
        fn test_try_parse_malformed_json_returns_none() {
            let body = "[UnityCrossThreadLogger]==> EventSetDeckV2 {broken json!!!}";
            let entry = unity_entry(body);
            assert!(try_parse(&entry, Some(test_timestamp())).is_none());
        }
    }
}
