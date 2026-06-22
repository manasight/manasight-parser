//! Integration tests for [`manasight_parser::parse_whole_log`] and
//! [`manasight_parser::wasm::StreamingParserCore`].
//!
//! Verifies that the sync entry point produces the same events as the
//! entry-by-entry [`Router`] path, including trailing entries not followed
//! by a header.
//!
//! Also verifies that [`StreamingParserCore`] (the host-testable streaming
//! core) yields event-for-event equality with `parse_whole_log` across
//! deterministic chunk sizes, CRLF inputs, mid-line splits, and random
//! chunk sizes (via `proptest`).

use manasight_parser::log::entry::LineBuffer;
use manasight_parser::router::Router;
use manasight_parser::{parse_whole_log, GameEvent};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Feeds `input` line-by-line through a fresh `LineBuffer` + `Router`,
/// returning all collected events (the reference path).
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

// ---------------------------------------------------------------------------
// Parity tests: parse_whole_log vs. entry-by-entry Router path
// ---------------------------------------------------------------------------

#[test]
fn test_parse_whole_log_empty_input_returns_empty_vec() {
    let events = parse_whole_log("");
    assert!(events.is_empty());
}

#[test]
fn test_parse_whole_log_metadata_parity_with_router() {
    let input = "DETAILED LOGS: ENABLED\n";
    let via_fn = parse_whole_log(input);
    let via_router = parse_via_router(input);
    assert_eq!(
        via_fn.len(),
        via_router.len(),
        "parse_whole_log and router path must emit the same number of events"
    );
    assert_eq!(via_fn.len(), 1);
    assert!(matches!(via_fn[0], GameEvent::DetailedLoggingStatus(_)));
}

#[test]
fn test_parse_whole_log_session_event_parity_with_router() {
    let input = "[UnityCrossThreadLogger]authenticateResponse\n\
                 {\"screenName\":\"TestPlayer\"}\n\
                 [UnityCrossThreadLogger]2/25/2026 12:00:00 PM\n\
                 some filler\n";

    let via_fn = parse_whole_log(input);
    let via_router = parse_via_router(input);

    assert_eq!(
        via_fn.len(),
        via_router.len(),
        "event count must match between parse_whole_log and router path"
    );
    assert_eq!(via_fn.len(), 1);
    assert!(matches!(via_fn[0], GameEvent::Session(_)));
}

#[test]
fn test_parse_whole_log_multiple_events_parity_with_router() {
    let gs_payload = serde_json::json!({
        "greToClientEvent": {
            "greToClientMessages": [{
                "type": "GREMessageType_GameStateMessage",
                "gameStateMessage": {
                    "gameInfo": { "stage": "GameStage_Play" },
                    "gameObjects": [],
                    "zones": []
                }
            }]
        }
    });

    let input = format!(
        "DETAILED LOGS: ENABLED\n\
         [UnityCrossThreadLogger]authenticateResponse\n\
         {{\"screenName\":\"TestPlayer\"}}\n\
         [UnityCrossThreadLogger]2/25/2026 12:00:00 PM\n{gs_payload}\n\
         [UnityCrossThreadLogger]2/25/2026 12:00:01 PM\nfiller\n"
    );

    let via_fn = parse_whole_log(&input);
    let via_router = parse_via_router(&input);

    assert_eq!(
        via_fn.len(),
        via_router.len(),
        "event count must match: parse_whole_log={}, router={}",
        via_fn.len(),
        via_router.len()
    );
    // Expected: DetailedLoggingStatus + Session + GameState = 3
    assert_eq!(via_fn.len(), 3);
    assert!(matches!(via_fn[0], GameEvent::DetailedLoggingStatus(_)));
    assert!(matches!(via_fn[1], GameEvent::Session(_)));
    assert!(matches!(via_fn[2], GameEvent::GameState(_)));
}

