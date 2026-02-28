//! Parser-only real-log smoke test (Level 1).
//!
//! Feeds log entries through individual `try_parse` functions with per-parser
//! attribution. Detects panics, double claims, and timestamp extraction failures.
//!
//! Gated on the `MANASIGHT_TEST_LOGS` environment variable.
//!
//! ```bash
//! MANASIGHT_TEST_LOGS=/path/to/logs cargo test smoke_parsers -- --nocapture
//! ```

mod smoke_common;

use std::collections::HashMap;
use std::fmt::Write as FmtWrite;
use std::path::Path;

use manasight_parser::log::entry::LineBuffer;
use manasight_parser::log::timestamp::parse_log_timestamp;

use smoke_common::{all_parsers, event_type_name, NamedParser, ParserStats};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Aggregated report for a single log file.
struct FileReport {
    filename: String,
    /// Set when the file could not be read; remaining fields are zeroed.
    read_error: bool,
    total_entries: usize,
    parser_stats: Vec<(&'static str, ParserStats)>,
    /// Counts of claimed events broken down by `GameEvent` variant name.
    event_type_counts: HashMap<&'static str, usize>,
    unclaimed: usize,
    double_claims: usize,
    timestamp_failures: usize,
}

// ---------------------------------------------------------------------------
// Timestamp extraction
// ---------------------------------------------------------------------------

/// Best-effort timestamp extraction from a log entry body.
///
/// Strips the bracket header prefix from the first line and tries to parse
/// the remaining text as a timestamp. If the full text doesn't parse
/// (e.g. timestamp followed by event content on the same line), tries
/// progressively shorter prefixes.
fn try_extract_timestamp(body: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    let first_line = body.lines().next()?;
    let content = match first_line.find(']') {
        Some(pos) => first_line.get(pos + 1..)?.trim(),
        None => return None,
    };

    if content.is_empty() {
        return None;
    }

    // Try full content (handles timestamp-only first lines).
    if let Ok(ts) = parse_log_timestamp(content) {
        return Some(ts);
    }

    // Try progressively shorter prefixes for lines with timestamp + content.
    // MTGA timestamps are typically 18–25 characters.
    let max_len = content.len().min(30);
    for len in (15..=max_len).rev() {
        if content.is_char_boundary(len) {
            if let Some(candidate) = content.get(..len) {
                if let Ok(ts) = parse_log_timestamp(candidate.trim_end()) {
                    return Some(ts);
                }
            }
        }
    }

    None
}

// ---------------------------------------------------------------------------
// Known overlapping parsers
// ---------------------------------------------------------------------------

/// Returns `true` if the set of claimant parser names represents a known,
/// expected overlap rather than a bug.
///
/// `<== StartHook` responses contain both `InventoryInfo` and `PlayerCards`,
/// so the `inventory` and `collection` parsers legitimately claim the same
/// entry — each extracting different data from the shared response.
fn is_known_overlap(claimants: &[&str]) -> bool {
    claimants.len() == 2 && claimants.contains(&"inventory") && claimants.contains(&"collection")
}

// ---------------------------------------------------------------------------
// File processing
// ---------------------------------------------------------------------------

/// Processes a single log file through all parsers and returns a report.
fn process_file(path: &Path, parsers: &[NamedParser]) -> FileReport {
    let filename = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();

    let Ok(content) = std::fs::read_to_string(path) else {
        return FileReport {
            filename,
            read_error: true,
            total_entries: 0,
            parser_stats: parsers
                .iter()
                .map(|p| (p.name, ParserStats::default()))
                .collect(),
            event_type_counts: HashMap::new(),
            unclaimed: 0,
            double_claims: 0,
            timestamp_failures: 0,
        };
    };

    // Split content into entries via LineBuffer.
    let mut buffer = LineBuffer::new();
    let mut entries = Vec::new();
    for line in content.lines() {
        if let Some(entry) = buffer.push_line(line) {
            entries.push(entry);
        }
    }
    if let Some(entry) = buffer.flush() {
        entries.push(entry);
    }

    let total_entries = entries.len();
    let mut stats: Vec<(&str, ParserStats)> = parsers
        .iter()
        .map(|p| (p.name, ParserStats::default()))
        .collect();
    let mut event_type_counts: HashMap<&'static str, usize> = HashMap::new();
    let mut unclaimed: usize = 0;
    let mut double_claims: usize = 0;
    let mut timestamp_failures: usize = 0;

    for entry in &entries {
        let timestamp = try_extract_timestamp(&entry.body);

        if timestamp.is_none() {
            timestamp_failures += 1;
        }

        let mut claimant_count: usize = 0;
        let mut claimant_names: Vec<&'static str> = Vec::new();

        for (idx, parser) in parsers.iter().enumerate() {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                (parser.func)(entry, timestamp)
            }));

