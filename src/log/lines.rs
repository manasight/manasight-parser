//! Incremental line splitting with a bounded pending line.
//!
//! [`LineSplitter`] turns text arriving in arbitrary chunks into complete
//! lines with the same result as [`str::lines`] on the concatenated input:
//!
//! - Lines are separated by `'\n'`.
//! - A `'\r'` directly before the `'\n'` is stripped (CRLF → LF).
//! - A final line with no trailing `'\n'` is held until [`LineSplitter::finish`]
//!   and returned unchanged, so a lone trailing `'\r'` stays on it.
//!
//! The unterminated remainder of each chunk is appended in place to one
//! reused buffer, so a line split across many chunks is assembled once, in
//! time proportional to its length. A line longer than the splitter's limit
//! ([`MAX_LINE_BYTES`] by default) is dropped whole, up to its `'\n'`, and a
//! warning logs only its length. Dropping rather than truncating keeps a
//! corrupt prefix out of the entry assembler, whose state is left exactly as
//! it was before the line.

/// Default maximum length of one line, in bytes, before it is dropped.
///
/// The length counts every byte before the terminating `'\n'`, including a
/// `'\r'` that is stripped from the returned line.
pub const MAX_LINE_BYTES: usize = 16 * 1024 * 1024;

/// Pending-buffer capacity above which the buffer is released, rather than
/// kept for reuse, once its line completes.
const RELEASE_CAPACITY_BYTES: usize = 1024 * 1024;

/// Splits chunked text into lines, holding at most one pending partial line.
///
/// Feed chunks with [`push`](Self::push), which calls `on_line` once for
/// every line the chunk completes, then call [`finish`](Self::finish) at the
/// end of input to take the final unterminated line, if any.
#[derive(Debug)]
pub struct LineSplitter {
    /// Unterminated text carried over from previous chunks.
    pending: String,
    /// While dropping an over-long line: the bytes of it seen so far.
    discarding: Option<usize>,
    /// Maximum line length kept, in bytes.
    max_line_bytes: usize,
}

impl LineSplitter {
    /// Creates an empty splitter with the default [`MAX_LINE_BYTES`] limit.
    pub fn new() -> Self {
        Self::with_max_line_bytes(MAX_LINE_BYTES)
    }

    /// Creates an empty splitter that drops lines longer than
    /// `max_line_bytes`.
    pub fn with_max_line_bytes(max_line_bytes: usize) -> Self {
        Self {
            pending: String::new(),
            discarding: None,
            max_line_bytes,
        }
    }

    /// Feeds one chunk, calling `on_line` for every line it completes.
    ///
    /// Each line passed to `on_line` has its `'\n'` and any `'\r'` directly
    /// before it removed. Text after the chunk's last `'\n'` is kept for the
    /// next call. An empty chunk changes nothing.
    pub fn push<F: FnMut(&str)>(&mut self, text: &str, mut on_line: F) {
        let mut rest = text;
        while let Some(pos) = rest.find('\n') {
            let piece = &rest[..pos];
            rest = &rest[pos + 1..];
            self.complete_line(piece, &mut on_line);
        }
        self.carry(rest);
    }

    /// Ends the input, returning the final line if it had no `'\n'`.
    ///
    /// The line is returned unchanged (a lone trailing `'\r'` is kept, as
    /// [`str::lines`] keeps it). An over-long final line is dropped and
    /// `None` is returned. The splitter is empty afterwards.
    pub fn finish(&mut self) -> Option<String> {
        if let Some(seen) = self.discarding.take() {
            log_dropped(seen);
            return None;
        }
        if self.pending.is_empty() {
            None
        } else {
            Some(std::mem::take(&mut self.pending))
        }
    }

    /// Discards any pending partial line, including one being dropped.
    pub fn clear(&mut self) {
        self.discarding = None;
        self.release();
    }

