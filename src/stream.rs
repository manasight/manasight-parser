//! Public entry point for streaming typed events from an MTG Arena log file.
//!
//! [`MtgaEventStream`] wires together the file tailer and router into a
//! single `async fn` that returns a [`Subscriber`] of typed [`GameEvent`]
//! values. It runs entirely on the caller's Tokio runtime -- no internal
//! runtime is created.
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
//! # Delivery and backpressure
//!
//! The [`Subscriber`] reads from a bounded queue that the router waits on.
//! When the subscriber falls behind, the router pauses (and with it the
//! tailer) instead of dropping events, so every event is delivered in log
//! order, including during the catch-up read of an existing log file. A
//! subscriber that stops reading pauses tailing; memory stays bounded.
//! [`GameEvent::LogFileRotated`] travels through the same ordered path, after
//! every event from the old file.
//!
//! # Shutdown
//!
//! Call [`MtgaEventStream::shutdown`] to stop the pipeline. Shutdown stops
//! rather than drains: it never waits on the subscriber. A subscriber that
//! keeps reading receives a gap-free, in-order prefix of the remaining
//! events and then `None`; the prefix ends where the router found the queue
//! full. A subscriber that is never read does not keep the pipeline alive.
//!
//! Dropping the [`MtgaEventStream`] without calling `shutdown` stops tailing
//! but loses nothing already read: the subscriber receives every remaining
//! event, then `None`.

use std::path::Path;

use crate::event_bus::Subscriber;
use crate::events::{GameEvent, LogFileRotatedEvent};
use crate::log::entry::LogEntry;
use crate::log::tailer::{FileTailer, TailerError};
use crate::router::Router;

/// Capacity of the channel from the tailer to the router, in items.
const PIPELINE_CAPACITY: usize = 256;

/// Capacity of the queue from the router to the [`Subscriber`], in events.
const EVENT_QUEUE_CAPACITY: usize = 256;

