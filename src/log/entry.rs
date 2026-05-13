//! Log entry prefix identification and empty-line-delimited accumulation.
//!
//! Detects the start of log entries using the `[UnityCrossThreadLogger]`,
//! `[Client GRE]`, `[ConnectionManager]`, `Matchmaking:`, and metadata
//! patterns, then accumulates subsequent lines until Arena's empty-line block
//! delimiter to form complete raw entries.
//!
//! # Entry delimiters
//!
//! MTG Arena writes log entries in bursts and terminates each logical entry
//! with an empty line. [`LineBuffer`] therefore treats a recognized header as
//! the start of an entry and an empty line as the flush point. Header-looking
//! lines inside an in-progress entry are preserved as continuation lines; they
//! do not split the entry.
//!
//! # Data flow
//!
//! ```text
//! File Tailer ──(raw lines)──▸ LineBuffer ──(complete entries)──▸ Router
//! ```
//!
//! The [`LineBuffer`] receives individual lines from the file tailer. When an
//! empty line is received, it flushes the currently accumulated lines as a
//! complete [`LogEntry`].

use regex::Regex;

use crate::util::truncate_for_log;

/// The known log entry header prefixes in MTG Arena's `Player.log`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum EntryHeader {
    /// `[UnityCrossThreadLogger]` — the most common header, used for
    /// game state, client actions, match lifecycle, and most other events.
    UnityCrossThreadLogger,
    /// `[Client GRE]` — used for Game Rules Engine messages.
    ClientGre,
    /// `[ConnectionManager]` — emitted for Arena's connection-lifecycle
    /// diagnostics (e.g., `Reconnect result : ...`, `Reconnect succeeded`,
    /// `Reconnect failed`). These lines are plain-text, single-line entries
    /// in practice.
    ConnectionManager,
    /// `Matchmaking:` — a bare (non-bracketed) prefix Arena emits for
    /// matchmaking-side connection markers such as
    /// `Matchmaking: GRE connection lost`. These lines are plain-text,
    /// single-line entries in practice.
    Matchmaking,
    /// Metadata lines that appear outside bracket-delimited entries.
    ///
    /// Currently covers `DETAILED LOGS: ENABLED` and `DETAILED LOGS: DISABLED`,
    /// which Arena writes near the top of every session (typically line 24).
    Metadata,
}

impl EntryHeader {
    /// Returns the header string as it appears in the log.
    ///
    /// Bracket-delimited headers return the full `[...]` prefix.
    /// `Metadata` returns `"METADATA"` as a synthetic label (metadata
    /// lines have no bracket prefix in the actual log).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UnityCrossThreadLogger => "[UnityCrossThreadLogger]",
            Self::ClientGre => "[Client GRE]",
            Self::ConnectionManager => "[ConnectionManager]",
            Self::Matchmaking => "Matchmaking:",
            Self::Metadata => "METADATA",
        }
    }
}

impl std::fmt::Display for EntryHeader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A complete log entry extracted from the line buffer.
///
/// Contains the detected header prefix and the full raw text of the entry
/// (header line plus any continuation lines for multi-line payloads).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogEntry {
    /// Which header prefix introduced this entry.
    pub header: EntryHeader,
    /// The full raw text of the entry, including the header line and all
    /// continuation lines. Lines are joined with `'\n'`.
    pub body: String,
}

/// Accumulates raw lines and produces complete [`LogEntry`] values when an
/// empty line boundary is detected.
///
/// # Usage
///
/// Feed lines one at a time via [`push_line`](Self::push_line). Each call
/// returns a `Vec<LogEntry>` containing zero or one complete entries:
///
/// - **Zero entries**: continuation line for an in-progress entry, or a
///   headerless line discarded with a warning.
/// - **One entry**: an entry being flushed by an empty line.
///
/// After the input stream ends (EOF or file rotation), call
/// [`flush`](Self::flush) to retrieve any remaining buffered entry.
pub struct LineBuffer {
    /// Compiled regex for detecting log entry starts.
    header_re: Regex,
    /// Header of the entry currently being accumulated, if any.
    current_header: Option<EntryHeader>,
    /// Lines accumulated for the current entry.
    lines: Vec<String>,
    /// Whether this buffer has ever emitted (or begun accumulating) an entry.
    ///
    /// Armed by [`push_line`](Self::push_line) when a real header is detected
    /// or a metadata line is emitted. Cleared back to `false` by
    /// [`reset`](Self::reset) so post-rotation orphan lines still surface a
    /// warning.
    has_emitted_anything: bool,
}

