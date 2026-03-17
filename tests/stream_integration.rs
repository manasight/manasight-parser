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
