//! Router-level real-log smoke test (Level 2).
//!
//! Feeds log entries through `Router::route()` which handles timestamp
//! extraction and parser dispatch internally.
//!
//! Gated on the `MANASIGHT_TEST_LOGS` environment variable.
//!
//! ```bash
//! MANASIGHT_TEST_LOGS=/path/to/logs cargo test smoke_router -- --nocapture
//! ```

mod smoke_common;

use std::collections::HashMap;
use std::fmt::Write as FmtWrite;
use std::path::Path;

use manasight_parser::router::Router;

use smoke_common::{event_type_name, read_entries};

// ---------------------------------------------------------------------------
// Types
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

// ---------------------------------------------------------------------------
// File processing
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Report formatting
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Test entry point
// ---------------------------------------------------------------------------

#[test]
fn smoke_test_router_real_logs() {
    let Some(logs_dir) = smoke_common::logs_dir_or_skip("smoke_test_router_real_logs") else {
        return;
    };

    let log_files = smoke_common::assert_logs_dir(&logs_dir);

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