impl LineBuffer {
    /// Creates a new, empty line buffer with the compiled header regex.
    pub fn new() -> Self {
        // The regex crate documents that `Regex::new` only fails on invalid
        // patterns. This pattern is a compile-time constant and is valid, so
        // the `Err` branch is unreachable in practice.
        let header_re =
            match Regex::new(r"^\[(UnityCrossThreadLogger|Client GRE|ConnectionManager)\]") {
                Ok(re) => re,
                Err(e) => unreachable!("invalid header regex: {e}"),
            };
        Self {
            header_re,
            current_header: None,
            lines: Vec::new(),
            has_emitted_anything: false,
        }
    }

    /// Feeds a single line into the buffer.
    ///
    /// Returns a `Vec<LogEntry>` containing 0 or 1 complete entries.
    ///
    /// # Empty Line Delimiters
    ///
    /// MTG Arena reliably terminates structured log entries with an empty
    /// line. When `line` is empty, any in-progress entry is flushed and
    /// emitted.
    ///
    /// # Input contract
    ///
    /// Callers must strip any trailing `\r` (Windows CRLF) before invoking
    /// this method. [`crate::log::tailer::FileTailer::poll`] already does
    /// this.
    pub fn push_line(&mut self, line: &str) -> Vec<LogEntry> {
        // Check if this is a block terminator
        if line.trim().is_empty() {
            let mut out = Vec::new();
            if let Some(entry) = self.take_entry() {
                out.push(entry);
                self.has_emitted_anything = true;
            }
            return out;
        }

        if self.current_header.is_none() {
            // Looking for the start of a new block
            if Self::is_metadata_line(line) {
                self.current_header = Some(EntryHeader::Metadata);
                self.lines.push(line.to_owned());
                self.has_emitted_anything = true;
            } else if let Some(header) = self.detect_header(line) {
                self.current_header = Some(header);
                self.lines.push(line.to_owned());
                self.has_emitted_anything = true;
            } else {
                // Headerless line with no entry in progress.
                if !self.has_emitted_anything {
                    ::log::warn!(
                        "Discarding headerless line at start of input: {:?}",
                        truncate_for_log(line, 120),
                    );
                }
            }
            Vec::new()
        } else {
            // Continuation line for the current entry
            self.lines.push(line.to_owned());
            Vec::new()
        }
    }

    /// Flushes any remaining buffered entry.
    ///
    /// Call this when the input stream ends (EOF or file rotation) to
    /// retrieve the last accumulated multi-line entry, if any. Single-line
    /// entries are never buffered — they are emitted by [`push_line`] in the
    /// same call that received them — so this method only ever returns at
    /// most one entry.
    pub fn flush(&mut self) -> Option<LogEntry> {
        self.take_entry()
    }

    /// Resets the buffer, discarding any in-progress entry.
    ///
    /// Useful on file rotation when the previous partial entry should be
    /// abandoned. Also re-arms the orphan-warning flag so the first
    /// post-rotation orphan still surfaces a warning (the rotation case
    /// the warning was originally meant to detect).
    pub fn reset(&mut self) {
        self.current_header = None;
        self.lines.clear();
        self.has_emitted_anything = false;
    }

    /// Returns `true` if no entry is currently being accumulated.
    pub fn is_empty(&self) -> bool {
        self.current_header.is_none()
    }

    /// Returns `true` if the line is a metadata line that should be
    /// treated as a self-contained entry.
    ///
    /// Currently matches `DETAILED LOGS: ENABLED` and
    /// `DETAILED LOGS: DISABLED`.
    fn is_metadata_line(line: &str) -> bool {
        let trimmed = line.trim();
        trimmed == "DETAILED LOGS: ENABLED" || trimmed == "DETAILED LOGS: DISABLED"
    }