    /// Returns `true` when no partial line is pending or being dropped.
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty() && self.discarding.is_none()
    }

    /// Handles a line terminated inside the current chunk. `piece` is the
    /// chunk's text before the `'\n'`; any pending text precedes it.
    fn complete_line<F: FnMut(&str)>(&mut self, piece: &str, on_line: &mut F) {
        if let Some(seen) = self.discarding.take() {
            log_dropped(seen + piece.len());
            return;
        }
        let total = self.pending.len() + piece.len();
        if total > self.max_line_bytes {
            log_dropped(total);
            self.release();
            return;
        }
        if self.pending.is_empty() {
            on_line(strip_cr(piece));
        } else {
            self.pending.push_str(piece);
            on_line(strip_cr(&self.pending));
            self.release();
        }
    }

    /// Keeps a chunk's unterminated remainder for the next chunk.
    fn carry(&mut self, rest: &str) {
        if rest.is_empty() {
            return;
        }
        if let Some(seen) = self.discarding.as_mut() {
            *seen += rest.len();
            return;
        }
        let total = self.pending.len() + rest.len();
        if total > self.max_line_bytes {
            self.discarding = Some(total);
            self.release();
            return;
        }
        if self.pending.capacity() < total {
            // Grow geometrically, but never past the line limit.
            let target = self
                .pending
                .capacity()
                .saturating_mul(2)
                .max(total)
                .min(self.max_line_bytes);
            self.pending.reserve_exact(target - self.pending.len());
        }
        self.pending.push_str(rest);
    }

    /// Empties the pending buffer, freeing it if it grew large.
    fn release(&mut self) {
        if self.pending.capacity() > RELEASE_CAPACITY_BYTES {
            self.pending = String::new();
        } else {
            self.pending.clear();
        }
    }
}

impl Default for LineSplitter {
    fn default() -> Self {
        Self::new()
    }
}

/// Strips one `'\r'` left before the removed `'\n'`.
fn strip_cr(line: &str) -> &str {
    line.strip_suffix('\r').unwrap_or(line)
}

