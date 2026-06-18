//! Integration tests for the `EventSetDeck` deck-submission parser.
//!
//! Covers:
//! - Sanitized corpus fixture: generic-constructed-queue submission
//!   (`Constructed_BestOf3` → registered `Format "TraditionalStandard"`) confirms
//!   the deck-submission path supplies the deck's registered `Format`, not an
//!   `event_id` heuristic.
//! - Router dispatch: `EventSetDeckV2` and `EventSetDeckV3` are claimed.
//! - `parse_whole_log` parity with the entry-by-entry `Router` path.
//! - `scrub_raw_log` preserves `DeckId` and `Format` fields.

use manasight_parser::events::GameEvent;
use manasight_parser::log::entry::LineBuffer;
use manasight_parser::router::Router;
use manasight_parser::{parse_whole_log, scrub_raw_log};

// ---------------------------------------------------------------------------
// Sanitized fixture (public repo — no PII)
//
// Source: session_2026-04-12_0844_edge-cases and session_2026-04-12_1240_edge-cases
// (manasight/manasight-corpus). Sanitized: request UUID replaced with zeros,
// deck UUID replaced with placeholder, deck name replaced with "Test Deck",
// LastPlayed/LastUpdated timestamps set to zero-offset placeholders.
// Card IDs and Format/Attributes are game data with no PII.
// ---------------------------------------------------------------------------

const FIXTURE_V2_CONSTRUCTED: &str = include_str!("fixtures/deck_submission_v2_constructed.log");

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

/// Finds all `DeckSubmission` events in a slice.
fn deck_submissions(events: &[GameEvent]) -> Vec<&GameEvent> {
    events
        .iter()
        .filter(|e| matches!(e, GameEvent::DeckSubmission(_)))
        .collect()
}

// ---------------------------------------------------------------------------
// Test 1: Corpus fixture — Format is surfaced for generic-constructed-queue
// ---------------------------------------------------------------------------

/// Asserts that a `Constructed_BestOf3` deck submission surfaces the deck's
/// registered `Format` (`TraditionalStandard`), not an event-id heuristic.
///
/// This is the acceptance criterion from issue #232: the deck-submission path
/// — not `event_id` parsing — supplies the deck's registered Format for the
/// C-2b format model fallback.
#[test]
fn test_corpus_v2_constructed_format_is_traditional_standard() {
    let events = parse_via_router(FIXTURE_V2_CONSTRUCTED);
    let submissions = deck_submissions(&events);

    assert_eq!(
        submissions.len(),
        1,
        "expected exactly one DeckSubmission from the V2 constructed fixture, got {submissions:?}",
    );

    let payload = submissions[0].payload();
    assert_eq!(
        payload["deck_format"], "TraditionalStandard",
        "deck_format must be the deck's registered Format, not an event_id heuristic",
    );
    assert_eq!(
        payload["event_name"], "Constructed_BestOf3",
        "event_name should be the generic queue string",
    );
    assert_eq!(
        payload["deck_id"], "aaaabbbb-0000-0000-0000-000000000001",
        "deck_id must match the sanitized fixture value",
    );
    assert_eq!(
        payload["is_singleton"], false,
        "standard constructed deck has an empty CommandZone",
    );
    assert_eq!(payload["type"], "deck_submission",);
}

// ---------------------------------------------------------------------------
// Test 2: Timestamp is preserved in EventMetadata
// ---------------------------------------------------------------------------

/// Asserts that the `EventSetDeckV2` fixture line's timestamp survives into
/// the `EventMetadata` so consumers can bind deck → match by time window.
#[test]
fn test_corpus_v2_constructed_timestamp_is_present() {
    let events = parse_via_router(FIXTURE_V2_CONSTRUCTED);
    let submissions = deck_submissions(&events);

    assert_eq!(submissions.len(), 1);
    assert!(
        submissions[0].metadata().timestamp().is_some(),
        "EventSetDeck entry has a timestamp in the header; it must reach EventMetadata",
    );
}

