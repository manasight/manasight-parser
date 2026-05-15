//! Log entry prefix identification and multi-line JSON accumulation.
//!
//! Detects log entry boundaries using the `[UnityCrossThreadLogger]`,
//! `[Client GRE]`, `[ConnectionManager]`, and `Matchmaking:` header patterns,
//! then accumulates subsequent lines until the entry is structurally complete.
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
//!   accumulate continuation lines until the entry's JSON body is
//!   structurally complete (brace-balance flush) or the next header arrives
//!   (fallback for non-JSON bodies).
//!
//! # Brace-balance flush (Phase 3 of #153 / #193)
//!
//! Multi-line entries whose body contains a `{` are flushed the moment the
//! brace depth returns to 0 — they no longer wait for the next header to
//! arrive. A small state machine counts `{` and `}` while tracking string
//! literals (`"`) and backslash escapes (`\\`), so braces appearing inside
//! JSON string values do not count. Corpus analysis (44 sessions, 47,412
//! multi-line entries) shows every entry that opens a `{` closes it within
//! the entry boundary; bodies that never open a `{` (a few `true`-only REST
//! responses and the [`EntryHeader::TruncationMarker`] entries whose
//! follow-on `:: ... Count = N` lines carry no JSON braces) still flush on
//! the next header via the original fallback path.
//!
//! This behavior is enabled by default via the `brace_depth_flush` cargo
//! feature. Disabling the feature reverts to the original "flush on next
//! header" behavior for every multi-line entry — kept as a one-flip rollback
//! in case a string-literal edge case surfaces in live Arena traffic.
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

/// Prefix of MTGA's GSM truncation marker line.
///
/// The full line in the log is
/// `[Message summarized because one or more GameStateMessages exceeded the 50
/// GameObject or 50 Annotation limit.]`. The shorter `"[Message summarized"`
/// prefix is sufficient to detect the marker without coupling to the exact
/// wording (Arena could vary punctuation or rephrase the suffix) and has zero
/// false-positive matches in the corpus.
const TRUNCATION_MARKER_PREFIX: &str = "[Message summarized";

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
    /// `[Message summarized because one or more GameStateMessages exceeded the
    /// 50 GameObject or 50 Annotation limit.]` — Arena's truncation marker
    /// emitted in place of an oversized `GameStateMessage` body. The marker
    /// is followed by `::: GameStateMessage`, `:: GameObject Count = N`,
    /// `:: Annotation Count = M`, and the next sibling message header. The
    /// GSM body itself is irrecoverable from `Player.log`; this header
    /// surfaces the signal so downstream consumers can detect a missed
    /// `gsm_id` via the gap.
    TruncationMarker,
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
            Self::TruncationMarker => "[Message summarized]",
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

/// Accumulates raw lines and produces complete [`LogEntry`] values when an
/// entry is structurally complete.
///
/// # Usage
///
/// Feed lines one at a time via [`push_line`](Self::push_line). Each call
/// returns a `Vec<LogEntry>` containing zero, one, or two complete entries:
///
/// - **Zero entries**: continuation line for an in-progress multi-line entry,
///   or a headerless line discarded with a warning.
/// - **One entry**: a single-line entry emitted on arrival, a multi-line
///   entry being brace-balance-flushed (default feature behavior), or a
///   multi-line entry being flushed by the arrival of the next header.
/// - **Two entries**: a multi-line entry being flushed by a new header
///   *plus* the new single-line entry that triggered the flush, both
///   emitted from one call.
///
/// After the input stream ends (EOF or file rotation), call
/// [`flush`](Self::flush) to retrieve any remaining buffered entry.
///
/// # Flush triggers
///
/// With the default `brace_depth_flush` feature enabled, a multi-line
/// entry flushes the moment its body's JSON depth returns to 0 — no need
/// to wait for the next header. Bodies that never contain a `{` (rare
/// non-JSON GRE markers and `true`-bodied REST responses) still fall back
/// to the original "flush on next header" path. See the module-level docs
/// for the corpus analysis backing this design.
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
/// // Continuation line opens a `{` — still accumulating until the body
/// // brace-balances (or, with the feature disabled, until the next header).
/// assert!(buf.push_line(r#"{"key": "ba"#).is_empty());
///
/// // The body's brace depth returns to 0 — entry flushes immediately
/// // (default feature on); the next header is not required.
/// let entries = buf.push_line(r#"  r"}"#);
/// # #[cfg(feature = "brace_depth_flush")]
/// assert_eq!(entries.len(), 1);
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

    /// Brace-balance state machine used to detect structurally complete
    /// JSON bodies inside multi-line entries. See [`BraceState`].
    #[cfg(feature = "brace_depth_flush")]
    brace_state: BraceState,
}

