//! Shared helpers for real-log smoke tests.
//!
//! All smoke tests are gated on the `MANASIGHT_TEST_LOGS` environment variable.
//! When **set**, it points to a directory of `.log` files that are processed.
//! When **unset**, the tests return early (pass) with a skip message.
//!
//! ## Ratchet & Bless
//!
//! After running smoke tests, results are compared against `smoke-baseline.json`
//! using ratchet semantics: regressions (decreased counts) fail, improvements
//! (increased counts) pass. Set `SMOKE_BLESS=1` to overwrite the baseline
//! instead of comparing.

// Each integration test file is its own crate and only uses a subset of these
// shared helpers, so unused items produce warnings. This is expected.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::fmt::Write as FmtWrite;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use manasight_parser::events::GameEvent;
use manasight_parser::log::entry::{LineBuffer, LogEntry};
use manasight_parser::parsers;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const ENV_VAR: &str = "MANASIGHT_TEST_LOGS";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Wrapper for parser function pointers with different return types.
///
/// Most parsers return `Option<GameEvent>` (single event per entry).
/// The GRE parser returns `Vec<GameEvent>` (batched messages produce
/// multiple events from one entry).
pub enum ParserFunc {
    /// A parser that returns at most one event per entry.
    Single(fn(&LogEntry, Option<DateTime<Utc>>) -> Option<GameEvent>),
    /// A parser that may return multiple events per entry.
    Multi(fn(&LogEntry, Option<DateTime<Utc>>) -> Vec<GameEvent>),
}

impl ParserFunc {
    /// Calls the parser and normalizes the result to a `Vec<GameEvent>`.
    pub fn call(&self, entry: &LogEntry, ts: Option<DateTime<Utc>>) -> Vec<GameEvent> {
        match self {
            Self::Single(f) => f(entry, ts).into_iter().collect(),
            Self::Multi(f) => f(entry, ts),
        }
    }
}

/// A parser identified by name and its `try_parse` function.
pub struct NamedParser {
    pub name: &'static str,
    pub func: ParserFunc,
}

/// Per-parser statistics accumulated while processing a single log file.
#[derive(Default)]
pub struct ParserStats {
    /// Entries successfully claimed (returned `Some`).
    pub claimed: usize,
    /// Entries where `try_parse` panicked (caught by `catch_unwind`).
    pub panics: usize,
}

// ---------------------------------------------------------------------------
// Parser registry
// ---------------------------------------------------------------------------

/// Returns all implemented parsers in dispatch order.
pub fn all_parsers() -> Vec<NamedParser> {
    vec![
        NamedParser {
            name: "metadata",
            func: ParserFunc::Single(parsers::metadata::try_parse),
        },
        NamedParser {
            name: "session",
            func: ParserFunc::Single(parsers::session::try_parse),
        },
        NamedParser {
            name: "match_state",
            func: ParserFunc::Single(parsers::match_state::try_parse),
        },
        NamedParser {
            name: "gre",
            func: ParserFunc::Multi(parsers::gre::try_parse),
        },
        NamedParser {
            name: "client_actions",
            func: ParserFunc::Single(parsers::client_actions::try_parse),
        },
        NamedParser {
            name: "draft_bot",
            func: ParserFunc::Single(parsers::draft::bot::try_parse),
        },
        NamedParser {
            name: "draft_human",
            func: ParserFunc::Single(parsers::draft::human::try_parse),
        },
        NamedParser {
            name: "draft_complete",
            func: ParserFunc::Single(parsers::draft::complete::try_parse),
        },
        NamedParser {
            name: "inventory",
            func: ParserFunc::Single(parsers::inventory::try_parse),
        },
        NamedParser {
            name: "rank",
            func: ParserFunc::Single(parsers::rank::try_parse),
        },
        NamedParser {
            name: "event_lifecycle",
            func: ParserFunc::Single(parsers::event_lifecycle::try_parse),
        },
    ]
}

// ---------------------------------------------------------------------------
// Event type name
// ---------------------------------------------------------------------------

