//! Public entry point for streaming typed events from an MTG Arena log file.
//!
//! [`MtgaEventStream`] wires together the file tailer, router, and event bus
//! into a single `async fn` that returns a [`Subscriber`] of typed
//! [`GameEvent`] values. It runs entirely on the caller's Tokio runtime --
//! no internal runtime is created.
//!
//! # Example
//!
//! ```rust,no_run
//! use std::path::Path;
//! use manasight_parser::stream::MtgaEventStream;
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let (stream, mut subscriber) = MtgaEventStream::start(Path::new("Player.log")).await?;
//!
//! // Receive events on the caller's runtime.
//! while let Some(event) = subscriber.recv().await {
//!     println!("got event: {event:?}");
//! }
//! # Ok(())
//! # }
//! ```
//!
//! # Shutdown
//!
//! Call [`MtgaEventStream::shutdown`] to stop the background tailing task.
//! The [`Subscriber`] will receive `None` once all buffered events have been
//! delivered.

use std::path::Path;

use crate::event_bus::{EventBus, Subscriber};
use crate::events::{DetailedLoggingStatusEvent, GameEvent, LogFileRotatedEvent};
use crate::log::tailer::{FileTailer, TailerError};
use crate::router::Router;

/// Default duration to wait for structured headers before emitting a
/// `DetailedLoggingStatus { enabled: false }` event.
const HEADER_DETECTION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors that can occur when starting the event stream.
#[derive(Debug, thiserror::Error)]
pub enum StreamError {
    /// The log file could not be opened for tailing.
    #[error(transparent)]
    Tailer(#[from] TailerError),
}

// ---------------------------------------------------------------------------
// MtgaEventStream
// ---------------------------------------------------------------------------

/// Handle for a running MTG Arena event stream.
///
/// Created by [`MtgaEventStream::start`], which opens the log file, wires
/// together the tailer, router, and event bus, and spawns a background task
/// on the caller's Tokio runtime. The returned [`Subscriber`] receives
/// typed [`GameEvent`] values as they are parsed.
///
/// Call [`shutdown`](Self::shutdown) to stop the background task and clean
/// up resources. Dropping the `MtgaEventStream` without calling `shutdown`
/// is safe -- the background task will stop when the `EventBus` is dropped
/// and the entry channel closes.
///
/// # Runtime requirement
///
/// `MtgaEventStream` does **not** create its own Tokio runtime. It must be
/// used from within an active Tokio context (e.g., inside `#[tokio::main]`
/// or `#[tokio::test]`).
pub struct MtgaEventStream {
    /// Sender half of the shutdown watch channel.
    shutdown_tx: tokio::sync::watch::Sender<bool>,
    /// Join handle for the background pipeline task.
    _pipeline_handle: tokio::task::JoinHandle<()>,
}

impl MtgaEventStream {
    /// Starts streaming events from the given log file path.
    ///
    /// Opens the log file for tailing from the beginning (catch-up mode),
    /// creates an event bus and router, and spawns a background task that:
    ///
    /// 1. Polls the file tailer for new log entries
    /// 2. Routes each entry through the parser dispatch chain
    /// 3. Sends recognized events to the event bus
    ///
    /// Returns a tuple of `(MtgaEventStream, Subscriber)`. The
    /// `Subscriber` receives cloned [`GameEvent`] values. Call
    /// [`shutdown`](Self::shutdown) on the `MtgaEventStream` to stop
    /// the background task.
    ///
    /// # Errors
    ///
    /// Returns [`StreamError::Tailer`] if the log file cannot be opened.
    pub async fn start(log_path: &Path) -> Result<(Self, Subscriber), StreamError> {
        let tailer = FileTailer::open_from_start(log_path).await?;
        let bus = EventBus::with_default_capacity();
        let subscriber = bus.subscribe();
        let router = Router::new();
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

        let (entry_tx, entry_rx) = tokio::sync::mpsc::channel(256);

        // Spawn the tailer task (with rotation → event bus).
        let rotation_bus = bus.clone();
        let tailer_handle = tokio::spawn(run_tailer(tailer, entry_tx, rotation_bus, shutdown_rx));

        // Spawn the routing task.
        let router_handle = tokio::spawn(run_router(entry_rx, router, bus));

        // Spawn a supervisor task that joins both.
        let pipeline_handle = tokio::spawn(async move {
            // Wait for both tasks to complete. Errors (panics) are logged.
            if let Err(e) = tailer_handle.await {
                ::log::error!("tailer task panicked: {e}");
            }
            if let Err(e) = router_handle.await {
                ::log::error!("router task panicked: {e}");
            }
        });

        let stream = Self {
            shutdown_tx,
            _pipeline_handle: pipeline_handle,
        };

        Ok((stream, subscriber))
    }