/// This test specifically exercises the trailing-entry flush path: the last
/// entry has no following header to trigger an implicit flush, so `flush()`
/// must drain it.
#[test]
fn test_parse_whole_log_trailing_entry_not_followed_by_header_parity() {
    // The session entry at the end has no subsequent header — it will only
    // be emitted if flush() is called after iterating all lines.
    let input = "[UnityCrossThreadLogger]authenticateResponse\n\
                 {\"screenName\":\"TrailingEntry\"}\n";

    let via_fn = parse_whole_log(input);
    let via_router = parse_via_router(input);

    assert_eq!(
        via_fn.len(),
        via_router.len(),
        "trailing entry must be drained by flush(): parse_whole_log={}, router={}",
        via_fn.len(),
        via_router.len()
    );
    assert_eq!(
        via_fn.len(),
        1,
        "expected exactly one event from trailing entry"
    );
    assert!(matches!(via_fn[0], GameEvent::Session(_)));
}

#[test]
fn test_parse_whole_log_unrecognized_entries_parity_with_router() {
    // Content with no parseable events — should return empty vec.
    let input = "[UnityCrossThreadLogger]2/25/2026 12:00:00 PM\n\
                 some completely unrecognized content\n\
                 [UnityCrossThreadLogger]2/25/2026 12:00:01 PM\n\
                 more unrecognized content\n";

    let via_fn = parse_whole_log(input);
    let via_router = parse_via_router(input);

    assert_eq!(
        via_fn.len(),
        via_router.len(),
        "unrecognized entries must produce identical (empty) results"
    );
    assert!(via_fn.is_empty());
}

// ---------------------------------------------------------------------------
// Frame-counter prefix (#240): UTC_Log archive variant
// ---------------------------------------------------------------------------

/// Feeds `input` line-by-line through a fresh [`LineBuffer`], returning all
/// complete [`manasight_parser::log::entry::LogEntry`] values (the layer
/// below the Router, exercising header/metadata detection directly).
fn collect_log_entries(input: &str) -> Vec<manasight_parser::log::entry::LogEntry> {
    let mut buffer = LineBuffer::new();
    let mut entries = Vec::new();
    for line in input.lines() {
        entries.extend(buffer.push_line(line));
    }
    if let Some(entry) = buffer.flush() {
        entries.push(entry);
    }
    entries
}

/// Synthesises a frame-prefixed copy of `flush_timing_corpus_slice.log` by
/// prepending `[<n>] ` to every line, then asserts the resulting `LogEntry`
/// stream is byte-identical to parsing the unprefixed original.
///
/// This reproduces the failure mode observed on newer MTGA Mac builds
/// (`UTC_Log` archive variant): before the fix, every prefixed header failed
/// to match, yielding 0 entries. After the fix the frame-counter prefix is
/// stripped in `LineBuffer::push_line` before detection, so the prefixed and
/// unprefixed logs produce the same `LogEntry` output.
///
/// The fixture is tested at the `LogEntry` level (below the Router) because
/// `flush_timing_corpus_slice.log` exercises `LineBuffer` entry-detection
/// patterns — the same layer where the frame-counter strip applies.
#[test]
fn test_frame_prefixed_fixture_log_entries_byte_identical_to_unprefixed() {
    let unprefixed = include_str!("fixtures/flush_timing_corpus_slice.log");

    // Strip comment lines as the fixture parser helper does, so we get
    // a clean comparison of real log content.
    let clean_unprefixed: String = unprefixed
        .lines()
        .filter(|line| !line.starts_with('#'))
        .fold(String::new(), |mut s, line| {
            use std::fmt::Write as _;
            let _ = writeln!(s, "{line}");
            s
        });

    // Prepend `[<n>] ` to every line with a monotonically incrementing
    // counter, mirroring the Unity frame-counter format. The exact digit
    // values must not affect parsing.
    let prefixed: String =
        clean_unprefixed
            .lines()
            .enumerate()
            .fold(String::new(), |mut s, (n, line)| {
                use std::fmt::Write as _;
                let _ = writeln!(s, "[{n}] {line}");
                s
            });

    let entries_unprefixed = collect_log_entries(&clean_unprefixed);
    let entries_prefixed = collect_log_entries(&prefixed);

    assert!(
        !entries_unprefixed.is_empty(),
        "fixture must yield at least one LogEntry — verify fixture path is correct",
    );

    assert_eq!(
        entries_prefixed.len(),
        entries_unprefixed.len(),
        "frame-prefixed log must yield the same LogEntry count as the unprefixed original \
         (got {}, expected {})",
        entries_prefixed.len(),
        entries_unprefixed.len(),
    );

    // Full entry-stream equality: headers and bodies must match.
    assert_eq!(
        entries_prefixed, entries_unprefixed,
        "frame-prefixed log must produce a byte-identical LogEntry stream to the unprefixed original",
    );
}