/// Returns the variant name of a `GameEvent` as a `'static str`.
pub fn event_type_name(event: &GameEvent) -> &'static str {
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
        GameEvent::Inventory(_) => "Inventory",
        GameEvent::GameResult(_) => "GameResult",
        GameEvent::LogFileRotated(_) => "LogFileRotated",
        GameEvent::DetailedLoggingStatus(_) => "DetailedLoggingStatus",
        // `GameEvent` is `#[non_exhaustive]`; this branch keeps the compiler
        // happy if new variants are added before this match is updated.
        _ => "Unknown",
    }
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Returns the logs directory from `MANASIGHT_TEST_LOGS`, or `None` if unset.
///
/// When `None`, callers should print a skip message and return early.
pub fn logs_dir_or_skip(test_name: &str) -> Option<PathBuf> {
    if let Ok(dir) = std::env::var(ENV_VAR) {
        Some(PathBuf::from(dir))
    } else {
        let msg = format!("{ENV_VAR} not set \u{2014} skipping {test_name}\n");
        let _ = std::io::Write::write_all(&mut std::io::stdout(), msg.as_bytes());
        None
    }
}

/// Discovers `.log` files in a directory, sorted alphabetically.
///
/// Excludes `.manasight.log` sidecar files — these are generated by
/// manasight-desktop alongside `Player.log` and are not MTGA logs.
pub fn discover_log_files(dir: &Path) -> Vec<PathBuf> {
    let mut log_files: Vec<PathBuf> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("log") {
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if !name.ends_with(".manasight.log") {
                    log_files.push(path);
                }
            }
        }
    }
    log_files.sort();
    log_files
}

/// Reads a log file and splits it into `LogEntry` objects via `LineBuffer`.
pub fn read_entries(path: &Path) -> Option<Vec<LogEntry>> {
    let content = std::fs::read_to_string(path).ok()?;
    let mut buffer = LineBuffer::new();
    let mut entries = Vec::new();
    for line in content.lines() {
        entries.extend(buffer.push_line(line));
    }
    if let Some(entry) = buffer.flush() {
        entries.push(entry);
    }
    Some(entries)
}

/// Asserts the given directory is valid and contains `.log` files.
///
/// Returns the sorted list of log file paths. Panics on assertion
/// failure (intended for test code only).
pub fn assert_logs_dir(logs_dir: &Path) -> Vec<PathBuf> {
    assert!(
        logs_dir.is_dir(),
        "{ENV_VAR} is not a directory: {}",
        logs_dir.display(),
    );

    let log_files = discover_log_files(logs_dir);

    assert!(
        !log_files.is_empty(),
        "no .log files found in {}",
        logs_dir.display(),
    );

    log_files
}

// ---------------------------------------------------------------------------
// Baseline types (serde)
// ---------------------------------------------------------------------------

/// Top-level baseline JSON structure.
///
/// Uses `BTreeMap` for deterministic (sorted-key) JSON serialization,
/// so every bless run produces identical output for the same data.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Baseline {
    #[serde(rename = "_meta")]
    pub meta: BaselineMeta,
    pub files: BTreeMap<String, BaselineFile>,
}

/// Metadata block in the baseline JSON.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BaselineMeta {
    pub description: String,
    pub generated_from_commit: String,
    pub corpus_tag: String,
}

/// Per-file data in the baseline JSON.
///
/// Uses `BTreeMap` for deterministic (sorted-key) JSON serialization.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BaselineFile {
    pub total_entries: u64,
    pub parsers: BTreeMap<String, u64>,
    pub event_types: BTreeMap<String, u64>,
    pub unclaimed: u64,
    pub double_claims: u64,
    pub timestamp_failures: u64,
}

/// Path to the baseline JSON file relative to the crate root.
const BASELINE_PATH: &str = "smoke-baseline.json";

/// Environment variable to enable bless mode.
const BLESS_ENV_VAR: &str = "SMOKE_BLESS";

// ---------------------------------------------------------------------------
// Baseline I/O
// ---------------------------------------------------------------------------

/// Reads the committed `smoke-baseline.json` from the crate root.
///
/// Returns `None` if the file does not exist or cannot be parsed.
pub fn read_baseline() -> Option<Baseline> {
    let content = std::fs::read_to_string(BASELINE_PATH).ok()?;
    serde_json::from_str(&content).ok()
}

