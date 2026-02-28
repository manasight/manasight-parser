//! Stream-level real-log smoke test (Level 3).
//!
//! Full async pipeline via `MtgaEventStream::start_once()`.
//!
//! Gated on the `MANASIGHT_TEST_LOGS` environment variable.
//!
//! ```bash
//! MANASIGHT_TEST_LOGS=/path/to/logs cargo test smoke_stream -- --nocapture
//! ```

mod smoke_common;

use std::collections::HashMap;
use std::fmt::Write as FmtWrite;
use std::path::Path;

use manasight_parser::stream::MtgaEventStream;

use smoke_common::event_type_name;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Report for a single file processed through the full `MtgaEventStream` pipeline.
struct StreamFileReport {
    filename: String,
    total_events: usize,
    event_type_counts: HashMap<&'static str, usize>,
}

// ---------------------------------------------------------------------------
// File processing
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Report formatting
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Test entry point
// ---------------------------------------------------------------------------

#[tokio::test]
async fn smoke_test_stream_real_logs() {
    let Some(logs_dir) = smoke_common::logs_dir_or_skip("smoke_test_stream_real_logs") else {
        return;
    };

    let log_files = smoke_common::assert_logs_dir(&logs_dir);

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
