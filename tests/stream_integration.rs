//! Integration tests for `MtgaEventStream::start`.
//!
//! Verifies the full pipeline: file tailer -> router -> event bus -> subscriber.
//! Each test writes a sample log file, starts the stream, and asserts that the
//! expected typed events arrive on the subscriber.
//!
//! Gated on the `tailer` feature (requires tokio + file tailing stack).
#![cfg(feature = "tailer")]

use std::fmt::Write as _;
use std::io::Write;

use tempfile::NamedTempFile;

use manasight_parser::stream::MtgaEventStream;
use manasight_parser::GameEvent;

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// Helper: create a temp log file with the given content.
fn temp_log(content: &str) -> Result<NamedTempFile, std::io::Error> {
    let mut f = NamedTempFile::new()?;
    f.write_all(content.as_bytes())?;
    f.flush()?;
    Ok(f)
}

// ---------------------------------------------------------------------------
// End-to-end: feed sample log, verify events arrive
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_stream_session_event_from_log_file() -> TestResult {
    let content = "[UnityCrossThreadLogger]authenticateResponse\n\
                    {\"screenName\":\"TestPlayer\"}\n\
                    [UnityCrossThreadLogger]2/25/2026 12:00:00 PM\nfiller\n";
    let f = temp_log(content)?;

    let (stream, mut sub) = MtgaEventStream::start(f.path()).await?;

    let event = tokio::time::timeout(std::time::Duration::from_secs(5), sub.recv()).await?;
    assert!(event.is_some(), "expected an event, got None");
    assert!(
        matches!(&event, Some(GameEvent::Session(_))),
        "expected Session event, got {event:?}"
    );

    stream.shutdown();
    Ok(())
}

#[tokio::test]
async fn test_stream_game_state_event_from_log_file() -> TestResult {
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

    let event = tokio::time::timeout(std::time::Duration::from_secs(5), sub.recv()).await?;
    assert!(event.is_some(), "expected an event, got None");
    assert!(
        matches!(&event, Some(GameEvent::GameState(_))),
        "expected GameState event, got {event:?}"
    );

    stream.shutdown();
    Ok(())
}

#[tokio::test]
async fn test_stream_multiple_event_types_in_order() -> TestResult {
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
    // Session event (no timestamp) followed by GameState event (with timestamp).
    let content = format!(
        "[UnityCrossThreadLogger]authenticateResponse\n\
         {{\"screenName\":\"TestPlayer\"}}\n\
         [UnityCrossThreadLogger]2/25/2026 12:00:00 PM\n{gs_payload}\n\
         [UnityCrossThreadLogger]2/25/2026 12:00:01 PM\nfiller\n"
    );
    let f = temp_log(&content)?;

    let (stream, mut sub) = MtgaEventStream::start(f.path()).await?;

    // Collect two events.
    let mut events = Vec::new();
    for _ in 0..2 {
        let event = tokio::time::timeout(std::time::Duration::from_secs(5), sub.recv()).await?;
        if let Some(e) = event {
            events.push(e);
        }
    }

    assert_eq!(events.len(), 2, "expected 2 events, got {}", events.len());
    assert!(
        matches!(&events[0], GameEvent::Session(_)),
        "first event should be Session, got {:?}",
        events[0]
    );
    assert!(
        matches!(&events[1], GameEvent::GameState(_)),
        "second event should be GameState, got {:?}",
        events[1]
    );

    stream.shutdown();
    Ok(())
}

#[tokio::test]
async fn test_stream_match_state_event() -> TestResult {
    let match_payload = serde_json::json!({
        "matchGameRoomStateChangedEvent": {
            "gameRoomInfo": {
                "stateType": "MatchGameRoomStateType_Playing",
                "gameRoomConfig": {
                    "matchId": "match-123",
                    "reservedPlayers": []
                }
            }
        }
    });
    let content = format!(
        "[UnityCrossThreadLogger]2/25/2026 12:00:00 PM\n{match_payload}\n\
         [UnityCrossThreadLogger]2/25/2026 12:00:01 PM\nfiller\n"
    );
    let f = temp_log(&content)?;

    let (stream, mut sub) = MtgaEventStream::start(f.path()).await?;

    let event = tokio::time::timeout(std::time::Duration::from_secs(5), sub.recv()).await?;
    assert!(event.is_some());
    assert!(
        matches!(&event, Some(GameEvent::MatchState(_))),
        "expected MatchState event, got {event:?}"
    );

    stream.shutdown();
    Ok(())
}

