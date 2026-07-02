//! Course-deck parser: `<== EventGetCoursesV2` responses.
//!
//! Recognises the `<==` **response** for `EventGetCoursesV2` using an exact
//! version match (`api_common::is_api_response(body, "EventGetCoursesV2")`),
//! deliberately **not** a bare family-prefix match like
//! [`crate::parsers::deck_submission`]'s `EventSetDeck` family. Only `V2`
//! exists in the observed corpus; a future `V3` may change the response
//! shape, and an exact match lets the smoke-test ratchet surface it as a new
//! unknown-entry residual rather than silently mis-parsing it.
//!
//! # Real log format
//!
//! ```text
//! [UnityCrossThreadLogger]6/17/2026 5:08:00 PM
//! <== EventGetCoursesV2(<uuid>)
//! {"Courses":[{"CourseId":"...","InternalEventName":"...","CourseDeckSummary":{"DeckId":"...","Name":"...","Attributes":[...]},"CourseDeck":{"MainDeck":[...],...}},...]}
//! ```
//!
//! The response carries a `Courses` array. Each `Course` object may describe
//! an event that has no registered deck yet (mid-draft, pre-deckbuild
//! limited runs) — those courses have `CourseDeck: null` and are skipped.
//!
//! # Extraction contract
//!
//! One [`GameEvent::CourseDeck`] is emitted per `Course` whose
//! `CourseDeck.MainDeck` is a non-empty array — the same "does a registered
//! deck exist" test as the null-`CourseDeck` skip case, applied uniformly.
//!
//! | Payload field | Source path |
//! |---|---|
//! | `deck_id` | `Course.CourseDeckSummary.DeckId` via [`api_common::extract_deck_id`] |
//! | `name` | `Course.CourseDeckSummary.Name` |
//! | `format` | `value` of `Course.CourseDeckSummary.Attributes[name == "Format"]` via [`api_common::extract_format_attribute`] |
//! | `maindeck_hash` | `Course.CourseDeck.MainDeck` via [`api_common::maindeck_hash`] |
//! | `internal_event_name` | `Course.InternalEventName` |
//! | `course_id` | `Course.CourseId` |
//!
//! `CourseDeckSummary` carries only deck *metadata* (id, name, format);
//! `CourseDeck` carries only card *lists* (`MainDeck`, `Sideboard`,
//! `CommandZone`, `Companions`, `CardSkins`, and sometimes
//! `ReducedSideboard`) — the two objects are corpus-verified to never
//! overlap. All payload fields besides `type` are individually nullable.
//!
//! # Provenance rule
//!
//! Nothing extracted here feeds match-format resolution — these are deck
//! attributes (the deck's registered legality), not match-format signals.
//! The format model continues to trust [`crate::parsers::deck_submission`]
//! only.
//!
//! # Multi-event raw bytes
//!
//! Like [`crate::parsers::gre`], a single log entry can produce many
//! `CourseDeck` events (one per qualifying `Course` in the `Courses` array —
//! up to 23 observed). Each sibling event carries the **full entry**
//! `raw_bytes` (and therefore shares one `raw_bytes_hash`), matching the GRE
//! precedent for batched multi-event entries.

use crate::events::{CourseDeckEvent, EventMetadata, GameEvent};
use crate::log::entry::LogEntry;
use crate::parsers::api_common;

/// The exact `EventGetCoursesV2` method name, matched via
/// [`api_common::is_api_response`] (`<== EventGetCoursesV2(`).
const GET_COURSES_METHOD: &str = "EventGetCoursesV2";

/// Attempts to parse a [`LogEntry`] as zero or more course-deck events.
///
/// Returns one [`GameEvent::CourseDeck`] per `Course` in the
/// `EventGetCoursesV2` response's `Courses` array whose `CourseDeck.MainDeck`
/// is a non-empty array. Returns an empty `Vec` for all other entries,
/// including malformed JSON, a missing/empty `Courses` array, or a response
/// where every `Course` lacks a registered deck.
///
/// The `timestamp` is `None` when the log-entry header did not contain a
/// parseable timestamp. It is passed through to [`EventMetadata`] so
/// downstream consumers can distinguish real vs missing timestamps.
pub fn try_parse(
    entry: &LogEntry,
    timestamp: Option<chrono::DateTime<chrono::Utc>>,
) -> Vec<GameEvent> {
    let body = &entry.body;

    if !api_common::is_api_response(body, GET_COURSES_METHOD) {
        return Vec::new();
    }

    let Some(parsed) = api_common::parse_json_from_body(body, "EventGetCoursesV2 response") else {
        return Vec::new();
    };

    let Some(courses) = parsed.get("Courses").and_then(serde_json::Value::as_array) else {
        return Vec::new();
    };

    courses
        .iter()
        .filter_map(|course| build_course_event(course, body, timestamp))
        .collect()
}