/// Verifies the complete `parse_whole_log` path (including Router dispatch)
/// for a frame-prefixed log that contains events the router can parse.
/// Uses `deck_submission_v2_constructed.log` which contains an `EventSetDeckV2`
/// entry that maps to a `GameEvent::DeckSubmission`.
#[test]
fn test_parse_whole_log_frame_prefixed_produces_same_game_events_as_unprefixed() {
    let unprefixed = include_str!("fixtures/deck_submission_v2_constructed.log");

    // Prepend `[<n>] ` to every line.
    let prefixed: String =
        unprefixed
            .lines()
            .enumerate()
            .fold(String::new(), |mut s, (n, line)| {
                use std::fmt::Write as _;
                let _ = writeln!(s, "[{n}] {line}");
                s
            });

    let events_unprefixed = parse_whole_log(unprefixed);
    let events_prefixed = parse_whole_log(&prefixed);

    assert!(
        !events_unprefixed.is_empty(),
        "unprefixed fixture must yield at least one GameEvent",
    );

    assert_eq!(
        events_prefixed.len(),
        events_unprefixed.len(),
        "frame-prefixed log must yield the same GameEvent count as the unprefixed original \
         (got {}, expected {})",
        events_prefixed.len(),
        events_unprefixed.len(),
    );

    assert_eq!(
        events_prefixed, events_unprefixed,
        "frame-prefixed log must produce a byte-identical GameEvent stream to the unprefixed original",
    );
}

// ---------------------------------------------------------------------------
// StreamingParserCore parity tests (#254)
// ---------------------------------------------------------------------------

/// Feeds `input` through a fresh [`StreamingParserCore`] in chunks of `chunk_size`
/// bytes (clamped to valid UTF-8 boundaries by splitting on char boundaries),
/// then finalises with `finish()`. Returns all collected events.
///
/// `chunk_size == 0` is treated as "whole input in one chunk".
fn parse_via_streaming_core(input: &str, chunk_size: usize) -> Vec<GameEvent> {
    use manasight_parser::wasm::StreamingParserCore;

    let mut core = StreamingParserCore::new();
    let mut events = Vec::new();

    if chunk_size == 0 || chunk_size >= input.len() {
        events.extend(core.push_chunk(input));
    } else {
        // Split on char boundaries so `&str` slices are always valid UTF-8.
        let bytes = input.as_bytes();
        let mut pos = 0;
        while pos < bytes.len() {
            let end = (pos + chunk_size).min(bytes.len());
            // Advance end to the next char boundary.
            let end = input
                .char_indices()
                .map(|(i, _)| i)
                .find(|&i| i >= end)
                .unwrap_or(bytes.len());
            let chunk = &input[pos..end];
            events.extend(core.push_chunk(chunk));
            pos = end;
        }
    }

    events.extend(core.finish());
    events
}

// Multi-event fixture used across streaming parity tests.
fn multi_event_input() -> String {
    let gs_payload = serde_json::json!({
        "greToClientEvent": {
            "greToClientMessages": [{
                "type": "GREMessageType_GameStateMessage",
                "gameStateMessage": {
                    "gameInfo": { "stage": "GameStage_Play" },
                    "gameObjects": [],
                    "zones": []
                }
            }]
        }
    });
    format!(
        "DETAILED LOGS: ENABLED\n\
         [UnityCrossThreadLogger]authenticateResponse\n\
         {{\"screenName\":\"TestPlayer\"}}\n\
         [UnityCrossThreadLogger]2/25/2026 12:00:00 PM\n{gs_payload}\n\
         [UnityCrossThreadLogger]2/25/2026 12:00:01 PM\nfiller\n"
    )
}

