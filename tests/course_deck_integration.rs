//! Integration tests for the `EventGetCoursesV2` course-deck parser.
//!
//! Covers:
//! - Sanitized corpus fixture: a single `EventGetCoursesV2` response with
//!   two qualifying Courses (constructed + brawl with non-empty
//!   `CommandZone`) plus one `CourseDeck: null` limited-run Course
//!   (skip-case).
//! - Router dispatch: the response is claimed and produces one
//!   `CourseDeck` event per qualifying Course.
//! - `parse_whole_log` parity with the entry-by-entry `Router` path.
//! - Per-course field assertions, including sort-before-hash proof (the
//!   fixture's `MainDeck` arrays are unsorted).

use manasight_parser::events::GameEvent;
use manasight_parser::log::entry::LineBuffer;
use manasight_parser::parse_whole_log;
use manasight_parser::router::Router;

// ---------------------------------------------------------------------------
// Sanitized fixture (public repo — no PII)
//
// Source: session_2026-06-17_1720_standard-event and session_2026-06-17_1708_brawl
// (manasight/manasight-corpus). Sanitized: CourseId/DeckId UUIDs replaced with
// zero-padded placeholders, deck names replaced with "Test Deck N",
// LastPlayed/LastUpdated timestamps set to zero-offset placeholders. Card IDs,
// Format, and event/course structure are game data with no PII. The third
// Course (CourseDeck: null, limited pre-deckbuild skip-case) is modeled on
// session_2026-03-11_1847_quick-draft-selesnya.
// ---------------------------------------------------------------------------

const FIXTURE_V2_MULTI: &str = include_str!("fixtures/course_deck_v2_multi.log");

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Routes `input` line-by-line through a `LineBuffer` + `Router`, collecting
/// all events — the reference entry-by-entry path.
fn parse_via_router(input: &str) -> Vec<GameEvent> {
    let mut buffer = LineBuffer::new();
    let router = Router::new();
    let mut events = Vec::new();
    for line in input.lines() {
        for entry in buffer.push_line(line) {
            events.extend(router.route(&entry));
        }
    }
    if let Some(entry) = buffer.flush() {
        events.extend(router.route(&entry));
    }
    events
}

/// Finds all `CourseDeck` events in a slice.
fn course_decks(events: &[GameEvent]) -> Vec<&GameEvent> {
    events
        .iter()
        .filter(|e| matches!(e, GameEvent::CourseDeck(_)))
        .collect()
}

// ---------------------------------------------------------------------------
// Test 1: Router claims the response and emits one event per qualifying Course
// ---------------------------------------------------------------------------

#[test]
fn test_corpus_v2_multi_emits_two_qualifying_course_decks() {
    let events = parse_via_router(FIXTURE_V2_MULTI);
    let decks = course_decks(&events);

    assert_eq!(
        decks.len(),
        2,
        "expected exactly 2 CourseDeck events (constructed + brawl); the third \
         Course has CourseDeck: null and must be skipped, got {decks:?}",
    );
}

// ---------------------------------------------------------------------------
// Test 2: Constructed course — field extraction + sort-before-hash
// ---------------------------------------------------------------------------

#[test]
fn test_corpus_v2_multi_constructed_course_fields() {
    let events = parse_via_router(FIXTURE_V2_MULTI);
    let decks = course_decks(&events);

    let constructed = decks
        .iter()
        .find(|e| e.payload()["internal_event_name"] == "Constructed_BestOf3")
        .unwrap_or_else(|| unreachable!("constructed course must be present"));

    let payload = constructed.payload();
    assert_eq!(payload["type"], "course_deck");
    assert_eq!(payload["deck_id"], "00000000-0000-0000-0000-00000000d001");
    assert_eq!(payload["name"], "Test Deck 1");
    assert_eq!(payload["format"], "TraditionalStandard");
    assert_eq!(payload["course_id"], "00000000-0000-0000-0000-00000000c001");
    // Fixture MainDeck is unsorted ([95817:3, 95816:4, 68740:4]); the hash
    // must match the sorted-canonical form, proving sort-before-hash.
    assert_eq!(
        payload["maindeck_hash"],
        "6abd62511e8248dcf93b56f6b4fff47a7a26d849d105b5a793b36715455451e6",
        "maindeck_hash must be computed from the sorted MainDeck, not raw order",
    );
}

// ---------------------------------------------------------------------------
// Test 3: Brawl course — non-empty CommandZone doesn't block emission
// ---------------------------------------------------------------------------

#[test]
fn test_corpus_v2_multi_brawl_course_fields() {
    let events = parse_via_router(FIXTURE_V2_MULTI);
    let decks = course_decks(&events);

    let brawl = decks
        .iter()
        .find(|e| e.payload()["internal_event_name"] == "Play_Brawl")
        .unwrap_or_else(|| unreachable!("brawl course must be present"));

    let payload = brawl.payload();
    assert_eq!(payload["name"], "Test Deck 2");
    assert_eq!(payload["format"], "Brawl");
    assert_eq!(
        payload["maindeck_hash"],
        "e6e0a255d8d5c8eef2857c1f64f51e79582dd03f4429714e6b3bd54f4175ca4d",
    );
}

// ---------------------------------------------------------------------------
// Test 4: Null CourseDeck course is skipped (limited pre-deckbuild)
// ---------------------------------------------------------------------------

#[test]
fn test_corpus_v2_multi_null_course_deck_skipped() {
    let events = parse_via_router(FIXTURE_V2_MULTI);
    let decks = course_decks(&events);

    assert!(
        !decks
            .iter()
            .any(|e| e.payload()["internal_event_name"] == "QuickDraft_Test"),
        "the null-CourseDeck limited course must not emit a CourseDeck event",
    );
}

// ---------------------------------------------------------------------------
// Test 5: parse_whole_log parity with Router path
// ---------------------------------------------------------------------------

#[test]
fn test_parse_whole_log_parity_with_router_for_course_deck() {
    let router_events = parse_via_router(FIXTURE_V2_MULTI);
    let whole_log_events = parse_whole_log(FIXTURE_V2_MULTI);

    let router_decks = course_decks(&router_events);
    let whole_log_decks = course_decks(&whole_log_events);

    assert_eq!(
        router_decks.len(),
        whole_log_decks.len(),
        "parse_whole_log and Router must produce the same number of CourseDeck events",
    );

    for (r, w) in router_decks.iter().zip(whole_log_decks.iter()) {
        assert_eq!(
            r.payload(),
            w.payload(),
            "payloads must match between parse_whole_log and Router paths",
        );
    }
}

// ---------------------------------------------------------------------------
// Test 6: Sibling events share raw_bytes_hash (multi-event, full-entry bytes)
// ---------------------------------------------------------------------------

#[test]
fn test_corpus_v2_multi_sibling_events_share_raw_bytes_hash() {
    let events = parse_via_router(FIXTURE_V2_MULTI);
    let decks = course_decks(&events);

    assert_eq!(decks.len(), 2);
    assert_eq!(
        decks[0].metadata().raw_bytes_hash(),
        decks[1].metadata().raw_bytes_hash(),
        "sibling CourseDeck events from the same entry must share raw_bytes_hash",
    );
}
