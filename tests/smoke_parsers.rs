//! Parser-only real-log smoke test (Level 1).
//!
//! Feeds log entries through individual `try_parse` functions with per-parser
//! attribution. Detects panics, double claims, and timestamp extraction failures.
//!
//! After processing, results are compared against `smoke-baseline.json` with
//! ratchet semantics: regressions fail, improvements pass. Set `SMOKE_BLESS=1`
//! to overwrite the baseline instead of comparing.
//!
//! Gated on the `MANASIGHT_TEST_LOGS` environment variable.
//!
//! ```bash
//! MANASIGHT_TEST_LOGS=/path/to/logs cargo test smoke_parsers -- --nocapture
//! SMOKE_BLESS=1 CORPUS_TAG=manasight-corpus-vN MANASIGHT_TEST_LOGS=/path/to/logs cargo test smoke_parsers -- --nocapture
//! ```

mod smoke_common;

use std::collections::HashMap;
use std::fmt::Write as FmtWrite;
use std::path::Path;

use manasight_parser::events::GameEvent;
use manasight_parser::log::entry::LineBuffer;
use manasight_parser::log::timestamp::parse_log_timestamp;

use smoke_common::{
    all_parsers, compare_against_baseline, event_type_name, is_bless_mode, read_baseline,
    write_baseline, Baseline, BaselineFile, BaselineMeta, NamedParser, ParserStats,
};

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
    /// Number of `GameStateMessage` events with `turn_info` present (non-null).
    gsm_turn_info_present: usize,
    /// Number of `GameStateMessage` events with `turn_info` absent or null.
    gsm_turn_info_absent: usize,
    /// Number of GSM events with at least one annotation.
    gsm_annotations_present: usize,
    /// Total annotation count across all GSM events.
    gsm_annotations_total: usize,
    /// Annotation counts broken down by primary type string.
    gsm_annotation_type_counts: HashMap<String, usize>,
    /// Number of GSM events with at least one timer.
    gsm_timers_present: usize,
    /// Total timer count across all GSM events.
    gsm_timers_total: usize,
    /// Number of GSM events with at least one `diff_deleted_instance_id`.
    gsm_diff_deleted_present: usize,
    /// Total `diff_deleted_instance_ids` count across all GSM events.
    gsm_diff_deleted_total: usize,
}

impl FileReport {
    /// Converts this report into a `BaselineFile` for ratchet comparison.
    fn to_baseline_file(&self) -> BaselineFile {
        let parsers: std::collections::BTreeMap<String, u64> = self
            .parser_stats
            .iter()
            .map(|(name, stats)| ((*name).to_string(), stats.claimed as u64))
            .collect();

        let event_types: std::collections::BTreeMap<String, u64> = self
            .event_type_counts
            .iter()
            .map(|(name, count)| ((*name).to_string(), *count as u64))
            .collect();

        BaselineFile {
            total_entries: self.total_entries as u64,
            parsers,
            event_types,
            unclaimed: self.unclaimed as u64,
            double_claims: self.double_claims as u64,
            timestamp_failures: self.timestamp_failures as u64,
        }
    }
}

// ---------------------------------------------------------------------------
// GSM field tracking
// ---------------------------------------------------------------------------

/// Tracks presence and counts of fields within `GameStateMessage` payloads.
#[derive(Default)]
struct GsmFieldStats {
    turn_info_present: usize,
    turn_info_absent: usize,
    annotations_present: usize,
    annotations_total: usize,
    annotation_type_counts: HashMap<String, usize>,
    timers_present: usize,
    timers_total: usize,
    diff_deleted_present: usize,
    diff_deleted_total: usize,
}

