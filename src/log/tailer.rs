//! Polling-based file tailer with rotation detection.
//!
//! Polls `Player.log` at a configurable interval (default 50 ms) for new
//! data, detecting file rotation (MTGA restart) by monitoring file size
//! and modification time.
//!
//! # Data flow
//!
//! ```text
//! Player.log ──(poll 50 ms)──▸ FileTailer ──(raw lines)──▸ LineBuffer
//! ```
//!
//! The [`FileTailer`] opens the log file read-only with shared access,
//! seeks to the end on startup (tail mode), and reads only new bytes
//! from the last offset on each poll cycle. Raw lines are fed into
//! the [`LineBuffer`](super::entry::LineBuffer) for entry boundary
//! detection.

use std::io::SeekFrom;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use chrono::{DateTime, Local, Utc};
use tokio::io::{AsyncReadExt, AsyncSeekExt};

use super::entry::{LineBuffer, LogEntry};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Default poll interval in milliseconds.
const DEFAULT_POLL_INTERVAL_MS: u64 = 50;

/// Threshold (in seconds) for the mtime-jump rotation heuristic.
///
/// If the file's modification time jumps forward by more than this duration
/// without any new data at the current offset, the file is considered rotated.
const MTIME_JUMP_THRESHOLD_SECS: u64 = 60;

/// Initial capacity for the read buffer in bytes.
///
/// 8 KiB is a reasonable default — most log entries are well under 4 KiB,
/// and this avoids frequent small allocations during rapid bursts.
const READ_BUFFER_CAPACITY: usize = 8 * 1024;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors that can occur during file tailing operations.
#[derive(Debug, thiserror::Error)]
pub enum TailerError {
    /// The log file could not be opened or read.
    #[error("I/O error on {path}: {source}", path = path.display())]
    Io {
        /// The file path involved in the error.
        path: PathBuf,
        /// The underlying I/O error.
        source: std::io::Error,
    },
}

// ---------------------------------------------------------------------------
// RotationInfo
// ---------------------------------------------------------------------------

/// Metadata about a detected log file rotation.
///
/// Produced by [`FileTailer::take_rotation`] when rotation is detected
/// during a [`poll`](FileTailer::poll) cycle.
#[derive(Debug, Clone)]
pub struct RotationInfo {
    /// Byte offset in the old file when rotation was detected.
    previous_file_size: u64,
    /// Wall-clock timestamp when the rotation was detected.
    detected_at: DateTime<Utc>,
}

impl RotationInfo {
    /// Returns the byte offset in the old file at the time rotation was
    /// detected.
    pub fn previous_file_size(&self) -> u64 {
        self.previous_file_size
    }

    /// Returns the wall-clock timestamp when rotation was detected.
    pub fn detected_at(&self) -> DateTime<Utc> {
        self.detected_at
    }
}

// ---------------------------------------------------------------------------
// FileTailer
// ---------------------------------------------------------------------------

/// Polls a log file for new data at a configurable interval.
///
/// Opens `Player.log` read-only with shared access (no file locking
/// conflicts with MTGA), seeks to the end on startup, and reads only
/// new bytes from the last offset on each poll cycle. Raw lines are
/// fed into a [`LineBuffer`] for log entry boundary detection.
///
/// # Connection health
///
/// The [`last_event_at`](Self::last_event_at) timestamp is updated
/// whenever new data is read, providing a heartbeat signal for
/// connection health monitoring.
///
/// # Example
///
/// ```rust,no_run
/// use std::path::Path;
/// use manasight_parser::log::tailer::FileTailer;
///
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let mut tailer = FileTailer::open(Path::new("/path/to/Player.log")).await?;
///
/// // Poll once for new data.
/// let entries = tailer.poll().await?;
/// for entry in &entries {
///     println!("Got entry: {:?}", entry.header);
/// }
///
/// // Check when data was last seen.
/// if let Some(ts) = tailer.last_event_at() {
///     println!("Last data at: {ts}");
/// }
/// # Ok(())
/// # }
/// ```
pub struct FileTailer {
    /// Path to the log file (kept for error messages).
    path: PathBuf,
    /// The open file handle.
    file: tokio::fs::File,
    /// Current read offset in the file.
    offset: u64,
    /// Timestamp of the last successful data read.
    last_event_at: Option<DateTime<Utc>>,
    /// Line buffer for accumulating raw lines into complete log entries.
    line_buffer: LineBuffer,
    /// Partial line leftover from the previous read (no trailing newline).
    partial_line: String,
    /// Reusable read buffer to avoid per-poll allocation.
    read_buf: Vec<u8>,
    /// Poll interval in milliseconds.
    poll_interval_ms: u64,
    /// Modification time observed on the previous poll cycle (for mtime-jump
    /// rotation heuristic).
    last_mtime: Option<SystemTime>,
    /// Set by [`poll`](Self::poll) when file rotation is detected. Retrieved
    /// (and cleared) via [`take_rotation`](Self::take_rotation).
    last_rotation: Option<RotationInfo>,
}

impl FileTailer {
    /// Opens a log file for tailing, seeking to the end.
    ///
    /// The file is opened read-only. On startup, the file position is
    /// set to the end so that only new data written after this point
    /// is returned by subsequent [`poll`](Self::poll) calls.
    ///
    /// # Errors
    ///
    /// Returns [`TailerError::Io`] if the file cannot be opened or
    /// the seek operation fails.
    pub async fn open(path: &Path) -> Result<Self, TailerError> {
        let file = tokio::fs::File::open(path)
            .await
            .map_err(|source| TailerError::Io {
                path: path.to_path_buf(),
                source,
            })?;

        // Capture initial mtime for rotation detection.
        let initial_mtime = tokio::fs::metadata(path)
            .await
            .ok()
            .and_then(|m| m.modified().ok());

        let mut tailer = Self {
            path: path.to_path_buf(),
            file,
            offset: 0,
            last_event_at: None,
            line_buffer: LineBuffer::new(),
            partial_line: String::new(),
            read_buf: vec![0u8; READ_BUFFER_CAPACITY],
            poll_interval_ms: DEFAULT_POLL_INTERVAL_MS,
            last_mtime: initial_mtime,
            last_rotation: None,
        };

        // Seek to end — tail mode.
        let end_pos =
            tailer
                .file
                .seek(SeekFrom::End(0))
                .await
                .map_err(|source| TailerError::Io {
                    path: path.to_path_buf(),
                    source,
                })?;
        tailer.offset = end_pos;

        ::log::info!(
            "opened log file for tailing: {} (offset: {end_pos})",
            path.display()
        );

        Ok(tailer)
    }

