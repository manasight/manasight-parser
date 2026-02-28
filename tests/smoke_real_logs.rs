//! Multi-level real-log smoke test harness.
//!
//! Three levels of smoke testing, each progressively more integrated:
//!
//! 1. **Parser-only** (`smoke_test_real_logs`): feeds entries through
//!    individual `try_parse` functions with per-parser attribution.
//! 2. **Router-level** (`smoke_test_router_real_logs`): feeds entries
//!    through `Router::route()` which handles timestamp extraction and
//!    parser dispatch internally.
//! 3. **Stream-level** (`smoke_test_stream_real_logs`): full async
//!    pipeline via `MtgaEventStream::start_once()`.
//!
//! # Gating
//!
//! All tests are gated on the `MANASIGHT_TEST_LOGS` environment variable.
//! When **set**, it points to a directory of `.log` files that are processed.
//! When **unset**, the tests return early (pass) with a skip message.
//!
//! ```bash
//! MANASIGHT_TEST_LOGS=/path/to/logs cargo test smoke -- --nocapture
//! ```

use std::collections::HashMap;
use std::fmt::Write as FmtWrite;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};

use manasight_parser::events::GameEvent;
use manasight_parser::log::entry::{LineBuffer, LogEntry};
use manasight_parser::log::timestamp::parse_log_timestamp;
use manasight_parser::parsers;
use manasight_parser::router::Router;
use manasight_parser::stream::MtgaEventStream;

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
    func: fn(&LogEntry, Option<DateTime<Utc>>) -> Option<GameEvent>,
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
    /// Counts of claimed events broken down by `GameEvent` variant name.
    event_type_counts: HashMap<&'static str, usize>,
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
        NamedParser {
            name: "inventory",
            func: parsers::inventory::try_parse,
        },
        NamedParser {
            name: "collection",
            func: parsers::collection::try_parse,
        },
        NamedParser {
            name: "rank",
            func: parsers::rank::try_parse,
        },
        NamedParser {
            name: "event_lifecycle",
            func: parsers::event_lifecycle::try_parse,
        },
    ]
}

// ---------------------------------------------------------------------------
// Event type name
// ---------------------------------------------------------------------------

/// Returns the variant name of a `GameEvent` as a `'static str`.
fn event_type_name(event: &GameEvent) -> &'static str {
    match event {
        GameEvent::GameState(_) => "GameState",
        GameEvent::ClientAction(_) => "ClientAction",
        GameEvent::MatchState(_) => "MatchState",
        GameEvent::DraftBot(_) => "DraftBot",
        GameEvent::DraftHuman(_) => "DraftHuman",
        GameEvent::DraftComplete(_) => "DraftComplete",
        GameEvent::EventLifecycle(_) => "EventLifecycle",
        GameEvent::Session(_) => "Session",
        GameEvent::Rank(_) => "Rank",
        GameEvent::Collection(_) => "Collection",
        GameEvent::Inventory(_) => "Inventory",
        GameEvent::GameResult(_) => "GameResult",
        // `GameEvent` is `#[non_exhaustive]`; this branch keeps the compiler
        // happy if new variants are added before this match is updated.
        _ => "Unknown",
    }
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
// Shared helpers
// ---------------------------------------------------------------------------

/// Returns the logs directory from `MANASIGHT_TEST_LOGS`, or `None` if unset.
///
/// When `None`, callers should print a skip message and return early.
fn logs_dir_or_skip(test_name: &str) -> Option<PathBuf> {
    if let Ok(dir) = std::env::var(ENV_VAR) {
        Some(PathBuf::from(dir))
    } else {
        let msg = format!("{ENV_VAR} not set \u{2014} skipping {test_name}\n");
        let _ = std::io::Write::write_all(&mut std::io::stdout(), msg.as_bytes());
        None
    }
}

/// Discovers `.log` files in a directory, sorted alphabetically.
fn discover_log_files(dir: &Path) -> Vec<PathBuf> {
    let mut log_files: Vec<PathBuf> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("log") {
                log_files.push(path);
            }
        }
    }
    log_files.sort();
    log_files
}

/// Reads a log file and splits it into `LogEntry` objects via `LineBuffer`.
fn read_entries(path: &Path) -> Option<Vec<LogEntry>> {
    let content = std::fs::read_to_string(path).ok()?;
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
    Some(entries)
}

// ---------------------------------------------------------------------------
// Test entry point: Parser-only (Level 1)
// ---------------------------------------------------------------------------

