//! Log entry prefix identification and multi-line JSON accumulation.
//!
//! Detects log entry boundaries using the `[UnityCrossThreadLogger]`,
//! `[Client GRE]`, `[ConnectionManager]`, and `Matchmaking:` header patterns,
//! then accumulates subsequent lines until the next header boundary to form
//! complete raw entries.
//!
//! # Data flow
//!
//! ```text
//! File Tailer ──(raw lines)──▸ LineBuffer ──(complete entries)──▸ Router
//! ```
//!
//! The [`LineBuffer`] receives individual lines from the file tailer. When a
//! new log entry header is detected, it flushes the previously accumulated
//! lines as a complete [`LogEntry`] and begins accumulating the new entry.

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

/// Accumulates raw lines and produces complete [`LogEntry`] values when a
/// new header boundary is detected.
///
/// # Usage
///
/// Feed lines one at a time via [`push_line`](Self::push_line). Each call
/// returns `Some(LogEntry)` when a new header flushes the previous entry.
/// After the input stream ends (EOF or file rotation), call
/// [`flush`](Self::flush) to retrieve any remaining buffered entry.
///
/// # Example
///
/// ```
/// use manasight_parser::log::entry::LineBuffer;
///
/// let mut buf = LineBuffer::new();
///
/// // First header — nothing to flush yet.
/// assert!(buf.push_line("[UnityCrossThreadLogger] 1/1/2025 Event1").is_none());
///
/// // Continuation line — still accumulating.
/// assert!(buf.push_line(r#"{"key": "value"}"#).is_none());
///
/// // Second header — flushes the first entry.
/// if let Some(entry) = buf.push_line("[Client GRE] 1/1/2025 Event2") {
///     assert_eq!(entry.body, "[UnityCrossThreadLogger] 1/1/2025 Event1\n{\"key\": \"value\"}");
/// }
///
/// // Flush the remaining entry.
/// if let Some(last) = buf.flush() {
///     assert_eq!(last.body, "[Client GRE] 1/1/2025 Event2");
/// }
/// ```
pub struct LineBuffer {
    /// Compiled regex for detecting log entry header boundaries.
    header_re: Regex,
    /// Header of the entry currently being accumulated, if any.
    current_header: Option<EntryHeader>,
    /// Lines accumulated for the current entry.
    lines: Vec<String>,
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
        }
    }

    /// Feeds a single line into the buffer.
    ///
    /// Returns `Some(LogEntry)` when `line` starts a new log entry header,
    /// flushing the previously accumulated entry. Returns `None` when the
    /// line is a continuation of the current entry, or when no entry was
    /// in progress (buffer was empty).
    ///
    /// Metadata lines (`DETAILED LOGS: ENABLED` / `DISABLED`) are treated
    /// as self-contained entries: the current in-progress entry (if any) is
    /// flushed, the metadata entry is returned, and no new accumulation
    /// begins. If a metadata line is the first line in the stream (nothing
    /// to flush), it is returned directly.
    ///
    /// Lines that arrive before any header has been seen are discarded with
    /// a warning log — this handles partial entries at the start of a file
    /// or after rotation.
    pub fn push_line(&mut self, line: &str) -> Option<LogEntry> {
        // Check for metadata lines first — these are self-contained.
        if Self::is_metadata_line(line) {
            let flushed = self.take_entry();
            let metadata_entry = LogEntry {
                header: EntryHeader::Metadata,
                body: line.to_owned(),
            };
            // If there was a buffered entry, return it now.
            // The metadata entry needs to be emitted too, so we buffer it
            // as a complete single-line entry that will flush on the next
            // header or explicit flush.
            if flushed.is_some() {
                self.current_header = Some(EntryHeader::Metadata);
                self.lines.push(line.to_owned());
                return flushed;
            }
            // No prior entry — return the metadata entry directly.
            return Some(metadata_entry);
        }

        if let Some(header) = self.detect_header(line) {
            let flushed = self.take_entry();
            self.current_header = Some(header);
            self.lines.push(line.to_owned());
            flushed
        } else if self.current_header.is_some() {
            // Continuation line for the current entry.
            self.lines.push(line.to_owned());
            None
        } else {
            // Line arrived before any header — discard with a warning.
            ::log::warn!(
                "Discarding headerless line at start of input: {:?}",
                truncate_for_log(line, 120),
            );
            None
        }
    }

    /// Flushes any remaining buffered entry.
    ///
    /// Call this when the input stream ends (EOF or file rotation) to
    /// retrieve the last accumulated entry.
    pub fn flush(&mut self) -> Option<LogEntry> {
        self.take_entry()
    }

    /// Resets the buffer, discarding any in-progress entry.
    ///
    /// Useful on file rotation when the previous partial entry should be
    /// abandoned.
    pub fn reset(&mut self) {
        self.current_header = None;
        self.lines.clear();
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
    ///
    /// Wrap in `Some(...)` to compare against `Option<LogEntry>` returns,
    /// avoiding `unwrap()`/`expect()` (denied crate-wide by `Cargo.toml`).
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
        fn test_push_line_first_header_returns_none() {
            let mut buf = LineBuffer::new();
            assert!(buf
                .push_line("[UnityCrossThreadLogger] 1/1/2025 12:00:00 Event")
                .is_none());
        }

        #[test]
        fn test_push_line_second_header_flushes_first_entry() {
            let mut buf = LineBuffer::new();
            buf.push_line("[UnityCrossThreadLogger] 1/1/2025 Event1");
            assert_eq!(
                buf.push_line("[Client GRE] 1/1/2025 Event2"),
                Some(expected(
                    EntryHeader::UnityCrossThreadLogger,
                    "[UnityCrossThreadLogger] 1/1/2025 Event1",
                )),
            );
        }

        #[test]
        fn test_push_line_continuation_appended() {
            let mut buf = LineBuffer::new();
            buf.push_line("[UnityCrossThreadLogger] 1/1/2025 Event1");
            buf.push_line(r#"{"key": "value"}"#);
            buf.push_line(r#"{"more": "data"}"#);
            assert_eq!(
                buf.push_line("[UnityCrossThreadLogger] 1/1/2025 Event2"),
                Some(expected(
                    EntryHeader::UnityCrossThreadLogger,
                    "[UnityCrossThreadLogger] 1/1/2025 Event1\n\
                     {\"key\": \"value\"}\n\
                     {\"more\": \"data\"}",
                )),
            );
        }

        #[test]
        fn test_push_line_client_gre_header_detected() {
            let mut buf = LineBuffer::new();
            buf.push_line("[Client GRE] GreMessage");
            assert_eq!(
                buf.flush(),
                Some(expected(EntryHeader::ClientGre, "[Client GRE] GreMessage")),
            );
        }

        #[test]
        fn test_push_line_alternating_headers() {
            let mut buf = LineBuffer::new();
            buf.push_line("[UnityCrossThreadLogger] Event1");

            assert_eq!(
                buf.push_line("[Client GRE] Event2"),
                Some(expected(
                    EntryHeader::UnityCrossThreadLogger,
                    "[UnityCrossThreadLogger] Event1",
                )),
            );

            assert_eq!(
                buf.push_line("[UnityCrossThreadLogger] Event3"),
                Some(expected(EntryHeader::ClientGre, "[Client GRE] Event2")),
            );

            assert_eq!(
                buf.flush(),
                Some(expected(
                    EntryHeader::UnityCrossThreadLogger,
                    "[UnityCrossThreadLogger] Event3",
                )),
            );
        }
    }

    // -- LineBuffer: headerless lines ---------------------------------------

    mod headerless {
        use super::*;

        #[test]
        fn test_push_line_headerless_before_first_header_returns_none() {
            let mut buf = LineBuffer::new();
            assert!(buf.push_line("some random line").is_none());
            assert!(buf.push_line("another orphan").is_none());
            // After discarding, the next header should still work.
            buf.push_line("[UnityCrossThreadLogger] Real entry");
            assert_eq!(
                buf.flush(),
                Some(expected(
                    EntryHeader::UnityCrossThreadLogger,
                    "[UnityCrossThreadLogger] Real entry",
                )),
            );
        }

        #[test]
        fn test_push_line_empty_line_as_continuation() {
            let mut buf = LineBuffer::new();
            buf.push_line("[UnityCrossThreadLogger] Event");
            buf.push_line("");
            buf.push_line("continuation");
            assert_eq!(
                buf.flush(),
                Some(expected(
                    EntryHeader::UnityCrossThreadLogger,
                    "[UnityCrossThreadLogger] Event\n\ncontinuation",
                )),
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
            buf.push_line("[UnityCrossThreadLogger] Event");
            assert_eq!(
                buf.flush(),
                Some(expected(
                    EntryHeader::UnityCrossThreadLogger,
                    "[UnityCrossThreadLogger] Event",
                )),
            );
        }

        #[test]
        fn test_flush_clears_buffer() {
            let mut buf = LineBuffer::new();
            buf.push_line("[UnityCrossThreadLogger] Event");
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
            buf.push_line("[UnityCrossThreadLogger] Event");
            buf.push_line("continuation");
            buf.reset();
            assert!(buf.is_empty());
            assert!(buf.flush().is_none());
        }

        #[test]
        fn test_reset_allows_fresh_accumulation() {
            let mut buf = LineBuffer::new();
            buf.push_line("[UnityCrossThreadLogger] Old");
            buf.reset();
            buf.push_line("[Client GRE] New");
            assert_eq!(
                buf.flush(),
                Some(expected(EntryHeader::ClientGre, "[Client GRE] New")),
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
            buf.push_line("[UnityCrossThreadLogger] Event");
            assert!(!buf.is_empty());
        }

        #[test]
        fn test_is_empty_true_after_flush() {
            let mut buf = LineBuffer::new();
            buf.push_line("[UnityCrossThreadLogger] Event");
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
            buf.push_line("[UnityCrossThreadLogger] Event");
            assert_eq!(
                buf.flush(),
                Some(expected(
                    EntryHeader::UnityCrossThreadLogger,
                    "[UnityCrossThreadLogger] Event",
                )),
            );
        }
    }

    // -- Header detection edge cases ----------------------------------------

    mod header_detection {
        use super::*;

        #[test]
        fn test_header_not_at_start_of_line_is_continuation() {
            let mut buf = LineBuffer::new();
            buf.push_line("[UnityCrossThreadLogger] Event");
            // Header pattern in the middle of a line is NOT a boundary.
            buf.push_line("some text [UnityCrossThreadLogger] not a header");
            assert_eq!(
                buf.flush(),
                Some(expected(
                    EntryHeader::UnityCrossThreadLogger,
                    "[UnityCrossThreadLogger] Event\n\
                     some text [UnityCrossThreadLogger] not a header",
                )),
            );
        }

        #[test]
        fn test_similar_but_wrong_header_is_continuation() {
            let mut buf = LineBuffer::new();
            buf.push_line("[UnityCrossThreadLogger] Event");
            buf.push_line("[UnityMainThreadLogger] not a valid header");
            let result = buf.flush();
            assert!(result.is_some());
            if let Some(e) = result {
                assert!(e.body.contains("[UnityMainThreadLogger]"));
            }
        }

        #[test]
        fn test_bracket_only_is_not_header() {
            let mut buf = LineBuffer::new();
            buf.push_line("[UnityCrossThreadLogger] Event");
            buf.push_line("[]");
            assert_eq!(
                buf.flush(),
                Some(expected(
                    EntryHeader::UnityCrossThreadLogger,
                    "[UnityCrossThreadLogger] Event\n[]",
                )),
            );
        }

        #[test]
        fn test_header_with_nothing_after_bracket() {
            let mut buf = LineBuffer::new();
            // Header with no trailing content — still a valid header.
            buf.push_line("[UnityCrossThreadLogger]");
            assert_eq!(
                buf.flush(),
                Some(expected(
                    EntryHeader::UnityCrossThreadLogger,
                    "[UnityCrossThreadLogger]",
                )),
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

            // [Client GRE] flushes the UnityCrossThreadLogger entry.
            let unity_entry = buf.push_line("[Client GRE] Next event");
            assert!(unity_entry.is_some());
            if let Some(e) = unity_entry {
                assert_eq!(e.header, EntryHeader::UnityCrossThreadLogger);
                assert!(e.body.contains("greToClientMessages"));
                assert!(e.body.contains("GameStateMessage"));
            }

            // Another header flushes the Client GRE entry.
            assert_eq!(
                buf.push_line("[UnityCrossThreadLogger] After"),
                Some(expected(EntryHeader::ClientGre, "[Client GRE] Next event",)),
            );
        }

        #[test]
        fn test_many_entries_in_sequence() {
            let mut buf = LineBuffer::new();
            let mut entries = Vec::new();

            for i in 0..5 {
                if let Some(e) = buf.push_line(&format!("[UnityCrossThreadLogger] Event{i}")) {
                    entries.push(e);
                }
            }
            if let Some(e) = buf.flush() {
                entries.push(e);
            }

            assert_eq!(entries.len(), 5);
            for (i, e) in entries.iter().enumerate() {
                assert_eq!(e.header, EntryHeader::UnityCrossThreadLogger);
                assert_eq!(e.body, format!("[UnityCrossThreadLogger] Event{i}"));
            }
        }
    }

    // -- Metadata line detection -----------------------------------------------

    mod metadata_lines {
        use super::*;

        #[test]
        fn test_push_line_detailed_logs_enabled_as_first_line() {
            let mut buf = LineBuffer::new();
            let result = buf.push_line("DETAILED LOGS: ENABLED");

            assert_eq!(
                result,
                Some(expected(EntryHeader::Metadata, "DETAILED LOGS: ENABLED")),
            );
            // Buffer should be empty after — metadata is self-contained.
            assert!(buf.is_empty());
        }

        #[test]
        fn test_push_line_detailed_logs_disabled_as_first_line() {
            let mut buf = LineBuffer::new();
            let result = buf.push_line("DETAILED LOGS: DISABLED");

            assert_eq!(
                result,
                Some(expected(EntryHeader::Metadata, "DETAILED LOGS: DISABLED")),
            );
        }

        #[test]
        fn test_push_line_metadata_flushes_buffered_entry() {
            let mut buf = LineBuffer::new();
            buf.push_line("[UnityCrossThreadLogger] Event1");

            // Metadata line should flush the buffered entry.
            let flushed = buf.push_line("DETAILED LOGS: ENABLED");
            assert_eq!(
                flushed,
                Some(expected(
                    EntryHeader::UnityCrossThreadLogger,
                    "[UnityCrossThreadLogger] Event1",
                )),
            );

            // The metadata entry should be available on next flush.
            let metadata = buf.flush();
            assert_eq!(
                metadata,
                Some(expected(EntryHeader::Metadata, "DETAILED LOGS: ENABLED")),
            );
        }

        #[test]
        fn test_push_line_metadata_then_header_flushes_metadata() {
            let mut buf = LineBuffer::new();
            buf.push_line("DETAILED LOGS: ENABLED");

            // Next header should work normally (nothing to flush since
            // the metadata entry was returned immediately).
            assert!(buf.push_line("[UnityCrossThreadLogger] Event").is_none());
            assert_eq!(
                buf.flush(),
                Some(expected(
                    EntryHeader::UnityCrossThreadLogger,
                    "[UnityCrossThreadLogger] Event",
                )),
            );
        }

        #[test]
        fn test_push_line_metadata_buffered_then_next_header_flushes() {
            let mut buf = LineBuffer::new();
            buf.push_line("[UnityCrossThreadLogger] Event1");

            // Metadata line flushes Event1, buffers itself.
            buf.push_line("DETAILED LOGS: DISABLED");

            // Next header flushes the metadata entry.
            let flushed = buf.push_line("[UnityCrossThreadLogger] Event2");
            assert_eq!(
                flushed,
                Some(expected(EntryHeader::Metadata, "DETAILED LOGS: DISABLED")),
            );
        }

        #[test]
        fn test_push_line_metadata_similar_text_not_matched() {
            let mut buf = LineBuffer::new();
            // Similar but not exact — should be treated as headerless.
            assert!(buf.push_line("DETAILED LOGS: UNKNOWN").is_none());
            assert!(buf.push_line("detailed logs: enabled").is_none());
            assert!(buf.push_line("DETAILED LOGS:ENABLED").is_none());
        }

        #[test]
        fn test_push_line_metadata_with_leading_trailing_whitespace() {
            let mut buf = LineBuffer::new();
            // Whitespace around the exact text should still match.
            let result = buf.push_line("  DETAILED LOGS: ENABLED  ");
            assert!(result.is_some());
            if let Some(entry) = result {
                assert_eq!(entry.header, EntryHeader::Metadata);
            }
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
            // The `Matchmaking:` prefix keeps the colon — this matches how
            // the line appears in Arena's actual log.
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
        fn test_connection_manager_header_mid_stream_flushes_unity() {
            let mut buf = LineBuffer::new();
            buf.push_line("[UnityCrossThreadLogger] 1/1/2025 Event1");

            let flushed = buf.push_line("[ConnectionManager] Reconnect result : Error");
            assert_eq!(
                flushed,
                Some(expected(
                    EntryHeader::UnityCrossThreadLogger,
                    "[UnityCrossThreadLogger] 1/1/2025 Event1",
                )),
            );

            // The ConnectionManager entry should be buffered and flushable.
            assert_eq!(
                buf.flush(),
                Some(expected(
                    EntryHeader::ConnectionManager,
                    "[ConnectionManager] Reconnect result : Error",
                )),
            );
        }

        #[test]
        fn test_matchmaking_header_mid_stream_flushes_unity() {
            let mut buf = LineBuffer::new();
            buf.push_line("[UnityCrossThreadLogger] 1/1/2025 Event1");

            let flushed = buf.push_line("Matchmaking: GRE connection lost");
            assert_eq!(
                flushed,
                Some(expected(
                    EntryHeader::UnityCrossThreadLogger,
                    "[UnityCrossThreadLogger] 1/1/2025 Event1",
                )),
            );

            assert_eq!(
                buf.flush(),
                Some(expected(
                    EntryHeader::Matchmaking,
                    "Matchmaking: GRE connection lost",
                )),
            );
        }

        #[test]
        fn test_connection_manager_as_first_line_no_warning_emitted() {
            // A ConnectionManager entry as the very first line should not
            // be discarded as headerless — push_line returns None only
            // because there is nothing to flush yet; the entry is buffered
            // and flushable normally.
            let mut buf = LineBuffer::new();
            assert!(buf
                .push_line("[ConnectionManager] Reconnect succeeded")
                .is_none());
            assert!(!buf.is_empty());
            assert_eq!(
                buf.flush(),
                Some(expected(
                    EntryHeader::ConnectionManager,
                    "[ConnectionManager] Reconnect succeeded",
                )),
            );
        }

        #[test]
        fn test_matchmaking_as_first_line_no_warning_emitted() {
            let mut buf = LineBuffer::new();
            assert!(buf.push_line("Matchmaking: GRE connection lost").is_none());
            assert!(!buf.is_empty());
            assert_eq!(
                buf.flush(),
                Some(expected(
                    EntryHeader::Matchmaking,
                    "Matchmaking: GRE connection lost",
                )),
            );
        }

        #[test]
        fn test_four_way_interleave_yields_four_entries() {
            // Realistic corpus-derived pattern from issues #528/#529:
            // Unity STATE CHANGED → Matchmaking: GRE connection lost →
            // ConnectionManager Reconnect result → Unity (next event).
            let mut buf = LineBuffer::new();
            let mut entries = Vec::new();

            if let Some(e) = buf.push_line(
                "[UnityCrossThreadLogger]STATE CHANGED {\"old\":\"Playing\",\"new\":\"Disconnected\"}",
            ) {
                entries.push(e);
            }
            if let Some(e) = buf.push_line("Matchmaking: GRE connection lost") {
                entries.push(e);
            }
            if let Some(e) = buf.push_line("[ConnectionManager] Reconnect result : Error") {
                entries.push(e);
            }
            if let Some(e) = buf.push_line("[UnityCrossThreadLogger] Next event") {
                entries.push(e);
            }
            if let Some(e) = buf.flush() {
                entries.push(e);
            }

            assert_eq!(entries.len(), 4);
            assert_eq!(entries[0].header, EntryHeader::UnityCrossThreadLogger);
            assert!(entries[0].body.contains("STATE CHANGED"));
            assert_eq!(entries[1].header, EntryHeader::Matchmaking);
            assert_eq!(entries[1].body, "Matchmaking: GRE connection lost");
            assert_eq!(entries[2].header, EntryHeader::ConnectionManager);
            assert_eq!(
                entries[2].body,
                "[ConnectionManager] Reconnect result : Error"
            );
            assert_eq!(entries[3].header, EntryHeader::UnityCrossThreadLogger);
            assert_eq!(entries[3].body, "[UnityCrossThreadLogger] Next event");
        }

        #[test]
        fn test_connection_manager_accumulates_continuation_lines() {
            // Corpus shows these entries are single-line in practice, but
            // verify continuation lines are accumulated if they appear.
            let mut buf = LineBuffer::new();
            buf.push_line("[ConnectionManager] Reconnect result : Error");
            buf.push_line("  extra detail line");
            buf.push_line("  another detail line");

            assert_eq!(
                buf.flush(),
                Some(expected(
                    EntryHeader::ConnectionManager,
                    "[ConnectionManager] Reconnect result : Error\n  extra detail line\n  another detail line",
                )),
            );
        }

        #[test]
        fn test_matchmaking_accumulates_continuation_lines() {
            let mut buf = LineBuffer::new();
            buf.push_line("Matchmaking: GRE connection lost");
            buf.push_line("extra continuation");

            assert_eq!(
                buf.flush(),
                Some(expected(
                    EntryHeader::Matchmaking,
                    "Matchmaking: GRE connection lost\nextra continuation",
                )),
            );
        }

        #[test]
        fn test_matchmaking_without_trailing_space_is_not_header() {
            // The starts_with check requires the trailing space ("Matchmaking: ")
            // to avoid matching unrelated prefixes that happen to start
            // with "Matchmaking:". Without the space it should be a
            // headerless line (discarded at start of stream).
            let mut buf = LineBuffer::new();
            assert!(buf.push_line("Matchmaking:compact-no-space").is_none());
            assert!(buf.is_empty());
        }

        #[test]
        fn test_connection_manager_mid_line_is_continuation() {
            let mut buf = LineBuffer::new();
            buf.push_line("[UnityCrossThreadLogger] Event");
            // ConnectionManager bracket pattern in the middle of a line is
            // NOT a boundary — same rule as other bracketed headers.
            buf.push_line("some text [ConnectionManager] not a header");
            assert_eq!(
                buf.flush(),
                Some(expected(
                    EntryHeader::UnityCrossThreadLogger,
                    "[UnityCrossThreadLogger] Event\n\
                     some text [ConnectionManager] not a header",
                )),
            );
        }
    }
}