/// In-entry brace-depth and string-literal state for the brace-balance
/// flush trigger. See [`LineBuffer::advance_brace_state`].
///
/// Grouped into its own struct so [`LineBuffer`] does not exceed clippy's
/// pedantic `struct_excessive_bools` threshold once all four fields are
/// added — and to make the "reset to defaults on take/reset/new" pattern
/// a single field swap rather than four parallel writes.
#[cfg(feature = "brace_depth_flush")]
#[derive(Default)]
struct BraceState {
    /// Running brace depth for the current entry's body. Zero when no `{`
    /// has been seen yet in this entry. Combined with [`Self::ever_opened`],
    /// returning to 0 signals a structurally complete JSON body.
    depth: u32,
    /// Whether the character cursor is currently inside a JSON string literal.
    /// Toggled by an unescaped `"`; braces inside a string literal are
    /// ignored so structurally-complete JSON bodies cannot be falsely
    /// signaled by `{`/`}` characters embedded in string values.
    in_string: bool,
    /// Whether the next character should be treated as escaped — i.e., the
    /// previous character was a backslash inside a string literal.
    escape_pending: bool,
    /// True once any `{` has been observed in the current entry's body.
    /// Combined with `depth == 0`, signals a complete JSON body and triggers
    /// an immediate flush. Entries that never open a `{` keep this false
    /// and fall through to the next-header flush path.
    ever_opened: bool,
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
            #[cfg(feature = "brace_depth_flush")]
            brace_state: BraceState::default(),
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
            #[cfg(feature = "brace_depth_flush")]
            if self.advance_brace_state(line) {
                // The body's JSON depth has returned to 0 with at least one
                // `{` seen — the entry is structurally complete. Flush now
                // rather than waiting for the next header to arrive.
                if let Some(entry) = self.take_entry() {
                    return vec![entry];
                }
            }
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
        #[cfg(feature = "brace_depth_flush")]
        {
            self.brace_state = BraceState::default();
        }
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
        if line.starts_with(TRUNCATION_MARKER_PREFIX) {
            return Some(EntryHeader::TruncationMarker);
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
            // `ClientGre` accumulates its full JSON body until brace-balance
            // flush (default feature) or the next header arrives.
            //
            // `TruncationMarker` is followed by 3 sub-header lines
            // (`::: GameStateMessage`, `:: GameObject Count = N`,
            // `:: Annotation Count = M`) that must accumulate into the entry
            // body so the thin truncation parser can extract the counts.
            // Its body never opens a `{`, so brace-balance flush doesn't
            // fire — accumulation terminates when the next header arrives.
            EntryHeader::ClientGre | EntryHeader::TruncationMarker => HeaderClass::MultiLine,
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
        #[cfg(feature = "brace_depth_flush")]
        {
            self.brace_state = BraceState::default();
        }
        Some(LogEntry { header, body })
    }

