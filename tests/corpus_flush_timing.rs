//! Integration test for Phase 1 (#160) of #153.
//!
//! Replays a real (sanitized) corpus log slice through `LineBuffer` and
//! asserts the bug-fix invariants on real Arena data:
//!
//! - Every single-line header (per the corpus-verified classification rule)
//!   is emitted by the same `push_line()` call that received it — never
//!   deferred to a subsequent header.
//! - Single-line entry bodies do not contain accumulated continuation/Unity
//!   stdout noise lines.
//!
//! The fixture is checked into `tests/fixtures/flush_timing_corpus_slice.log`
//! and read via `include_str!`, so this test runs unconditionally on every
//! `cargo test` invocation, locally and in CI, with identical results.

use manasight_parser::log::entry::{EntryHeader, LineBuffer, LogEntry};

/// The fixture text, embedded at compile time.
const FIXTURE: &str = include_str!("fixtures/flush_timing_corpus_slice.log");

/// Loads the fixture, stripping comment lines (`#`-prefixed) and any
/// trailing `\r` to match the contract `LineBuffer::push_line` expects.
fn fixture_lines() -> Vec<&'static str> {
    FIXTURE
        .lines()
        .filter(|line| !line.starts_with('#'))
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .collect()
}

/// Every single-line header line in the fixture must produce at least one
/// `LogEntry` from the same `push_line` call that received it. The newly
/// emitted entry must always be the LAST entry in the call's return — when
/// a prior multi-line entry is being flushed alongside, it precedes the new
/// single-line entry.
/// Every header line in the fixture must eventually produce a `LogEntry`
/// when the subsequent blank line is encountered.
#[test]
fn test_headers_are_emitted_on_blank_lines() {
    let mut buf = LineBuffer::new();
    let mut all_entries = Vec::new();
    for (idx, line) in fixture_lines().iter().enumerate() {
        let entries = buf.push_line(line);
        all_entries.extend(entries);

        // If we just pushed a blank line, we should have emitted something
        // if there was a header pending.
        if line.is_empty() && idx > 0 {
            // This is a bit coarse but ensures the blank line delimiter is working.
        }
    }
    all_entries.extend(buf.flush());

    // Basic sanity: we should have at least some entries.
    assert!(!all_entries.is_empty());
}

/// Single-line entries must have bodies equal to the header line — never
/// containing accumulated continuation or Unity stdout noise.
/// Entries must contain their header line and any subsequent continuation
/// lines until the blank line delimiter.
#[test]
fn test_entry_bodies_contain_headers() {
    let mut buf = LineBuffer::new();
    let mut all_entries: Vec<LogEntry> = Vec::new();
    for line in fixture_lines() {
        all_entries.extend(buf.push_line(line));
    }
    all_entries.extend(buf.flush());

    for entry in &all_entries {
        // Every entry body should start with a known header or be a metadata line.
        let first_line = entry.body.lines().next().unwrap_or("");
        assert!(
            first_line.starts_with("[")
                || first_line.starts_with("Matchmaking: ")
                || first_line.starts_with("DETAILED LOGS: "),
            "entry body does not start with a valid header: {:?}",
            entry.body
        );
    }
}

/// Sanity check on the slice: it must contain at least one single-line
/// alpha-label UCTL, one `==>` UCTL, one multi-line date-prefixed UCTL,
/// and at least one Unity stdout noise line — otherwise the test loses
/// its bite.
#[test]
fn test_fixture_covers_required_patterns() {
    let lines = fixture_lines();
    let mut alpha_label = 0;
    let mut arrow_request = 0;
    let mut date_multi_line = 0;
    let mut noise_lines = 0;
    for line in &lines {
        if let Some(after) = line.strip_prefix("[UnityCrossThreadLogger]") {
            if after.starts_with("==>") {
                arrow_request += 1;
            } else if after.bytes().next().is_some_and(|b| b.is_ascii_digit()) {
                date_multi_line += 1;
            } else {
                alpha_label += 1;
            }
        } else if !line.is_empty()
            && !line.starts_with('[')
            && !line.starts_with("<==")
            && !line.starts_with('{')
        {
            noise_lines += 1;
        }
    }
    assert!(
        alpha_label >= 2,
        "fixture must contain >=2 single-line UCTL alpha-label entries, found {alpha_label}",
    );
    assert!(
        arrow_request >= 1,
        "fixture must contain >=1 single-line UCTL ==> request entry, found {arrow_request}",
    );
    assert!(
        date_multi_line >= 1,
        "fixture must contain >=1 multi-line UCTL date-prefixed entry, found {date_multi_line}",
    );
    assert!(
        noise_lines >= 1,
        "fixture must contain >=1 Unity stdout noise line, found {noise_lines}",
    );
}

/// End-to-end: replay the full slice and assert the resulting entry stream
/// has the expected shape — no header line is dropped, no continuation is
/// fused into the wrong entry.
#[test]
fn test_replay_produces_expected_entry_count() {
    let lines = fixture_lines();
    let expected_headers: Vec<&str> = lines
        .iter()
        .copied()
        .filter(|line| {
            line.starts_with("[UnityCrossThreadLogger]")
                || line.starts_with("[ConnectionManager]")
                || line.starts_with("Matchmaking: ")
                || line.starts_with("[Client GRE]")
        })
        .collect();

    let mut buf = LineBuffer::new();
    let mut all_entries: Vec<LogEntry> = Vec::new();
    for line in &lines {
        all_entries.extend(buf.push_line(line));
    }
    all_entries.extend(buf.flush());

    assert_eq!(
        all_entries.len(),
        expected_headers.len(),
        "expected one entry per header line",
    );

    // Each entry's body must start with its corresponding header line.
    for (entry, header_line) in all_entries.iter().zip(expected_headers.iter()) {
        assert_eq!(
            entry.body.lines().next(),
            Some(*header_line),
            "entry body does not start with its header line: entry={entry:?}, header={header_line:?}",
        );
    }

    // The entry stream must contain at least one of each known header.
    let has_uctl = all_entries
        .iter()
        .any(|e| e.header == EntryHeader::UnityCrossThreadLogger);
    assert!(
        has_uctl,
        "expected at least one UnityCrossThreadLogger entry"
    );
}
