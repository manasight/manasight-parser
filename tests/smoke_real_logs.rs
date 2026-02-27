//! Real-log smoke test harness with per-parser attribution.
//!
//! Feeds saved Player.log files through every implemented parser and produces
//! a per-parser report: claim counts, panics, double claims, and unclaimed
//! entries.
//!
//! # Gating
//!
//! The test is gated on the `MANASIGHT_TEST_LOGS` environment variable.
//! When **set**, it points to a directory of `.log` files that are processed.
//! When **unset**, the test returns early (passes) with a skip message.
//!
//! ```bash
//! MANASIGHT_TEST_LOGS=/path/to/logs cargo test smoke -- --nocapture
//! ```

use std::fmt::Write as FmtWrite;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};

use manasight_parser::events::GameEvent;
use manasight_parser::log::entry::{LineBuffer, LogEntry};
use manasight_parser::log::timestamp::parse_log_timestamp;
use manasight_parser::parsers;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const ENV_VAR: &str = "MANASIGHT_TEST_LOGS";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// A parser identified by name and its `try_parse` function pointer.
struct NamedParser {
    name: &'static str,
    func: fn(&LogEntry, DateTime<Utc>) -> Option<GameEvent>,
}

/// Per-parser statistics accumulated while processing a single log file.
#[derive(Default)]
struct ParserStats {
    /// Entries successfully claimed (returned `Some`).
    claimed: usize,
    /// Entries where `try_parse` panicked (caught by `catch_unwind`).
    panics: usize,
}

/// Aggregated report for a single log file.
struct FileReport {
    filename: String,
    /// Set when the file could not be read; remaining fields are zeroed.
    read_error: bool,
    total_entries: usize,
    parser_stats: Vec<(&'static str, ParserStats)>,
    unclaimed: usize,
    double_claims: usize,
    timestamp_failures: usize,
}

// ---------------------------------------------------------------------------
// Parser registry
// ---------------------------------------------------------------------------

/// Returns all implemented parsers in dispatch order.
fn all_parsers() -> Vec<NamedParser> {
    vec![
        NamedParser {
            name: "session",
            func: parsers::session::try_parse,
        },
        NamedParser {
            name: "match_state",
            func: parsers::match_state::try_parse,
        },
        NamedParser {
            name: "gre",
            func: parsers::gre::try_parse,
        },
        NamedParser {
            name: "client_actions",
            func: parsers::client_actions::try_parse,
        },
        NamedParser {
            name: "game_result",
            func: parsers::game_result::try_parse,
        },
        NamedParser {
            name: "draft_bot",
            func: parsers::draft::bot::try_parse,
        },
        NamedParser {
            name: "draft_human",
            func: parsers::draft::human::try_parse,
        },
        NamedParser {
            name: "draft_complete",
            func: parsers::draft::complete::try_parse,
        },
    ]
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
fn try_extract_timestamp(body: &str) -> Option<DateTime<Utc>> {
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
    let mut unclaimed: usize = 0;
    let mut double_claims: usize = 0;
    let mut timestamp_failures: usize = 0;

    let default_ts = DateTime::<Utc>::default();

    for entry in &entries {
        let timestamp = if let Some(ts) = try_extract_timestamp(&entry.body) {
            ts
        } else {
            timestamp_failures += 1;
            default_ts
        };

        let mut claimant_count: usize = 0;

        for (idx, parser) in parsers.iter().enumerate() {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                (parser.func)(entry, timestamp)
            }));

            match result {
                Ok(Some(_)) => {
                    stats[idx].1.claimed += 1;
                    claimant_count += 1;
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
            _ => double_claims += 1,
        }
    }

    FileReport {
        filename,
        read_error: false,
        total_entries,
        parser_stats: stats,
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
    let logs_dir = if let Ok(dir) = std::env::var(ENV_VAR) {
        PathBuf::from(dir)
    } else {
        let msg = format!("{ENV_VAR} not set \u{2014} skipping smoke test\n");
        let _ = std::io::Write::write_all(&mut std::io::stdout(), msg.as_bytes());
        return;
    };

    assert!(
        logs_dir.is_dir(),
        "{ENV_VAR} is not a directory: {}",
        logs_dir.display(),
    );

    // Discover .log files (excludes .log.gz).
    let mut log_files: Vec<PathBuf> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&logs_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("log") {
                log_files.push(path);
            }
        }
    }
    log_files.sort();

    assert!(
        !log_files.is_empty(),
        "no .log files found in {}",
        logs_dir.display(),
    );

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