#[test]
fn smoke_test_real_logs() {
    let Some(logs_dir) = logs_dir_or_skip("smoke_test_real_logs") else {
        return;
    };

    assert!(
        logs_dir.is_dir(),
        "{ENV_VAR} is not a directory: {}",
        logs_dir.display(),
    );

    let log_files = discover_log_files(&logs_dir);

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

// ---------------------------------------------------------------------------
// Router-level smoke test (Level 2)
// ---------------------------------------------------------------------------

/// Report for a single file processed through the Router.
struct RouterFileReport {
    filename: String,
    total_entries: usize,
    routed: u64,
    unknown: u64,
    timestamp_failures: u64,
    event_type_counts: HashMap<&'static str, usize>,
}

/// Processes a single log file through `Router::route()`.
fn process_file_router(path: &Path) -> Option<RouterFileReport> {
    let filename = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();

    let entries = read_entries(path)?;
    let total_entries = entries.len();
    let router = Router::new();
    let mut event_type_counts: HashMap<&'static str, usize> = HashMap::new();

    for entry in &entries {
        if let Some(event) = router.route(entry) {
            *event_type_counts
                .entry(event_type_name(&event))
                .or_insert(0) += 1;
        }
    }

    Some(RouterFileReport {
        filename,
        total_entries,
        routed: router.stats().routed_count(),
        unknown: router.stats().unknown_count(),
        timestamp_failures: router.stats().timestamp_failure_count(),
        event_type_counts,
    })
}

/// Formats router-level reports into a human-readable summary.
fn format_router_report(reports: &[RouterFileReport]) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "\n=== Router-Level Smoke Test Report ===\n");

    for report in reports {
        let _ = writeln!(
            out,
            "File: {} ({} entries)",
            report.filename, report.total_entries,
        );
        let _ = writeln!(out, "  {:<18} {:>6}", "routed:", report.routed);
        let _ = writeln!(out, "  {:<18} {:>6}", "unknown:", report.unknown);
        let _ = writeln!(
            out,
            "  {:<18} {:>6}",
            "ts_failures:", report.timestamp_failures,
        );
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
        let _ = writeln!(out);
    }

    let _ = writeln!(out, "=== PASS ===");
    out
}

#[test]
fn smoke_test_router_real_logs() {
    let Some(logs_dir) = logs_dir_or_skip("smoke_test_router_real_logs") else {
        return;
    };

    assert!(
        logs_dir.is_dir(),
        "{ENV_VAR} is not a directory: {}",
        logs_dir.display(),
    );

    let log_files = discover_log_files(&logs_dir);

    assert!(
        !log_files.is_empty(),
        "no .log files found in {}",
        logs_dir.display(),
    );

    let mut reports: Vec<RouterFileReport> = Vec::new();
    for path in &log_files {
        let report = process_file_router(path);
        assert!(
            report.is_some(),
            "failed to read log file: {}",
            path.display()
        );
        if let Some(r) = report {
            reports.push(r);
        }
    }

    let report = format_router_report(&reports);
    let _ = std::io::Write::write_all(&mut std::io::stdout(), report.as_bytes());
}

// ---------------------------------------------------------------------------
// Stream-level smoke test (Level 3)
// ---------------------------------------------------------------------------

/// Report for a single file processed through the full `MtgaEventStream` pipeline.
struct StreamFileReport {
    filename: String,
    total_events: usize,
    event_type_counts: HashMap<&'static str, usize>,
}

/// Processes a single log file through the full async pipeline.
///
/// Returns `None` if the file cannot be opened.
async fn process_file_stream(path: &Path) -> Option<StreamFileReport> {
    let filename = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();

    let Ok((_stream, mut subscriber)) = MtgaEventStream::start_once(path).await else {
        return None;
    };

    let mut event_type_counts: HashMap<&'static str, usize> = HashMap::new();
    let mut total_events: usize = 0;

    while let Some(event) = subscriber.recv().await {
        total_events += 1;
        *event_type_counts
            .entry(event_type_name(&event))
            .or_insert(0) += 1;
    }

    Some(StreamFileReport {
        filename,
        total_events,
        event_type_counts,
    })
}

/// Formats stream-level reports into a human-readable summary.
fn format_stream_report(reports: &[StreamFileReport]) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "\n=== Stream-Level Smoke Test Report ===\n");

    for report in reports {
        let _ = writeln!(
            out,
            "File: {} ({} events)",
            report.filename, report.total_events,
        );
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
        let _ = writeln!(out);
    }

    let _ = writeln!(out, "=== PASS ===");
    out
}

#[tokio::test]
async fn smoke_test_stream_real_logs() {
    let Some(logs_dir) = logs_dir_or_skip("smoke_test_stream_real_logs") else {
        return;
    };

    assert!(
        logs_dir.is_dir(),
        "{ENV_VAR} is not a directory: {}",
        logs_dir.display(),
    );

    let log_files = discover_log_files(&logs_dir);

    assert!(
        !log_files.is_empty(),
        "no .log files found in {}",
        logs_dir.display(),
    );

    let mut reports: Vec<StreamFileReport> = Vec::new();
    for path in &log_files {
        let report = process_file_stream(path).await;
        assert!(
            report.is_some(),
            "failed to open log file: {}",
            path.display()
        );
        if let Some(r) = report {
            reports.push(r);
        }
    }

    let report = format_stream_report(&reports);
    let _ = std::io::Write::write_all(&mut std::io::stdout(), report.as_bytes());
}