    /// Opens a log file for tailing from the beginning.
    ///
    /// Unlike [`open`](Self::open), this does **not** seek to the end.
    /// All existing content will be read on the first [`poll`](Self::poll).
    /// Useful for testing or for catch-up parsing of `Player-prev.log`.
    ///
    /// # Errors
    ///
    /// Returns [`TailerError::Io`] if the file cannot be opened.
    pub async fn open_from_start(path: &Path) -> Result<Self, TailerError> {
        let file = tokio::fs::File::open(path)
            .await
            .map_err(|source| TailerError::Io {
                path: path.to_path_buf(),
                source,
            })?;

        let initial_mtime = tokio::fs::metadata(path)
            .await
            .ok()
            .and_then(|m| m.modified().ok());

        ::log::info!("opened log file for reading from start: {}", path.display());

        Ok(Self {
            path: path.to_path_buf(),
            file,
            offset: 0,
            last_event_at: None,
            line_buffer: LineBuffer::new(),
            partial_line: String::new(),
            read_buf: vec![0u8; READ_BUFFER_CAPACITY],
            poll_interval_ms: DEFAULT_POLL_INTERVAL_MS,
            last_mtime: initial_mtime,
            last_rotation: None,
        })
    }

    /// Sets the poll interval in milliseconds.
    ///
    /// The default is 50 ms. Values below 10 ms are clamped to 10 ms
    /// to avoid busy-spinning.
    pub fn set_poll_interval_ms(&mut self, ms: u64) {
        self.poll_interval_ms = ms.max(10);
    }

    /// Returns the poll interval in milliseconds.
    pub fn poll_interval_ms(&self) -> u64 {
        self.poll_interval_ms
    }

    /// Returns the timestamp of the last successful data read.
    ///
    /// `None` if no data has been read yet. This is intended for
    /// connection health monitoring — if this timestamp is stale,
    /// the log file may not be updating (MTGA closed, crashed, etc.).
    pub fn last_event_at(&self) -> Option<DateTime<Utc>> {
        self.last_event_at
    }

    /// Returns the current byte offset in the file.
    ///
    /// This is the position from which the next [`poll`](Self::poll)
    /// will read.
    pub fn offset(&self) -> u64 {
        self.offset
    }

    /// Returns a reference to the file path being tailed.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Takes the rotation info from the last poll cycle, if any.
    ///
    /// Returns `Some(RotationInfo)` exactly once after a rotation is
    /// detected by [`poll`](Self::poll). Subsequent calls return `None`
    /// until the next rotation.
    pub fn take_rotation(&mut self) -> Option<RotationInfo> {
        self.last_rotation.take()
    }

    /// Checks whether the file at the monitored path has been rotated
    /// (replaced by MTGA on restart). If rotation is detected, closes the
    /// old handle, re-opens the file, and resets internal state.
    ///
    /// Two signals are checked:
    /// 1. **Size shrinkage** — file at path is smaller than `self.offset`.
    /// 2. **Mtime jump** — modification time jumped >60 s without new data.
    async fn check_rotation(&mut self) -> Result<(), TailerError> {
        if self.offset == 0 {
            return Ok(());
        }

        let Ok(path_meta) = tokio::fs::metadata(&self.path).await else {
            return Ok(()); // File may be temporarily absent.
        };

        let path_size = path_meta.len();
        let path_mtime = path_meta.modified().ok();

        let mut rotated = false;

        // Primary signal: file at path is smaller than our offset.
        if path_size < self.offset {
            ::log::info!(
                "rotation detected: file size ({path_size}) < offset ({})",
                self.offset,
            );
            rotated = true;
        }

        // Secondary signal: mtime jumped >60 s without new data.
        if !rotated {
            if let (Some(current_mtime), Some(prev_mtime)) = (path_mtime, self.last_mtime) {
                let elapsed = current_mtime.duration_since(prev_mtime).unwrap_or_default();
                if elapsed.as_secs() > MTIME_JUMP_THRESHOLD_SECS && path_size <= self.offset {
                    ::log::info!(
                        "rotation detected: mtime jumped {:.0}s without new data",
                        elapsed.as_secs_f64(),
                    );
                    rotated = true;
                }
            }
        }

        if rotated {
            let previous_file_size = self.offset;

            // Close old handle, re-open at path (picks up new inode).
            self.file =
                tokio::fs::File::open(&self.path)
                    .await
                    .map_err(|source| TailerError::Io {
                        path: self.path.clone(),
                        source,
                    })?;

            // Reset state for the new file.
            self.offset = 0;
            self.partial_line.clear();
            self.line_buffer.reset();
            self.last_mtime = path_mtime;

            self.last_rotation = Some(RotationInfo {
                previous_file_size,
                detected_at: Local::now().naive_local().and_utc(),
            });

            ::log::info!(
                "re-opened {} after rotation (old offset: {previous_file_size})",
                self.path.display(),
            );
        } else if path_mtime.is_some() {
            self.last_mtime = path_mtime;
        }

        Ok(())
    }