    /// Walks `line` one character at a time, updating the in-string /
    /// escape / depth state used by the brace-balance flush trigger.
    ///
    /// Returns `true` when the entry's body is structurally complete — i.e.,
    /// the running brace depth is 0 *and* at least one `{` has been observed
    /// since the entry started accumulating. Returning `true` signals
    /// [`push_line`](Self::push_line) to flush the entry immediately.
    ///
    /// The state machine treats `"` as a string-literal toggle (when not
    /// preceded by an unescaped backslash) and `\\` as an escape marker for
    /// the next character. Braces appearing inside string literals are
    /// ignored. Corpus analysis (44 sessions, 47,412 multi-line entries)
    /// shows this state machine balances correctly on every entry that
    /// opens a `{`, including 585 with nested JSON-in-string values.
    #[cfg(feature = "brace_depth_flush")]
    fn advance_brace_state(&mut self, line: &str) -> bool {
        let state = &mut self.brace_state;
        for ch in line.chars() {
            if state.escape_pending {
                state.escape_pending = false;
                continue;
            }
            if state.in_string {
                match ch {
                    '\\' => state.escape_pending = true,
                    '"' => state.in_string = false,
                    _ => {}
                }
                continue;
            }
            match ch {
                '"' => state.in_string = true,
                '{' => {
                    state.depth = state.depth.saturating_add(1);
                    state.ever_opened = true;
                }
                '}' => {
                    if state.depth == 0 {
                        // Corpus has zero unbalanced cases — log an
                        // observability warning so any future drift surfaces
                        // rather than being silently floored at zero.
                        ::log::warn!(
                            "brace_depth underflow at unbalanced '}}' in entry body \
                             (line prefix: {:?})",
                            truncate_for_log(line, 120),
                        );
                    }
                    state.depth = state.depth.saturating_sub(1);
                }
                _ => {}
            }
        }
        state.ever_opened && state.depth == 0
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
            // Body has no `{`/`}`, so brace-balance flush does not trigger
            // — the entry accumulates until the next header arrives under
            // both feature configurations.
            let mut buf = LineBuffer::new();
            buf.push_line("[UnityCrossThreadLogger]1/1/2025 Event1");
            buf.push_line("plain text continuation one");
            buf.push_line("plain text continuation two");
            assert_eq!(
                buf.push_line("[UnityCrossThreadLogger]1/1/2025 Event2"),
                vec![expected(
                    EntryHeader::UnityCrossThreadLogger,
                    "[UnityCrossThreadLogger]1/1/2025 Event1\n\
                     plain text continuation one\n\
                     plain text continuation two",
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
        /// lines after Phase 1 (multi-line classification preserved). With
        /// the default `brace_depth_flush` feature on, the entry is emitted
        /// by the closing `}` line via `push_line` rather than waiting for
        /// `flush()` — both code paths assemble the same body.
        #[test]
        fn test_push_line_client_gre_header_accumulates() {
            let expected_body = "[Client GRE] GreToClientEvent\n{\n  \"key\": \"value\"\n}";
            let mut buf = LineBuffer::new();
            buf.push_line("[Client GRE] GreToClientEvent");
            buf.push_line("{");
            buf.push_line(r#"  "key": "value""#);
            let closing = buf.push_line("}");
            #[cfg(feature = "brace_depth_flush")]
            {
                assert_eq!(
                    closing,
                    vec![expected(EntryHeader::ClientGre, expected_body)],
                    "closing brace must flush the entry under brace_depth_flush",
                );
                assert!(buf.flush().is_none());
            }
            #[cfg(not(feature = "brace_depth_flush"))]
            {
                assert!(closing.is_empty());
                assert_eq!(
                    buf.flush(),
                    Some(expected(EntryHeader::ClientGre, expected_body)),
                );
            }
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

        /// Multi-line headers (`[UnityCrossThreadLogger]<digit>`) accumulate
        /// continuation lines and produce the same body under both feature
        /// configurations. The only difference is *when* the entry is
        /// emitted: with `brace_depth_flush` on, the closing `}` of
        /// `{"Courses":[]}` flushes it; without the feature, the next
        /// header flushes it alongside its own single-line emission.
        #[test]
        fn test_push_line_multi_line_date_header_accumulates() {
            let expected_multi_body = "[UnityCrossThreadLogger]3/11/2026 6:08:24 PM\n\
                                       <== EventGetCoursesV2(abc-123)\n\
                                       {\"Courses\":[]}";
            let expected_single_body = "[UnityCrossThreadLogger]Client.SceneChange {}";

            let mut buf = LineBuffer::new();
            assert!(buf
                .push_line("[UnityCrossThreadLogger]3/11/2026 6:08:24 PM")
                .is_empty());
            assert!(buf.push_line("<== EventGetCoursesV2(abc-123)").is_empty());
            let closing = buf.push_line(r#"{"Courses":[]}"#);

            #[cfg(feature = "brace_depth_flush")]
            {
                // The closing `}` of `{"Courses":[]}` brace-balance flushes.
                assert_eq!(
                    closing,
                    vec![expected(
                        EntryHeader::UnityCrossThreadLogger,
                        expected_multi_body
                    )],
                );
                // The next single-line header now stands alone.
                let entries = buf.push_line("[UnityCrossThreadLogger]Client.SceneChange {}");
                assert_eq!(entries.len(), 1);
                assert_eq!(entries[0].header, EntryHeader::UnityCrossThreadLogger);
                assert_eq!(entries[0].body, expected_single_body);
            }
            #[cfg(not(feature = "brace_depth_flush"))]
            {
                assert!(closing.is_empty());
                let entries = buf.push_line("[UnityCrossThreadLogger]Client.SceneChange {}");
                assert_eq!(entries.len(), 2);
                assert_eq!(entries[0].header, EntryHeader::UnityCrossThreadLogger);
                assert_eq!(entries[0].body, expected_multi_body);
                assert_eq!(entries[1].header, EntryHeader::UnityCrossThreadLogger);
                assert_eq!(entries[1].body, expected_single_body);
            }
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
            let expected_body = [
                "[Client GRE] GreToClientEvent",
                "{",
                r#"  "gameObjects": ["obj1", "obj2"],"#,
                r#"  "actions": []"#,
                "}",
            ]
            .join("\n");

            let mut buf = LineBuffer::new();
            buf.push_line("[Client GRE] GreToClientEvent");
            buf.push_line("{");
            buf.push_line(r#"  "gameObjects": ["obj1", "obj2"],"#);
            buf.push_line(r#"  "actions": []"#);
            let closing = buf.push_line("}");

            #[cfg(feature = "brace_depth_flush")]
            {
                // The closing `}` brace-balance flushes the entry; `flush()`
                // is left with nothing to return.
                assert_eq!(
                    closing,
                    vec![expected(EntryHeader::ClientGre, &expected_body)],
                );
                assert!(buf.flush().is_none());
            }
            #[cfg(not(feature = "brace_depth_flush"))]
            {
                assert!(closing.is_empty());
                assert_eq!(
                    buf.flush(),
                    Some(expected(EntryHeader::ClientGre, &expected_body)),
                );
            }
        }
    }

    // -- LineBuffer: reset --------------------------------------------------

    mod reset {
        use super::*;

        #[test]
        fn test_reset_clears_in_progress_entry() {
            let mut buf = LineBuffer::new();
            buf.push_line("[UnityCrossThreadLogger]1/1/2025 Event");
            // Continuation with an open `{` so brace state is non-trivial
            // when we reset — depth=1, ever_opened=true, in_string=true.
            buf.push_line(r#"{"k": "unfinished"#);
            buf.reset();
            assert!(buf.is_empty());
            assert!(buf.flush().is_none());

            // Brace state must also clear so the next accumulation starts
            // from a clean slate (otherwise stale `ever_opened` would
            // spuriously flush the next entry).
            #[cfg(feature = "brace_depth_flush")]
            {
                assert_eq!(buf.brace_state.depth, 0, "reset() must clear depth");
                assert!(!buf.brace_state.in_string, "reset() must clear in_string");
                assert!(
                    !buf.brace_state.escape_pending,
                    "reset() must clear escape_pending",
                );
                assert!(
                    !buf.brace_state.ever_opened,
                    "reset() must clear ever_opened",
                );
            }
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
            let final_brace = buf.push_line("}");

            #[cfg(feature = "brace_depth_flush")]
            {
                // The matching final `}` brace-balance flushes the UCTL
                // entry inside the same `push_line` that received it.
                assert_eq!(final_brace.len(), 1);
                assert_eq!(final_brace[0].header, EntryHeader::UnityCrossThreadLogger);
                assert!(final_brace[0].body.contains("greToClientMessages"));
                assert!(final_brace[0].body.contains("GameStateMessage"));

                // `[Client GRE] Next event` now begins a new accumulation
                // — nothing else to flush.
                assert!(buf.push_line("[Client GRE] Next event").is_empty());

                // The Client-GRE body has no `{`, so it falls through to
                // the legacy "flush on next header" path.
                assert_eq!(
                    buf.push_line("[UnityCrossThreadLogger]1/15/2025 After"),
                    vec![expected(EntryHeader::ClientGre, "[Client GRE] Next event")],
                );
            }
            #[cfg(not(feature = "brace_depth_flush"))]
            {
                assert!(final_brace.is_empty());

                // [Client GRE] (multi-line) flushes the UCTL entry.
                let unity_entries = buf.push_line("[Client GRE] Next event");
                assert_eq!(unity_entries.len(), 1);
                assert_eq!(unity_entries[0].header, EntryHeader::UnityCrossThreadLogger);
                assert!(unity_entries[0].body.contains("greToClientMessages"));
                assert!(unity_entries[0].body.contains("GameStateMessage"));

                // The next header flushes the Client GRE entry.
                assert_eq!(
                    buf.push_line("[UnityCrossThreadLogger]1/15/2025 After"),
                    vec![expected(EntryHeader::ClientGre, "[Client GRE] Next event")],
                );
            }
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

    // -- Brace-depth flush (#193) -------------------------------------------

    #[cfg(feature = "brace_depth_flush")]
    mod brace_depth_flush {
        use super::*;

        /// Header + single-line `{...}` body — the closing `}` flushes the
        /// entry immediately, no next header required.
        #[test]
        fn test_single_line_json_body_flushes_immediately() {
            let mut buf = LineBuffer::new();
            assert!(buf
                .push_line("[UnityCrossThreadLogger]1/1/2025 Event")
                .is_empty());
            let result = buf.push_line(r#"{"key":"value"}"#);
            assert_eq!(
                result,
                vec![expected(
                    EntryHeader::UnityCrossThreadLogger,
                    "[UnityCrossThreadLogger]1/1/2025 Event\n{\"key\":\"value\"}",
                )],
            );
            assert!(buf.is_empty(), "buffer must be idle after brace-flush");
        }

        /// Pretty-printed multi-line JSON: opening `{`, key/value lines,
        /// closing `}` on its own line. The closing `}` flushes the entry.
        #[test]
        fn test_multi_line_pretty_printed_json_flushes_on_closing_brace() {
            let mut buf = LineBuffer::new();
            buf.push_line("[Client GRE] GreToClientEvent");
            buf.push_line("{");
            buf.push_line(r#"  "key": "val""#);
            let result = buf.push_line("}");
            assert_eq!(result.len(), 1);
            assert_eq!(result[0].header, EntryHeader::ClientGre);
            assert_eq!(
                result[0].body,
                "[Client GRE] GreToClientEvent\n{\n  \"key\": \"val\"\n}",
            );
            assert!(buf.is_empty());
        }

        /// Header + `<==` response marker continuation + JSON body. The
        /// response marker has no `{`; the JSON body line flushes on its
        /// closing `}`.
        #[test]
        fn test_response_marker_then_json_flushes() {
            let mut buf = LineBuffer::new();
            assert!(buf
                .push_line("[UnityCrossThreadLogger]1/1/2025 12:00:00 PM")
                .is_empty());
            assert!(buf.push_line("<== EventGetCoursesV2(abc)").is_empty());
            let result = buf.push_line(r#"{"Courses":[]}"#);
            assert_eq!(result.len(), 1);
            assert_eq!(result[0].header, EntryHeader::UnityCrossThreadLogger);
            assert!(result[0].body.contains("<== EventGetCoursesV2(abc)"));
            assert!(result[0].body.contains(r#"{"Courses":[]}"#));
            assert!(buf.is_empty());
        }

        /// Non-JSON bodies (no `{` anywhere) fall through to the legacy
        /// "flush on next header" path — corresponds to the rare
        /// `true`-bodied REST responses and similar header-less continuations
        /// whose body never opens a brace.
        #[test]
        fn test_non_json_body_falls_through_to_next_header() {
            let mut buf = LineBuffer::new();
            buf.push_line("[Client GRE] GreToClientEvent");
            assert!(buf.push_line("(payload elided)").is_empty());
            assert!(buf.push_line(":: 12345 entries").is_empty());
            assert!(buf.push_line(":: payload trimmed").is_empty());

            // Next header flushes the accumulating Client-GRE entry — the
            // entry was never brace-flushed because no `{` appeared.
            let entries = buf.push_line("[UnityCrossThreadLogger]1/1/2025 After");
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].header, EntryHeader::ClientGre);
            assert!(entries[0].body.contains("(payload elided)"));
            assert!(entries[0].body.contains(":: 12345 entries"));
        }

        /// Brace state must not leak between entries: after a brace-flush,
        /// a follow-up entry with no `{` must NOT trigger a stale flush.
        #[test]
        fn test_brace_state_clears_between_entries() {
            let mut buf = LineBuffer::new();

            // First entry — brace-balance flushes it.
            buf.push_line("[UnityCrossThreadLogger]1/1/2025 First");
            let first = buf.push_line(r#"{"a":1}"#);
            assert_eq!(first.len(), 1);
            assert!(buf.is_empty());

            // Brace state should be reset — internal sanity check so a
            // regression here surfaces directly rather than through
            // downstream behavior.
            assert_eq!(buf.brace_state.depth, 0);
            assert!(!buf.brace_state.in_string);
            assert!(!buf.brace_state.escape_pending);
            assert!(!buf.brace_state.ever_opened);

            // Second entry has no `{`. Without proper state reset, stale
            // `ever_opened=true` would falsely flush this entry's first
            // continuation line. With reset, it accumulates normally and
            // the next header flushes it.
            buf.push_line("[Client GRE] PlainBodyEvent");
            assert!(buf.push_line("just text").is_empty());
            let entries = buf.push_line("[UnityCrossThreadLogger]1/1/2025 Third");
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].header, EntryHeader::ClientGre);
            assert_eq!(entries[0].body, "[Client GRE] PlainBodyEvent\njust text");
        }

        /// After a brace-flush, subsequent headerless lines must be treated
        /// as routine post-flush noise (silently discarded, no warn) — the
        /// brace-flush path must arm the same `has_emitted_anything` gate
        /// the next-header flush path arms.
        #[test]
        fn test_brace_flush_arms_orphan_warn_gating() {
            let mut buf = LineBuffer::new();

            // Brace-flush an entry.
            buf.push_line("[UnityCrossThreadLogger]1/1/2025 Event");
            let flushed = buf.push_line(r#"{"k":"v"}"#);
            assert_eq!(flushed.len(), 1);

            // `has_emitted_anything` was armed by the header detection,
            // not by the flush itself — verify the gate is still set
            // after brace-flush so the next orphan is silenced.
            assert!(
                buf.has_emitted_anything,
                "brace-flush path must leave has_emitted_anything armed",
            );

            // A subsequent orphan line must be silently discarded.
            assert!(buf.push_line("orphan stdout noise").is_empty());
            assert!(buf.is_empty());
        }
    }

    // -- Brace-depth string-literal handling (property tests) ---------------

    #[cfg(feature = "brace_depth_flush")]
    mod brace_depth_property {
        use super::*;
        use proptest::prelude::*;
        use serde_json::Value;

        /// Recursive strategy producing arbitrary JSON values. Strings include
        /// the `{`, `}`, `"`, and `\` characters specifically because those
        /// are the characters the brace-state machine must handle without
        /// being fooled by content inside string literals.
        fn arb_json_value() -> impl Strategy<Value = Value> {
            // Strings sample from a character set that includes every
            // character the state machine special-cases.
            let arb_string = r#"[a-z0-9 \{\}\"\\]{0,12}"#.prop_map(Value::String);
            let leaf = prop_oneof![
                Just(Value::Null),
                any::<bool>().prop_map(Value::Bool),
                any::<i32>().prop_map(|n| Value::Number(n.into())),
                arb_string,
            ];
            leaf.prop_recursive(3, 24, 4, |inner| {
                prop_oneof![
                    prop::collection::vec(inner.clone(), 0..4).prop_map(Value::Array),
                    prop::collection::vec((r"[a-z]{1,6}", inner), 0..4)
                        .prop_map(|kvs| { Value::Object(kvs.into_iter().collect()) }),
                ]
            })
        }

        proptest! {
            /// Any serialized JSON value, when fed as one continuation line
            /// after a multi-line header, must brace-balance and flush.
            #[test]
            fn prop_balanced_json_flushes_exactly_once(value in arb_json_value()) {
                // Force the top-level value to be an object so the body
                // opens with `{` — the property is about closed JSON
                // structures, not bare leaves.
                // `serde_json::to_string` only errors on serializers that
                // refuse some `Serialize` shape — `Value` always serializes
                // cleanly, so the `Err` branch is unreachable in practice.
                let body = match serde_json::to_string(&Value::Object(
                    [("v".to_owned(), value)].into_iter().collect(),
                )) {
                    Ok(s) => s,
                    Err(e) => unreachable!("serde_json::to_string on Value failed: {e}"),
                };
                let mut buf = LineBuffer::new();
                let header = buf.push_line("[UnityCrossThreadLogger]1/1/2025 PropTest");
                prop_assert!(header.is_empty());
                let out = buf.push_line(&body);
                prop_assert_eq!(out.len(), 1, "balanced JSON must brace-flush");
                prop_assert!(buf.is_empty());
            }

            /// An unterminated string literal — `"abc` with no closing `"`
            /// — must never appear balanced no matter what comes after the
            /// opening `{`.
            #[test]
            fn prop_unterminated_string_never_balances(
                prefix in r"[a-z0-9 ]{0,16}",
                trailing in r"[a-z0-9 \{\}]{0,16}",
            ) {
                let body = format!(r#"{{"k":"{prefix}{trailing}"#);
                let mut buf = LineBuffer::new();
                buf.push_line("[UnityCrossThreadLogger]1/1/2025 PropTest");
                let out = buf.push_line(&body);
                prop_assert_eq!(
                    out.len(),
                    0,
                    "unterminated string literal must not be reported balanced",
                );
                prop_assert!(!buf.is_empty(), "entry should remain accumulating");
            }

            /// `{` and `}` characters embedded in a string literal must not
            /// affect the brace-balance counter — a well-formed JSON object
            /// containing brace-noise in a string value still flushes.
            #[test]
            fn prop_braces_in_strings_dont_count(
                noise in r"[\{\}]{0,16}",
            ) {
                let body = format!(r#"{{"junk":"{noise}"}}"#);
                let mut buf = LineBuffer::new();
                buf.push_line("[UnityCrossThreadLogger]1/1/2025 PropTest");
                let out = buf.push_line(&body);
                prop_assert_eq!(
                    out.len(),
                    1,
                    "braces inside string literals must not affect the counter",
                );
            }
        }

        // -- Hand-written regression cases derived from corpus analysis ----

        /// `{"request":"{\"foo\":\"bar\"}"}` — a JSON object whose string
        /// value contains a nested escaped JSON object. Corpus has 585 such
        /// entries; all must brace-balance correctly because the inner
        /// braces appear inside a string literal.
        #[test]
        fn test_regression_nested_json_in_string() {
            let mut buf = LineBuffer::new();
            buf.push_line("[UnityCrossThreadLogger]1/1/2025 Nested");
            let body = r#"{"request":"{\"foo\":\"bar\"}"}"#;
            let out = buf.push_line(body);
            assert_eq!(out.len(), 1, "nested-string body must brace-balance");
            assert_eq!(
                out[0].body,
                format!("[UnityCrossThreadLogger]1/1/2025 Nested\n{body}")
            );
        }

        /// Escaped quote inside a string literal: the `\"` does NOT close
        /// the string, so the next unescaped `"` is the real closer.
        #[test]
        fn test_regression_escaped_quote_inside_string() {
            let mut buf = LineBuffer::new();
            buf.push_line("[UnityCrossThreadLogger]1/1/2025 EscQuote");
            let body = r#"{"name":"a \"quoted\" name"}"#;
            let out = buf.push_line(body);
            assert_eq!(out.len(), 1);
            assert!(out[0].body.contains(r#""a \"quoted\" name""#));
        }

        /// Escaped backslashes: `\\` is an escape pair; the next character
        /// is NOT escaped, so an `"` immediately after `\\` correctly
        /// toggles string state.
        #[test]
        fn test_regression_escaped_backslash_inside_string() {
            let mut buf = LineBuffer::new();
            buf.push_line("[UnityCrossThreadLogger]1/1/2025 EscBackslash");
            let body = r#"{"path":"C:\\Users\\foo"}"#;
            let out = buf.push_line(body);
            assert_eq!(out.len(), 1);
            assert!(out[0].body.contains(r#""C:\\Users\\foo""#));
        }

        /// Bare `{` and `}` inside a string literal must not move the
        /// counter — the entry balances on the outer `}` alone.
        #[test]
        fn test_regression_brace_inside_string_literal() {
            let mut buf = LineBuffer::new();
            buf.push_line("[UnityCrossThreadLogger]1/1/2025 BraceInStr");
            let body = r#"{"emoji":"{ :) }"}"#;
            let out = buf.push_line(body);
            assert_eq!(out.len(), 1);
            assert!(out[0].body.contains(r#""{ :) }""#));
        }

        /// Pathological unbalanced JSON — opens a `{` but never closes
        /// it. Depth stays > 0 forever; the entry never brace-flushes
        /// and must fall through to the next-header flush path. Defined
        /// behavior, no panic.
        #[test]
        fn test_regression_unbalanced_json_falls_through() {
            let mut buf = LineBuffer::new();
            buf.push_line("[UnityCrossThreadLogger]1/1/2025 Unbalanced");
            assert!(buf.push_line(r#"{"unclosed":"#).is_empty());
            assert!(buf.push_line(r#"  "more":"data""#).is_empty());

            // Next header flushes via the fallback path.
            let next = buf.push_line("[UnityCrossThreadLogger]1/1/2025 NextEvent");
            assert_eq!(next.len(), 1);
            assert_eq!(next[0].header, EntryHeader::UnityCrossThreadLogger);
            assert!(next[0].body.contains(r#"{"unclosed":"#));
        }

        /// A JSON string value containing the `\n` escape sequence (not a
        /// real newline) keeps the value on one logical body line —
        /// `\\n` is two characters, not a line break.
        #[test]
        fn test_regression_escaped_newline_in_string() {
            let mut buf = LineBuffer::new();
            buf.push_line("[UnityCrossThreadLogger]1/1/2025 EscNewline");
            // `\n` in the source string is the two-character escape sequence
            // `\` followed by `n` — not a real newline.
            let body = r#"{"raw":"line1\nline2"}"#;
            let out = buf.push_line(body);
            assert_eq!(out.len(), 1);
            assert!(out[0].body.contains(r#""line1\nline2""#));
        }
    }

    // -- LineBuffer: GSM truncation marker header (#200) ---------------------

    mod truncation_marker {
        use super::*;

        const MARKER: &str = "[Message summarized because one or more GameStateMessages \
             exceeded the 50 GameObject or 50 Annotation limit.]";

        #[test]
        fn test_as_str_truncation_marker() {
            assert_eq!(
                EntryHeader::TruncationMarker.as_str(),
                "[Message summarized]"
            );
        }

        #[test]
        fn test_display_truncation_marker() {
            assert_eq!(
                EntryHeader::TruncationMarker.to_string(),
                "[Message summarized]"
            );
        }

        #[test]
        fn test_marker_is_detected_as_header() {
            let buf = LineBuffer::new();
            assert_eq!(
                buf.detect_header(MARKER),
                Some(EntryHeader::TruncationMarker)
            );
        }

        #[test]
        fn test_marker_with_prefix_only_is_detected() {
            // Detection uses the `[Message summarized` prefix to stay
            // tolerant of minor wording variations. Any line starting with
            // that prefix is classified as a truncation marker.
            let buf = LineBuffer::new();
            let line = "[Message summarized for some other reason]";
            assert_eq!(buf.detect_header(line), Some(EntryHeader::TruncationMarker));
        }

        #[test]
        fn test_marker_classified_as_multi_line() {
            assert_eq!(
                LineBuffer::classify_header(EntryHeader::TruncationMarker, MARKER),
                HeaderClass::MultiLine,
            );
        }

        #[test]
        fn test_marker_as_first_line_starts_accumulation() {
            let mut buf = LineBuffer::new();
            let out = buf.push_line(MARKER);
            // MultiLine — entry is not flushed yet.
            assert!(out.is_empty());
            assert!(!buf.is_empty());
        }

        #[test]
        fn test_marker_mid_stream_flushes_prior_uctl_envelope() {
            // The truncation marker arrives inside a `[UnityCrossThreadLogger]`
            // envelope and the prior (now header-only) UCTL entry must flush
            // when the marker triggers a new MultiLine entry.
            let mut buf = LineBuffer::new();
            buf.push_line(
                "[UnityCrossThreadLogger]5/13/2026 10:01:12 AM: \
                 Match to <transaction>: GreToClientEvent",
            );

            let out = buf.push_line(MARKER);
            assert_eq!(out.len(), 1);
            assert_eq!(out[0].header, EntryHeader::UnityCrossThreadLogger);
            assert!(out[0].body.contains("GreToClientEvent"));

            // Truncation entry is now accumulating in the buffer.
            assert!(!buf.is_empty());
        }

        #[test]
        fn test_marker_accumulates_count_lines_until_next_header() {
            // The marker + follow-on `::: GameStateMessage`, `:: GameObject
            // Count = N`, `:: Annotation Count = M`, and `::: ActionsAvailableReq`
            // lines are all accumulated into the truncation entry. The entry
            // flushes when the next real header arrives (here, the next
            // `[UnityCrossThreadLogger]` line).
            let mut buf = LineBuffer::new();
            assert!(buf.push_line(MARKER).is_empty());
            assert!(buf.push_line("::: GameStateMessage").is_empty());
            assert!(buf.push_line(":: GameObject Count = 63").is_empty());
            assert!(buf.push_line(":: Annotation Count = 4").is_empty());
            assert!(buf.push_line("::: ActionsAvailableReq").is_empty());

            let out = buf.push_line("[UnityCrossThreadLogger]5/13/2026 10:01:13 AM Next");
            assert_eq!(out.len(), 1);
            assert_eq!(out[0].header, EntryHeader::TruncationMarker);
            assert!(out[0].body.starts_with(MARKER));
            assert!(out[0].body.contains(":: GameObject Count = 63"));
            assert!(out[0].body.contains(":: Annotation Count = 4"));
            assert!(out[0].body.contains("::: ActionsAvailableReq"));
        }

        #[test]
        fn test_marker_flush_via_eof() {
            // EOF flush also produces the truncation entry, in case the log
            // ends immediately after the marker block.
            let mut buf = LineBuffer::new();
            buf.push_line(MARKER);
            buf.push_line("::: GameStateMessage");
            buf.push_line(":: GameObject Count = 7");
            buf.push_line(":: Annotation Count = 11");
            let flushed = buf.flush();
            assert!(flushed.is_some());
            let entry = flushed.unwrap_or_else(|| unreachable!());
            assert_eq!(entry.header, EntryHeader::TruncationMarker);
            assert!(entry.body.contains("GameObject Count = 7"));
            assert!(entry.body.contains("Annotation Count = 11"));
        }
    }
}