impl GsmFieldStats {
    /// Updates stats from a single `GameState` event payload.
    fn track(&mut self, payload: &serde_json::Value) {
        // turn_info
        let ti = payload.get("turn_info");
        if ti.is_some_and(|v| !v.is_null()) {
            self.turn_info_present += 1;
        } else {
            self.turn_info_absent += 1;
        }

        // annotations
        if let Some(anns) = payload.get("annotations").and_then(|v| v.as_array()) {
            let count = anns.len();
            if count > 0 {
                self.annotations_present += 1;
            }
            self.annotations_total += count;
            for ann in anns {
                if let Some(t) = ann.get("type").and_then(|v| v.as_str()) {
                    *self
                        .annotation_type_counts
                        .entry(t.to_string())
                        .or_insert(0) += 1;
                }
            }
        }

        // timers
        if let Some(timers) = payload.get("timers").and_then(|v| v.as_array()) {
            let count = timers.len();
            if count > 0 {
                self.timers_present += 1;
            }
            self.timers_total += count;
        }

        // diff_deleted_instance_ids
        if let Some(ids) = payload
            .get("diff_deleted_instance_ids")
            .and_then(|v| v.as_array())
        {
            let count = ids.len();
            if count > 0 {
                self.diff_deleted_present += 1;
            }
            self.diff_deleted_total += count;
        }
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
    // MTGA timestamps are typically 18-25 characters.
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
/// entry -- each extracting different data from the shared response.
fn is_known_overlap(claimants: &[&str]) -> bool {
    claimants.len() == 2 && claimants.contains(&"inventory") && claimants.contains(&"collection")
}

// ---------------------------------------------------------------------------
// File processing
// ---------------------------------------------------------------------------

/// Creates an error `FileReport` when a file cannot be read.
fn error_report(filename: String, parsers: &[NamedParser]) -> FileReport {
    FileReport {
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
        gsm_turn_info_present: 0,
        gsm_turn_info_absent: 0,
        gsm_annotations_present: 0,
        gsm_annotations_total: 0,
        gsm_annotation_type_counts: HashMap::new(),
        gsm_timers_present: 0,
        gsm_timers_total: 0,
        gsm_diff_deleted_present: 0,
        gsm_diff_deleted_total: 0,
    }
}

/// Processes a single log file through all parsers and returns a report.
fn process_file(path: &Path, parsers: &[NamedParser]) -> FileReport {
    let filename = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();

    let Ok(content) = std::fs::read_to_string(path) else {
        return error_report(filename, parsers);
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
    let mut gsm_stats = GsmFieldStats::default();

    for entry in &entries {
        let timestamp = try_extract_timestamp(&entry.body);

        if timestamp.is_none() {
            timestamp_failures += 1;
        }

        let mut claimant_count: usize = 0;
        let mut claimant_names: Vec<&'static str> = Vec::new();

        for (idx, parser) in parsers.iter().enumerate() {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                parser.func.call(entry, timestamp)
            }));

            match result {
                Ok(events) if !events.is_empty() => {
                    stats[idx].1.claimed += 1;
                    claimant_count += 1;
                    claimant_names.push(parser.name);

                    for event in &events {
                        // Track GSM field presence for GameState events.
                        if let GameEvent::GameState(ref gs) = event {
                            gsm_stats.track(gs.payload());
                        }

                        *event_type_counts.entry(event_type_name(event)).or_insert(0) += 1;
                    }
                }
                Ok(_) => {}
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
        gsm_turn_info_present: gsm_stats.turn_info_present,
        gsm_turn_info_absent: gsm_stats.turn_info_absent,
        gsm_annotations_present: gsm_stats.annotations_present,
        gsm_annotations_total: gsm_stats.annotations_total,
        gsm_annotation_type_counts: gsm_stats.annotation_type_counts,
        gsm_timers_present: gsm_stats.timers_present,
        gsm_timers_total: gsm_stats.timers_total,
        gsm_diff_deleted_present: gsm_stats.diff_deleted_present,
        gsm_diff_deleted_total: gsm_stats.diff_deleted_total,
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
                "File: {} -- READ ERROR (unreadable file)",
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
        let _ = writeln!(out, "  GSM turn_info:");
        let _ = writeln!(
            out,
            "    {:<16} {:>6}",
            "present:", report.gsm_turn_info_present,
        );
        let _ = writeln!(
            out,
            "    {:<16} {:>6}",
            "absent:", report.gsm_turn_info_absent,
        );
        let _ = writeln!(out, "  GSM annotations:");
        let _ = writeln!(
            out,
            "    {:<16} {:>6} GSMs, {:>6} total",
            "present:", report.gsm_annotations_present, report.gsm_annotations_total,
        );
        if !report.gsm_annotation_type_counts.is_empty() {
            let mut sorted_ann: Vec<(&String, &usize)> =
                report.gsm_annotation_type_counts.iter().collect();
            sorted_ann.sort_by_key(|(name, _)| name.as_str());
            for (ann_type, count) in &sorted_ann {
                // Strip the "AnnotationType_" prefix for readability.
                let short = ann_type.strip_prefix("AnnotationType_").unwrap_or(ann_type);
                let label = format!("      {short}:");
                let _ = writeln!(out, "  {label:<30} {count:>6}");
            }
        }
        let _ = writeln!(out, "  GSM timers:");
        let _ = writeln!(
            out,
            "    {:<16} {:>6} GSMs, {:>6} total",
            "present:", report.gsm_timers_present, report.gsm_timers_total,
        );
        let _ = writeln!(out, "  GSM diff_deleted_instance_ids:");
        let _ = writeln!(
            out,
            "    {:<16} {:>6} GSMs, {:>6} total",
            "present:", report.gsm_diff_deleted_present, report.gsm_diff_deleted_total,
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
// Baseline conversion
// ---------------------------------------------------------------------------

/// Converts file reports into a map suitable for ratchet comparison
/// or baseline generation.
fn reports_to_baseline_files(
    reports: &[FileReport],
) -> std::collections::BTreeMap<String, BaselineFile> {
    reports
        .iter()
        .filter(|r| !r.read_error)
        .map(|r| (r.filename.clone(), r.to_baseline_file()))
        .collect()
}

/// Reads the corpus tag from the `CORPUS_TAG` environment variable.
///
/// In CI, this is set to the release tag of the downloaded corpus (e.g.
/// `manasight-corpus-v3`).  For local runs it falls back to `"local"`.
fn read_corpus_tag() -> String {
    std::env::var("CORPUS_TAG").unwrap_or_else(|_| "local".to_string())
}

/// Builds a full `Baseline` from actual results for bless mode.
fn build_baseline(actual: &std::collections::BTreeMap<String, BaselineFile>) -> Baseline {
    // Get current git commit hash for metadata, falling back gracefully.
    let commit = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                String::from_utf8(o.stdout)
                    .ok()
                    .map(|s| s.trim().to_string())
            } else {
                None
            }
        })
        .unwrap_or_else(|| "unknown".to_string());

    Baseline {
        meta: BaselineMeta {
            description: "Smoke test baseline -- per-file, per-parser event counts from Level 1 \
                          (parser-only) smoke tests."
                .to_string(),
            generated_from_commit: commit,
            corpus_tag: read_corpus_tag(),
        },
        files: actual.clone(),
    }
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
        "unreadable log files detected -- see report above"
    );
    assert_eq!(
        total_panics, 0,
        "parser panics detected -- see report above"
    );
    assert_eq!(
        total_double_claims, 0,
        "double claims detected -- see report above"
    );

    // --- Ratchet / Bless ---
    let actual_files = reports_to_baseline_files(&reports);

    if is_bless_mode() {
        let baseline = build_baseline(&actual_files);
        let write_result = write_baseline(&baseline);
        assert!(
            write_result.is_ok(),
            "failed to write baseline in bless mode: {}",
            write_result.err().unwrap_or_default(),
        );
        let msg = format!(
            "\nBless mode: wrote updated baseline with {} file(s) to smoke-baseline.json\n",
            actual_files.len(),
        );
        let _ = std::io::Write::write_all(&mut std::io::stdout(), msg.as_bytes());
    } else if let Some(baseline) = read_baseline() {
        let ratchet = compare_against_baseline(&baseline, &actual_files);
        let ratchet_report = ratchet.format_report();
        let _ = std::io::Write::write_all(&mut std::io::stdout(), ratchet_report.as_bytes());

        let regressions = ratchet.regressions();
        assert!(
            regressions.is_empty(),
            "ratchet regressions detected ({} regression(s)) -- see report above. \
             Run with SMOKE_BLESS=1 to update the baseline if the changes are intentional.",
            regressions.len(),
        );
    } else {
        let msg = "\nNo smoke-baseline.json found -- skipping ratchet comparison.\n\
                    Run with SMOKE_BLESS=1 to generate the initial baseline.\n";
        let _ = std::io::Write::write_all(&mut std::io::stdout(), msg.as_bytes());
    }
}

// ---------------------------------------------------------------------------
// Baseline metadata validation
// ---------------------------------------------------------------------------

/// Validates the committed `smoke-baseline.json` has well-formed metadata.
///
/// Moved from `smoke_metadata.rs` after removing the corpus manifest dependency.
/// The corpus repo now owns file-level integrity checks; the parser validates
/// only its own baseline structure.
#[test]
fn test_baseline_meta_fields_present() {
    let Some(baseline) = read_baseline() else {
        let msg = "smoke-baseline.json not found — skipping metadata validation\n";
        let _ = std::io::Write::write_all(&mut std::io::stdout(), msg.as_bytes());
        return;
    };

    assert!(
        !baseline.meta.description.is_empty(),
        "_meta.description should not be empty"
    );
    assert!(
        !baseline.meta.generated_from_commit.is_empty(),
        "_meta.generated_from_commit should not be empty"
    );
    assert!(
        !baseline.meta.corpus_tag.is_empty(),
        "_meta.corpus_tag should not be empty"
    );
    assert!(
        baseline.meta.corpus_tag == "local"
            || baseline.meta.corpus_tag.starts_with("manasight-corpus-"),
        "_meta.corpus_tag should be 'local' or start with 'manasight-corpus-', got '{}'",
        baseline.meta.corpus_tag
    );
}