/// An item on the tailer-to-router channel, which keeps file order.
enum PipelineItem {
    /// A complete log entry for the router to parse.
    Entry(LogEntry),
    /// An event produced by the tailer itself (log rotation), passed through.
    Event(GameEvent),
}

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
/// together the tailer and router, and spawns a background task on the
/// caller's Tokio runtime. The returned [`Subscriber`] receives typed
/// [`GameEvent`] values as they are parsed, without loss (see the
/// [module documentation](self)).
///
/// Call [`shutdown`](Self::shutdown) to stop the background task and clean
/// up resources; it never waits on the subscriber. Dropping the
/// `MtgaEventStream` without calling `shutdown` is safe -- tailing stops,
/// the subscriber still receives every event already read, and the
/// background task ends once those are delivered or the subscriber is
/// dropped.
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
    /// 3. Sends recognized events to the subscriber's queue, waiting for
    ///    space when the subscriber falls behind
    ///
    /// Returns a tuple of `(MtgaEventStream, Subscriber)`. Call
    /// [`shutdown`](Self::shutdown) on the `MtgaEventStream` to stop
    /// the background task.
    ///
    /// # Errors
    ///
    /// Returns [`StreamError::Tailer`] if the log file cannot be opened.
    pub async fn start(log_path: &Path) -> Result<(Self, Subscriber), StreamError> {
        let tailer = FileTailer::open_from_start(log_path).await?;
        let router = Router::new();
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

        let (item_tx, item_rx) = tokio::sync::mpsc::channel(PIPELINE_CAPACITY);
        let (event_tx, event_rx) = tokio::sync::mpsc::channel(EVENT_QUEUE_CAPACITY);

        // Spawn the tailer task (rotation travels in-band with the entries).
        let tailer_handle = tokio::spawn(run_tailer(tailer, item_tx, shutdown_rx.clone()));

        // Spawn the routing task.
        let router_handle = tokio::spawn(run_router(item_rx, router, event_tx, shutdown_rx));

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

        Ok((stream, Subscriber::from_queue(event_rx)))
    }

    /// Starts a one-shot event stream that reads an entire log file and exits.
    ///
    /// Opens the file via [`FileTailer::open_from_start`], reads all
    /// entries, routes them through the parser dispatch chain, and sends
    /// recognized events to the subscriber's queue, waiting for space when
    /// the subscriber falls behind. The pipeline stops automatically at EOF
    /// rather than polling indefinitely.
    ///
    /// This is useful for batch processing complete log files (smoke tests,
    /// replay analysis, importing `Player-prev.log`).
    ///
    /// Returns a tuple of `(MtgaEventStream, Subscriber)`. The `Subscriber`
    /// will receive `None` once all events have been delivered and the
    /// pipeline finishes. The pipeline exits at EOF on its own, so the
    /// `MtgaEventStream` may be dropped right away without losing events.
    /// Calling [`shutdown`](Self::shutdown) stops a pipeline that is still
    /// waiting on the subscriber; once everything has been delivered it is
    /// a no-op.
    ///
    /// # Errors
    ///
    /// Returns [`StreamError::Tailer`] if the log file cannot be opened.
    pub async fn start_once(log_path: &Path) -> Result<(Self, Subscriber), StreamError> {
        let tailer = FileTailer::open_from_start(log_path).await?;
        let router = Router::new();
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

        let (item_tx, item_rx) = tokio::sync::mpsc::channel(PIPELINE_CAPACITY);
        let (event_tx, event_rx) = tokio::sync::mpsc::channel(EVENT_QUEUE_CAPACITY);

        // Spawn the tailer task — uses run_once, exits at EOF.
        let tailer_handle = tokio::spawn(run_tailer_once(tailer, item_tx));

        // Spawn the routing task.
        let router_handle = tokio::spawn(run_router(item_rx, router, event_tx, shutdown_rx));

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

        Ok((stream, Subscriber::from_queue(event_rx)))
    }

    /// Signals the background pipeline to stop.
    ///
    /// Shutdown stops rather than drains, and never waits on the
    /// subscriber. The tailer stops at its next poll and flushes any
    /// remaining buffered entry; the router keeps delivering while the
    /// subscriber's queue has space and stops as soon as it finds the queue
    /// full. The [`Subscriber`] therefore receives a gap-free, in-order
    /// prefix of the remaining events, then `None`. A subscriber that is
    /// never read does not keep the pipeline alive.
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
/// the routing channel. When file rotation is detected, sends a
/// [`GameEvent::LogFileRotated`] event through the same channel, after every
/// entry from the old file and before the new file's entries, so that
/// downstream consumers can reset their state at the right point.
async fn run_tailer(
    mut tailer: FileTailer,
    item_tx: tokio::sync::mpsc::Sender<PipelineItem>,
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
                            if item_tx.send(PipelineItem::Event(event)).await.is_err() {
                                ::log::info!("entry channel closed, stopping tailer");
                                return;
                            }
                        }

                        for entry in entries {
                            if item_tx.send(PipelineItem::Entry(entry)).await.is_err() {
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
                    let _ = item_tx.send(PipelineItem::Entry(entry)).await;
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
async fn run_tailer_once(mut tailer: FileTailer, item_tx: tokio::sync::mpsc::Sender<PipelineItem>) {
    match tailer.run_once().await {
        Ok(entries) => {
            for entry in entries {
                if item_tx.send(PipelineItem::Entry(entry)).await.is_err() {
                    ::log::info!("entry channel closed during one-shot read");
                    return;
                }
            }
        }
        Err(e) => {
            ::log::error!("tailer error during one-shot read: {e}");
        }
    }
    // Dropping item_tx signals the router that no more entries are coming.
}

/// Receives pipeline items, routes entries through parsers, and delivers
/// every event to the subscriber's queue in order.
///
/// Waits for queue space when the subscriber falls behind (backpressure).
/// Stops when the subscriber is dropped, when the tailer closes the channel,
/// or when [`MtgaEventStream::shutdown`] is called while the queue is full.
async fn run_router(
    mut item_rx: tokio::sync::mpsc::Receiver<PipelineItem>,
    router: Router,
    event_tx: tokio::sync::mpsc::Sender<GameEvent>,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) {
    let mut watching_shutdown = true;
    'items: while let Some(item) = item_rx.recv().await {
        let events = match item {
            PipelineItem::Entry(entry) => router.route(&entry),
            PipelineItem::Event(event) => vec![event],
        };
        for event in events {
            if !deliver(&event_tx, event, &mut shutdown, &mut watching_shutdown).await {
                break 'items;
            }
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

/// Sends one event to the subscriber's queue, waiting for space.
///
/// Returns `false` when the router should stop: the subscriber was dropped,
/// or [`MtgaEventStream::shutdown`] was called while the queue was full. A
/// closed shutdown channel (the handle was dropped without `shutdown()`) is
/// not a stop request: the router stops watching it and keeps delivering.
async fn deliver(
    event_tx: &tokio::sync::mpsc::Sender<GameEvent>,
    event: GameEvent,
    shutdown: &mut tokio::sync::watch::Receiver<bool>,
    watching_shutdown: &mut bool,
) -> bool {
    loop {
        tokio::select! {
            // Prefer delivery: shutdown only abandons a send that is blocked.
            biased;
            permit = event_tx.reserve() => {
                return if let Ok(permit) = permit {
                    permit.send(event);
                    true
                } else {
                    ::log::info!("subscriber dropped, stopping router");
                    false
                };
            }
            changed = shutdown.changed(), if *watching_shutdown => {
                if changed.is_ok() {
                    ::log::info!("shutdown with a full subscriber queue, stopping router");
                    return false;
                }
                // The handle was dropped without `shutdown()`: keep delivering.
                *watching_shutdown = false;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::GameEvent;
    use std::fmt::Write as _;
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
        let content = "[UnityCrossThreadLogger]authenticateResponse\n\
                        {\"screenName\":\"TestPlayer\"}\n\
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
            "[UnityCrossThreadLogger]authenticateResponse\n\
             {{\"screenName\":\"TestPlayer\"}}\n\
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
        let content = "[UnityCrossThreadLogger]authenticateResponse\n\
                        {\"screenName\":\"TestPlayer\"}\n\
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
        let content = "[UnityCrossThreadLogger]authenticateResponse\n\
                        {\"screenName\":\"TestPlayer\"}\n\
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

    // -- shutdown under backpressure -------------------------------------------

    /// Builds a synthetic log of `entries` entries, each batching four
    /// `GameStateMessage`s with consecutive `gameStateId`s from 1, plus a
    /// terminator entry that yields no event.
    fn gsm_log(entries: u64) -> String {
        let mut out = String::new();
        for entry in 0..entries {
            let messages: Vec<serde_json::Value> = (1..=4)
                .map(|i| {
                    let id = entry * 4 + i;
                    serde_json::json!({
                        "type": "GREMessageType_GameStateMessage",
                        "gameStateId": id,
                        "gameStateMessage": {
                            "gameStateId": id,
                            "gameInfo": { "stage": "GameStage_Play" },
                            "gameObjects": [],
                            "zones": []
                        }
                    })
                })
                .collect();
            let payload = serde_json::json!({
                "greToClientEvent": { "greToClientMessages": messages }
            });
            let _ = writeln!(
                out,
                "[UnityCrossThreadLogger]2/25/2026 12:00:00 PM\n{payload}"
            );
        }
        out.push_str("[UnityCrossThreadLogger]2/25/2026 12:00:01 PM\nfiller\n");
        out
    }

    /// Calls `shutdown()` on a stream whose subscriber is held but never
    /// read, then checks that the pipeline ends and that the subscriber
    /// drains a gap-free prefix of the 1600 events, then `None`.
    async fn assert_shutdown_stops_unread_pipeline(
        stream: MtgaEventStream,
        mut sub: Subscriber,
    ) -> TestResult {
        // Six 50 ms polls: the router is parked on the full subscriber queue.
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;

        stream.shutdown();
        let MtgaEventStream {
            _pipeline_handle: pipeline_handle,
            ..
        } = stream;
        tokio::time::timeout(std::time::Duration::from_secs(5), pipeline_handle).await??;

        let drained = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            let mut events = Vec::new();
            while let Some(event) = sub.recv().await {
                events.push(event);
            }
            events
        })
        .await?;
        assert!(
            !drained.is_empty() && drained.len() < 1600,
            "expected a partial prefix, got {} event(s)",
            drained.len()
        );
        let ids: Vec<Option<u64>> = drained
            .iter()
            .map(|e| match e {
                GameEvent::GameState(gs) => gs.payload()["game_state_id"].as_u64(),
                _ => None,
            })
            .collect();
        let expected: Vec<Option<u64>> = (1..=drained.len() as u64).map(Some).collect();
        assert_eq!(ids, expected, "drained events must be a gap-free prefix");
        Ok(())
    }

    #[tokio::test]
    async fn test_shutdown_stops_start_pipeline_with_unread_subscriber() -> TestResult {
        let f = temp_log(&gsm_log(400))?;
        let (stream, sub) = MtgaEventStream::start(f.path()).await?;
        assert_shutdown_stops_unread_pipeline(stream, sub).await
    }

    #[tokio::test]
    async fn test_shutdown_stops_start_once_pipeline_with_unread_subscriber() -> TestResult {
        let f = temp_log(&gsm_log(400))?;
        let (stream, sub) = MtgaEventStream::start_once(f.path()).await?;
        assert_shutdown_stops_unread_pipeline(stream, sub).await
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
        let initial = "[UnityCrossThreadLogger]authenticateResponse\n\
                        {\"screenName\":\"TestPlayer\"}\n\
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
                        [UnityCrossThreadLogger]authenticateResponse\n\
                        {\"screenName\":\"TestPlayer\"}\n\
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
