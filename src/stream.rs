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
use crate::events::{GameEvent, LogFileRotatedEvent};
use crate::log::tailer::{FileTailer, TailerError};
use crate::router::Router;

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
// Background tasks
// ---------------------------------------------------------------------------

/// Runs the file tailer with rotation detection.
///
/// Polls the tailer at its configured interval, forwarding log entries to
/// the routing channel. When file rotation is detected, emits a
/// [`GameEvent::LogFileRotated`] event directly to the event bus so that
/// downstream consumers can reset their state.
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

    loop {
        tokio::select! {
            _ = interval.tick() => {
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

    // -- Literal DETAILED LOGS detection (start_once) --------------------------

    #[tokio::test]
    async fn test_start_once_detailed_logs_enabled() -> TestResult {
        let content = "DETAILED LOGS: ENABLED\n\
                        [UnityCrossThreadLogger]Updated account. \
                        DisplayName:TestPlayer, \
                        AccountID:abc123, \
                        Token:sometoken\n\
                        [UnityCrossThreadLogger]2/25/2026 12:00:00 PM\n\
                        some filler\n";
        let f = temp_log(content)?;

        let (_stream, mut sub) = MtgaEventStream::start_once(f.path()).await?;

        // Collect all events.
        let mut events = Vec::new();
        loop {
            let result =
                tokio::time::timeout(std::time::Duration::from_secs(3), sub.recv()).await?;
            match result {
                Some(e) => events.push(e),
                None => break,
            }
        }

        // Should have a DetailedLoggingStatus(enabled=true) event.
        let dls_events: Vec<_> = events
            .iter()
            .filter(|e| matches!(e, GameEvent::DetailedLoggingStatus(_)))
            .collect();
        assert_eq!(
            dls_events.len(),
            1,
            "expected exactly one DetailedLoggingStatus event, got {}",
            dls_events.len(),
        );
        if let GameEvent::DetailedLoggingStatus(ref e) = dls_events[0] {
            assert_eq!(e.enabled(), Some(true));
        }

        // Should also have Session event.
        assert!(events.iter().any(|e| matches!(e, GameEvent::Session(_))));
        Ok(())
    }

    #[tokio::test]
    async fn test_start_once_detailed_logs_disabled() -> TestResult {
        let content = "DETAILED LOGS: DISABLED\n\
                        some unstructured line\n\
                        another unstructured line\n";
        let f = temp_log(content)?;

        let (_stream, mut sub) = MtgaEventStream::start_once(f.path()).await?;

        // Collect all events.
        let mut events = Vec::new();
        loop {
            let result =
                tokio::time::timeout(std::time::Duration::from_secs(3), sub.recv()).await?;
            match result {
                Some(e) => events.push(e),
                None => break,
            }
        }

        // Should have a DetailedLoggingStatus(enabled=false) event.
        let dls_events: Vec<_> = events
            .iter()
            .filter(|e| matches!(e, GameEvent::DetailedLoggingStatus(_)))
            .collect();
        assert_eq!(
            dls_events.len(),
            1,
            "expected exactly one DetailedLoggingStatus event, got {}",
            dls_events.len(),
        );
        if let GameEvent::DetailedLoggingStatus(ref e) = dls_events[0] {
            assert_eq!(e.enabled(), Some(false));
        }
        Ok(())
    }
}
