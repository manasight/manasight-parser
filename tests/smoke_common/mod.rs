//! Shared helpers for real-log smoke tests.
//!
//! All smoke tests are gated on the `MANASIGHT_TEST_LOGS` environment variable.
//! When **set**, it points to a directory of `.log` files that are processed.
//! When **unset**, the tests return early (pass) with a skip message.

// Each integration test file is its own crate and only uses a subset of these
// shared helpers, so unused items produce warnings. This is expected.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};

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
            name: "collection",
            func: ParserFunc::Single(parsers::collection::try_parse),
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
        GameEvent::Collection(_) => "Collection",
        GameEvent::Inventory(_) => "Inventory",
        GameEvent::GameResult(_) => "GameResult",
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
pub fn discover_log_files(dir: &Path) -> Vec<PathBuf> {
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
pub fn read_entries(path: &Path) -> Option<Vec<LogEntry>> {
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