#[tokio::test]
async fn test_stream_rank_event() -> TestResult {
    let rank_payload = serde_json::json!({
        "constructedClass": "Gold",
        "constructedLevel": 2,
        "limitedClass": "Silver",
        "limitedLevel": 1
    });
    let content = format!(
        "[UnityCrossThreadLogger]2/25/2026 12:00:00 PM\n\
         <== RankGetCombinedRankInfo(abc-123)\n{rank_payload}\n\
         [UnityCrossThreadLogger]2/25/2026 12:00:01 PM\nfiller\n"
    );
    let f = temp_log(&content)?;

    let (stream, mut sub) = MtgaEventStream::start(f.path()).await?;

    let event = tokio::time::timeout(std::time::Duration::from_secs(5), sub.recv()).await?;
    assert!(event.is_some());
    assert!(
        matches!(&event, Some(GameEvent::Rank(_))),
        "expected Rank event, got {event:?}"
    );

    stream.shutdown();
    Ok(())
}

#[tokio::test]
async fn test_stream_subscriber_ends_after_shutdown() -> TestResult {
    let f = temp_log("")?;
    let (stream, mut sub) = MtgaEventStream::start(f.path()).await?;

    stream.shutdown();

    let result = tokio::time::timeout(std::time::Duration::from_secs(5), sub.recv()).await?;
    assert!(
        result.is_none(),
        "subscriber should receive None after shutdown"
    );
    Ok(())
}

#[tokio::test]
async fn test_stream_detailed_logs_enabled_event() -> TestResult {
    let content = "DETAILED LOGS: ENABLED\n\
                    [UnityCrossThreadLogger]authenticateResponse\n\
                    {\"screenName\":\"TestPlayer\"}\n\
                    [UnityCrossThreadLogger]2/25/2026 12:00:00 PM\nfiller\n";
    let f = temp_log(content)?;

    let (stream, mut sub) = MtgaEventStream::start(f.path()).await?;

    // The first event should be DetailedLoggingStatus(enabled=true).
    let event = tokio::time::timeout(std::time::Duration::from_secs(5), sub.recv()).await?;
    assert!(event.is_some(), "expected an event, got None");
    assert!(
        matches!(&event, Some(GameEvent::DetailedLoggingStatus(_))),
        "expected DetailedLoggingStatus event, got {event:?}"
    );
    if let Some(GameEvent::DetailedLoggingStatus(ref e)) = event {
        assert_eq!(e.enabled(), Some(true));
    }

    stream.shutdown();
    Ok(())
}

#[tokio::test]
async fn test_stream_detailed_logs_disabled_event() -> TestResult {
    let content = "DETAILED LOGS: DISABLED\n\
                    some unstructured line\n";
    let f = temp_log(content)?;

    let (stream, mut sub) = MtgaEventStream::start(f.path()).await?;

    // The first event should be DetailedLoggingStatus(enabled=false).
    let event = tokio::time::timeout(std::time::Duration::from_secs(5), sub.recv()).await?;
    assert!(event.is_some(), "expected an event, got None");
    assert!(
        matches!(&event, Some(GameEvent::DetailedLoggingStatus(_))),
        "expected DetailedLoggingStatus event, got {event:?}"
    );
    if let Some(GameEvent::DetailedLoggingStatus(ref e)) = event {
        assert_eq!(e.enabled(), Some(false));
    }

    stream.shutdown();
    Ok(())
}

#[tokio::test]
async fn test_stream_re_exports_accessible() -> TestResult {
    // Verify that key types are accessible via the crate root re-exports.
    // Using them in type annotations proves the re-exports compile.
    let class: manasight_parser::PerformanceClass =
        manasight_parser::PerformanceClass::InteractiveDispatch;
    assert_eq!(class.as_class_number(), 1);

    // Verify MtgaEventStream and Subscriber are re-exported.
    let f = temp_log("")?;
    let (stream, _sub): (
        manasight_parser::MtgaEventStream,
        manasight_parser::Subscriber,
    ) = manasight_parser::MtgaEventStream::start(f.path()).await?;
    stream.shutdown();
    Ok(())
}

// ---------------------------------------------------------------------------
// Lossless delivery: a large catch-up read reaches the subscriber intact
// ---------------------------------------------------------------------------

/// Number of `GameStateMessage`s batched into each synthetic log entry.
const GSMS_PER_ENTRY: u64 = 4;