    /// Starts a one-shot event stream that reads an entire log file and exits.
    ///
    /// Opens the file via [`FileTailer::open_from_start`], reads all
    /// entries, routes them through the parser dispatch chain, and sends
    /// recognized events to the event bus. The pipeline stops
    /// automatically at EOF rather than polling indefinitely.
    ///
    /// This is useful for batch processing complete log files (smoke tests,
    /// replay analysis, importing `Player-prev.log`).
    ///
    /// Returns a tuple of `(MtgaEventStream, Subscriber)`. The `Subscriber`
    /// will receive `None` once all events have been delivered and the
    /// pipeline finishes. Calling [`shutdown`](Self::shutdown) on a
    /// one-shot stream is a no-op -- the pipeline exits at EOF on its own.
    ///
    /// # Errors
    ///
    /// Returns [`StreamError::Tailer`] if the log file cannot be opened.
    pub async fn start_once(log_path: &Path) -> Result<(Self, Subscriber), StreamError> {
        let tailer = FileTailer::open_from_start(log_path).await?;
        let bus = EventBus::with_default_capacity();
        let subscriber = bus.subscribe();
        let router = Router::new();
        let (shutdown_tx, _shutdown_rx) = tokio::sync::watch::channel(false);

        let (entry_tx, entry_rx) = tokio::sync::mpsc::channel(256);

        // Spawn the tailer task — uses run_once, exits at EOF.
        let tailer_handle = tokio::spawn(run_tailer_once(tailer, entry_tx));

        // Spawn the routing task.
        let router_handle = tokio::spawn(run_router(entry_rx, router, bus));

        // Spawn a supervisor task that joins both.
        let pipeline_handle = tokio::spawn(async move {
            if let Err(e) = tailer_handle.await {
                ::log::error!("tailer task panicked: {e}");
            }
            if let Err(e) = router_handle.await {
                ::log::error!("router task panicked: {e}");
            }
        });

        let stream = Self {
            shutdown_tx,
            _pipeline_handle: pipeline_handle,
        };

        Ok((stream, subscriber))
    }

    /// Signals the background pipeline to stop.
    ///
    /// The tailer flushes any remaining buffered entries before exiting.
    /// The [`Subscriber`] will receive `None` once all buffered events
    /// have been delivered and the event bus is dropped.
    pub fn shutdown(&self) {
        let _ = self.shutdown_tx.send(true);
    }
}

impl std::fmt::Debug for MtgaEventStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MtgaEventStream")
            .field("shutdown_sent", &*self.shutdown_tx.borrow())
            .finish_non_exhaustive()
    }
}

// ---------------------------------------------------------------------------
// Header detection state
// ---------------------------------------------------------------------------

/// Tracks whether structured log headers have been observed.
///
/// Used by [`run_tailer`] to detect when Arena's "Detailed Logs (Plugin
/// Support)" setting is disabled. The 30-second timer starts on the first
/// non-empty read (not on tailer open) to avoid false positives when Arena
/// hasn't launched yet.
struct HeaderDetectionState {
    /// When the tailer first read non-zero bytes from the log file.
    first_bytes_at: Option<tokio::time::Instant>,
    /// Whether any structured log entry has been produced.
    headers_seen: bool,
    /// Whether the `enabled: false` event has been emitted.
    disabled_emitted: bool,
}

impl HeaderDetectionState {
    fn new() -> Self {
        Self {
            first_bytes_at: None,
            headers_seen: false,
            disabled_emitted: false,
        }
    }