#[test]
fn test_streaming_core_chunk_size_1_parity() {
    let input = multi_event_input();
    let expected = parse_whole_log(&input);
    let actual = parse_via_streaming_core(&input, 1);
    assert_eq!(
        expected, actual,
        "chunk_size=1 must yield identical events to parse_whole_log"
    );
}

#[test]
fn test_streaming_core_chunk_size_2_parity() {
    let input = multi_event_input();
    let expected = parse_whole_log(&input);
    let actual = parse_via_streaming_core(&input, 2);
    assert_eq!(
        expected, actual,
        "chunk_size=2 must yield identical events to parse_whole_log"
    );
}

#[test]
fn test_streaming_core_chunk_size_3_parity() {
    let input = multi_event_input();
    let expected = parse_whole_log(&input);
    let actual = parse_via_streaming_core(&input, 3);
    assert_eq!(
        expected, actual,
        "chunk_size=3 must yield identical events to parse_whole_log"
    );
}

#[test]
fn test_streaming_core_chunk_size_7_parity() {
    let input = multi_event_input();
    let expected = parse_whole_log(&input);
    let actual = parse_via_streaming_core(&input, 7);
    assert_eq!(
        expected, actual,
        "chunk_size=7 must yield identical events to parse_whole_log"
    );
}

#[test]
fn test_streaming_core_chunk_size_64_parity() {
    let input = multi_event_input();
    let expected = parse_whole_log(&input);
    let actual = parse_via_streaming_core(&input, 64);
    assert_eq!(
        expected, actual,
        "chunk_size=64 must yield identical events to parse_whole_log"
    );
}

#[test]
fn test_streaming_core_whole_input_single_chunk_parity() {
    let input = multi_event_input();
    let expected = parse_whole_log(&input);
    let actual = parse_via_streaming_core(&input, input.len());
    assert_eq!(
        expected, actual,
        "whole-input single chunk must yield identical events to parse_whole_log"
    );
}

/// CRLF line endings — `\r\n` sequences must be handled identically to `\n`.
#[test]
fn test_streaming_core_crlf_parity() {
    // Build an input with CRLF line endings.
    let lf_input = "DETAILED LOGS: ENABLED\n\
                     [UnityCrossThreadLogger]authenticateResponse\n\
                     {\"screenName\":\"CrlfPlayer\"}\n";
    let crlf_input = lf_input.replace('\n', "\r\n");

    let expected = parse_whole_log(lf_input);
    // Feed the CRLF version through the streaming core in a single chunk.
    let actual = parse_via_streaming_core(&crlf_input, crlf_input.len());
    assert_eq!(
        expected, actual,
        "CRLF line endings must produce identical events to LF-only input"
    );
}

/// Mid-line split: chunk boundary falls inside a line (not at `\n`).
#[test]
fn test_streaming_core_mid_line_split_parity() {
    let input = "DETAILED LOGS: ENABLED\n\
                  [UnityCrossThreadLogger]authenticateResponse\n\
                  {\"screenName\":\"MidLinePlayer\"}\n";
    let expected = parse_whole_log(input);
    // chunk_size=5 is deliberately chosen to split mid-line.
    let actual = parse_via_streaming_core(input, 5);
    assert_eq!(
        expected, actual,
        "mid-line chunk split must not lose or duplicate events"
    );
}

