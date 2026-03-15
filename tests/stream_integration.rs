//! Integration tests for `MtgaEventStream::start`.
//!
//! Verifies the full pipeline: file tailer -> router -> event bus -> subscriber.
//! Each test writes a sample log file, starts the stream, and asserts that the
//! expected typed events arrive on the subscriber.

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
    let content = "[UnityCrossThreadLogger]Updated account. \
                    DisplayName:TestPlayer, \
                    AccountID:abc123, \
                    Token:sometoken\n\
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
        "[UnityCrossThreadLogger]Updated account. \
         DisplayName:TestPlayer, \
         AccountID:abc123, \
         Token:sometoken\n\
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
                    [UnityCrossThreadLogger]Updated account. \
                    DisplayName:TestPlayer, \
                    AccountID:abc123, \
                    Token:sometoken\n\
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

// ---------------------------------------------------------------------------
// Detailed logging: timeout detection via run_tailer (deterministic time)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_stream_detailed_logging_timeout_fires_without_headers() -> TestResult {
    // Verifies that run_tailer's 30-second timeout fires when a log file
    // contains bytes but no structured headers (`[UnityCrossThreadLogger]`
    // or `[Client GRE]`). This simulates a Player.log written with
    // detailed logging disabled and WITHOUT the `DETAILED LOGS:` literal
    // (pre-#341 log files, or edge cases where the literal is absent).
    //
    // Uses tokio::time::pause() + advance() for deterministic timing.
    tokio::time::pause();

    // Log content with NO structured headers and NO `DETAILED LOGS:` literal.
    let content = "some unstructured line without a header\n\
                   another line of unstructured content\n\
                   yet another line that has no bracket prefix\n";
    let f = temp_log(content)?;

    let (stream, mut sub) = MtgaEventStream::start(f.path()).await?;

    // Let the tailer's first poll tick fire and read the file content.
    // With paused time, `tokio::time::sleep` yields to other tasks.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Advance time past the 30-second timeout threshold.
    tokio::time::advance(std::time::Duration::from_secs(31)).await;

    // Let the tailer's next poll tick fire after the advance.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Collect events within a short window after the timeout.
    let mut events = Vec::new();
    loop {
        let result = tokio::time::timeout(std::time::Duration::from_millis(200), sub.recv()).await;
        match result {
            Ok(Some(e)) => events.push(e),
            _ => break,
        }
    }

    // Should have a DetailedLoggingStatus(enabled=false) from timeout.
    let dls_events: Vec<_> = events
        .iter()
        .filter(|e| matches!(e, GameEvent::DetailedLoggingStatus(_)))
        .collect();
    assert!(
        !dls_events.is_empty(),
        "expected DetailedLoggingStatus event from timeout, got events: {events:?}"
    );
    if let GameEvent::DetailedLoggingStatus(ref e) = dls_events[0] {
        assert_eq!(
            e.enabled(),
            Some(false),
            "timeout should emit enabled=false"
        );
    }

    stream.shutdown();
    Ok(())
}

#[tokio::test]
async fn test_stream_literal_detection_suppresses_timeout() -> TestResult {
    // Verifies that when the `DETAILED LOGS: ENABLED` literal is present
    // at the top of the log file (with structured headers following),
    // only ONE DetailedLoggingStatus event is emitted — from the literal
    // detection — and the 30-second timeout does NOT produce a duplicate.
    //
    // Uses tokio::time::pause() + advance() for deterministic timing.
    tokio::time::pause();

    let content = "DETAILED LOGS: ENABLED\n\
                   [UnityCrossThreadLogger]Updated account. \
                   DisplayName:TestPlayer, \
                   AccountID:abc123, \
                   Token:sometoken\n\
                   [UnityCrossThreadLogger]2/25/2026 12:00:00 PM\nfiller\n";
    let f = temp_log(content)?;

    let (stream, mut sub) = MtgaEventStream::start(f.path()).await?;

    // Let the tailer's first poll tick fire and read the file content.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Advance time well past the 30-second timeout threshold.
    tokio::time::advance(std::time::Duration::from_secs(35)).await;

    // Let the tailer's next poll tick fire after the advance.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Collect all events within a short window.
    let mut events = Vec::new();
    loop {
        let result = tokio::time::timeout(std::time::Duration::from_millis(200), sub.recv()).await;
        match result {
            Ok(Some(e)) => events.push(e),
            _ => break,
        }
    }

    // Count DetailedLoggingStatus events.
    let dls_events: Vec<_> = events
        .iter()
        .filter(|e| matches!(e, GameEvent::DetailedLoggingStatus(_)))
        .collect();

    assert_eq!(
        dls_events.len(),
        1,
        "expected exactly ONE DetailedLoggingStatus event (from literal detection, \
         timeout should be suppressed), got {}: {dls_events:?}",
        dls_events.len(),
    );

    // The single event should be enabled=true (from the literal).
    if let GameEvent::DetailedLoggingStatus(ref e) = dls_events[0] {
        assert_eq!(
            e.enabled(),
            Some(true),
            "the single event should be enabled=true from literal detection"
        );
    }

    // Should also have other expected events (Session from the account line).
    assert!(
        events.iter().any(|e| matches!(e, GameEvent::Session(_))),
        "expected Session event alongside DetailedLoggingStatus"
    );

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