/// Per-event receive timeout for the lossless-delivery tests.
const RECV_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Builds a synthetic log of `entries` entries, each batching four
/// `GameStateMessage`s with consecutive `gameStateId`s starting at
/// `first_id`. A trailing terminator entry completes the last GSM entry and
/// yields no event of its own.
fn gsm_log(entries: u64, first_id: u64) -> String {
    let mut out = String::new();
    for entry in 0..entries {
        let messages: Vec<serde_json::Value> = (0..GSMS_PER_ENTRY)
            .map(|i| {
                let id = first_id + entry * GSMS_PER_ENTRY + i;
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

/// Returns the `game_state_id` of a `GameState` event, if any.
fn game_state_id(event: &GameEvent) -> Option<u64> {
    match event {
        GameEvent::GameState(e) => e.payload()["game_state_id"].as_u64(),
        _ => None,
    }
}

/// Builds the 400-entry × 4-GSM fixture and its `parse_whole_log` oracle,
/// asserting the oracle holds exactly 1600 events.
fn lossless_fixture() -> Result<(NamedTempFile, Vec<GameEvent>), std::io::Error> {
    let content = gsm_log(400, 1);
    let expected = manasight_parser::parse_whole_log(&content);
    assert_eq!(expected.len(), 1600, "fixture must yield 1600 events");
    Ok((temp_log(&content)?, expected))
}

/// Receives up to `count` events, stopping at the first timeout or `None`.
async fn recv_up_to(sub: &mut manasight_parser::Subscriber, count: usize) -> Vec<GameEvent> {
    let mut received = Vec::with_capacity(count);
    while received.len() < count {
        match tokio::time::timeout(RECV_TIMEOUT, sub.recv()).await {
            Ok(Some(event)) => received.push(event),
            Ok(None) | Err(_) => break,
        }
    }
    received
}

/// Receives events until `None`, failing if the stream does not end in time.
async fn recv_to_end(
    sub: &mut manasight_parser::Subscriber,
) -> Result<Vec<GameEvent>, tokio::time::error::Elapsed> {
    tokio::time::timeout(RECV_TIMEOUT, async {
        let mut received = Vec::new();
        while let Some(event) = sub.recv().await {
            received.push(event);
        }
        received
    })
    .await
}

/// Asserts the subscriber received exactly the oracle's events, in order.
fn assert_all_delivered(received: &[GameEvent], expected: &[GameEvent]) {
    let missed = expected.len().saturating_sub(received.len());
    assert_eq!(
        received.len(),
        expected.len(),
        "missed {missed} event(s): received {} of {}",
        received.len(),
        expected.len(),
    );
    assert!(
        received == expected,
        "received all {} events, but not in order or not identical",
        expected.len()
    );
}

/// Shared body of the `start` lossless-delivery tests.
async fn assert_start_delivers_every_event() -> TestResult {
    let (f, expected) = lossless_fixture()?;
    let (stream, mut sub) = MtgaEventStream::start(f.path()).await?;

    let received = recv_up_to(&mut sub, expected.len()).await;
    assert_all_delivered(&received, &expected);

    stream.shutdown();
    let next = tokio::time::timeout(RECV_TIMEOUT, sub.recv()).await?;
    assert!(next.is_none(), "expected None after shutdown, got {next:?}");
    Ok(())
}

/// Shared body of the `start_once` lossless-delivery tests.
async fn assert_start_once_delivers_every_event() -> TestResult {
    let (f, expected) = lossless_fixture()?;
    let (stream, mut sub) = MtgaEventStream::start_once(f.path()).await?;
    // Dropping the handle without `shutdown()` must not lose anything.
    drop(stream);
    // Let the pipeline fill the subscriber queue, so the router waits on it
    // with the handle already gone.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    let received = recv_to_end(&mut sub).await?;
    assert_all_delivered(&received, &expected);
    Ok(())
}

#[tokio::test]
async fn test_stream_start_delivers_every_event_current_thread() -> TestResult {
    assert_start_delivers_every_event().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_stream_start_delivers_every_event_multi_thread() -> TestResult {
    assert_start_delivers_every_event().await
}

#[tokio::test]
async fn test_stream_start_once_delivers_every_event_current_thread() -> TestResult {
    assert_start_once_delivers_every_event().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_stream_start_once_delivers_every_event_multi_thread() -> TestResult {
    assert_start_once_delivers_every_event().await
}

#[tokio::test]
async fn test_stream_rotation_event_follows_queued_old_file_events() -> TestResult {
    // 200 entries (800 events) fit in the tailer-to-router channel, so the
    // whole old file is read and queued while nobody reads the subscriber.
    let f = temp_log(&gsm_log(200, 1))?;
    let path = f.path().to_path_buf();
    let (stream, mut sub) = MtgaEventStream::start(&path).await?;

    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    // Rotate: replace the file with a smaller one while old events are queued.
    std::fs::write(&path, gsm_log(2, 9001))?;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    let received = recv_up_to(&mut sub, 809).await;
    assert_eq!(received.len(), 809, "expected 800 old + 1 rotation + 8 new");

    let rotation_index = received
        .iter()
        .position(|e| matches!(e, GameEvent::LogFileRotated(_)));
    assert_eq!(
        rotation_index,
        Some(800),
        "LogFileRotated must follow every old-file event"
    );
    let ids: Vec<Option<u64>> = received.iter().map(game_state_id).collect();
    let expected_ids: Vec<Option<u64>> = (1..=800)
        .map(Some)
        .chain(std::iter::once(None))
        .chain((9001..=9008).map(Some))
        .collect();
    assert_eq!(ids, expected_ids);

    stream.shutdown();
    Ok(())
}