/// Logs a dropped over-long line by its length only (never its content).
fn log_dropped(len: usize) {
    ::log::warn!("dropping over-long log line ({len} bytes)");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Feeds `chunks` through `splitter` and returns every line, including
    /// the one returned by `finish`.
    fn split_all(splitter: &mut LineSplitter, chunks: &[&str]) -> Vec<String> {
        let mut out = Vec::new();
        for chunk in chunks {
            splitter.push(chunk, |line| out.push(line.to_owned()));
        }
        out.extend(splitter.finish());
        out
    }

    /// Splits `input` at every char boundary into chunks of `size` bytes
    /// (rounded up to the next char boundary).
    fn chunks_of(input: &str, size: usize) -> Vec<&str> {
        let mut chunks = Vec::new();
        let mut start = 0;
        while start < input.len() {
            let mut end = (start + size).min(input.len());
            while !input.is_char_boundary(end) {
                end += 1;
            }
            chunks.push(&input[start..end]);
            start = end;
        }
        chunks
    }

    const PARITY_INPUTS: &[&str] = &[
        "",
        "\n",
        "\n\n",
        "a",
        "a\n",
        "a\nb",
        "a\nb\n",
        "a\r\nb\r\n",
        "a\r\nb\r",
        "a\r",
        "\r",
        "\r\n",
        "a\r\r\n",
        "a\rb\n",
        "line one\n\nline three\r\n  indented\nlast without newline",
        "caf\u{e9}\n\u{1F600} emoji\r\n\u{1F600}",
    ];

    #[test]
    fn test_push_every_chunk_size_matches_str_lines() {
        for input in PARITY_INPUTS {
            let expected: Vec<String> = input.lines().map(str::to_owned).collect();
            for size in 1..=input.len().max(1) {
                let mut splitter = LineSplitter::new();
                let got = split_all(&mut splitter, &chunks_of(input, size));
                assert_eq!(got, expected, "input {input:?}, chunk size {size}");
                assert!(splitter.is_empty());
            }
        }
    }

    #[test]
    fn test_push_every_two_way_split_matches_str_lines() {
        for input in PARITY_INPUTS {
            let expected: Vec<String> = input.lines().map(str::to_owned).collect();
            for at in (0..=input.len()).filter(|&i| input.is_char_boundary(i)) {
                let mut splitter = LineSplitter::new();
                let got = split_all(&mut splitter, &[&input[..at], &input[at..]]);
                assert_eq!(got, expected, "input {input:?}, split at {at}");
            }
        }
    }

    #[test]
    fn test_push_carried_text_joins_next_chunk() {
        let mut splitter = LineSplitter::new();
        let got = split_all(&mut splitter, &["ab", "c", "d\ne", "f\n"]);
        assert_eq!(got, vec!["abcd", "ef"]);
    }

    #[test]
    fn test_push_over_long_line_across_chunks_dropped_and_resumes() {
        let mut splitter = LineSplitter::with_max_line_bytes(8);
        let got = split_all(
            &mut splitter,
            &["first\n", "abcde", "fghij", "klm\r", "\nnext\n", "tail"],
        );
        assert_eq!(got, vec!["first", "next", "tail"]);
        assert!(splitter.is_empty());
    }

    #[test]
    fn test_push_over_long_line_within_one_chunk_dropped() {
        let mut splitter = LineSplitter::with_max_line_bytes(4);
        let got = split_all(&mut splitter, &["ok\ntoolong\nok2\n"]);
        assert_eq!(got, vec!["ok", "ok2"]);
    }

    #[test]
    fn test_push_pending_plus_piece_over_limit_dropped() {
        let mut splitter = LineSplitter::with_max_line_bytes(4);
        let got = split_all(&mut splitter, &["abc", "de\nok\n"]);
        assert_eq!(got, vec!["ok"]);
    }

    #[test]
    fn test_finish_over_long_unterminated_final_line_dropped() {
        let mut splitter = LineSplitter::with_max_line_bytes(4);
        let got = split_all(&mut splitter, &["a\n", "abc", "def"]);
        assert_eq!(got, vec!["a"]);
        assert!(splitter.is_empty());
        assert_eq!(splitter.finish(), None);
    }

    #[test]
    fn test_push_line_exactly_at_limit_kept() {
        let mut splitter = LineSplitter::with_max_line_bytes(5);
        let got = split_all(
            &mut splitter,
            &["abcde\n", "ab", "cde\n", "abcd\r\n", "abcdef\n", "vwxyz"],
        );
        assert_eq!(got, vec!["abcde", "abcde", "abcd", "vwxyz"]);
    }

    #[test]
    fn test_push_newline_less_run_pending_capacity_bounded() {
        let max = 1024;
        let mut splitter = LineSplitter::with_max_line_bytes(max);
        let chunk = "\0".repeat(100);
        let mut lines = Vec::new();
        for _ in 0..100 {
            splitter.push(&chunk, |line| lines.push(line.to_owned()));
            assert!(
                splitter.pending.capacity() <= max,
                "pending capacity {} exceeds limit {max}",
                splitter.pending.capacity()
            );
        }
        assert!(!splitter.is_empty());
        splitter.push("\r\nafter\n", |line| lines.push(line.to_owned()));
        assert_eq!(lines, vec!["after"]);
        assert!(splitter.is_empty());
    }

    #[test]
    fn test_push_completed_large_line_releases_buffer() {
        let mut splitter = LineSplitter::new();
        let big = "x".repeat(2 * RELEASE_CAPACITY_BYTES);
        let mut count = 0;
        splitter.push(&big, |_| count += 1);
        splitter.push("\n", |_| count += 1);
        assert_eq!(count, 1);
        assert_eq!(splitter.pending.capacity(), 0);
    }

    #[test]
    fn test_clear_discards_pending_and_dropping_state() {
        let mut splitter = LineSplitter::with_max_line_bytes(4);
        splitter.push("abcdef", |_| {});
        assert!(!splitter.is_empty());
        splitter.clear();
        assert!(splitter.is_empty());
        let got = split_all(&mut splitter, &["ok\n"]);
        assert_eq!(got, vec!["ok"]);

        splitter.push("ab", |_| {});
        splitter.clear();
        let got = split_all(&mut splitter, &["cd\n"]);
        assert_eq!(got, vec!["cd"]);
    }

    #[test]
    fn test_default_uses_max_line_bytes() {
        let splitter = LineSplitter::default();
        assert_eq!(splitter.max_line_bytes, MAX_LINE_BYTES);
        assert!(splitter.is_empty());
    }
}