/// Writes the baseline JSON to the crate root.
///
/// Used in bless mode to update the committed baseline.
/// Returns an error message on failure.
///
/// Serializes via [`serde_json::Value`] and recursively sorts all object keys
/// so the output is independent of Rust struct field declaration order.
pub fn write_baseline(baseline: &Baseline) -> Result<(), String> {
    let value = serde_json::to_value(baseline).map_err(|e| format!("serialize error: {e}"))?;
    let sorted = sort_json_keys(&value);
    let json =
        serde_json::to_string_pretty(&sorted).map_err(|e| format!("serialize error: {e}"))?;
    // Ensure trailing newline for POSIX compliance.
    let content = format!("{json}\n");
    std::fs::write(BASELINE_PATH, content)
        .map_err(|e| format!("failed to write {BASELINE_PATH}: {e}"))?;
    Ok(())
}

/// Recursively sorts all object keys in a JSON value alphabetically.
fn sort_json_keys(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let sorted: serde_json::Map<String, serde_json::Value> = map
                .iter()
                .map(|(k, v)| (k.clone(), sort_json_keys(v)))
                .collect::<BTreeMap<String, serde_json::Value>>()
                .into_iter()
                .collect();
            serde_json::Value::Object(sorted)
        }
        serde_json::Value::Array(arr) => {
            serde_json::Value::Array(arr.iter().map(sort_json_keys).collect())
        }
        other => other.clone(),
    }
}

/// Returns `true` if `SMOKE_BLESS=1` is set in the environment.
pub fn is_bless_mode() -> bool {
    std::env::var(BLESS_ENV_VAR).ok().is_some_and(|v| v == "1")
}

// ---------------------------------------------------------------------------
// Ratchet comparison
// ---------------------------------------------------------------------------

/// A single ratchet difference for one (file, parser/metric) pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RatchetDiff {
    pub filename: String,
    pub metric: String,
    pub baseline_value: u64,
    pub actual_value: u64,
}

impl RatchetDiff {
    /// Returns `true` if this diff is a regression (count decreased).
    pub fn is_regression(&self) -> bool {
        self.actual_value < self.baseline_value
    }

    /// Returns `true` if this diff is an improvement (count increased).
    pub fn is_improvement(&self) -> bool {
        self.actual_value > self.baseline_value
    }
}

/// Result of comparing actual results against the baseline.
#[derive(Debug, Clone)]
pub struct RatchetResult {
    pub diffs: Vec<RatchetDiff>,
}

impl RatchetResult {
    /// Returns only the regressions (counts that decreased).
    pub fn regressions(&self) -> Vec<&RatchetDiff> {
        self.diffs.iter().filter(|d| d.is_regression()).collect()
    }

    /// Returns only the improvements (counts that increased).
    pub fn improvements(&self) -> Vec<&RatchetDiff> {
        self.diffs.iter().filter(|d| d.is_improvement()).collect()
    }

    /// Returns `true` if there are no regressions.
    pub fn is_pass(&self) -> bool {
        self.regressions().is_empty()
    }

    /// Formats the ratchet result into a human-readable report section.
    pub fn format_report(&self) -> String {
        let mut out = String::new();
        let regressions = self.regressions();
        let improvements = self.improvements();

        if regressions.is_empty() && improvements.is_empty() {
            let _ = writeln!(out, "Ratchet: all counts match baseline.");
            return out;
        }

        if !improvements.is_empty() {
            let _ = writeln!(out, "Ratchet improvements ({}):", improvements.len());
            for diff in &improvements {
                let _ = writeln!(
                    out,
                    "  [+] {}/{}: {} -> {} (+{})",
                    diff.filename,
                    diff.metric,
                    diff.baseline_value,
                    diff.actual_value,
                    diff.actual_value - diff.baseline_value,
                );
            }
        }

        if !regressions.is_empty() {
            let _ = writeln!(out, "Ratchet REGRESSIONS ({}):", regressions.len());
            for diff in &regressions {
                let _ = writeln!(
                    out,
                    "  [-] {}/{}: {} -> {} (-{})",
                    diff.filename,
                    diff.metric,
                    diff.baseline_value,
                    diff.actual_value,
                    diff.baseline_value - diff.actual_value,
                );
            }
        }

        out
    }
}