            match result {
                Ok(Some(event)) => {
                    stats[idx].1.claimed += 1;
                    claimant_count += 1;
                    claimant_names.push(parser.name);
                    *event_type_counts
                        .entry(event_type_name(&event))
                        .or_insert(0) += 1;
                }
                Ok(None) => {}
                Err(_) => {
                    stats[idx].1.panics += 1;
                }
            }
        }

        match claimant_count {
            0 => unclaimed += 1,
            1 => {}
            _ => {
                if !is_known_overlap(&claimant_names) {
                    double_claims += 1;
                }
            }
        }
    }

    FileReport {
        filename,
        read_error: false,
        total_entries,
        parser_stats: stats,
        event_type_counts,
        unclaimed,
        double_claims,
        timestamp_failures,
    }
}

// ---------------------------------------------------------------------------
// Report formatting
// ---------------------------------------------------------------------------

/// Formats all file reports into a human-readable summary.
fn format_report(reports: &[FileReport]) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "\n=== Smoke Test Report ===\n");

    let mut total_panics: usize = 0;
    let mut total_double_claims: usize = 0;

    for report in reports {
        if report.read_error {
            let _ = writeln!(
                out,
                "File: {} — READ ERROR (unreadable file)",
                report.filename
            );
            let _ = writeln!(out);
            continue;
        }

        let _ = writeln!(
            out,
            "File: {} ({} entries)",
            report.filename, report.total_entries,
        );

        for (name, stats) in &report.parser_stats {
            let label = format!("{name}:");
            let _ = writeln!(
                out,
                "  {label:<18} {claimed:>6} claimed, {panics:>3} panics",
                claimed = stats.claimed,
                panics = stats.panics,
            );
            total_panics += stats.panics;
        }

        let _ = writeln!(out, "  Event type breakdown:");
        let mut sorted_types: Vec<(&&'static str, &usize)> =
            report.event_type_counts.iter().collect();
        sorted_types.sort_by_key(|(name, _)| **name);
        for (type_name, count) in &sorted_types {
            let label = format!("    {type_name}:");
            let _ = writeln!(out, "  {label:<18} {count:>6}");
        }
        if sorted_types.is_empty() {
            let _ = writeln!(out, "    (none)");
        }

        let _ = writeln!(out, "  {:<18} {:>6}", "unclaimed:", report.unclaimed);
        let _ = writeln!(
            out,
            "  {:<18} {:>6}",
            "double_claims:", report.double_claims,
        );
        let _ = writeln!(
            out,
            "  {:<18} {:>6}",
            "ts_failures:", report.timestamp_failures,
        );
        let _ = writeln!(out);

        total_double_claims += report.double_claims;
    }

    let status = if total_panics == 0 && total_double_claims == 0 {
        "PASS"
    } else {
        "FAIL"
    };
    let _ = writeln!(out, "=== {status} ===");

    out
}

// ---------------------------------------------------------------------------
// Test entry point
// ---------------------------------------------------------------------------

#[test]
fn smoke_test_real_logs() {
    let Some(logs_dir) = smoke_common::logs_dir_or_skip("smoke_test_real_logs") else {
        return;
    };

    let log_files = smoke_common::assert_logs_dir(&logs_dir);

    // Process each file.
    let parsers = all_parsers();
    let reports: Vec<FileReport> = log_files
        .iter()
        .map(|path| process_file(path, &parsers))
        .collect();

    // Build and output the report.
    let report = format_report(&reports);
    let _ = std::io::Write::write_all(&mut std::io::stdout(), report.as_bytes());

    // Aggregate totals for assertions.
    let read_errors: usize = reports.iter().filter(|r| r.read_error).count();
    let total_panics: usize = reports
        .iter()
        .flat_map(|r| r.parser_stats.iter())
        .map(|(_, s)| s.panics)
        .sum();
    let total_double_claims: usize = reports.iter().map(|r| r.double_claims).sum();

    assert_eq!(
        read_errors, 0,
        "unreadable log files detected \u{2014} see report above"
    );
    assert_eq!(
        total_panics, 0,
        "parser panics detected \u{2014} see report above"
    );
    assert_eq!(
        total_double_claims, 0,
        "double claims detected \u{2014} see report above"
    );
}