/// Builds a single [`GameEvent::CourseDeck`] from a `Course` object.
///
/// Returns `None` when `CourseDeck.MainDeck` is absent, `null`, not an
/// array, or an empty array — the "no registered deck" case, whether or not
/// `CourseDeck` itself is `null`.
fn build_course_event(
    course: &serde_json::Value,
    body: &str,
    timestamp: Option<chrono::DateTime<chrono::Utc>>,
) -> Option<GameEvent> {
    let course_deck = course.get("CourseDeck");
    let maindeck = course_deck.and_then(|cd| cd.get("MainDeck"));

    // Require a non-empty MainDeck array — this is the single "does a
    // registered deck exist" test, applied whether CourseDeck is null or
    // present-but-empty.
    let has_maindeck = maindeck
        .and_then(serde_json::Value::as_array)
        .is_some_and(|arr| !arr.is_empty());
    if !has_maindeck {
        return None;
    }

    let summary = course.get("CourseDeckSummary");

    let deck_id = summary.and_then(api_common::extract_deck_id);
    let name = summary
        .and_then(|s| s.get("Name"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    let format = summary.and_then(api_common::extract_format_attribute);
    let maindeck_hash = maindeck.and_then(api_common::maindeck_hash);
    let internal_event_name = course
        .get("InternalEventName")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    let course_id = course
        .get("CourseId")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);

    let payload = serde_json::json!({
        "type": "course_deck",
        "deck_id": deck_id,
        "name": name,
        "format": format,
        "maindeck_hash": maindeck_hash,
        "internal_event_name": internal_event_name,
        "course_id": course_id,
    });

    // Full-entry raw_bytes per Course sibling, matching the GRE multi-event
    // precedent — sibling events share one raw_bytes_hash.
    let metadata = EventMetadata::new(timestamp, body.as_bytes().to_vec());
    Some(GameEvent::CourseDeck(CourseDeckEvent::new(
        metadata, payload,
    )))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(deprecated)]
mod tests {
    use super::*;
    use crate::parsers::test_helpers::{course_deck_payload, test_timestamp, unity_entry};

    /// Builds a single `Course` JSON object with the given qualifying deck
    /// fields. `maindeck` entries are `(cardId, quantity)` pairs.
    fn make_course(
        course_id: &str,
        internal_event_name: &str,
        deck_id: &str,
        name: &str,
        format: Option<&str>,
        maindeck: &[(u64, u64)],
        command_zone: &[u64],
    ) -> serde_json::Value {
        let attributes = match format {
            Some(fmt) => serde_json::json!([{"name": "Format", "value": fmt}]),
            None => serde_json::json!([]),
        };
        let maindeck_json: Vec<serde_json::Value> = maindeck
            .iter()
            .map(|(card_id, quantity)| serde_json::json!({"cardId": card_id, "quantity": quantity}))
            .collect();
        let command_zone_json: Vec<serde_json::Value> = command_zone
            .iter()
            .map(|card_id| serde_json::json!({"cardId": card_id, "quantity": 1}))
            .collect();
        serde_json::json!({
            "CourseId": course_id,
            "InternalEventName": internal_event_name,
            "CourseDeckSummary": {
                "DeckId": deck_id,
                "Name": name,
                "Attributes": attributes
            },
            "CourseDeck": {
                "MainDeck": maindeck_json,
                "Sideboard": [],
                "CommandZone": command_zone_json,
                "Companions": [],
                "CardSkins": []
            }
        })
    }

    /// Builds a `Course` object with `CourseDeck: null` (no registered deck
    /// yet — mid-draft/pre-deckbuild limited runs).
    fn make_null_course_deck(course_id: &str, internal_event_name: &str) -> serde_json::Value {
        serde_json::json!({
            "CourseId": course_id,
            "InternalEventName": internal_event_name,
            "CourseDeckSummary": {
                "Attributes": []
            },
            "CourseDeck": null
        })
    }

    /// Builds a `Course` object whose `CourseDeck.MainDeck` is present but
    /// empty (a registered but empty deck).
    fn make_empty_maindeck_course(course_id: &str, internal_event_name: &str) -> serde_json::Value {
        serde_json::json!({
            "CourseId": course_id,
            "InternalEventName": internal_event_name,
            "CourseDeckSummary": {
                "DeckId": "deck-empty",
                "Name": "Empty Deck",
                "Attributes": []
            },
            "CourseDeck": {
                "MainDeck": [],
                "Sideboard": [],
                "CommandZone": [],
                "Companions": [],
                "CardSkins": []
            }
        })
    }

    fn make_response_body(courses: &[serde_json::Value]) -> String {
        let payload = serde_json::json!({ "Courses": courses });
        format!(
            "[UnityCrossThreadLogger]6/17/2026 5:08:00 PM\n<== EventGetCoursesV2(00000000-0000-0000-0000-00000000c000)\n{payload}"
        )
    }

    // -- Claiming ---------------------------------------------------------------

    mod claiming {
        use super::*;

        #[test]
        fn test_try_parse_claims_response_with_qualifying_course() {
            let course = make_course(
                "course-1",
                "Constructed_BestOf3",
                "deck-1",
                "Test Deck",
                Some("TraditionalStandard"),
                &[(1, 4)],
                &[],
            );
            let body = make_response_body(&[course]);
            let entry = unity_entry(&body);
            let events = try_parse(&entry, Some(test_timestamp()));
            assert_eq!(events.len(), 1);
            assert!(matches!(events[0], GameEvent::CourseDeck(_)));
        }

        #[test]
        fn test_try_parse_ignores_request_line() {
            let body =
                "[UnityCrossThreadLogger]6/17/2026 5:08:00 PM ==> EventGetCoursesV2 {\"id\":\"x\"}";
            let entry = unity_entry(body);
            assert!(try_parse(&entry, Some(test_timestamp())).is_empty());
        }

        #[test]
        fn test_try_parse_ignores_other_response() {
            let body = "[UnityCrossThreadLogger]6/17/2026 5:08:00 PM\n<== RankGetCombinedRankInfo(uuid)\n{}";
            let entry = unity_entry(body);
            assert!(try_parse(&entry, Some(test_timestamp())).is_empty());
        }

        #[test]
        fn test_try_parse_malformed_json_returns_empty() {
            let body = "[UnityCrossThreadLogger]6/17/2026 5:08:00 PM\n<== EventGetCoursesV2(uuid)\n{broken";
            let entry = unity_entry(body);
            assert!(try_parse(&entry, Some(test_timestamp())).is_empty());
        }

        #[test]
        fn test_try_parse_missing_courses_field_returns_empty() {
            let body =
                "[UnityCrossThreadLogger]6/17/2026 5:08:00 PM\n<== EventGetCoursesV2(uuid)\n{}";
            let entry = unity_entry(body);
            assert!(try_parse(&entry, Some(test_timestamp())).is_empty());
        }

        #[test]
        fn test_try_parse_empty_courses_array_returns_empty() {
            let body = make_response_body(&[]);
            let entry = unity_entry(&body);
            assert!(try_parse(&entry, Some(test_timestamp())).is_empty());
        }
    }

    // -- Multi-event -----------------------------------------------------------

    mod multi_event {
        use super::*;

        #[test]
        fn test_try_parse_emits_one_event_per_qualifying_course() {
            let course_a = make_course(
                "course-a",
                "Constructed_BestOf3",
                "deck-a",
                "Test Deck A",
                Some("Standard"),
                &[(1, 4)],
                &[],
            );
            let course_b = make_course(
                "course-b",
                "Play_Brawl",
                "deck-b",
                "Test Deck B",
                Some("Brawl"),
                &[(2, 1)],
                &[85103],
            );
            let body = make_response_body(&[course_a, course_b]);
            let entry = unity_entry(&body);
            let events = try_parse(&entry, Some(test_timestamp()));
            assert_eq!(events.len(), 2);
        }

        #[test]
        fn test_try_parse_skips_null_course_deck() {
            let qualifying = make_course(
                "course-1",
                "Constructed_BestOf3",
                "deck-1",
                "Test Deck",
                Some("Standard"),
                &[(1, 4)],
                &[],
            );
            let skip = make_null_course_deck("course-2", "QuickDraft_Test");
            let body = make_response_body(&[qualifying, skip]);
            let entry = unity_entry(&body);
            let events = try_parse(&entry, Some(test_timestamp()));
            assert_eq!(events.len(), 1);
        }

        #[test]
        fn test_try_parse_skips_empty_maindeck() {
            let qualifying = make_course(
                "course-1",
                "Constructed_BestOf3",
                "deck-1",
                "Test Deck",
                Some("Standard"),
                &[(1, 4)],
                &[],
            );
            let skip = make_empty_maindeck_course("course-2", "Historic_Ladder");
            let body = make_response_body(&[qualifying, skip]);
            let entry = unity_entry(&body);
            let events = try_parse(&entry, Some(test_timestamp()));
            assert_eq!(events.len(), 1);
        }

        #[test]
        fn test_try_parse_all_null_returns_empty() {
            let skip1 = make_null_course_deck("course-1", "QuickDraft_A");
            let skip2 = make_null_course_deck("course-2", "QuickDraft_B");
            let body = make_response_body(&[skip1, skip2]);
            let entry = unity_entry(&body);
            assert!(try_parse(&entry, Some(test_timestamp())).is_empty());
        }
    }

    // -- Payload fields ----------------------------------------------------

    mod payload_fields {
        use super::*;

        #[test]
        fn test_try_parse_payload_type_is_course_deck() {
            let course = make_course(
                "course-1",
                "Ladder",
                "deck-1",
                "Test Deck",
                Some("Standard"),
                &[(1, 4)],
                &[],
            );
            let body = make_response_body(&[course]);
            let entry = unity_entry(&body);
            let events = try_parse(&entry, Some(test_timestamp()));
            let payload = course_deck_payload(&events[0]);
            assert_eq!(payload["type"], "course_deck");
        }

        #[test]
        fn test_try_parse_extracts_all_fields() {
            let course = make_course(
                "course-uuid-1",
                "Constructed_BestOf3",
                "deck-uuid-1",
                "Test Deck",
                Some("TraditionalStandard"),
                &[(95816, 4), (95817, 3), (68740, 4)],
                &[],
            );
            let body = make_response_body(&[course]);
            let entry = unity_entry(&body);
            let events = try_parse(&entry, Some(test_timestamp()));
            let payload = course_deck_payload(&events[0]);

            assert_eq!(payload["deck_id"], "deck-uuid-1");
            assert_eq!(payload["name"], "Test Deck");
            assert_eq!(payload["format"], "TraditionalStandard");
            assert_eq!(
                payload["maindeck_hash"],
                "6abd62511e8248dcf93b56f6b4fff47a7a26d849d105b5a793b36715455451e6"
            );
            assert_eq!(payload["internal_event_name"], "Constructed_BestOf3");
            assert_eq!(payload["course_id"], "course-uuid-1");
        }

        #[test]
        fn test_try_parse_nullable_fields_absent_deck_id() {
            let course = serde_json::json!({
                "CourseId": "course-1",
                "InternalEventName": "Ladder",
                "CourseDeckSummary": {
                    "Attributes": []
                },
                "CourseDeck": {
                    "MainDeck": [{"cardId": 1, "quantity": 4}],
                    "Sideboard": [],
                    "CommandZone": [],
                    "Companions": [],
                    "CardSkins": []
                }
            });
            let body = make_response_body(&[course]);
            let entry = unity_entry(&body);
            let events = try_parse(&entry, Some(test_timestamp()));
            let payload = course_deck_payload(&events[0]);

            assert!(payload["deck_id"].is_null());
            assert!(payload["name"].is_null());
            assert!(payload["format"].is_null());
        }
    }

    // -- Metadata preservation ------------------------------------------------

    mod metadata {
        use super::*;

        #[test]
        fn test_try_parse_preserves_raw_bytes() {
            let course = make_course(
                "course-1",
                "Ladder",
                "deck-1",
                "Test Deck",
                Some("Standard"),
                &[(1, 4)],
                &[],
            );
            let body = make_response_body(&[course]);
            let entry = unity_entry(&body);
            let events = try_parse(&entry, Some(test_timestamp()));
            assert_eq!(events[0].metadata().raw_bytes(), body.as_bytes());
        }

        #[test]
        fn test_try_parse_stores_timestamp() {
            let course = make_course(
                "course-1",
                "Ladder",
                "deck-1",
                "Test Deck",
                Some("Standard"),
                &[(1, 4)],
                &[],
            );
            let body = make_response_body(&[course]);
            let entry = unity_entry(&body);
            let ts = Some(test_timestamp());
            let events = try_parse(&entry, ts);
            assert_eq!(events[0].metadata().timestamp(), ts);
        }

        #[test]
        fn test_try_parse_sibling_events_share_raw_bytes_hash() {
            let course_a = make_course(
                "course-a",
                "Ladder",
                "deck-a",
                "Test Deck A",
                Some("Standard"),
                &[(1, 4)],
                &[],
            );
            let course_b = make_course(
                "course-b",
                "Play_Brawl",
                "deck-b",
                "Test Deck B",
                Some("Brawl"),
                &[(2, 1)],
                &[85103],
            );
            let body = make_response_body(&[course_a, course_b]);
            let entry = unity_entry(&body);
            let events = try_parse(&entry, Some(test_timestamp()));
            assert_eq!(events.len(), 2);
            assert_eq!(
                events[0].metadata().raw_bytes_hash(),
                events[1].metadata().raw_bytes_hash()
            );
        }
    }
}