/// Compares actual smoke test results against the baseline.
///
/// Compares per-parser claimed counts and per-event-type counts for files in
/// the baseline, and detects new files in actual that are not yet baselined.
pub fn compare_against_baseline(
    baseline: &Baseline,
    actual: &BTreeMap<String, BaselineFile>,
) -> RatchetResult {
    let mut diffs = Vec::new();

    for (filename, baseline_file) in &baseline.files {
        let Some(actual_file) = actual.get(filename) else {
            // File missing from actual results -- skip (may not be in corpus).
            continue;
        };

        // Compare per-parser counts.
        for (parser_name, &baseline_count) in &baseline_file.parsers {
            let actual_count = actual_file.parsers.get(parser_name).copied().unwrap_or(0);
            if actual_count != baseline_count {
                diffs.push(RatchetDiff {
                    filename: filename.clone(),
                    metric: format!("parser/{parser_name}"),
                    baseline_value: baseline_count,
                    actual_value: actual_count,
                });
            }
        }

        // Compare per-event-type counts.
        for (event_type, &baseline_count) in &baseline_file.event_types {
            let actual_count = actual_file
                .event_types
                .get(event_type)
                .copied()
                .unwrap_or(0);
            if actual_count != baseline_count {
                diffs.push(RatchetDiff {
                    filename: filename.clone(),
                    metric: format!("event_type/{event_type}"),
                    baseline_value: baseline_count,
                    actual_value: actual_count,
                });
            }
        }

        // Check for new event types in actual that were not in baseline.
        for (event_type, &actual_count) in &actual_file.event_types {
            if !baseline_file.event_types.contains_key(event_type) && actual_count > 0 {
                diffs.push(RatchetDiff {
                    filename: filename.clone(),
                    metric: format!("event_type/{event_type}"),
                    baseline_value: 0,
                    actual_value: actual_count,
                });
            }
        }

        // Check for new parsers in actual that were not in baseline.
        for (parser_name, &actual_count) in &actual_file.parsers {
            if !baseline_file.parsers.contains_key(parser_name) && actual_count > 0 {
                diffs.push(RatchetDiff {
                    filename: filename.clone(),
                    metric: format!("parser/{parser_name}"),
                    baseline_value: 0,
                    actual_value: actual_count,
                });
            }
        }
    }

    // Detect new files in actual that are not in the baseline.
    for (filename, actual_file) in actual {
        if baseline.files.contains_key(filename) {
            continue;
        }

        for (parser_name, &actual_count) in &actual_file.parsers {
            if actual_count > 0 {
                diffs.push(RatchetDiff {
                    filename: filename.clone(),
                    metric: format!("parser/{parser_name}"),
                    baseline_value: 0,
                    actual_value: actual_count,
                });
            }
        }

        for (event_type, &actual_count) in &actual_file.event_types {
            if actual_count > 0 {
                diffs.push(RatchetDiff {
                    filename: filename.clone(),
                    metric: format!("event_type/{event_type}"),
                    baseline_value: 0,
                    actual_value: actual_count,
                });
            }
        }
    }

    // Sort diffs for deterministic output.
    diffs.sort_by(|a, b| {
        a.filename
            .cmp(&b.filename)
            .then_with(|| a.metric.cmp(&b.metric))
    });

    RatchetResult { diffs }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sort_json_keys_reorders_object_keys_alphabetically() {
        let input: serde_json::Value = serde_json::json!({
            "z_last": 1,
            "a_first": 2,
            "m_middle": { "beta": 10, "alpha": 20 }
        });
        let sorted = sort_json_keys(&input);
        // Serialized output must have keys in alphabetical order.
        let json = serde_json::to_string(&sorted).unwrap_or_default();
        assert!(json.starts_with(r#"{"a_first":2,"m_middle":{"alpha":20,"beta":10},"z_last":1}"#));
    }

    #[test]
    fn test_sort_json_keys_preserves_array_order() {
        let input: serde_json::Value = serde_json::json!([
            {"b": 1, "a": 2},
            {"d": 3, "c": 4}
        ]);
        let sorted = sort_json_keys(&input);
        let json = serde_json::to_string(&sorted).unwrap_or_default();
        assert_eq!(json, r#"[{"a":2,"b":1},{"c":4,"d":3}]"#);
    }

    #[test]
    fn test_sort_json_keys_leaves_scalars_unchanged() {
        assert_eq!(
            sort_json_keys(&serde_json::json!(42)),
            serde_json::json!(42)
        );
        assert_eq!(
            sort_json_keys(&serde_json::json!("hi")),
            serde_json::json!("hi")
        );
        assert_eq!(
            sort_json_keys(&serde_json::json!(null)),
            serde_json::json!(null)
        );
        assert_eq!(
            sort_json_keys(&serde_json::json!(true)),
            serde_json::json!(true)
        );
    }
}