    /// Detects whether `line` starts with a known header prefix.
    ///
    /// Bracketed headers (`[UnityCrossThreadLogger]`, `[Client GRE]`,
    /// `[ConnectionManager]`) are matched via the compiled regex. The
    /// bare `Matchmaking: ` prefix is matched via a separate
    /// `starts_with` check because it has no brackets.
    fn detect_header(&self, line: &str) -> Option<EntryHeader> {
        if let Some(caps) = self.header_re.captures(line) {
            let prefix = caps.get(1)?.as_str();
            return match prefix {
                "UnityCrossThreadLogger" => Some(EntryHeader::UnityCrossThreadLogger),
                "Client GRE" => Some(EntryHeader::ClientGre),
                "ConnectionManager" => Some(EntryHeader::ConnectionManager),
                _ => None,
            };
        }
        if line.starts_with("Matchmaking: ") {
            return Some(EntryHeader::Matchmaking);
        }
        None
    }

    /// Takes the current entry out of the buffer, leaving it empty.
    fn take_entry(&mut self) -> Option<LogEntry> {
        let header = self.current_header.take()?;
        let body = self.lines.join("\n");
        self.lines.clear();
        Some(LogEntry { header, body })
    }
}

impl Default for LineBuffer {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: build an expected `LogEntry` for concise assertions.
    fn expected(header: EntryHeader, body: &str) -> LogEntry {
        LogEntry {
            header,
            body: body.to_owned(),
        }
    }

    // -- EntryHeader --------------------------------------------------------

    mod entry_header {
        use super::*;

        #[test]
        fn test_as_str_unity() {
            assert_eq!(
                EntryHeader::UnityCrossThreadLogger.as_str(),
                "[UnityCrossThreadLogger]"
            );
        }

        #[test]
        fn test_as_str_client_gre() {
            assert_eq!(EntryHeader::ClientGre.as_str(), "[Client GRE]");
        }

        #[test]
        fn test_display_unity() {
            assert_eq!(
                EntryHeader::UnityCrossThreadLogger.to_string(),
                "[UnityCrossThreadLogger]"
            );
        }

        #[test]
        fn test_display_client_gre() {
            assert_eq!(EntryHeader::ClientGre.to_string(), "[Client GRE]");
        }

        #[test]
        fn test_clone_and_eq() {
            let a = EntryHeader::UnityCrossThreadLogger;
            let b = a;
            assert_eq!(a, b);
        }
    }

    // -- LineBuffer: basic operation ----------------------------------------

    mod push_line {
        use super::*;

        #[test]
        fn test_push_line_header_returns_empty() {
            let mut buf = LineBuffer::new();
            assert!(buf
                .push_line("[UnityCrossThreadLogger]1/1/2025 12:00:00 Event")
                .is_empty());
        }

        #[test]
        fn test_push_line_empty_line_flushes_entry() {
            let mut buf = LineBuffer::new();
            buf.push_line("[UnityCrossThreadLogger]1/1/2025 Event1");
            assert_eq!(
                buf.push_line(""),
                vec![expected(
                    EntryHeader::UnityCrossThreadLogger,
                    "[UnityCrossThreadLogger]1/1/2025 Event1",
                )],
            );
        }