// ---------------------------------------------------------------------------
// Test 3: parse_whole_log parity with Router path
// ---------------------------------------------------------------------------

/// Asserts that `parse_whole_log` and the entry-by-entry `Router` path
/// produce the same `DeckSubmission` event sequence on the corpus fixture.
#[test]
fn test_parse_whole_log_parity_with_router_for_deck_submission() {
    let router_events = parse_via_router(FIXTURE_V2_CONSTRUCTED);
    let whole_log_events = parse_whole_log(FIXTURE_V2_CONSTRUCTED);

    let router_submissions = deck_submissions(&router_events);
    let whole_log_submissions = deck_submissions(&whole_log_events);

    assert_eq!(
        router_submissions.len(),
        whole_log_submissions.len(),
        "parse_whole_log and Router must produce the same number of DeckSubmission events",
    );

    for (r, w) in router_submissions.iter().zip(whole_log_submissions.iter()) {
        assert_eq!(
            r.payload(),
            w.payload(),
            "payloads must match between parse_whole_log and Router paths",
        );
    }
}

// ---------------------------------------------------------------------------
// Test 4: scrub_raw_log preserves Format and DeckId
// ---------------------------------------------------------------------------

/// Confirms that `scrub_raw_log` (the default-options path that strips names,
/// tokens, IPs, and hardware identifiers) preserves `Format` and `DeckId`
/// fields — both are game data, not PII.
#[test]
fn test_scrub_raw_log_preserves_format_and_deck_id() {
    let scrubbed_str = scrub_raw_log(FIXTURE_V2_CONSTRUCTED);

    assert!(
        scrubbed_str.contains("TraditionalStandard"),
        "scrub_raw_log must NOT strip the Format value",
    );
    assert!(
        scrubbed_str.contains("aaaabbbb-0000-0000-0000-000000000001"),
        "scrub_raw_log must NOT strip the DeckId UUID",
    );
}

// ---------------------------------------------------------------------------
// Test 5: Router claims V2 and V3 entries
// ---------------------------------------------------------------------------

/// Asserts that both `EventSetDeckV2` and `EventSetDeckV3` entries are claimed
/// by the router (not left as unknowns) and produce `DeckSubmission` events.
#[test]
fn test_router_claims_v2_and_v3() {
    let router = Router::new();

    let v2_body = format!(
        "[UnityCrossThreadLogger]4/12/2026 8:44:00 AM ==> EventSetDeckV2 {}",
        make_request_json("Ladder", "deck-v2", "Standard")
    );
    let v3_body = format!(
        "[UnityCrossThreadLogger]4/12/2026 8:44:00 AM ==> EventSetDeckV3 {}",
        make_request_json("Play", "deck-v3", "Alchemy")
    );

    for (body, expected_format, expected_deck_id) in [
        (v2_body.as_str(), "Standard", "deck-v2"),
        (v3_body.as_str(), "Alchemy", "deck-v3"),
    ] {
        let events = parse_via_router(body);
        let submissions = deck_submissions(&events);

        assert_eq!(
            submissions.len(),
            1,
            "body starting with ==> EventSetDeck... should produce 1 DeckSubmission",
        );

        let payload = submissions[0].payload();
        assert_eq!(payload["deck_format"], expected_format);
        assert_eq!(payload["deck_id"], expected_deck_id);
        assert_eq!(router.stats().unknown_count(), 0);
    }
}

// ---------------------------------------------------------------------------
// Helpers for inline test construction
// ---------------------------------------------------------------------------

fn make_request_json(event_name: &str, deck_id: &str, format: &str) -> String {
    let inner = serde_json::json!({
        "EventName": event_name,
        "Summary": {
            "DeckId": deck_id,
            "Attributes": [{"name": "Format", "value": format}]
        },
        "Deck": {
            "MainDeck": [],
            "Sideboard": [],
            "CommandZone": [],
            "Companions": []
        }
    });
    let outer = serde_json::json!({"id": "test-uuid", "request": inner.to_string()});
    outer.to_string()
}