    /// Resets detection state for a new log file (after rotation).
    fn reset(&mut self) {
        self.first_bytes_at = None;
        self.headers_seen = false;
        self.disabled_emitted = false;
    }

    /// Records that the tailer read bytes. Called on every poll that
    /// produces non-zero bytes (even if no structured entries result).
    fn record_bytes_read(&mut self) {
        if self.first_bytes_at.is_none() {
            self.first_bytes_at = Some(tokio::time::Instant::now());
        }
    }

    /// Records that structured log entries were produced.
    fn record_headers_seen(&mut self) {
        self.headers_seen = true;
    }

    /// Checks the current state and returns an event to emit, if any.
    ///
    /// - Returns `Some(enabled: false)` if the timeout has elapsed without
    ///   headers (emitted once).
    /// - Returns `Some(enabled: true)` if headers are seen after a
    ///   previous `enabled: false` was emitted.
    /// - Returns `None` otherwise.
    fn check(&mut self) -> Option<bool> {
        if self.headers_seen && self.disabled_emitted {
            // Headers appeared after we told the UI logging was disabled.
            self.disabled_emitted = false;
            return Some(true);
        }

        if self.headers_seen || self.disabled_emitted {
            return None;
        }

        if let Some(first) = self.first_bytes_at {
            if first.elapsed() >= HEADER_DETECTION_TIMEOUT {
                self.disabled_emitted = true;
                return Some(false);
            }
        }

        None
    }
}

// ---------------------------------------------------------------------------
// Background tasks
// ---------------------------------------------------------------------------

/// Runs the file tailer with rotation and header detection.
///
/// Polls the tailer at its configured interval, forwarding log entries to
/// the routing channel. When file rotation is detected, emits a
/// [`GameEvent::LogFileRotated`] event directly to the event bus so that
/// downstream consumers can reset their state.
///
/// Also tracks whether structured log headers (`[UnityCrossThreadLogger]`
/// or `[Client GRE]`) have been observed. If 30 seconds of log writes pass
/// without any structured entries, emits a
/// [`GameEvent::DetailedLoggingStatus`] with `enabled: false`. If headers
/// are subsequently detected, emits `enabled: true` to clear the UI state.
async fn run_tailer(
    mut tailer: FileTailer,
    entry_tx: tokio::sync::mpsc::Sender<crate::log::entry::LogEntry>,
    bus: EventBus,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) {
    let mut interval =
        tokio::time::interval(std::time::Duration::from_millis(tailer.poll_interval_ms()));
    // First tick completes immediately.
    interval.tick().await;

    let mut header_state = HeaderDetectionState::new();

    loop {
        tokio::select! {
            _ = interval.tick() => {
                // Snapshot last_event_at before poll to detect new bytes.
                let pre_poll_event_at = tailer.last_event_at();

                match tailer.poll().await {
                    Ok(entries) => {
                        // Check for rotation before sending entries.
                        if let Some(rotation) = tailer.take_rotation() {
                            let event = GameEvent::LogFileRotated(
                                LogFileRotatedEvent::for_rotation(
                                    rotation.detected_at(),
                                    rotation.previous_file_size(),
                                ),
                            );
                            bus.send(event);
                            header_state.reset();
                        }

                        // Detect new bytes read by comparing last_event_at.
                        if tailer.last_event_at() != pre_poll_event_at {
                            header_state.record_bytes_read();
                        }

                        if !entries.is_empty() {
                            header_state.record_headers_seen();
                        }

                        // Check if a detailed logging status event should
                        // be emitted.
                        if let Some(enabled) = header_state.check() {
                            let event = GameEvent::DetailedLoggingStatus(
                                DetailedLoggingStatusEvent::new_status(
                                    chrono::Utc::now(),
                                    enabled,
                                ),
                            );
                            bus.send(event);
                        }

                        for entry in entries {
                            if entry_tx.send(entry).await.is_err() {
                                ::log::info!("entry channel closed, stopping tailer");
                                return;
                            }
                        }
                    }
                    Err(e) => {
                        ::log::error!("tailer error: {e}");
                        return;
                    }
                }
            }
            _ = shutdown.changed() => {
                ::log::info!("shutdown signal received, stopping tailer");
                // Flush any remaining partial entries.
                for entry in tailer.flush() {
                    let _ = entry_tx.send(entry).await;
                }
                return;
            }
        }
    }
}