    /// Polls the file for new data and returns any complete log entries.
    ///
    /// Reads all new bytes appended since the last poll, splits them
    /// into lines, and feeds each line into the [`LineBuffer`]. Any
    /// complete log entries (flushed by a new header boundary) are
    /// collected and returned.
    ///
    /// A partial line at the end of the read (not terminated by a
    /// newline) is buffered internally and prepended to the next read.
    ///
    /// Returns an empty `Vec` if no new data is available.
    ///
    /// # Errors
    ///
    /// Returns [`TailerError::Io`] if the read operation fails.
    pub async fn poll(&mut self) -> Result<Vec<LogEntry>, TailerError> {
        self.check_rotation().await?;

        let mut entries = Vec::new();
        let mut total_bytes_read: u64 = 0;

        loop {
            let bytes_read =
                self.file
                    .read(&mut self.read_buf)
                    .await
                    .map_err(|source| TailerError::Io {
                        path: self.path.clone(),
                        source,
                    })?;

            if bytes_read == 0 {
                break;
            }

            total_bytes_read += bytes_read as u64;

            // Convert the raw bytes to a string. MTGA logs are UTF-8 (or
            // at least ASCII-compatible). Invalid sequences are replaced
            // with U+FFFD, which is safe — they will simply fail to match
            // any parser patterns and be logged as unrecognized entries.
            let chunk = String::from_utf8_lossy(&self.read_buf[..bytes_read]);

            // Prepend any leftover partial line from the previous read.
            let text = if self.partial_line.is_empty() {
                chunk.into_owned()
            } else {
                let mut combined = std::mem::take(&mut self.partial_line);
                combined.push_str(&chunk);
                combined
            };

            // Split into lines. The last segment may be a partial line
            // (no trailing newline).
            let mut lines_iter = text.split('\n').peekable();
            while let Some(line) = lines_iter.next() {
                if lines_iter.peek().is_none() {
                    // Last segment — may be partial (no trailing newline).
                    if !line.is_empty() {
                        line.clone_into(&mut self.partial_line);
                    }
                } else {
                    // Complete line — strip trailing \r if present (Windows CRLF).
                    let clean = line.strip_suffix('\r').unwrap_or(line);
                    entries.extend(self.line_buffer.push_line(clean));
                }
            }
        }

        if total_bytes_read > 0 {
            self.offset += total_bytes_read;
            self.last_event_at = Some(Utc::now());
            ::log::debug!(
                "read {total_bytes_read} bytes from {} (new offset: {})",
                self.path.display(),
                self.offset
            );
        }

        Ok(entries)
    }

    /// Flushes any remaining buffered entries from the line buffer.
    ///
    /// Call this when the input stream ends (EOF or file rotation) to
    /// retrieve any accumulated entries that have not yet been flushed
    /// by a subsequent header boundary.
    ///
    /// Returns a `Vec` because flushing a partial line that is itself
    /// a log entry header can produce two entries: the previously
    /// buffered entry (flushed by the new header) and the new header's
    /// own entry (drained from the line buffer or, for single-line
    /// headers, emitted directly by `push_line`).
    pub fn flush(&mut self) -> Vec<LogEntry> {
        let mut entries = Vec::new();

        // Feed any partial line as a final complete line.
        if !self.partial_line.is_empty() {
            let line = std::mem::take(&mut self.partial_line);
            entries.extend(self.line_buffer.push_line(&line));
        }

        // Drain any remaining buffered entry (always at most one, since
        // single-line entries are emitted directly by `push_line`).
        if let Some(entry) = self.line_buffer.flush() {
            entries.push(entry);
        }

        entries
    }

    /// Runs the polling loop, sending complete log entries to the
    /// provided channel.
    ///
    /// This method runs indefinitely until the `shutdown` signal is
    /// received. It polls the file at the configured interval and
    /// sends each complete [`LogEntry`] to the `entry_tx` channel.
    ///
    /// # Cancellation
    ///
    /// The loop exits when `shutdown` resolves. Callers should use a
    /// `tokio::sync::watch` or `CancellationToken` to signal shutdown.
    ///
    /// # Errors
    ///
    /// Returns [`TailerError::Io`] if a read operation fails. Callers
    /// should decide whether to retry or propagate the error.
    pub async fn run(
        &mut self,
        entry_tx: tokio::sync::mpsc::Sender<LogEntry>,
        mut shutdown: tokio::sync::watch::Receiver<bool>,
    ) -> Result<(), TailerError> {
        let mut interval =
            tokio::time::interval(std::time::Duration::from_millis(self.poll_interval_ms));
        // The first tick completes immediately; subsequent ticks wait
        // for the full interval.
        interval.tick().await;

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    let entries = self.poll().await?;
                    for entry in entries {
                        // If the receiver is dropped, stop tailing.
                        if entry_tx.send(entry).await.is_err() {
                            ::log::info!("entry channel closed, stopping tailer");
                            return Ok(());
                        }
                    }
                }
                _ = shutdown.changed() => {
                    ::log::info!("shutdown signal received, stopping tailer");
                    // Flush any remaining partial entries.
                    for entry in self.flush() {
                        // Best-effort send — receiver may already be gone.
                        let _ = entry_tx.send(entry).await;
                    }
                    return Ok(());
                }
            }
        }
    }

    /// Reads the entire file and returns all complete log entries.
    ///
    /// Polls until no new complete entries are returned (typically at
    /// EOF), then flushes the line buffer to capture any trailing
    /// entry. Unlike [`run`](Self::run), this method does **not** poll
    /// indefinitely or require a shutdown signal.
    ///
    /// Note: the entire file is buffered into a `Vec<LogEntry>` before
    /// returning. This is suitable for batch processing (smoke tests,
    /// replay analysis, `Player-prev.log` imports) but not for
    /// memory-constrained streaming of very large files.
    ///
    /// Works with any tailer opened from the start of a file via
    /// [`open_from_start`](Self::open_from_start).
    ///
    /// # Errors
    ///
    /// Returns [`TailerError::Io`] if a read operation fails.
    pub async fn run_once(&mut self) -> Result<Vec<LogEntry>, TailerError> {
        let mut all_entries = Vec::new();

        loop {
            let entries = self.poll().await?;
            if entries.is_empty() {
                break;
            }
            all_entries.extend(entries);
        }

        // Flush any remaining buffered entries.
        all_entries.extend(self.flush());

        ::log::info!(
            "one-shot read complete: {} entries from {}",
            all_entries.len(),
            self.path.display(),
        );

        Ok(all_entries)
    }
}