        #[test]
        fn test_push_line_continuation_appended() {
            let mut buf = LineBuffer::new();
            buf.push_line("[UnityCrossThreadLogger]1/1/2025 Event1");
            buf.push_line(r#"{"key": "value"}"#);
            buf.push_line(r#"{"more": "data"}"#);
            assert_eq!(
                buf.push_line(""),
                vec![expected(
                    EntryHeader::UnityCrossThreadLogger,
                    "[UnityCrossThreadLogger]1/1/2025 Event1\n{\"key\": \"value\"}\n{\"more\": \"data\"}",
                )],
            );
        }

        #[test]
        fn test_push_line_client_gre_header_detected() {
            let mut buf = LineBuffer::new();
            buf.push_line("[Client GRE] GreMessage");
            assert_eq!(
                buf.push_line(""),
                vec![expected(EntryHeader::ClientGre, "[Client GRE] GreMessage")],
            );
        }

        #[test]
        fn test_push_line_multiple_entries() {
            let mut buf = LineBuffer::new();
            buf.push_line("[UnityCrossThreadLogger]1/1/2025 Event1");

            assert_eq!(
                buf.push_line(""),
                vec![expected(
                    EntryHeader::UnityCrossThreadLogger,
                    "[UnityCrossThreadLogger]1/1/2025 Event1",
                )],
            );

            buf.push_line("[Client GRE] Event2");
            assert_eq!(
                buf.push_line(""),
                vec![expected(EntryHeader::ClientGre, "[Client GRE] Event2")],
            );
        }

        #[test]
        fn test_push_line_multiple_headers_accumulate() {
            // If a second header arrives without a blank line, it just gets added to the body
            let mut buf = LineBuffer::new();
            buf.push_line("[UnityCrossThreadLogger]1/1/2025 Event1");
            buf.push_line("[Client GRE] Event2");
            assert_eq!(
                buf.push_line(""),
                vec![expected(
                    EntryHeader::UnityCrossThreadLogger,
                    "[UnityCrossThreadLogger]1/1/2025 Event1\n[Client GRE] Event2",
                )],
            );
        }
    }

    // -- LineBuffer: headerless lines ---------------------------------------

    mod headerless {
        use super::*;

        #[test]
        fn test_push_line_headerless_before_first_header_returns_empty() {
            let mut buf = LineBuffer::new();
            assert!(buf.push_line("some random line").is_empty());
            assert!(buf.push_line("another orphan").is_empty());
            // After discarding, the next header should still work.
            buf.push_line("[UnityCrossThreadLogger]1/1/2025 Real entry");
            assert_eq!(
                buf.push_line(""),
                vec![expected(
                    EntryHeader::UnityCrossThreadLogger,
                    "[UnityCrossThreadLogger]1/1/2025 Real entry",
                )],
            );
        }
    }

    // -- LineBuffer: flush --------------------------------------------------

    mod flush {
        use super::*;

        #[test]
        fn test_flush_empty_buffer_returns_none() {
            let mut buf = LineBuffer::new();
            assert!(buf.flush().is_none());
        }

        #[test]
        fn test_flush_returns_buffered_entry() {
            let mut buf = LineBuffer::new();
            buf.push_line("[UnityCrossThreadLogger]1/1/2025 Event");
            assert_eq!(
                buf.flush(),
                Some(expected(
                    EntryHeader::UnityCrossThreadLogger,
                    "[UnityCrossThreadLogger]1/1/2025 Event",
                )),
            );
        }

        #[test]
        fn test_flush_clears_buffer() {
            let mut buf = LineBuffer::new();
            buf.push_line("[UnityCrossThreadLogger]1/1/2025 Event");
            buf.flush();
            assert!(buf.flush().is_none());
            assert!(buf.is_empty());
        }

        #[test]
        fn test_flush_multi_line_entry() {
            let mut buf = LineBuffer::new();
            buf.push_line("[Client GRE] GreToClientEvent");
            buf.push_line("{");
            buf.push_line(r#"  "gameObjects": ["obj1", "obj2"],"#);
            buf.push_line(r#"  "actions": []"#);
            buf.push_line("}");
            let expected_body = [
                "[Client GRE] GreToClientEvent",
                "{",
                r#"  "gameObjects": ["obj1", "obj2"],"#,
                r#"  "actions": []"#,
                "}",
            ]
            .join("\n");
            assert_eq!(
                buf.flush(),
                Some(expected(EntryHeader::ClientGre, &expected_body)),
            );
        }
    }

    // -- LineBuffer: reset --------------------------------------------------

    mod reset {
        use super::*;

        #[test]
        fn test_reset_clears_in_progress_entry() {
            let mut buf = LineBuffer::new();
            buf.push_line("[UnityCrossThreadLogger]1/1/2025 Event");
            buf.push_line("continuation");
            buf.reset();
            assert!(buf.is_empty());
            assert!(buf.flush().is_none());
        }

        #[test]
        fn test_reset_allows_fresh_accumulation() {
            let mut buf = LineBuffer::new();
            buf.push_line("[UnityCrossThreadLogger]1/1/2025 Old");
            buf.reset();
            buf.push_line("[Client GRE] New");
            assert_eq!(
                buf.push_line(""),
                vec![expected(EntryHeader::ClientGre, "[Client GRE] New")],
            );
        }
    }

    // -- LineBuffer: is_empty -----------------------------------------------

    mod is_empty {
        use super::*;

        #[test]
        fn test_is_empty_on_new_buffer() {
            let buf = LineBuffer::new();
            assert!(buf.is_empty());
        }

        #[test]
        fn test_is_empty_false_after_header() {
            let mut buf = LineBuffer::new();
            buf.push_line("[UnityCrossThreadLogger]1/1/2025 Event");
            assert!(!buf.is_empty());
        }

        #[test]
        fn test_is_empty_true_after_empty_line_flush() {
            let mut buf = LineBuffer::new();
            buf.push_line("[UnityCrossThreadLogger]STATE CHANGED");
            buf.push_line("");
            assert!(buf.is_empty());
        }

        #[test]
        fn test_is_empty_true_after_flush() {
            let mut buf = LineBuffer::new();
            buf.push_line("[UnityCrossThreadLogger]1/1/2025 Event");
            buf.flush();
            assert!(buf.is_empty());
        }

        #[test]
        fn test_is_empty_true_after_headerless_lines() {
            let mut buf = LineBuffer::new();
            buf.push_line("orphan line");
            assert!(buf.is_empty());
        }
    }

    // -- LineBuffer: default ------------------------------------------------

    mod default_impl {
        use super::*;

        #[test]
        fn test_default_creates_functional_buffer() {
            let mut buf = LineBuffer::default();
            buf.push_line("[UnityCrossThreadLogger]1/1/2025 Event");
            assert_eq!(
                buf.push_line(""),
                vec![expected(
                    EntryHeader::UnityCrossThreadLogger,
                    "[UnityCrossThreadLogger]1/1/2025 Event",
                )],
            );
        }
    }

    // -- Header detection edge cases ----------------------------------------

    mod header_detection {
        use super::*;

        #[test]
        fn test_header_not_at_start_of_line_is_continuation() {
            let mut buf = LineBuffer::new();
            buf.push_line("[UnityCrossThreadLogger]1/1/2025 Event");
            // Header pattern in the middle of a line is NOT a boundary.
            buf.push_line("some text [UnityCrossThreadLogger] not a header");
            assert_eq!(
                buf.push_line(""),
                vec![expected(
                    EntryHeader::UnityCrossThreadLogger,
                    "[UnityCrossThreadLogger]1/1/2025 Event\nsome text [UnityCrossThreadLogger] not a header",
                )],
            );
        }

        #[test]
        fn test_similar_but_wrong_header_is_continuation() {
            let mut buf = LineBuffer::new();
            buf.push_line("[UnityCrossThreadLogger]1/1/2025 Event");
            buf.push_line("[UnityMainThreadLogger] not a valid header");
            let result = buf.push_line("");
            assert_eq!(result.len(), 1);
            if let Some(e) = result.first() {
                assert!(e.body.contains("[UnityMainThreadLogger]"));
            }
        }

        #[test]
        fn test_bracket_only_is_not_header() {
            let mut buf = LineBuffer::new();
            buf.push_line("[UnityCrossThreadLogger]1/1/2025 Event");
            buf.push_line("[]");
            assert_eq!(
                buf.push_line(""),
                vec![expected(
                    EntryHeader::UnityCrossThreadLogger,
                    "[UnityCrossThreadLogger]1/1/2025 Event\n[]",
                )],
            );
        }
    }

    // -- Realistic multi-line entry -----------------------------------------

    mod realistic_entries {
        use super::*;

        #[test]
        fn test_realistic_game_state_message() {
            let mut buf = LineBuffer::new();
            buf.push_line(
                "[UnityCrossThreadLogger]1/15/2025 3:42:17 PM \
                 greToClientEvent",
            );
            buf.push_line("{");
            buf.push_line(r#"  "greToClientMessages": ["#);
            buf.push_line(r"    {");
            buf.push_line(r#"      "type": "GREMessageType_GameStateMessage","#);
            buf.push_line(r#"      "gameStateMessage": {"#);
            buf.push_line(r#"        "gameObjects": []"#);
            buf.push_line(r"      }");
            buf.push_line(r"    }");
            buf.push_line(r"  ]");
            buf.push_line("}");

            let unity_entries = buf.push_line("");
            assert_eq!(unity_entries.len(), 1);
            assert_eq!(unity_entries[0].header, EntryHeader::UnityCrossThreadLogger);
            assert!(unity_entries[0].body.contains("greToClientMessages"));
            assert!(unity_entries[0].body.contains("GameStateMessage"));
        }
    }

    // -- Metadata line detection -----------------------------------------------

    mod metadata_lines {
        use super::*;

        #[test]
        fn test_push_line_detailed_logs_enabled_as_first_line() {
            let mut buf = LineBuffer::new();
            let result = buf.push_line("DETAILED LOGS: ENABLED");

            assert!(
                result.is_empty(),
                "metadata lines now require empty line to flush"
            );
            let result = buf.push_line("");

            assert_eq!(
                result,
                vec![expected(EntryHeader::Metadata, "DETAILED LOGS: ENABLED")],
            );
            assert!(buf.is_empty());
        }

        #[test]
        fn test_push_line_detailed_logs_disabled_as_first_line() {
            let mut buf = LineBuffer::new();
            buf.push_line("DETAILED LOGS: DISABLED");
            let result = buf.push_line("");

            assert_eq!(
                result,
                vec![expected(EntryHeader::Metadata, "DETAILED LOGS: DISABLED")],
            );
            assert!(buf.is_empty());
        }

        #[test]
        fn test_push_line_metadata_similar_text_not_matched() {
            let mut buf = LineBuffer::new();
            // Similar but not exact — should be treated as headerless.
            assert!(buf.push_line("DETAILED LOGS: UNKNOWN").is_empty());
            assert!(buf.push_line("detailed logs: enabled").is_empty());
            assert!(buf.push_line("DETAILED LOGS:ENABLED").is_empty());
        }

        #[test]
        fn test_push_line_metadata_with_leading_trailing_whitespace() {
            let mut buf = LineBuffer::new();
            buf.push_line("  DETAILED LOGS: ENABLED  ");
            let result = buf.push_line("");
            assert_eq!(result.len(), 1);
            assert_eq!(result[0].header, EntryHeader::Metadata);
        }

        #[test]
        fn test_entry_header_metadata_as_str() {
            assert_eq!(EntryHeader::Metadata.as_str(), "METADATA");
        }

        #[test]
        fn test_entry_header_metadata_display() {
            assert_eq!(EntryHeader::Metadata.to_string(), "METADATA");
        }
    }

    // -- Phase 2 (#161): orphan-warn gating ---------------------------------

    mod orphan_warn_gating {
        use super::*;
        use std::sync::{Mutex, OnceLock};

        struct CaptureLogger {
            records: Mutex<Vec<(::log::Level, String)>>,
        }

        impl ::log::Log for CaptureLogger {
            fn enabled(&self, _metadata: &::log::Metadata<'_>) -> bool {
                true
            }
            fn log(&self, record: &::log::Record<'_>) {
                let mut guard = match self.records.lock() {
                    Ok(g) => g,
                    Err(poisoned) => poisoned.into_inner(),
                };
                guard.push((record.level(), record.args().to_string()));
            }
            fn flush(&self) {}
        }

        static LOGGER: OnceLock<&'static CaptureLogger> = OnceLock::new();

        type RecordsRef = &'static Mutex<Vec<(::log::Level, String)>>;

        fn install_capture() -> RecordsRef {
            let logger = LOGGER.get_or_init(|| {
                let leaked: &'static CaptureLogger = Box::leak(Box::new(CaptureLogger {
                    records: Mutex::new(Vec::new()),
                }));
                let _ = ::log::set_logger(leaked);
                ::log::set_max_level(::log::LevelFilter::Trace);
                leaked
            });
            &logger.records
        }

        fn warn_count_matching(
            records: &Mutex<Vec<(::log::Level, String)>>,
            marker: &str,
        ) -> usize {
            let guard = match records.lock() {
                Ok(g) => g,
                Err(poisoned) => poisoned.into_inner(),
            };
            guard
                .iter()
                .filter(|(lvl, msg)| {
                    *lvl == ::log::Level::Warn
                        && msg.starts_with("Discarding headerless line at start of input")
                        && msg.contains(marker)
                })
                .count()
        }

        #[test]
        fn test_push_line_first_orphan_warns() {
            const MARKER: &str = "P2-MARKER-FIRST-ORPHAN-WARNS-zX9q";
            let records = install_capture();
            let mut buf = LineBuffer::new();

            assert!(buf.push_line(MARKER).is_empty());

            assert_eq!(
                warn_count_matching(records, MARKER),
                1,
                "first orphan at file start must warn (rotation/file-start anomaly)",
            );
        }

        #[test]
        fn test_push_line_post_flush_orphan_silent() {
            const MARKER: &str = "P2-MARKER-POST-FLUSH-SILENT-kJ7w";
            let records = install_capture();
            let mut buf = LineBuffer::new();

            buf.push_line(r#"[UnityCrossThreadLogger]STATE CHANGED {"x":1}"#);
            let entries = buf.push_line(""); // flush arms the gating flag
            assert_eq!(entries.len(), 1);
            assert!(buf.is_empty());

            assert!(buf.push_line(MARKER).is_empty());

            assert_eq!(
                warn_count_matching(records, MARKER),
                0,
                "post-flush orphan must be silently discarded (no warn)",
            );
        }

        #[test]
        fn test_push_line_orphan_after_reset_warns() {
            const MARKER: &str = "P2-MARKER-AFTER-RESET-WARNS-vN2t";
            let records = install_capture();
            let mut buf = LineBuffer::new();

            buf.push_line("[UnityCrossThreadLogger]STATE CHANGED {}");
            assert_eq!(buf.push_line("").len(), 1);

            buf.reset();

            assert!(buf.push_line(MARKER).is_empty());

            assert_eq!(
                warn_count_matching(records, MARKER),
                1,
                "first orphan after reset must warn (rotation anomaly)",
            );
        }

        #[test]
        fn test_push_line_orphan_after_metadata_silent() {
            const MARKER: &str = "P2-MARKER-AFTER-METADATA-SILENT-bH4r";
            let records = install_capture();
            let mut buf = LineBuffer::new();

            buf.push_line("DETAILED LOGS: ENABLED");
            let entries = buf.push_line("");
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].header, EntryHeader::Metadata);

            assert!(buf.push_line(MARKER).is_empty());

            assert_eq!(
                warn_count_matching(records, MARKER),
                0,
                "orphan after metadata must be silently discarded (no warn)",
            );
        }
    }

    // -- ConnectionManager / Matchmaking header framing ---------------------

    mod connection_and_matchmaking_headers {
        use super::*;

        #[test]
        fn test_as_str_connection_manager() {
            assert_eq!(
                EntryHeader::ConnectionManager.as_str(),
                "[ConnectionManager]"
            );
        }

        #[test]
        fn test_as_str_matchmaking() {
            assert_eq!(EntryHeader::Matchmaking.as_str(), "Matchmaking:");
        }

        #[test]
        fn test_display_connection_manager() {
            assert_eq!(
                EntryHeader::ConnectionManager.to_string(),
                "[ConnectionManager]"
            );
        }

        #[test]
        fn test_display_matchmaking() {
            assert_eq!(EntryHeader::Matchmaking.to_string(), "Matchmaking:");
        }

        #[test]
        fn test_push_line_connection_manager_header_detected() {
            let mut buf = LineBuffer::new();
            buf.push_line("[ConnectionManager] Hello");
            assert_eq!(
                buf.push_line(""),
                vec![expected(
                    EntryHeader::ConnectionManager,
                    "[ConnectionManager] Hello"
                )],
            );
        }

        #[test]
        fn test_push_line_matchmaking_header_detected() {
            let mut buf = LineBuffer::new();
            buf.push_line("Matchmaking: Hello");
            assert_eq!(
                buf.push_line(""),
                vec![expected(EntryHeader::Matchmaking, "Matchmaking: Hello")],
            );
        }
    }
}