/// No trailing newline + empty-chunk case: final line ends without `\n`, and
/// an additional empty chunk after finish must not cause panics or lost events.
#[test]
fn test_streaming_core_no_trailing_newline_parity() {
    use manasight_parser::wasm::StreamingParserCore;

    // The authenticateResponse entry has no trailing newline.
    let input = "[UnityCrossThreadLogger]authenticateResponse\n\
                  {\"screenName\":\"NoNewline\"}";
    let expected = parse_whole_log(input);

    let mut core = StreamingParserCore::new();
    // Push the input, then push an empty chunk, then finish.
    let mut events: Vec<GameEvent> = core.push_chunk(input);
    let empty_batch = core.push_chunk(""); // must be a no-op
    assert!(
        empty_batch.is_empty(),
        "empty chunk must return no events (tail unchanged)"
    );
    events.extend(core.finish());

    assert_eq!(
        expected, events,
        "no-trailing-newline input must produce the same events via streaming core"
    );
}

// ---------------------------------------------------------------------------
// FIX 2 — Trailing-\r parity: finish() must match str::lines() byte-for-byte
// ---------------------------------------------------------------------------

/// `finish()` must NOT strip a lone trailing `\r` from the final partial line.
///
/// `str::lines()` only removes `'\r'` as part of `"\r\n"`. When the final line
/// has no trailing `'\n'`, the `'\r'` (if any) is part of the line's content
/// and must be passed through unchanged. Stripping it in `finish()` would
/// diverge from `parse_whole_log`'s `input.lines()` last element.
///
/// These tests confirm byte-for-byte equality between `StreamingParserCore`
/// and `parse_whole_log` for inputs where the trailing `\r` case matters.
#[test]
fn test_streaming_core_crlf_with_trailing_cr_parity() {
    use manasight_parser::wasm::StreamingParserCore;

    // "a\r\nb\r" — CRLF-terminated first line, then a line ending in bare \r.
    // str::lines() splits this as ["a", "b\r"] (the \r\n pair is stripped,
    // but the final \r is not).
    let input = "DETAILED LOGS: ENABLED\r\n[UnityCrossThreadLogger]authenticateResponse\r\n{\"screenName\":\"TrailingCR\"}\r";
    let expected = parse_whole_log(input);

    let mut core = StreamingParserCore::new();
    let mut events: Vec<GameEvent> = core.push_chunk(input);
    events.extend(core.finish());

    assert_eq!(
        expected, events,
        "input ending with bare \\r (not \\r\\n): streaming core must match parse_whole_log"
    );
}

#[test]
fn test_streaming_core_lone_trailing_cr_parity() {
    use manasight_parser::wasm::StreamingParserCore;

    // "foo\r" — a single line with a bare trailing \r and no \n.
    // str::lines() returns ["foo\r"] (the \r is NOT stripped because there is no \n).
    let input = "DETAILED LOGS: ENABLED\rfoo\r";
    let expected = parse_whole_log(input);

    let mut core = StreamingParserCore::new();
    let mut events: Vec<GameEvent> = core.push_chunk(input);
    events.extend(core.finish());

    assert_eq!(
        expected, events,
        "input with only bare \\r (no \\n): streaming core must match parse_whole_log"
    );
}

/// Regression guard: `"a\r\n"` (CRLF + trailing newline) must still produce
/// the same events as `parse_whole_log` — the `push_chunk` CRLF stripping
/// path must remain intact.
#[test]
fn test_streaming_core_crlf_trailing_newline_parity() {
    let input = "DETAILED LOGS: ENABLED\r\n";
    let expected = parse_whole_log(input);
    let actual = parse_via_streaming_core(input, input.len());
    assert_eq!(
        expected, actual,
        "CRLF input with trailing newline must match parse_whole_log"
    );
}

/// Proptest: random chunk sizes must produce identical events to `parse_whole_log`.
#[cfg(not(target_arch = "wasm32"))]
mod streaming_proptest {
    use super::{multi_event_input, parse_via_streaming_core};
    use manasight_parser::parse_whole_log;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn test_streaming_core_random_chunk_size_parity(chunk_size in 1usize..=512) {
            let input = multi_event_input();
            let expected = parse_whole_log(&input);
            let actual = parse_via_streaming_core(&input, chunk_size);
            prop_assert_eq!(
                expected, actual,
                "chunk_size={} must yield identical events", chunk_size
            );
        }
    }
}
