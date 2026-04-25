//! Log entry prefix identification and multi-line JSON accumulation.
//!
//! Detects log entry boundaries using the `[UnityCrossThreadLogger]`,
//! `[Client GRE]`, `[ConnectionManager]`, and `Matchmaking:` header patterns,
//! then accumulates subsequent lines until the next header boundary to form
//! complete raw entries.
//!
//! # Header classification (Phase 1 of #153)
//!
//! Each detected header is classified as either single-line or multi-line:
//!
//! - **Single-line**: `[UnityCrossThreadLogger]` followed by anything other
//!   than a date digit (e.g., alpha labels like `STATE CHANGED`,
//!   `Client.SceneChange`, or `==>` API request markers),
//!   `[ConnectionManager]…`, and `Matchmaking:…`. These entries are
//!   flushed in the same [`LineBuffer::push_line`] call that received them
//!   — no continuation accumulation.
//! - **Multi-line**: `[UnityCrossThreadLogger]<digit>` (date-prefixed API
//!   responses, match events) and `[Client GRE]…`. These entries
//!   accumulate continuation lines until the next header boundary, matching
//!   the historical behavior.
//!
//! # Data flow
//!
//! ```text
//! File Tailer ──(raw lines)──▸ LineBuffer ──(complete entries)──▸ Router
//! ```
//!
//! The [`LineBuffer`] receives individual lines from the file tailer. When a
//! new log entry header is detected, it flushes the previously accumulated
//! lines as a complete [`LogEntry`] and either emits the new entry
//! immediately (single-line class) or begins accumulating it (multi-line
//! class).

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

/// Internal classification of a header line for flush-timing decisions.
///
/// See module-level docs for the full classification rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HeaderClass {
    /// The entry is self-contained — flush immediately.
    SingleLine,
    /// The entry may span multiple lines — accumulate until the next header.
    MultiLine,
}