/// Runs the file tailer in one-shot mode, reading the entire file then exiting.
///
/// Buffers all entries from [`FileTailer::run_once`] before streaming them
/// through the channel. This trades memory for simplicity — suitable for
/// batch processing, not for memory-constrained streaming of very large files.
async fn run_tailer_once(
    mut tailer: FileTailer,
    entry_tx: tokio::sync::mpsc::Sender<crate::log::entry::LogEntry>,
) {
    match tailer.run_once().await {
        Ok(entries) => {
            for entry in entries {
                if entry_tx.send(entry).await.is_err() {
                    ::log::info!("entry channel closed during one-shot read");
                    return;
                }
            }
        }
        Err(e) => {
            ::log::error!("tailer error during one-shot read: {e}");
        }
    }
    // Dropping entry_tx signals the router that no more entries are coming.
}

/// Receives log entries, routes them through parsers, and sends events to the bus.
async fn run_router(
    mut entry_rx: tokio::sync::mpsc::Receiver<crate::log::entry::LogEntry>,
    router: Router,
    bus: EventBus,
) {
    while let Some(entry) = entry_rx.recv().await {
        for event in router.route(&entry) {
            bus.send(event);
        }
    }

    let stats = router.stats();
    ::log::info!(
        "router task exiting (routed: {}, unknown: {}, ts_failures: {})",
        stats.routed_count(),
        stats.unknown_count(),
        stats.timestamp_failure_count(),
    );
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::GameEvent;
    use std::io::Write;
    use tempfile::NamedTempFile;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// Helper: create a temp log file with given content.
    fn temp_log(content: &str) -> Result<NamedTempFile, std::io::Error> {
        let mut f = NamedTempFile::new()?;
        f.write_all(content.as_bytes())?;
        f.flush()?;
        Ok(f)
    }

    // -- start ---------------------------------------------------------------

    #[tokio::test]
    async fn test_start_returns_stream_and_subscriber() -> TestResult {
        let f = temp_log("")?;
        let (stream, _sub) = MtgaEventStream::start(f.path()).await?;
        stream.shutdown();
        Ok(())
    }

    #[tokio::test]
    async fn test_start_nonexistent_file_returns_error() {
        let result = MtgaEventStream::start(Path::new("/nonexistent/Player.log")).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_start_error_is_stream_error() {
        let result = MtgaEventStream::start(Path::new("/nonexistent/Player.log")).await;
        assert!(matches!(result, Err(StreamError::Tailer(_))));
    }

    // -- event delivery -------------------------------------------------------

    #[tokio::test]
    async fn test_start_delivers_session_event() -> TestResult {
        let content = "[UnityCrossThreadLogger]Updated account. \
                        DisplayName:TestPlayer, \
                        AccountID:abc123, \
                        Token:sometoken\n\
                        [UnityCrossThreadLogger]2/25/2026 12:00:00 PM\n\
                        some filler\n";
        let f = temp_log(content)?;

        let (stream, mut sub) = MtgaEventStream::start(f.path()).await?;

        let event = tokio::time::timeout(std::time::Duration::from_secs(3), sub.recv()).await?;
        assert!(event.is_some());
        assert!(
            matches!(&event, Some(GameEvent::Session(_))),
            "expected Session event, got {event:?}"
        );

        stream.shutdown();
        Ok(())
    }

    #[tokio::test]
    async fn test_start_delivers_game_state_event() -> TestResult {
        let payload = serde_json::json!({
            "greToClientEvent": {
                "greToClientMessages": [{
                    "type": "GREMessageType_GameStateMessage",
                    "gameStateMessage": {
                        "gameInfo": { "stage": "GameStage_Play" },
                        "gameObjects": [],
                        "zones": []
                    }
                }]
            }
        });
        let content = format!(
            "[UnityCrossThreadLogger]2/25/2026 12:00:00 PM\n{payload}\n\
             [UnityCrossThreadLogger]2/25/2026 12:00:01 PM\nfiller\n"
        );
        let f = temp_log(&content)?;

        let (stream, mut sub) = MtgaEventStream::start(f.path()).await?;

        let event = tokio::time::timeout(std::time::Duration::from_secs(3), sub.recv()).await?;
        assert!(event.is_some());
        assert!(matches!(event, Some(GameEvent::GameState(_))));

        stream.shutdown();
        Ok(())
    }

    #[tokio::test]
    async fn test_start_delivers_multiple_events() -> TestResult {
        let gs_payload = serde_json::json!({
            "greToClientEvent": {
                "greToClientMessages": [{
                    "type": "GREMessageType_GameStateMessage",
                    "gameStateMessage": {
                        "gameInfo": { "stage": "GameStage_Play" },
                        "gameObjects": [],
                        "zones": []
                    }
                }]
            }
        });
        let content = format!(
            "[UnityCrossThreadLogger]Updated account. \
             DisplayName:TestPlayer, \
             AccountID:abc123, \
             Token:sometoken\n\
             [UnityCrossThreadLogger]2/25/2026 12:00:00 PM\n{gs_payload}\n\
             [UnityCrossThreadLogger]2/25/2026 12:00:01 PM\nfiller\n"
        );
        let f = temp_log(&content)?;

        let (stream, mut sub) = MtgaEventStream::start(f.path()).await?;

        let mut events = Vec::new();
        for _ in 0..2 {
            let event = tokio::time::timeout(std::time::Duration::from_secs(3), sub.recv()).await?;
            if let Some(e) = event {
                events.push(e);
            }
        }

        assert_eq!(events.len(), 2);
        assert!(matches!(events[0], GameEvent::Session(_)));
        assert!(matches!(events[1], GameEvent::GameState(_)));

        stream.shutdown();
        Ok(())
    }

    // -- start_once -----------------------------------------------------------

    #[tokio::test]
    async fn test_start_once_returns_stream_and_subscriber() -> TestResult {
        let f = temp_log("")?;
        let (stream, _sub) = MtgaEventStream::start_once(f.path()).await?;
        stream.shutdown();
        Ok(())
    }

    #[tokio::test]
    async fn test_start_once_nonexistent_file_returns_error() {
        let result = MtgaEventStream::start_once(Path::new("/nonexistent/Player.log")).await;
        assert!(matches!(result, Err(StreamError::Tailer(_))));
    }

    #[tokio::test]
    async fn test_start_once_delivers_session_event() -> TestResult {
        let content = "[UnityCrossThreadLogger]Updated account. \
                        DisplayName:TestPlayer, \
                        AccountID:abc123, \
                        Token:sometoken\n\
                        [UnityCrossThreadLogger]2/25/2026 12:00:00 PM\n\
                        some filler\n";
        let f = temp_log(content)?;

        let (_stream, mut sub) = MtgaEventStream::start_once(f.path()).await?;

        let event = tokio::time::timeout(std::time::Duration::from_secs(3), sub.recv()).await?;
        assert!(event.is_some());
        assert!(
            matches!(&event, Some(GameEvent::Session(_))),
            "expected Session event, got {event:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_start_once_subscriber_ends_after_eof() -> TestResult {
        let content = "[UnityCrossThreadLogger]Updated account. \
                        DisplayName:TestPlayer, \
                        AccountID:abc123, \
                        Token:sometoken\n\
                        [UnityCrossThreadLogger]2/25/2026 12:00:00 PM\n\
                        some filler\n";
        let f = temp_log(content)?;

        let (_stream, mut sub) = MtgaEventStream::start_once(f.path()).await?;

        // Collect all events until None.
        let mut events = Vec::new();
        loop {
            let result =
                tokio::time::timeout(std::time::Duration::from_secs(3), sub.recv()).await?;
            match result {
                Some(e) => events.push(e),
                None => break,
            }
        }
        // Should have at least the Session event.
        assert!(!events.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn test_start_once_empty_file_subscriber_ends() -> TestResult {
        let f = temp_log("")?;
        let (_stream, mut sub) = MtgaEventStream::start_once(f.path()).await?;

        // Subscriber should receive None (pipeline exits at EOF).
        let result = tokio::time::timeout(std::time::Duration::from_secs(3), sub.recv()).await?;
        assert!(result.is_none());
        Ok(())
    }

    // -- shutdown --------------------------------------------------------------

    #[tokio::test]
    async fn test_shutdown_causes_subscriber_to_end() -> TestResult {
        let f = temp_log("")?;
        let (stream, mut sub) = MtgaEventStream::start(f.path()).await?;

        stream.shutdown();

        // After shutdown, subscriber should eventually receive None.
        let result = tokio::time::timeout(std::time::Duration::from_secs(3), sub.recv()).await?;
        assert!(result.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn test_double_shutdown_is_safe() -> TestResult {
        let f = temp_log("")?;
        let (stream, _sub) = MtgaEventStream::start(f.path()).await?;
        stream.shutdown();
        stream.shutdown(); // Should not panic.
        Ok(())
    }

    // -- debug ----------------------------------------------------------------

    #[tokio::test]
    async fn test_debug_format() -> TestResult {
        let f = temp_log("")?;
        let (stream, _sub) = MtgaEventStream::start(f.path()).await?;
        let debug = format!("{stream:?}");
        assert!(debug.contains("MtgaEventStream"));
        stream.shutdown();
        Ok(())
    }

    // -- rotation integration -------------------------------------------------

    #[tokio::test]
    async fn test_start_emits_log_file_rotated_event_on_rotation() -> TestResult {
        // Create initial log with enough content to set a non-zero offset.
        let initial = "[UnityCrossThreadLogger]Updated account. \
                        DisplayName:TestPlayer, \
                        AccountID:abc123, \
                        Token:sometoken\n\
                        [UnityCrossThreadLogger]2/25/2026 12:00:00 PM\n\
                        some filler\n";
        let f = temp_log(initial)?;
        let path = f.path().to_path_buf();

        let (stream, mut sub) = MtgaEventStream::start(&path).await?;

        // Wait for the initial Session event to be parsed.
        let event = tokio::time::timeout(std::time::Duration::from_secs(3), sub.recv()).await?;
        assert!(
            matches!(&event, Some(GameEvent::Session(_))),
            "expected Session event, got {event:?}"
        );

        // Give the tailer time to advance its offset past the initial content.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        // Simulate file rotation: replace with smaller content.
        std::fs::write(
            &path,
            "[UnityCrossThreadLogger] NewSession\n\
             [UnityCrossThreadLogger] AfterRotation\n",
        )?;

        // Wait for the LogFileRotated event.
        let mut found_rotation = false;
        for _ in 0..20 {
            let result = tokio::time::timeout(std::time::Duration::from_secs(2), sub.recv()).await;
            match result {
                Ok(Some(GameEvent::LogFileRotated(ref e))) => {
                    assert!(e.previous_file_size().is_some());
                    found_rotation = true;
                    break;
                }
                Ok(Some(_)) => {}           // Other events, keep looking.
                Ok(None) | Err(_) => break, // Bus closed or timeout.
            }
        }

        assert!(
            found_rotation,
            "expected a LogFileRotated event after file replacement"
        );

        stream.shutdown();
        Ok(())
    }

    // -- StreamError ----------------------------------------------------------

    #[test]
    fn test_stream_error_display() {
        let err = StreamError::Tailer(TailerError::Io {
            path: std::path::PathBuf::from("/test/Player.log"),
            source: std::io::Error::new(std::io::ErrorKind::NotFound, "file not found"),
        });
        let msg = err.to_string();
        assert!(msg.contains("/test/Player.log"));
        assert!(msg.contains("file not found"));
    }

    #[test]
    fn test_stream_error_is_debug() {
        let err = StreamError::Tailer(TailerError::Io {
            path: std::path::PathBuf::from("/test"),
            source: std::io::Error::other("test"),
        });
        let debug = format!("{err:?}");
        assert!(debug.contains("Tailer"));
    }

    // -- HeaderDetectionState -------------------------------------------------

    #[test]
    fn test_header_detection_initial_check_returns_none() {
        let mut state = HeaderDetectionState::new();
        assert!(state.check().is_none());
    }

    #[test]
    fn test_header_detection_no_timeout_before_bytes() {
        let mut state = HeaderDetectionState::new();
        // No bytes read yet — timer hasn't started.
        assert!(state.check().is_none());
    }

    #[tokio::test]
    async fn test_header_detection_emits_disabled_after_timeout() {
        tokio::time::pause();
        let mut state = HeaderDetectionState::new();
        state.record_bytes_read();

        // Before timeout: no event.
        tokio::time::advance(std::time::Duration::from_secs(29)).await;
        assert!(state.check().is_none());

        // After timeout: disabled.
        tokio::time::advance(std::time::Duration::from_secs(2)).await;
        assert_eq!(state.check(), Some(false));
    }

    #[tokio::test]
    async fn test_header_detection_disabled_emitted_once() {
        tokio::time::pause();
        let mut state = HeaderDetectionState::new();
        state.record_bytes_read();

        tokio::time::advance(std::time::Duration::from_secs(31)).await;
        assert_eq!(state.check(), Some(false));

        // Second check should not re-emit.
        assert!(state.check().is_none());
    }

    #[tokio::test]
    async fn test_header_detection_no_event_when_headers_seen_early() {
        tokio::time::pause();
        let mut state = HeaderDetectionState::new();
        state.record_bytes_read();
        state.record_headers_seen();

        // Even after timeout, should not emit disabled.
        tokio::time::advance(std::time::Duration::from_secs(31)).await;
        assert!(state.check().is_none());
    }

    #[tokio::test]
    async fn test_header_detection_emits_enabled_after_disabled() {
        tokio::time::pause();
        let mut state = HeaderDetectionState::new();
        state.record_bytes_read();

        // Trigger disabled.
        tokio::time::advance(std::time::Duration::from_secs(31)).await;
        assert_eq!(state.check(), Some(false));

        // Headers seen later — should emit enabled.
        state.record_headers_seen();
        assert_eq!(state.check(), Some(true));
    }

    #[tokio::test]
    async fn test_header_detection_enabled_emitted_once() {
        tokio::time::pause();
        let mut state = HeaderDetectionState::new();
        state.record_bytes_read();

        tokio::time::advance(std::time::Duration::from_secs(31)).await;
        assert_eq!(state.check(), Some(false));

        state.record_headers_seen();
        assert_eq!(state.check(), Some(true));

        // Should not re-emit.
        assert!(state.check().is_none());
    }

    #[tokio::test]
    async fn test_header_detection_reset_clears_state() {
        tokio::time::pause();
        let mut state = HeaderDetectionState::new();
        state.record_bytes_read();

        tokio::time::advance(std::time::Duration::from_secs(31)).await;
        assert_eq!(state.check(), Some(false));

        // Reset simulates log rotation.
        state.reset();

        // After reset, timer hasn't started — no event.
        assert!(state.check().is_none());

        // New bytes after reset start a fresh timer.
        state.record_bytes_read();
        tokio::time::advance(std::time::Duration::from_secs(31)).await;
        assert_eq!(state.check(), Some(false));
    }

    // -- Detailed logging integration (start) ---------------------------------

    #[tokio::test]
    async fn test_start_no_detailed_logging_event_with_headers() -> TestResult {
        // File with structured headers should NOT produce a
        // DetailedLoggingStatus event.
        let content = "[UnityCrossThreadLogger]Updated account. \
                        DisplayName:TestPlayer, \
                        AccountID:abc123, \
                        Token:sometoken\n\
                        [UnityCrossThreadLogger]2/25/2026 12:00:00 PM\n\
                        some filler\n";
        let f = temp_log(content)?;

        let (stream, mut sub) = MtgaEventStream::start(f.path()).await?;

        // Collect events for a short window.
        let mut events = Vec::new();
        loop {
            let result =
                tokio::time::timeout(std::time::Duration::from_millis(500), sub.recv()).await;
            match result {
                Ok(Some(e)) => events.push(e),
                _ => break,
            }
        }

        // Should have a Session event but no DetailedLoggingStatus.
        assert!(
            events.iter().any(|e| matches!(e, GameEvent::Session(_))),
            "expected at least one Session event"
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, GameEvent::DetailedLoggingStatus(_))),
            "should not emit DetailedLoggingStatus when headers are present"
        );

        stream.shutdown();
        Ok(())
    }
}
