//! Per-match parse CPU + latency measurement (A-6, manasight/manasight-docs#817).
//!
//! Measures the CPU cost of [`manasight_parser::parse_whole_log`] over real
//! corpus log files, reporting per-blob and per-match timing to stdout.
//!
//! This is the dominant, measurable component of server-side on-demand
//! action-log regeneration: the server reads the stored blob from object
//! storage (outside the CPU clock), then runs the WASM-compiled parser over
//! the decompressed bytes.
//!
//! **⚠️ Native ≠ WASM caveat.** The numbers emitted by this test are a
//! *lower bound* on the deployed `wasm32` CPU time, which is roughly 1.5–3×
//! slower than a native `--release` build; the server CPU budget is sized as
//! headroom, not the target. A `wasm32` timing pass in the deployed runtime
//! is a follow-up task.
//!
//! ## Gating
//!
//! Gated on the `MANASIGHT_TEST_LOGS` environment variable. When unset the
//! test prints a skip message and passes immediately (zero assertions).
//!
//! ## Running
//!
//! ```bash
//! MANASIGHT_TEST_LOGS=/path/to/corpus cargo test --release corpus_parse_timing -- --nocapture
//! ```
//!
//! Files larger than ~10 MB are the primary worst-case targets, but the test
//! iterates all `.log` files discovered in the corpus directory.

mod smoke_common;

use std::fmt::Write as FmtWrite;
use std::time::Instant;

use manasight_parser::{parse_whole_log, GameEvent};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Timing measurements for a single corpus blob.
struct BlobTiming {
    filename: String,
    bytes: usize,
    elapsed_ms: f64,
    /// Number of completed matches found in this blob.
    ///
    /// Stored as `u32` (not `usize`) so that conversion to `f64` for
    /// per-match arithmetic is lossless (f64 mantissa covers all u32 values).
    match_count: u32,
}

impl BlobTiming {
    /// Per-match milliseconds, or `None` when no completed matches were found.
    fn per_match_ms(&self) -> Option<f64> {
        if self.match_count == 0 {
            None
        } else {
            // u32 → f64 is lossless (all u32 values are representable exactly).
            Some(self.elapsed_ms / f64::from(self.match_count))
        }
    }
}

// ---------------------------------------------------------------------------
// Measurement helpers
// ---------------------------------------------------------------------------

/// Counts completed matches in an event slice.
///
/// A completed match is a `GameEvent::MatchState` whose payload carries
/// `type == "match_completed"` (set by the match-state parser when the
/// Arena room state is `MatchGameRoomStateType_MatchCompleted`).
///
/// Returns `u32` so that conversion to `f64` for per-match arithmetic
/// is lossless.
fn count_completed_matches(events: &[GameEvent]) -> u32 {
    events
        .iter()
        .filter(|e| {
            if let GameEvent::MatchState(ms) = e {
                ms.payload()["type"] == "match_completed"
            } else {
                false
            }
        })
        .fold(0u32, |acc, _| acc.saturating_add(1))
}

/// Reads a corpus file to a `String` and times only the parse step.
///
/// I/O (`std::fs::read_to_string`) is performed *outside* the timed region
/// to match the deployment model: the server reads the blob from object
/// storage before the CPU clock starts, then runs the WASM parser over the
/// in-memory bytes.
///
/// Returns `None` if the file cannot be read.
fn measure_blob(path: &std::path::Path) -> Option<BlobTiming> {
    let filename = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();

    // I/O is outside the timed region.
    let content = std::fs::read_to_string(path).ok()?;
    let bytes = content.len();

    // Time only parse_whole_log.
    let start = Instant::now();
    let events = parse_whole_log(&content);
    let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;

    let match_count = count_completed_matches(&events);

    Some(BlobTiming {
        filename,
        bytes,
        elapsed_ms,
        match_count,
    })
}

// ---------------------------------------------------------------------------
// Report formatting
// ---------------------------------------------------------------------------

/// Formats a bytes count as a human-readable MB/KB string.
fn fmt_bytes(bytes: usize) -> String {
    // Divide first so the value fits in u32 (lossless to f64) before
    // converting — avoids cast_precision_loss on 64-bit targets.
    if bytes >= 1_000_000 {
        let mb_tenths = bytes / 100_000; // tenths of MB
        format!("{}.{} MB", mb_tenths / 10, mb_tenths % 10)
    } else if bytes >= 1_000 {
        let kb_tenths = bytes / 100; // tenths of KB
        format!("{}.{} KB", kb_tenths / 10, kb_tenths % 10)
    } else {
        format!("{bytes} B")
    }
}

/// Builds the timing report string from a slice of blob measurements.
fn format_timing_report(timings: &[BlobTiming]) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "\n=== Corpus Parse Timing Report ===\n");
    let _ = writeln!(
        out,
        "{:<52}  {:>8}  {:>10}  {:>8}  {:>12}",
        "File", "Size", "Total ms", "Matches", "Per-match ms",
    );
    let _ = writeln!(out, "{}", "-".repeat(98));

    for t in timings {
        let per_match = t
            .per_match_ms()
            .map_or_else(|| "n/a".to_owned(), |ms| format!("{ms:.1}"));
        let _ = writeln!(
            out,
            "{:<52}  {:>8}  {:>10.1}  {:>8}  {:>12}",
            t.filename,
            fmt_bytes(t.bytes),
            t.elapsed_ms,
            t.match_count,
            per_match,
        );
    }

    // Worst-case line: blob with highest per-match ms among blobs that have
    // at least one completed match.
    let worst = timings.iter().filter(|t| t.match_count > 0).max_by(|a, b| {
        a.per_match_ms()
            .unwrap_or(0.0)
            .partial_cmp(&b.per_match_ms().unwrap_or(0.0))
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let _ = writeln!(out);
    if let Some(w) = worst {
        let _ = writeln!(
            out,
            "Worst-case per-match: {:.1} ms/match  [{} · {} · {} matches · {:.1} ms total]",
            w.per_match_ms().unwrap_or(0.0),
            w.filename,
            fmt_bytes(w.bytes),
            w.match_count,
            w.elapsed_ms,
        );
        let _ = writeln!(
            out,
            "  (native --release; wasm32 in the deployed runtime is ~1.5–3× slower)"
        );
    } else {
        let _ = writeln!(
            out,
            "Worst-case: no completed matches found in any blob — check corpus contents."
        );
    }

    let _ = writeln!(out, "\n=== PASS ===");
    out
}

// ---------------------------------------------------------------------------
// Test entry point
// ---------------------------------------------------------------------------

#[test]
fn corpus_parse_timing() {
    let Some(logs_dir) = smoke_common::logs_dir_or_skip("corpus_parse_timing") else {
        return;
    };

    let log_files = smoke_common::assert_logs_dir(&logs_dir);

    let mut timings: Vec<BlobTiming> = Vec::new();
    for path in &log_files {
        let Some(t) = measure_blob(path) else {
            let msg = format!(
                "corpus_parse_timing: could not read {}, skipping\n",
                path.display()
            );
            let _ = std::io::Write::write_all(&mut std::io::stdout(), msg.as_bytes());
            continue;
        };
        timings.push(t);
    }

    assert!(
        !timings.is_empty(),
        "no corpus files could be measured — check MANASIGHT_TEST_LOGS path"
    );

    let report = format_timing_report(&timings);
    let _ = std::io::Write::write_all(&mut std::io::stdout(), report.as_bytes());
}