/// Accumulates raw lines and produces complete [`LogEntry`] values when a
/// new header boundary is detected.
///
/// # Usage
///
/// Feed lines one at a time via [`push_line`](Self::push_line). Each call
/// returns a `Vec<LogEntry>` containing zero, one, or two complete entries:
///
/// - **Zero entries**: continuation line for an in-progress multi-line entry,
///   or a headerless line discarded with a warning.
/// - **One entry**: either a multi-line entry being flushed by a new
///   single-line entry's arrival, or a single-line entry emitted alone when
///   no prior entry was in progress.
/// - **Two entries**: a multi-line entry being flushed *plus* the new
///   single-line entry that triggered the flush, both emitted from one call.
///
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
/// // First header (multi-line, date-prefixed) — nothing to flush yet.
/// assert!(buf.push_line("[UnityCrossThreadLogger]1/1/2025 12:00:00 PM").is_empty());
///
/// // Continuation line — still accumulating.
/// assert!(buf.push_line(r#"{"key": "value"}"#).is_empty());
///
/// // A single-line header arrives — flushes the multi-line entry AND
/// // emits the single-line entry, both in one call.
/// let entries = buf.push_line("[UnityCrossThreadLogger]STATE CHANGED");
/// assert_eq!(entries.len(), 2);
/// ```
pub struct LineBuffer {
    /// Compiled regex for detecting log entry header boundaries.
    header_re: Regex,
    /// Header of the entry currently being accumulated, if any.
    ///
    /// Only ever populated for multi-line entries. Single-line entries are
    /// emitted immediately and never set this field — leaving the buffer in
    /// an idle state after every single-line flush.
    current_header: Option<EntryHeader>,
    /// Lines accumulated for the current entry.
    lines: Vec<String>,
    /// Whether this buffer has ever emitted (or begun accumulating) an entry.
    ///
    /// Armed by [`push_line`](Self::push_line) when a real header is detected
    /// or a metadata line is emitted. Cleared back to `false` by
    /// [`reset`](Self::reset) so post-rotation orphan lines still surface a
    /// warning. Used to silence the routine post-flush "orphan discarded"
    /// warning (Phase 2 of #153 / #161): once any entry has been seen, an
    /// arriving headerless line is Unity stdout noise rather than a true
    /// file-start anomaly.
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
    /// Returns a `Vec<LogEntry>` containing 0, 1, or 2 complete entries
    /// — see the [type-level documentation](Self) for the full semantics.
    ///
    /// # Header classification
    ///
    /// When `line` matches a known header pattern, it is classified as either
    /// single-line or multi-line (see module-level docs). Single-line
    /// headers (`[UnityCrossThreadLogger]<non-digit>`, `[ConnectionManager]…`,
    /// `Matchmaking:…`) flush any prior multi-line entry and emit the new
    /// entry in the same call. Multi-line headers
    /// (`[UnityCrossThreadLogger]<digit>`, `[Client GRE]…`) flush any prior
    /// entry and begin a fresh accumulation.
    ///
    /// Metadata lines (`DETAILED LOGS: ENABLED` / `DISABLED`) are
    /// self-contained — treated as single-line entries that flush any prior
    /// in-progress entry alongside themselves.
    ///
    /// Lines that arrive before any header has been seen are discarded with
    /// a warning log — this handles partial entries at the start of a file
    /// or after rotation.
    ///
    /// # Input contract
    ///
    /// Callers must strip any trailing `\r` (Windows CRLF) before invoking
    /// this method. [`crate::log::tailer::FileTailer::poll`] already does
    /// this; direct callers in tests must do the same to keep classification
    /// well-defined.
    pub fn push_line(&mut self, line: &str) -> Vec<LogEntry> {
        // Check for metadata lines first — these are self-contained.
        if Self::is_metadata_line(line) {
            let mut out = Vec::new();
            if let Some(prior) = self.take_entry() {
                out.push(prior);
            }
            out.push(LogEntry {
                header: EntryHeader::Metadata,
                body: line.to_owned(),
            });
            // Metadata is a successfully emitted entry — subsequent orphan
            // lines are routine post-flush noise, not a file-start anomaly.
            self.has_emitted_anything = true;
            return out;
        }

        if let Some(header) = self.detect_header(line) {
            let class = Self::classify_header(header, line);
            let mut out = Vec::new();
            if let Some(prior) = self.take_entry() {
                out.push(prior);
            }
            match class {
                HeaderClass::SingleLine => {
                    // Emit the new entry immediately; leave the buffer idle
                    // so Phase 2 (#161) can distinguish post-flush orphans.
                    out.push(LogEntry {
                        header,
                        body: line.to_owned(),
                    });
                }
                HeaderClass::MultiLine => {
                    // Begin accumulating the new multi-line entry.
                    self.current_header = Some(header);
                    self.lines.push(line.to_owned());
                }
            }
            // A real header was seen — arm the flag so subsequent orphans
            // are silenced.
            self.has_emitted_anything = true;
            out
        } else if self.current_header.is_some() {
            // Continuation line for the current multi-line entry.
            self.lines.push(line.to_owned());
            Vec::new()
        } else {
            // Headerless line with no entry in progress. Two cases:
            //
            // 1. True file-start / post-rotation anomaly (no header has ever
            //    been seen): warn — this is what the message is meant to
            //    flag.
            // 2. Routine post-flush orphan (Unity stdout noise arriving
            //    between Arena entries after Phase 1's single-line flush
            //    landed): silently discard — the warn would be pure noise.
            if !self.has_emitted_anything {
                ::log::warn!(
                    "Discarding headerless line at start of input: {:?}",
                    truncate_for_log(line, 120),
                );
            }
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

    /// Classifies a header line as single-line or multi-line.
    ///
    /// Rule (corpus-verified across 27 sessions / 37,593 entries; see #153
    /// analysis comment):
    ///
    /// - `[UnityCrossThreadLogger]` followed by an ASCII digit → multi-line
    ///   (date-prefixed API responses and match events).
    /// - `[UnityCrossThreadLogger]` followed by anything else → single-line
    ///   (alpha labels and `==>` request markers).
    /// - `[Client GRE]` → multi-line (current behavior preserved; corpus
    ///   has zero coverage of this header).
    /// - `[ConnectionManager]…` → single-line.
    /// - `Matchmaking:…` → single-line.
    fn classify_header(header: EntryHeader, line: &str) -> HeaderClass {
        match header {
            EntryHeader::UnityCrossThreadLogger => {
                // Look at the first byte after the closing bracket.
                let after = line
                    .strip_prefix("[UnityCrossThreadLogger]")
                    .unwrap_or(line);
                if after.bytes().next().is_some_and(|b| b.is_ascii_digit()) {
                    HeaderClass::MultiLine
                } else {
                    HeaderClass::SingleLine
                }
            }
            EntryHeader::ClientGre => HeaderClass::MultiLine,
            // ConnectionManager and Matchmaking are corpus-confirmed
            // single-line. Metadata (`DETAILED LOGS: …`) is handled directly
            // in `push_line` and never reaches this function — but it must
            // appear here because `EntryHeader` is non_exhaustive, and a
            // single-line classification is the safe default.
            EntryHeader::ConnectionManager | EntryHeader::Matchmaking | EntryHeader::Metadata => {
                HeaderClass::SingleLine
            }
        }
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
        fn test_push_line_first_multi_line_header_returns_empty() {
            let mut buf = LineBuffer::new();
            // Date-prefixed UCTL = multi-line; nothing to flush yet.
            assert!(buf
                .push_line("[UnityCrossThreadLogger]1/1/2025 12:00:00 Event")
                .is_empty());
        }

        #[test]
        fn test_push_line_second_multi_line_header_flushes_first_entry() {
            let mut buf = LineBuffer::new();
            buf.push_line("[UnityCrossThreadLogger]1/1/2025 Event1");
            assert_eq!(
                buf.push_line("[Client GRE] 1/1/2025 Event2"),
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
                buf.push_line("[UnityCrossThreadLogger]1/1/2025 Event2"),
                vec![expected(
                    EntryHeader::UnityCrossThreadLogger,
                    "[UnityCrossThreadLogger]1/1/2025 Event1\n\
                     {\"key\": \"value\"}\n\
                     {\"more\": \"data\"}",
                )],
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

        /// Regression: `[Client GRE]` continues to accumulate continuation
        /// lines after Phase 1 (multi-line classification preserved).
        #[test]
        fn test_push_line_client_gre_header_accumulates() {
            let mut buf = LineBuffer::new();
            buf.push_line("[Client GRE] GreToClientEvent");
            buf.push_line("{");
            buf.push_line(r#"  "key": "value""#);
            buf.push_line("}");
            assert_eq!(
                buf.flush(),
                Some(expected(
                    EntryHeader::ClientGre,
                    "[Client GRE] GreToClientEvent\n{\n  \"key\": \"value\"\n}",
                )),
            );
        }

        #[test]
        fn test_push_line_alternating_multi_line_headers() {
            let mut buf = LineBuffer::new();
            buf.push_line("[UnityCrossThreadLogger]1/1/2025 Event1");

            assert_eq!(
                buf.push_line("[Client GRE] Event2"),
                vec![expected(
                    EntryHeader::UnityCrossThreadLogger,
                    "[UnityCrossThreadLogger]1/1/2025 Event1",
                )],
            );

            assert_eq!(
                buf.push_line("[UnityCrossThreadLogger]1/1/2025 Event3"),
                vec![expected(EntryHeader::ClientGre, "[Client GRE] Event2")],
            );

            assert_eq!(
                buf.flush(),
                Some(expected(
                    EntryHeader::UnityCrossThreadLogger,
                    "[UnityCrossThreadLogger]1/1/2025 Event3",
                )),
            );
        }
    }

    // -- LineBuffer: single-line flush (Phase 1 of #153) -------------------

    mod single_line_flush {
        use super::*;

        /// `[UnityCrossThreadLogger]` followed by an alpha label (e.g.,
        /// `STATE CHANGED`) is single-line — emit immediately, leave the
        /// buffer idle.
        #[test]
        fn test_push_line_single_line_uctl_label_flushes_immediately() {
            let mut buf = LineBuffer::new();
            let entries = buf.push_line(
                "[UnityCrossThreadLogger]STATE CHANGED \
                 {\"old\":\"None\",\"new\":\"ConnectedToMatchDoor\"}",
            );
            assert_eq!(
                entries,
                vec![expected(
                    EntryHeader::UnityCrossThreadLogger,
                    "[UnityCrossThreadLogger]STATE CHANGED \
                     {\"old\":\"None\",\"new\":\"ConnectedToMatchDoor\"}",
                )],
            );
            assert!(
                buf.is_empty(),
                "buffer must be idle after a single-line flush",
            );
        }

        /// `[UnityCrossThreadLogger]==>` API request markers are single-line.
        #[test]
        fn test_push_line_single_line_uctl_arrow_flushes_immediately() {
            let mut buf = LineBuffer::new();
            let entries = buf.push_line(
                "[UnityCrossThreadLogger]==> GraphGetGraphState \
                 {\"id\":\"abc\",\"request\":\"{}\"}",
            );
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].header, EntryHeader::UnityCrossThreadLogger);
            assert!(buf.is_empty());
        }

        /// `[UnityCrossThreadLogger]Client.SceneChange {…}` exercises a
        /// nested-bracket case where the continuation-detection logic must
        /// not be confused by the inner `{` body.
        #[test]
        fn test_push_line_single_line_uctl_nested_bracket_flushes_immediately() {
            let mut buf = LineBuffer::new();
            let entries = buf.push_line(
                "[UnityCrossThreadLogger]Client.SceneChange \
                 {\"fromSceneName\":\"Home\",\"toSceneName\":\"Draft\"}",
            );
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].header, EntryHeader::UnityCrossThreadLogger);
            assert!(buf.is_empty());
        }

        /// `[ConnectionManager]…` is single-line.
        #[test]
        fn test_push_line_single_line_connection_manager_flushes_immediately() {
            let mut buf = LineBuffer::new();
            let entries = buf.push_line("[ConnectionManager] Reconnect succeeded");
            assert_eq!(
                entries,
                vec![expected(
                    EntryHeader::ConnectionManager,
                    "[ConnectionManager] Reconnect succeeded",
                )],
            );
            assert!(buf.is_empty());
        }

        /// `Matchmaking:…` is single-line.
        #[test]
        fn test_push_line_single_line_matchmaking_flushes_immediately() {
            let mut buf = LineBuffer::new();
            let entries = buf.push_line("Matchmaking: GRE connection lost");
            assert_eq!(
                entries,
                vec![expected(
                    EntryHeader::Matchmaking,
                    "Matchmaking: GRE connection lost",
                )],
            );
            assert!(buf.is_empty());
        }

        /// Multi-line headers (`[UnityCrossThreadLogger]<digit>`) keep
        /// accumulating continuation lines until the next header — regression
        /// guard for API-response handling.
        #[test]
        fn test_push_line_multi_line_date_header_accumulates() {
            let mut buf = LineBuffer::new();
            assert!(buf
                .push_line("[UnityCrossThreadLogger]3/11/2026 6:08:24 PM")
                .is_empty());
            assert!(buf.push_line("<== EventGetCoursesV2(abc-123)").is_empty());
            assert!(buf.push_line(r#"{"Courses":[]}"#).is_empty());

            // Next header (a single-line UCTL alpha label) flushes the
            // multi-line entry AND emits itself — both in one call.
            let entries = buf.push_line("[UnityCrossThreadLogger]Client.SceneChange {}");
            assert_eq!(entries.len(), 2);
            assert_eq!(entries[0].header, EntryHeader::UnityCrossThreadLogger);
            assert_eq!(
                entries[0].body,
                "[UnityCrossThreadLogger]3/11/2026 6:08:24 PM\n\
                 <== EventGetCoursesV2(abc-123)\n\
                 {\"Courses\":[]}",
            );
            assert_eq!(entries[1].header, EntryHeader::UnityCrossThreadLogger);
            assert_eq!(
                entries[1].body,
                "[UnityCrossThreadLogger]Client.SceneChange {}",
            );
        }

        /// Unity stdout noise that arrives *after* a single-line flush is
        /// orphaned (the buffer is idle) and discarded — it must not be
        /// absorbed into the prior entry's body.
        #[test]
        fn test_push_line_post_single_line_orphan_discarded() {
            let mut buf = LineBuffer::new();
            // Single-line header — buffer goes idle immediately after.
            let first = buf.push_line("[UnityCrossThreadLogger]STATE CHANGED {\"x\":1}");
            assert_eq!(first.len(), 1);
            assert!(buf.is_empty());

            // Unity stdout noise → orphan, discarded.
            let noise = buf.push_line("PreviousPlayBladeVisualState is being set ...");
            assert!(noise.is_empty());
            assert!(buf.is_empty());

            // Next header — emit cleanly with no contamination from the noise.
            let next = buf.push_line("[UnityCrossThreadLogger]Connecting to matchId abc");
            assert_eq!(next.len(), 1);
            assert!(!next[0].body.contains("PreviousPlayBladeVisualState"));
        }

        /// A multi-line entry being flushed by a single-line header must
        /// emit BOTH entries from one `push_line` call.
        #[test]
        fn test_push_line_multi_line_then_single_line_emits_two() {
            let mut buf = LineBuffer::new();
            buf.push_line("[UnityCrossThreadLogger]3/11/2026 6:08:24 PM");
            buf.push_line("<== Foo(123)");

            let entries = buf.push_line("[ConnectionManager] Reconnect failed");
            assert_eq!(entries.len(), 2);
            assert_eq!(entries[0].header, EntryHeader::UnityCrossThreadLogger);
            assert!(entries[0].body.contains("<== Foo(123)"));
            assert_eq!(entries[1].header, EntryHeader::ConnectionManager);
            assert_eq!(entries[1].body, "[ConnectionManager] Reconnect failed");
            assert!(buf.is_empty());
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
                buf.flush(),
                Some(expected(
                    EntryHeader::UnityCrossThreadLogger,
                    "[UnityCrossThreadLogger]1/1/2025 Real entry",
                )),
            );
        }

        #[test]
        fn test_push_line_empty_line_as_continuation() {
            let mut buf = LineBuffer::new();
            buf.push_line("[UnityCrossThreadLogger]1/1/2025 Event");
            buf.push_line("");
            buf.push_line("continuation");
            assert_eq!(
                buf.flush(),
                Some(expected(
                    EntryHeader::UnityCrossThreadLogger,
                    "[UnityCrossThreadLogger]1/1/2025 Event\n\ncontinuation",
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
        fn test_flush_returns_buffered_multi_line_entry() {
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
        fn test_is_empty_false_after_multi_line_header() {
            let mut buf = LineBuffer::new();
            buf.push_line("[UnityCrossThreadLogger]1/1/2025 Event");
            assert!(!buf.is_empty());
        }

        /// Single-line entries leave the buffer idle — invariant relied on
        /// by Phase 2 (#161).
        #[test]
        fn test_is_empty_true_after_single_line_flush() {
            let mut buf = LineBuffer::new();
            buf.push_line("[UnityCrossThreadLogger]STATE CHANGED");
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
                buf.flush(),
                Some(expected(
                    EntryHeader::UnityCrossThreadLogger,
                    "[UnityCrossThreadLogger]1/1/2025 Event",
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
            buf.push_line("[UnityCrossThreadLogger]1/1/2025 Event");
            // Header pattern in the middle of a line is NOT a boundary.
            buf.push_line("some text [UnityCrossThreadLogger] not a header");
            assert_eq!(
                buf.flush(),
                Some(expected(
                    EntryHeader::UnityCrossThreadLogger,
                    "[UnityCrossThreadLogger]1/1/2025 Event\n\
                     some text [UnityCrossThreadLogger] not a header",
                )),
            );
        }

        #[test]
        fn test_similar_but_wrong_header_is_continuation() {
            let mut buf = LineBuffer::new();
            buf.push_line("[UnityCrossThreadLogger]1/1/2025 Event");
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
            buf.push_line("[UnityCrossThreadLogger]1/1/2025 Event");
            buf.push_line("[]");
            assert_eq!(
                buf.flush(),
                Some(expected(
                    EntryHeader::UnityCrossThreadLogger,
                    "[UnityCrossThreadLogger]1/1/2025 Event\n[]",
                )),
            );
        }

        #[test]
        fn test_header_with_nothing_after_bracket() {
            let mut buf = LineBuffer::new();
            // `[UnityCrossThreadLogger]` with no trailing content classifies
            // as single-line (no leading digit) — emit and go idle.
            let entries = buf.push_line("[UnityCrossThreadLogger]");
            assert_eq!(
                entries,
                vec![expected(
                    EntryHeader::UnityCrossThreadLogger,
                    "[UnityCrossThreadLogger]",
                )],
            );
            assert!(buf.is_empty());
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

            // [Client GRE] (multi-line) flushes the UnityCrossThreadLogger entry.
            let unity_entries = buf.push_line("[Client GRE] Next event");
            assert_eq!(unity_entries.len(), 1);
            assert_eq!(unity_entries[0].header, EntryHeader::UnityCrossThreadLogger);
            assert!(unity_entries[0].body.contains("greToClientMessages"));
            assert!(unity_entries[0].body.contains("GameStateMessage"));

            // Another header flushes the Client GRE entry.
            assert_eq!(
                buf.push_line("[UnityCrossThreadLogger]1/15/2025 After"),
                vec![expected(EntryHeader::ClientGre, "[Client GRE] Next event")],
            );
        }

        #[test]
        fn test_many_single_line_entries_in_sequence() {
            let mut buf = LineBuffer::new();
            let mut entries = Vec::new();

            for i in 0..5 {
                // Single-line UCTL alpha labels — each flushes immediately.
                entries.extend(buf.push_line(&format!("[UnityCrossThreadLogger]Event{i}")));
            }
            entries.extend(buf.flush());

            assert_eq!(entries.len(), 5);
            for (i, e) in entries.iter().enumerate() {
                assert_eq!(e.header, EntryHeader::UnityCrossThreadLogger);
                assert_eq!(e.body, format!("[UnityCrossThreadLogger]Event{i}"));
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
                vec![expected(EntryHeader::Metadata, "DETAILED LOGS: ENABLED")],
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
                vec![expected(EntryHeader::Metadata, "DETAILED LOGS: DISABLED")],
            );
            assert!(buf.is_empty());
        }

        /// Metadata after an in-progress multi-line entry flushes the prior
        /// entry AND emits the metadata entry — both in one call.
        #[test]
        fn test_push_line_metadata_flushes_buffered_entry() {
            let mut buf = LineBuffer::new();
            buf.push_line("[UnityCrossThreadLogger]1/1/2025 Event1");

            let entries = buf.push_line("DETAILED LOGS: ENABLED");
            assert_eq!(
                entries,
                vec![
                    expected(
                        EntryHeader::UnityCrossThreadLogger,
                        "[UnityCrossThreadLogger]1/1/2025 Event1",
                    ),
                    expected(EntryHeader::Metadata, "DETAILED LOGS: ENABLED"),
                ],
            );
            // Buffer is idle after — metadata is self-contained.
            assert!(buf.is_empty());
        }

        #[test]
        fn test_push_line_metadata_then_header_flushes_metadata() {
            let mut buf = LineBuffer::new();
            buf.push_line("DETAILED LOGS: ENABLED");

            // Next multi-line header — nothing to flush (metadata was emitted
            // immediately on its own call).
            assert!(buf
                .push_line("[UnityCrossThreadLogger]1/1/2025 Event")
                .is_empty());
            assert_eq!(
                buf.flush(),
                Some(expected(
                    EntryHeader::UnityCrossThreadLogger,
                    "[UnityCrossThreadLogger]1/1/2025 Event",
                )),
            );
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
            // Whitespace around the exact text should still match.
            let result = buf.push_line("  DETAILED LOGS: ENABLED  ");
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

        /// In-test log capture: records every record's level + message so the
        /// gating tests can assert whether a warn fired.
        ///
        /// `log` only allows one global logger per process. We install it
        /// once via `OnceLock` and serialize the gating tests through a mutex
        /// so the captured-record buffer can be inspected race-free.
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

        /// Installs the capture logger (idempotent) and returns a handle to
        /// the global capture buffer.
        ///
        /// The capture buffer accumulates records from every test that runs
        /// in this process, so callers MUST filter the captured records by a
        /// per-test sentinel marker — see [`warn_count_matching`]. This
        /// avoids the parallel-test race that a "clear before each test"
        /// strategy would introduce.
        fn install_capture() -> RecordsRef {
            let logger = LOGGER.get_or_init(|| {
                let leaked: &'static CaptureLogger = Box::leak(Box::new(CaptureLogger {
                    records: Mutex::new(Vec::new()),
                }));
                // `set_logger` errors if a logger is already installed by
                // another test setup; in that case our captures will be
                // silently dropped, which is acceptable here because the
                // gating logic is also covered by behavioral tests
                // (`is_empty`, header round-trips) above.
                let _ = ::log::set_logger(leaked);
                ::log::set_max_level(::log::LevelFilter::Trace);
                leaked
            });
            &logger.records
        }

        /// Counts captured warn-level records that contain `marker` in the
        /// message body.
        ///
        /// Tests pass a per-test sentinel string as the orphan input so the
        /// captured warning's truncated payload contains that sentinel.
        /// Filtering on the sentinel makes the count race-free even though
        /// Rust's test harness runs tests in parallel by default and other
        /// modules' tests share the same global logger.
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

        /// Orphan line before any header has been seen still produces the
        /// existing warning — this is the file-start anomaly the message was
        /// originally meant to flag.
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

        /// After a single-line entry has flushed, a subsequent headerless
        /// line is routine Unity stdout noise — silently discard, no warn.
        #[test]
        fn test_push_line_post_flush_orphan_silent() {
            const MARKER: &str = "P2-MARKER-POST-FLUSH-SILENT-kJ7w";
            let records = install_capture();
            let mut buf = LineBuffer::new();

            // Single-line flush arms the gating flag.
            let entries = buf.push_line("[UnityCrossThreadLogger]STATE CHANGED {\"x\":1}");
            assert_eq!(entries.len(), 1);
            assert!(buf.is_empty());

            // Unity stdout noise — should be silently dropped.
            assert!(buf.push_line(MARKER).is_empty());

            assert_eq!(
                warn_count_matching(records, MARKER),
                0,
                "post-flush orphan must be silently discarded (no warn)",
            );
        }

        /// `reset()` re-arms the warning so post-rotation orphans still
        /// surface — the rotation case the warn was originally meant to
        /// catch.
        #[test]
        fn test_push_line_orphan_after_reset_warns() {
            const MARKER: &str = "P2-MARKER-AFTER-RESET-WARNS-vN2t";
            let records = install_capture();
            let mut buf = LineBuffer::new();

            // Flush an entry to arm the flag.
            assert_eq!(
                buf.push_line("[UnityCrossThreadLogger]STATE CHANGED {}")
                    .len(),
                1,
            );

            // Simulate file rotation — flag must drop back to false.
            buf.reset();

            // First orphan after reset must warn again.
            assert!(buf.push_line(MARKER).is_empty());

            assert_eq!(
                warn_count_matching(records, MARKER),
                1,
                "first orphan after reset must warn (rotation anomaly)",
            );
        }

        /// A metadata line (`DETAILED LOGS: ENABLED`) is a successfully
        /// emitted entry, so subsequent orphan lines are post-flush noise
        /// and must be silently discarded.
        #[test]
        fn test_push_line_orphan_after_metadata_silent() {
            const MARKER: &str = "P2-MARKER-AFTER-METADATA-SILENT-bH4r";
            let records = install_capture();
            let mut buf = LineBuffer::new();

            // Metadata arms the flag.
            let entries = buf.push_line("DETAILED LOGS: ENABLED");
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].header, EntryHeader::Metadata);

            // Subsequent orphan — silent.
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
            buf.push_line("[UnityCrossThreadLogger]1/1/2025 Event1");

            let entries = buf.push_line("[ConnectionManager] Reconnect result : Error");
            assert_eq!(
                entries,
                vec![
                    expected(
                        EntryHeader::UnityCrossThreadLogger,
                        "[UnityCrossThreadLogger]1/1/2025 Event1",
                    ),
                    expected(
                        EntryHeader::ConnectionManager,
                        "[ConnectionManager] Reconnect result : Error",
                    ),
                ],
            );
            // ConnectionManager is single-line — buffer is idle.
            assert!(buf.is_empty());
        }

        #[test]
        fn test_matchmaking_header_mid_stream_flushes_unity() {
            let mut buf = LineBuffer::new();
            buf.push_line("[UnityCrossThreadLogger]1/1/2025 Event1");

            let entries = buf.push_line("Matchmaking: GRE connection lost");
            assert_eq!(
                entries,
                vec![
                    expected(
                        EntryHeader::UnityCrossThreadLogger,
                        "[UnityCrossThreadLogger]1/1/2025 Event1",
                    ),
                    expected(EntryHeader::Matchmaking, "Matchmaking: GRE connection lost",),
                ],
            );
            assert!(buf.is_empty());
        }

        #[test]
        fn test_connection_manager_as_first_line_emits_immediately() {
            // Single-line semantics: the ConnectionManager entry is emitted
            // by the same `push_line` call that received it.
            let mut buf = LineBuffer::new();
            let entries = buf.push_line("[ConnectionManager] Reconnect succeeded");
            assert_eq!(
                entries,
                vec![expected(
                    EntryHeader::ConnectionManager,
                    "[ConnectionManager] Reconnect succeeded",
                )],
            );
            assert!(buf.is_empty());
        }

        #[test]
        fn test_matchmaking_as_first_line_emits_immediately() {
            let mut buf = LineBuffer::new();
            let entries = buf.push_line("Matchmaking: GRE connection lost");
            assert_eq!(
                entries,
                vec![expected(
                    EntryHeader::Matchmaking,
                    "Matchmaking: GRE connection lost",
                )],
            );
            assert!(buf.is_empty());
        }

        #[test]
        fn test_four_way_interleave_yields_four_entries() {
            // Realistic corpus-derived pattern from issues #528/#529:
            // Unity STATE CHANGED → Matchmaking: GRE connection lost →
            // ConnectionManager Reconnect result → Unity (next event).
            // All four are single-line, so each `push_line` returns 1 entry.
            let mut buf = LineBuffer::new();
            let mut entries = Vec::new();

            entries.extend(buf.push_line(
                "[UnityCrossThreadLogger]STATE CHANGED \
                 {\"old\":\"Playing\",\"new\":\"Disconnected\"}",
            ));
            entries.extend(buf.push_line("Matchmaking: GRE connection lost"));
            entries.extend(buf.push_line("[ConnectionManager] Reconnect result : Error"));
            entries.extend(buf.push_line("[UnityCrossThreadLogger]Next event"));
            entries.extend(buf.flush());

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
            assert_eq!(entries[3].body, "[UnityCrossThreadLogger]Next event");
        }

        #[test]
        fn test_matchmaking_without_trailing_space_is_not_header() {
            // The starts_with check requires the trailing space ("Matchmaking: ")
            // to avoid matching unrelated prefixes that happen to start
            // with "Matchmaking:". Without the space it should be a
            // headerless line (discarded at start of stream).
            let mut buf = LineBuffer::new();
            assert!(buf.push_line("Matchmaking:compact-no-space").is_empty());
            assert!(buf.is_empty());
        }

        #[test]
        fn test_connection_manager_mid_line_is_continuation() {
            let mut buf = LineBuffer::new();
            buf.push_line("[UnityCrossThreadLogger]1/1/2025 Event");
            // ConnectionManager bracket pattern in the middle of a line is
            // NOT a boundary — same rule as other bracketed headers.
            buf.push_line("some text [ConnectionManager] not a header");
            assert_eq!(
                buf.flush(),
                Some(expected(
                    EntryHeader::UnityCrossThreadLogger,
                    "[UnityCrossThreadLogger]1/1/2025 Event\n\
                     some text [ConnectionManager] not a header",
                )),
            );
        }
    }
}