impl std::fmt::Debug for FileTailer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileTailer")
            .field("path", &self.path)
            .field("offset", &self.offset)
            .field("last_event_at", &self.last_event_at)
            .field("poll_interval_ms", &self.poll_interval_ms)
            .finish_non_exhaustive()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// Helper: create a temp file with initial content and return the
    /// `NamedTempFile` (which keeps the file alive while in scope).
    fn temp_log(content: &str) -> Result<NamedTempFile, std::io::Error> {
        let mut f = NamedTempFile::new()?;
        f.write_all(content.as_bytes())?;
        f.flush()?;
        Ok(f)
    }

    // -- open ---------------------------------------------------------------

    mod open {
        use super::*;

        #[tokio::test]
        async fn test_open_seeks_to_end() -> TestResult {
            let f = temp_log("existing content\n")?;
            let tailer = FileTailer::open(f.path()).await?;
            assert_eq!(tailer.offset(), "existing content\n".len() as u64);
            Ok(())
        }

        #[tokio::test]
        async fn test_open_last_event_at_is_none() -> TestResult {
            let f = temp_log("")?;
            let tailer = FileTailer::open(f.path()).await?;
            assert!(tailer.last_event_at().is_none());
            Ok(())
        }

        #[tokio::test]
        async fn test_open_nonexistent_file_returns_error() {
            let result = FileTailer::open(Path::new("/nonexistent/Player.log")).await;
            assert!(result.is_err());
        }

        #[tokio::test]
        async fn test_open_default_poll_interval() -> TestResult {
            let f = temp_log("")?;
            let tailer = FileTailer::open(f.path()).await?;
            assert_eq!(tailer.poll_interval_ms(), DEFAULT_POLL_INTERVAL_MS);
            Ok(())
        }

        #[tokio::test]
        async fn test_open_path_preserved() -> TestResult {
            let f = temp_log("")?;
            let tailer = FileTailer::open(f.path()).await?;
            assert_eq!(tailer.path(), f.path());
            Ok(())
        }
    }

    // -- open_from_start ----------------------------------------------------

    mod open_from_start {
        use super::*;

        #[tokio::test]
        async fn test_open_from_start_offset_is_zero() -> TestResult {
            let f = temp_log("existing content\n")?;
            let tailer = FileTailer::open_from_start(f.path()).await?;
            assert_eq!(tailer.offset(), 0);
            Ok(())
        }

        #[tokio::test]
        async fn test_open_from_start_reads_existing_content() -> TestResult {
            // Date-prefixed UCTL = multi-line; first header buffers, second flushes.
            let f = temp_log(
                "[UnityCrossThreadLogger]1/1/2025 Event1\n\
                 [UnityCrossThreadLogger]1/1/2025 Event2\n",
            )?;
            let mut tailer = FileTailer::open_from_start(f.path()).await?;
            let entries = tailer.poll().await?;
            // First header doesn't flush; second header flushes first entry.
            assert_eq!(entries.len(), 1);
            assert!(entries[0].body.contains("Event1"));
            Ok(())
        }
    }

    // -- run_once -----------------------------------------------------------

    mod run_once_tests {
        use super::*;

        #[tokio::test]
        async fn test_run_once_reads_entire_file() -> TestResult {
            let f = temp_log(
                "[UnityCrossThreadLogger] Event1\n\
                 [UnityCrossThreadLogger] Event2\n\
                 [UnityCrossThreadLogger] Event3\n",
            )?;
            let mut tailer = FileTailer::open_from_start(f.path()).await?;
            let entries = tailer.run_once().await?;
            // 3 headers: Event1 flushed by Event2, Event2 flushed by Event3,
            // Event3 flushed by run_once's flush().
            assert_eq!(entries.len(), 3);
            assert!(entries[0].body.contains("Event1"));
            assert!(entries[1].body.contains("Event2"));
            assert!(entries[2].body.contains("Event3"));
            Ok(())
        }

        #[tokio::test]
        async fn test_run_once_empty_file_returns_empty() -> TestResult {
            let f = temp_log("")?;
            let mut tailer = FileTailer::open_from_start(f.path()).await?;
            let entries = tailer.run_once().await?;
            assert!(entries.is_empty());
            Ok(())
        }

        #[tokio::test]
        async fn test_run_once_single_entry_flushed() -> TestResult {
            let f = temp_log("[UnityCrossThreadLogger] Only\n")?;
            let mut tailer = FileTailer::open_from_start(f.path()).await?;
            let entries = tailer.run_once().await?;
            assert_eq!(entries.len(), 1);
            assert!(entries[0].body.contains("Only"));
            Ok(())
        }

        #[tokio::test]
        async fn test_run_once_multiline_entry() -> TestResult {
            // Date-prefixed UCTL = multi-line; the JSON line is accumulated as
            // a continuation until the next header arrives.
            let f = temp_log(
                "[UnityCrossThreadLogger]1/1/2025 Event1\n\
                 {\"key\": \"value\"}\n\
                 [UnityCrossThreadLogger]1/1/2025 Event2\n",
            )?;
            let mut tailer = FileTailer::open_from_start(f.path()).await?;
            let entries = tailer.run_once().await?;
            assert_eq!(entries.len(), 2);
            assert!(entries[0].body.contains("key"));
            Ok(())
        }

        #[tokio::test]
        async fn test_run_once_works_with_open_from_start() -> TestResult {
            let f = temp_log(
                "[UnityCrossThreadLogger] Event1\n\
                 [UnityCrossThreadLogger] Event2\n",
            )?;
            let mut tailer = FileTailer::open_from_start(f.path()).await?;
            let entries = tailer.run_once().await?;
            assert_eq!(entries.len(), 2);
            Ok(())
        }

        #[tokio::test]
        async fn test_run_once_handles_partial_last_line() -> TestResult {
            // File with no trailing newline on the last entry.
            let f = temp_log(
                "[UnityCrossThreadLogger] Event1\n\
                 [UnityCrossThreadLogger] Event2",
            )?;
            let mut tailer = FileTailer::open_from_start(f.path()).await?;
            let entries = tailer.run_once().await?;
            assert_eq!(entries.len(), 2);
            assert!(entries[0].body.contains("Event1"));
            assert!(entries[1].body.contains("Event2"));
            Ok(())
        }
    }

    // -- poll ---------------------------------------------------------------

    mod poll_tests {
        use super::*;

        #[tokio::test]
        async fn test_poll_no_new_data_returns_empty() -> TestResult {
            let f = temp_log("initial data\n")?;
            let mut tailer = FileTailer::open(f.path()).await?;
            let entries = tailer.poll().await?;
            assert!(entries.is_empty());
            Ok(())
        }

        #[tokio::test]
        async fn test_poll_reads_new_data() -> TestResult {
            let mut f = temp_log("")?;
            let mut tailer = FileTailer::open(f.path()).await?;

            // Append new content after opening. Date-prefixed = multi-line.
            writeln!(f, "[UnityCrossThreadLogger]1/1/2025 Event1")?;
            writeln!(f, "[UnityCrossThreadLogger]1/1/2025 Event2")?;
            f.flush()?;

            let entries = tailer.poll().await?;
            // Second header flushes first entry.
            assert_eq!(entries.len(), 1);
            assert!(entries[0].body.contains("Event1"));
            Ok(())
        }

        #[tokio::test]
        async fn test_poll_updates_offset() -> TestResult {
            let mut f = temp_log("")?;
            let mut tailer = FileTailer::open(f.path()).await?;
            let initial_offset = tailer.offset();

            writeln!(f, "new data")?;
            f.flush()?;

            tailer.poll().await?;
            assert!(tailer.offset() > initial_offset);
            Ok(())
        }

        #[tokio::test]
        async fn test_poll_updates_last_event_at() -> TestResult {
            let mut f = temp_log("")?;
            let mut tailer = FileTailer::open(f.path()).await?;
            assert!(tailer.last_event_at().is_none());

            writeln!(f, "new data")?;
            f.flush()?;

            tailer.poll().await?;
            assert!(tailer.last_event_at().is_some());
            Ok(())
        }

        #[tokio::test]
        async fn test_poll_does_not_update_last_event_at_on_no_data() -> TestResult {
            let f = temp_log("")?;
            let mut tailer = FileTailer::open(f.path()).await?;
            tailer.poll().await?;
            assert!(tailer.last_event_at().is_none());
            Ok(())
        }

        #[tokio::test]
        async fn test_poll_multiline_entry() -> TestResult {
            let mut f = temp_log("")?;
            let mut tailer = FileTailer::open(f.path()).await?;

            // Date-prefixed UCTL = multi-line, so the JSON line is accumulated.
            writeln!(f, "[UnityCrossThreadLogger]1/1/2025 Event1")?;
            writeln!(f, "{{\"key\": \"value\"}}")?;
            writeln!(f, "[UnityCrossThreadLogger]1/1/2025 Event2")?;
            f.flush()?;

            let entries = tailer.poll().await?;
            assert_eq!(entries.len(), 1);
            assert!(entries[0].body.contains("Event1"));
            assert!(entries[0].body.contains("{\"key\": \"value\"}"));
            Ok(())
        }

        #[tokio::test]
        async fn test_poll_incremental_reads() -> TestResult {
            let mut f = temp_log("")?;
            let mut tailer = FileTailer::open(f.path()).await?;

            // First write — one multi-line header, no flush yet.
            writeln!(f, "[UnityCrossThreadLogger]1/1/2025 Event1")?;
            f.flush()?;
            let entries1 = tailer.poll().await?;
            assert!(entries1.is_empty());

            // Second write — new header flushes previous entry.
            writeln!(f, "[Client GRE] Event2")?;
            f.flush()?;
            let entries2 = tailer.poll().await?;
            assert_eq!(entries2.len(), 1);
            assert!(entries2[0].body.contains("Event1"));

            Ok(())
        }

        #[tokio::test]
        async fn test_poll_handles_partial_lines() -> TestResult {
            let mut f = temp_log("")?;
            let mut tailer = FileTailer::open(f.path()).await?;

            // Write a line without a trailing newline (multi-line header).
            write!(f, "[UnityCrossThreadLogger]1/1/2025 Partial")?;
            f.flush()?;
            let entries1 = tailer.poll().await?;
            assert!(entries1.is_empty());

            // Complete the line and add another header.
            writeln!(f)?; // Finish the partial line.
            writeln!(f, "[UnityCrossThreadLogger]1/1/2025 Next")?;
            f.flush()?;
            let entries2 = tailer.poll().await?;
            assert_eq!(entries2.len(), 1);
            assert!(entries2[0].body.contains("Partial"));

            Ok(())
        }

        #[tokio::test]
        async fn test_poll_handles_crlf_line_endings() -> TestResult {
            let mut f = temp_log("")?;
            let mut tailer = FileTailer::open(f.path()).await?;

            // Write content with CRLF line endings (multi-line headers).
            write!(
                f,
                "[UnityCrossThreadLogger]1/1/2025 Event1\r\n\
                 [UnityCrossThreadLogger]1/1/2025 Event2\r\n"
            )?;
            f.flush()?;

            let entries = tailer.poll().await?;
            assert_eq!(entries.len(), 1);
            // The body should not contain \r.
            assert!(!entries[0].body.contains('\r'));
            assert!(entries[0].body.contains("Event1"));
            Ok(())
        }

        #[tokio::test]
        async fn test_poll_only_reads_new_bytes() -> TestResult {
            let mut f = temp_log("")?;
            let mut tailer = FileTailer::open(f.path()).await?;

            // Write and poll first batch (multi-line headers).
            writeln!(f, "[UnityCrossThreadLogger]1/1/2025 Event1")?;
            writeln!(f, "[UnityCrossThreadLogger]1/1/2025 Event2")?;
            f.flush()?;
            let entries1 = tailer.poll().await?;
            assert_eq!(entries1.len(), 1);

            // Write and poll second batch — should only see new data.
            writeln!(f, "[UnityCrossThreadLogger]1/1/2025 Event3")?;
            f.flush()?;
            let entries2 = tailer.poll().await?;
            assert_eq!(entries2.len(), 1);
            // Should be Event2, not Event1 (Event2 flushed by Event3 header).
            assert!(entries2[0].body.contains("Event2"));

            Ok(())
        }
    }

    // -- flush --------------------------------------------------------------

    mod flush_tests {
        use super::*;

        #[tokio::test]
        async fn test_flush_returns_remaining_entry() -> TestResult {
            let mut f = temp_log("")?;
            let mut tailer = FileTailer::open(f.path()).await?;

            // Multi-line UCTL header — buffered until flush() drains it.
            writeln!(f, "[UnityCrossThreadLogger]1/1/2025 Final")?;
            f.flush()?;
            tailer.poll().await?;

            let entries = tailer.flush();
            assert_eq!(entries.len(), 1);
            assert!(entries[0].body.contains("Final"));
            Ok(())
        }

        #[tokio::test]
        async fn test_flush_empty_returns_empty_vec() -> TestResult {
            let f = temp_log("")?;
            let mut tailer = FileTailer::open(f.path()).await?;
            assert!(tailer.flush().is_empty());
            Ok(())
        }

        #[tokio::test]
        async fn test_flush_includes_partial_line() -> TestResult {
            let mut f = temp_log("")?;
            let mut tailer = FileTailer::open(f.path()).await?;

            // Write multi-line header + partial continuation (no trailing newline).
            writeln!(f, "[UnityCrossThreadLogger]1/1/2025 Event")?;
            write!(f, "partial continuation")?;
            f.flush()?;
            tailer.poll().await?;

            let entries = tailer.flush();
            assert_eq!(entries.len(), 1);
            assert!(entries[0].body.contains("Event"));
            assert!(entries[0].body.contains("partial continuation"));
            Ok(())
        }

        #[tokio::test]
        async fn test_flush_partial_line_is_header_returns_both_entries() -> TestResult {
            let mut f = temp_log("")?;
            let mut tailer = FileTailer::open(f.path()).await?;

            // Write a complete (multi-line) header line followed by a
            // partial line that is itself a header (no trailing newline).
            writeln!(f, "[UnityCrossThreadLogger]1/1/2025 First")?;
            write!(f, "[Client GRE] Second")?;
            f.flush()?;
            tailer.poll().await?;

            // flush() should return both: the "First" entry flushed by the
            // "[Client GRE]" header, and the "[Client GRE] Second" entry
            // drained from the line buffer.
            let entries = tailer.flush();
            assert_eq!(
                entries.len(),
                2,
                "expected 2 entries, got {}: {entries:?}",
                entries.len()
            );
            assert!(entries[0].body.contains("First"));
            assert!(entries[1].body.contains("Second"));
            Ok(())
        }
    }

    // -- set_poll_interval_ms -----------------------------------------------

    mod poll_interval {
        use super::*;

        #[tokio::test]
        async fn test_set_poll_interval() -> TestResult {
            let f = temp_log("")?;
            let mut tailer = FileTailer::open(f.path()).await?;
            tailer.set_poll_interval_ms(100);
            assert_eq!(tailer.poll_interval_ms(), 100);
            Ok(())
        }

        #[tokio::test]
        async fn test_set_poll_interval_clamps_to_minimum() -> TestResult {
            let f = temp_log("")?;
            let mut tailer = FileTailer::open(f.path()).await?;
            tailer.set_poll_interval_ms(1);
            assert_eq!(tailer.poll_interval_ms(), 10);
            Ok(())
        }

        #[tokio::test]
        async fn test_set_poll_interval_zero_clamps_to_minimum() -> TestResult {
            let f = temp_log("")?;
            let mut tailer = FileTailer::open(f.path()).await?;
            tailer.set_poll_interval_ms(0);
            assert_eq!(tailer.poll_interval_ms(), 10);
            Ok(())
        }
    }

    // -- run ----------------------------------------------------------------

    mod run_tests {
        use super::*;

        #[tokio::test]
        async fn test_run_sends_entries_to_channel() -> TestResult {
            let mut f = temp_log("")?;
            let mut tailer = FileTailer::open(f.path()).await?;
            tailer.set_poll_interval_ms(10);

            let (entry_tx, mut entry_rx) = tokio::sync::mpsc::channel(16);
            let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

            // Write data that will produce an entry.
            writeln!(f, "[UnityCrossThreadLogger] Event1")?;
            writeln!(f, "[UnityCrossThreadLogger] Event2")?;
            f.flush()?;

            // Run the tailer in a background task.
            let handle = tokio::spawn(async move { tailer.run(entry_tx, shutdown_rx).await });

            // Wait for the entry to arrive.
            let entry =
                tokio::time::timeout(std::time::Duration::from_secs(2), entry_rx.recv()).await?;
            assert!(entry.is_some());
            if let Some(e) = entry {
                assert!(e.body.contains("Event1"));
            }

            // Shut down.
            let _ = shutdown_tx.send(true);
            let result = tokio::time::timeout(std::time::Duration::from_secs(2), handle).await?;
            assert!(result.is_ok());
            Ok(())
        }

        #[tokio::test]
        async fn test_run_stops_on_shutdown() -> TestResult {
            let f = temp_log("")?;
            let mut tailer = FileTailer::open(f.path()).await?;
            tailer.set_poll_interval_ms(10);

            let (entry_tx, _entry_rx) = tokio::sync::mpsc::channel(16);
            let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

            let handle = tokio::spawn(async move { tailer.run(entry_tx, shutdown_rx).await });

            // Send shutdown signal.
            let _ = shutdown_tx.send(true);

            // The run loop should exit promptly.
            let result = tokio::time::timeout(std::time::Duration::from_secs(2), handle).await?;
            assert!(result.is_ok());
            Ok(())
        }

        #[tokio::test]
        async fn test_run_stops_when_receiver_dropped() -> TestResult {
            let mut f = temp_log("")?;
            let mut tailer = FileTailer::open(f.path()).await?;
            tailer.set_poll_interval_ms(10);

            let (entry_tx, entry_rx) = tokio::sync::mpsc::channel(16);
            let (_shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

            // Write data to trigger a send.
            writeln!(f, "[UnityCrossThreadLogger] Event1")?;
            writeln!(f, "[UnityCrossThreadLogger] Event2")?;
            f.flush()?;

            // Drop the receiver before starting.
            drop(entry_rx);

            let handle = tokio::spawn(async move { tailer.run(entry_tx, shutdown_rx).await });

            // Should exit because the channel is closed.
            let result = tokio::time::timeout(std::time::Duration::from_secs(2), handle).await?;
            assert!(result.is_ok());
            Ok(())
        }

        #[tokio::test]
        async fn test_run_continuous_data_stream() -> TestResult {
            let mut f = temp_log("")?;
            let mut tailer = FileTailer::open(f.path()).await?;
            tailer.set_poll_interval_ms(10);

            let (entry_tx, mut entry_rx) = tokio::sync::mpsc::channel(64);
            let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

            let handle = tokio::spawn(async move { tailer.run(entry_tx, shutdown_rx).await });

            // Write entries over time. Sleeps are generous (50 ms) to avoid
            // flakiness on slow CI runners — the tailer polls at 10 ms, so
            // 50 ms is ~5 poll cycles per write.
            for i in 0..3 {
                writeln!(f, "[UnityCrossThreadLogger] Event{i}")?;
                f.flush()?;
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
            // Write a final header to flush the last entry.
            writeln!(f, "[UnityCrossThreadLogger] Final")?;
            f.flush()?;
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;

            // Shutdown and collect remaining.
            let _ = shutdown_tx.send(true);
            let result = tokio::time::timeout(std::time::Duration::from_secs(5), handle).await?;
            assert!(result.is_ok());

            // Collect all received entries.
            let mut received = Vec::new();
            while let Ok(entry) = entry_rx.try_recv() {
                received.push(entry);
            }

            // We should have received at least 2 entries (Event0, Event1, Event2
            // flushed by subsequent headers, plus possibly Final from shutdown flush).
            assert!(
                received.len() >= 2,
                "expected at least 2 entries, got {}",
                received.len()
            );
            Ok(())
        }
    }

    // -- rotation detection -------------------------------------------------

    mod rotation_tests {
        use super::*;

        /// Helper: simulate file rotation by replacing the temp file at the
        /// same path with new (smaller) content. Uses `std::fs::write` to
        /// atomically write new content (truncating to a shorter size).
        fn replace_file_at_path(path: &Path, content: &str) -> Result<(), std::io::Error> {
            std::fs::write(path, content.as_bytes())
        }

        #[tokio::test]
        async fn test_rotation_detected_when_file_shrinks() -> TestResult {
            // Write initial content and open from start.
            let f = temp_log(
                "[UnityCrossThreadLogger] Event1\n\
                 [UnityCrossThreadLogger] Event2\n",
            )?;
            let path = f.path().to_path_buf();
            let mut tailer = FileTailer::open_from_start(&path).await?;

            // Read all existing content so offset advances.
            let _entries = tailer.run_once().await?;
            assert!(tailer.offset() > 0);

            // Simulate rotation: replace with smaller content.
            replace_file_at_path(&path, "[UnityCrossThreadLogger] NewSession\n")?;

            // Re-open from start to reset file handle for test (run_once
            // exhausted the file). We need to create a new tailer since the
            // old one has already reached EOF. Instead, let's manually set
            // up a tailer that has an offset > new file size.
            let mut tailer = FileTailer::open(&path).await?;
            // open() seeks to end, so offset == new file size. We need
            // offset > file size. Manually set a large offset to simulate
            // the old session's offset.
            tailer.offset = 10_000;

            // Poll should detect rotation (file size < offset).
            let entries = tailer.poll().await?;
            let rotation = tailer.take_rotation();
            assert!(
                rotation.is_some(),
                "rotation should be detected when file shrinks"
            );
            if let Some(info) = rotation {
                assert_eq!(info.previous_file_size(), 10_000);
            }
            // Offset should be reset and new content readable.
            assert!(tailer.offset() > 0, "should have read from new file");
            // No double-fire.
            assert!(tailer.take_rotation().is_none());

            // Entries from the new file should be empty or contain new data
            // (depends on what poll reads after re-open).
            drop(entries);
            Ok(())
        }

        #[tokio::test]
        async fn test_rotation_resets_offset_to_zero_before_reading() -> TestResult {
            let f = temp_log(
                "[UnityCrossThreadLogger]1/1/2025 OldEvent\n\
                 [UnityCrossThreadLogger]1/1/2025 OldEvent2\n",
            )?;
            let path = f.path().to_path_buf();

            let mut tailer = FileTailer::open(&path).await?;
            // Simulate old session with a large offset.
            tailer.offset = 50_000;

            // Replace with new content (multi-line headers).
            replace_file_at_path(
                &path,
                "[UnityCrossThreadLogger]1/1/2025 NewA\n\
                 [UnityCrossThreadLogger]1/1/2025 NewB\n",
            )?;

            // Poll detects rotation, re-opens, reads from start.
            let entries = tailer.poll().await?;
            assert!(tailer.take_rotation().is_some());
            // First header doesn't flush, second header flushes first.
            assert_eq!(entries.len(), 1);
            assert!(entries[0].body.contains("NewA"));
            Ok(())
        }

        #[tokio::test]
        async fn test_rotation_clears_partial_line_and_line_buffer() -> TestResult {
            let f = temp_log("")?;
            let path = f.path().to_path_buf();
            let mut tailer = FileTailer::open(&path).await?;

            // Write a partial line (no newline at end) and poll.
            std::fs::write(&path, "[UnityCrossThreadLogger]1/1/2025 Partial")?;
            tailer.poll().await?;
            // partial_line should be non-empty.
            assert!(
                !tailer.partial_line.is_empty(),
                "partial_line should hold the incomplete line"
            );

            // Simulate rotation by making offset > new file size.
            tailer.offset = 50_000;
            replace_file_at_path(
                &path,
                "[UnityCrossThreadLogger]1/1/2025 Fresh\n\
                 [UnityCrossThreadLogger]1/1/2025 Fresh2\n",
            )?;

            let entries = tailer.poll().await?;
            assert!(tailer.take_rotation().is_some());
            // The old partial line should NOT contaminate new entries.
            // "Fresh" should be a clean entry.
            assert_eq!(entries.len(), 1);
            assert!(entries[0].body.contains("Fresh"));
            assert!(!entries[0].body.contains("Partial"));
            Ok(())
        }

        #[tokio::test]
        async fn test_no_false_positive_on_normal_growth() -> TestResult {
            let mut f = temp_log("")?;
            let mut tailer = FileTailer::open(f.path()).await?;

            // Write some data and poll.
            writeln!(f, "[UnityCrossThreadLogger] Event1")?;
            f.flush()?;
            tailer.poll().await?;
            assert!(
                tailer.take_rotation().is_none(),
                "no rotation on normal append"
            );

            // Write more data (file grows).
            writeln!(f, "[UnityCrossThreadLogger] Event2")?;
            f.flush()?;
            tailer.poll().await?;
            assert!(
                tailer.take_rotation().is_none(),
                "no rotation on continued growth"
            );

            Ok(())
        }

        #[tokio::test]
        async fn test_no_rotation_when_offset_is_zero() -> TestResult {
            // When offset is 0 (just opened), rotation check is skipped.
            let f = temp_log("")?;
            let tailer = FileTailer::open_from_start(f.path()).await?;
            assert_eq!(tailer.offset(), 0);
            // offset == 0 → rotation detection is bypassed.
            Ok(())
        }

        #[tokio::test]
        async fn test_rotation_emits_correct_previous_file_size() -> TestResult {
            let f = temp_log("x".repeat(5000).as_str())?;
            let path = f.path().to_path_buf();
            let mut tailer = FileTailer::open(&path).await?;
            // open() seeks to end, offset = 5000.
            assert_eq!(tailer.offset(), 5000);

            // Replace with smaller file.
            replace_file_at_path(&path, "small\n")?;

            tailer.poll().await?;
            let rotation = tailer.take_rotation();
            assert!(rotation.is_some());
            if let Some(info) = rotation {
                assert_eq!(info.previous_file_size(), 5000);
            }
            Ok(())
        }

        #[tokio::test]
        async fn test_rotation_info_has_timestamp() -> TestResult {
            let f = temp_log("x".repeat(1000).as_str())?;
            let path = f.path().to_path_buf();
            let mut tailer = FileTailer::open(&path).await?;

            replace_file_at_path(&path, "y\n")?;
            tailer.poll().await?;

            let rotation = tailer.take_rotation();
            assert!(rotation.is_some());
            if let Some(info) = rotation {
                // detected_at uses local-as-UTC convention (Local::now()
                // stored as DateTime<Utc>) to match Arena's timestamp format.
                let local_as_utc = Local::now().naive_local().and_utc();
                let elapsed = local_as_utc - info.detected_at();
                assert!(
                    elapsed.num_seconds() < 5,
                    "detected_at should be recent, got {elapsed}"
                );
            }
            Ok(())
        }

        #[tokio::test]
        async fn test_take_rotation_returns_none_after_first_call() -> TestResult {
            let f = temp_log("x".repeat(1000).as_str())?;
            let path = f.path().to_path_buf();
            let mut tailer = FileTailer::open(&path).await?;

            replace_file_at_path(&path, "y\n")?;
            tailer.poll().await?;

            assert!(tailer.take_rotation().is_some());
            assert!(
                tailer.take_rotation().is_none(),
                "second take_rotation should return None"
            );
            Ok(())
        }
    }

    // -- Debug impl ---------------------------------------------------------

    mod debug_impl {
        use super::*;

        #[tokio::test]
        async fn test_debug_does_not_expose_file_handle() -> TestResult {
            let f = temp_log("")?;
            let tailer = FileTailer::open(f.path()).await?;
            let debug = format!("{tailer:?}");
            assert!(debug.contains("FileTailer"));
            assert!(debug.contains("offset"));
            // Should not expose internal file handle details.
            assert!(!debug.contains("read_buf"));
            Ok(())
        }
    }

    // -- TailerError --------------------------------------------------------

    mod error_tests {
        use super::*;

        #[test]
        fn test_tailer_error_display_includes_path() {
            let err = TailerError::Io {
                path: PathBuf::from("/test/Player.log"),
                source: std::io::Error::new(std::io::ErrorKind::NotFound, "file not found"),
            };
            let msg = err.to_string();
            assert!(msg.contains("/test/Player.log"));
            assert!(msg.contains("file not found"));
        }

        #[test]
        fn test_tailer_error_is_debug() {
            let err = TailerError::Io {
                path: PathBuf::from("/test"),
                source: std::io::Error::other("test"),
            };
            let debug = format!("{err:?}");
            assert!(debug.contains("Io"));
        }
    }
}
