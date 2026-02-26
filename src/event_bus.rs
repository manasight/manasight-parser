//! Async broadcast channel for distributing parsed events to subscribers.
//!
//! Uses `tokio::sync::broadcast` to fan out [`GameEvent`] values from the
//! parser to multiple consumers (game state engine, game accumulator, test
//! harnesses, etc.). The parser library owns the [`EventBus`]; consumers
//! call [`EventBus::subscribe`] to obtain a [`Subscriber`] that receives
//! cloned events.
//!
//! # Slow subscribers
//!
//! `tokio::broadcast` drops the oldest messages for subscribers that fall
//! behind. When a [`Subscriber`] detects lag, it logs a warning with the
//! number of skipped messages and continues from the next available event.
//! This ensures a slow consumer never blocks the sender or other subscribers.
//!
//! # Example
//!
//! ```rust
//! use manasight_parser::event_bus::EventBus;
//!
//! let bus = EventBus::new(64);
//! let mut sub = bus.subscribe();
//!
//! assert_eq!(bus.subscriber_count(), 1);
//! ```

use crate::events::GameEvent;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Default broadcast channel capacity.
///
/// 256 is large enough to absorb short bursts of rapid events (e.g., a
/// sequence of `GameStateMessage` updates during combat) while keeping
/// memory usage modest. Each slot holds one `GameEvent` clone.
const DEFAULT_CAPACITY: usize = 256;

// ---------------------------------------------------------------------------
// EventBus
// ---------------------------------------------------------------------------

/// A broadcast event bus that fans out [`GameEvent`] values to subscribers.
///
/// Wraps a `tokio::sync::broadcast` channel. The bus owns the sender half;
/// each call to [`subscribe`](Self::subscribe) creates a new receiver that
/// independently tracks its read position.
///
/// # Capacity
///
/// The channel has a fixed capacity set at construction time (default 256).
/// When the channel is full the oldest message is overwritten, and any
/// subscriber that has not yet read it will receive a lag notification on
/// its next `recv()`.
pub struct EventBus {
    /// The broadcast sender. Cloning this is cheap (Arc internally).
    sender: tokio::sync::broadcast::Sender<GameEvent>,
}

impl EventBus {
    /// Creates a new event bus with the given channel capacity.
    ///
    /// `capacity` is the maximum number of events that can be buffered
    /// before the oldest event is overwritten. Values below 1 are clamped
    /// to 1 (the minimum `tokio::broadcast` allows).
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        let (sender, _) = tokio::sync::broadcast::channel(capacity);
        Self { sender }
    }

    /// Creates a new event bus with the default capacity (256).
    pub fn with_default_capacity() -> Self {
        Self::new(DEFAULT_CAPACITY)
    }

    /// Sends a [`GameEvent`] to all current subscribers.
    ///
    /// Returns the number of subscribers that received the event. If there
    /// are no active subscribers the event is silently dropped and `0` is
    /// returned.
    pub fn send(&self, event: GameEvent) -> usize {
        if let Ok(n) = self.sender.send(event) {
            n
        } else {
            // No active receivers — the event is dropped.
            ::log::debug!("event bus: no active subscribers, event dropped");
            0
        }
    }

    /// Creates a new [`Subscriber`] that will receive all future events.
    ///
    /// Subscribers can be added at any time. A new subscriber starts
    /// receiving events from the next `send()` call; it does not see
    /// events that were sent before it subscribed.
    pub fn subscribe(&self) -> Subscriber {
        let receiver = self.sender.subscribe();
        Subscriber { receiver }
    }

    /// Returns the current number of active subscribers (receivers).
    pub fn subscriber_count(&self) -> usize {
        self.sender.receiver_count()
    }
}

impl std::fmt::Debug for EventBus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventBus")
            .field("subscriber_count", &self.sender.receiver_count())
            .finish()
    }
}

// ---------------------------------------------------------------------------
// Subscriber
// ---------------------------------------------------------------------------

/// A subscriber that receives [`GameEvent`] values from an [`EventBus`].
///
/// Wraps a `tokio::sync::broadcast::Receiver`. When the subscriber falls
/// behind (the sender has overwritten messages it hasn't read), the next
/// call to [`recv`](Self::recv) logs a warning and skips ahead to the
/// oldest available message.
pub struct Subscriber {
    /// The broadcast receiver.
    receiver: tokio::sync::broadcast::Receiver<GameEvent>,
}

impl Subscriber {
    /// Receives the next [`GameEvent`], waiting asynchronously.
    ///
    /// If the subscriber has fallen behind, the lagged messages are
    /// skipped, a warning is logged, and the next available event is
    /// returned.
    ///
    /// Returns `None` if the sender (event bus) has been dropped and
    /// there are no more buffered messages.
    pub async fn recv(&mut self) -> Option<GameEvent> {
        loop {
            match self.receiver.recv().await {
                Ok(event) => return Some(event),
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    ::log::warn!("event bus subscriber lagged: {n} message(s) skipped");
                    // Loop continues to receive the next available event.
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    return None;
                }
            }
        }
    }
}

impl std::fmt::Debug for Subscriber {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Subscriber").finish_non_exhaustive()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{EventMetadata, GameStateEvent, SessionEvent};
    use chrono::{TimeZone, Utc};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// Helper: build an `EventMetadata` with a fixed timestamp.
    fn make_metadata(raw: &[u8]) -> EventMetadata {
        let timestamp = Utc
            .with_ymd_and_hms(2026, 2, 25, 12, 0, 0)
            .single()
            .unwrap_or_default();
        EventMetadata::new(timestamp, raw.to_vec())
    }

    /// Helper: build a `GameEvent::GameState` variant for testing.
    fn make_game_state_event(label: &str) -> GameEvent {
        let meta = make_metadata(label.as_bytes());
        let payload = serde_json::json!({"type": label});
        GameEvent::GameState(GameStateEvent::new(meta, payload))
    }

    /// Helper: build a `GameEvent::Session` variant for testing.
    fn make_session_event(label: &str) -> GameEvent {
        let meta = make_metadata(label.as_bytes());
        let payload = serde_json::json!({"action": label});
        GameEvent::Session(SessionEvent::new(meta, payload))
    }

    // -- EventBus construction -----------------------------------------------

    #[test]
    fn test_new_creates_bus_with_zero_subscribers() {
        let bus = EventBus::new(16);
        assert_eq!(bus.subscriber_count(), 0);
    }

    #[test]
    fn test_with_default_capacity_creates_bus() {
        let bus = EventBus::with_default_capacity();
        assert_eq!(bus.subscriber_count(), 0);
    }

    #[test]
    fn test_new_clamps_capacity_minimum_to_one() {
        // capacity 0 should not panic — clamped to 1.
        let bus = EventBus::new(0);
        assert_eq!(bus.subscriber_count(), 0);
    }

    // -- subscribe -----------------------------------------------------------

    #[test]
    fn test_subscribe_increments_subscriber_count() {
        let bus = EventBus::new(16);
        let _sub1 = bus.subscribe();
        assert_eq!(bus.subscriber_count(), 1);
        let _sub2 = bus.subscribe();
        assert_eq!(bus.subscriber_count(), 2);
    }

    #[test]
    fn test_subscriber_drop_decrements_count() {
        let bus = EventBus::new(16);
        let sub = bus.subscribe();
        assert_eq!(bus.subscriber_count(), 1);
        drop(sub);
        assert_eq!(bus.subscriber_count(), 0);
    }

    #[test]
    fn test_subscribe_dynamically_after_send() {
        let bus = EventBus::new(16);
        // Send with no subscribers — should not panic.
        bus.send(make_game_state_event("before-sub"));
        // Subscribe after some events were already sent.
        let _sub = bus.subscribe();
        assert_eq!(bus.subscriber_count(), 1);
    }

    // -- send ----------------------------------------------------------------

    #[test]
    fn test_send_no_subscribers_returns_zero() {
        let bus = EventBus::new(16);
        let count = bus.send(make_game_state_event("test"));
        assert_eq!(count, 0);
    }

    #[test]
    fn test_send_with_one_subscriber_returns_one() {
        let bus = EventBus::new(16);
        let _sub = bus.subscribe();
        let count = bus.send(make_game_state_event("test"));
        assert_eq!(count, 1);
    }

    #[test]
    fn test_send_with_multiple_subscribers_returns_count() {
        let bus = EventBus::new(16);
        let _sub1 = bus.subscribe();
        let _sub2 = bus.subscribe();
        let _sub3 = bus.subscribe();
        let count = bus.send(make_game_state_event("test"));
        assert_eq!(count, 3);
    }

    // -- recv (single subscriber) -------------------------------------------

    #[tokio::test]
    async fn test_recv_receives_sent_event() -> TestResult {
        let bus = EventBus::new(16);
        let mut sub = bus.subscribe();
        let sent = make_game_state_event("hello");
        bus.send(sent.clone());

        let received = sub.recv().await;
        assert_eq!(received, Some(sent));
        Ok(())
    }

    #[tokio::test]
    async fn test_recv_preserves_event_order() -> TestResult {
        let bus = EventBus::new(16);
        let mut sub = bus.subscribe();

        let events: Vec<GameEvent> = (0..5)
            .map(|i| make_game_state_event(&format!("event-{i}")))
            .collect();
        for event in &events {
            bus.send(event.clone());
        }

        for expected in &events {
            let received = sub.recv().await;
            assert_eq!(received.as_ref(), Some(expected));
        }
        Ok(())
    }

    #[tokio::test]
    async fn test_recv_returns_none_when_bus_dropped() -> TestResult {
        let bus = EventBus::new(16);
        let mut sub = bus.subscribe();

        // Drop the bus (sender).
        drop(bus);

        let received = sub.recv().await;
        assert_eq!(received, None);
        Ok(())
    }

    // -- fan-out to multiple subscribers ------------------------------------

    #[tokio::test]
    async fn test_fan_out_all_subscribers_receive_same_event() -> TestResult {
        let bus = EventBus::new(16);
        let mut sub1 = bus.subscribe();
        let mut sub2 = bus.subscribe();
        let mut sub3 = bus.subscribe();

        let event = make_game_state_event("fan-out");
        bus.send(event.clone());

        assert_eq!(sub1.recv().await, Some(event.clone()));
        assert_eq!(sub2.recv().await, Some(event.clone()));
        assert_eq!(sub3.recv().await, Some(event));
        Ok(())
    }

    #[tokio::test]
    async fn test_fan_out_multiple_events_to_multiple_subscribers() -> TestResult {
        let bus = EventBus::new(16);
        let mut sub1 = bus.subscribe();
        let mut sub2 = bus.subscribe();

        let event_a = make_game_state_event("alpha");
        let event_b = make_session_event("beta");
        bus.send(event_a.clone());
        bus.send(event_b.clone());

        // Both subscribers should receive both events in order.
        assert_eq!(sub1.recv().await, Some(event_a.clone()));
        assert_eq!(sub1.recv().await, Some(event_b.clone()));

        assert_eq!(sub2.recv().await, Some(event_a));
        assert_eq!(sub2.recv().await, Some(event_b));
        Ok(())
    }

    #[tokio::test]
    async fn test_fan_out_different_event_types() -> TestResult {
        let bus = EventBus::new(16);
        let mut sub = bus.subscribe();

        let gs_event = make_game_state_event("game-state");
        let sess_event = make_session_event("session");

        bus.send(gs_event.clone());
        bus.send(sess_event.clone());

        let r1 = sub.recv().await;
        let r2 = sub.recv().await;
        assert_eq!(r1, Some(gs_event));
        assert_eq!(r2, Some(sess_event));
        Ok(())
    }

    // -- slow subscriber (lag) -----------------------------------------------

    #[tokio::test]
    async fn test_slow_subscriber_skips_lagged_messages() -> TestResult {
        // Capacity of 4: after sending 6 events, the first 2 are overwritten.
        let bus = EventBus::new(4);
        let mut sub = bus.subscribe();

        // Send more events than the channel can hold.
        for i in 0..6 {
            bus.send(make_game_state_event(&format!("event-{i}")));
        }

        // The subscriber should still receive events (possibly fewer than 6
        // due to lag) without blocking or panicking.
        let mut received = Vec::new();
        for _ in 0..4 {
            if let Some(event) = sub.recv().await {
                received.push(event);
            }
        }

        // Should have received some events (the non-overwritten ones).
        assert!(
            !received.is_empty(),
            "subscriber should receive at least one event after lag"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_slow_subscriber_does_not_block_sender() -> TestResult {
        let bus = EventBus::new(2);
        let _sub = bus.subscribe(); // Never reads.

        // Sending more than capacity should not block or panic.
        for i in 0..10 {
            bus.send(make_game_state_event(&format!("event-{i}")));
        }

        // If we got here, the sender was not blocked.
        Ok(())
    }

    // -- dynamic subscription ------------------------------------------------

    #[tokio::test]
    async fn test_late_subscriber_only_sees_future_events() -> TestResult {
        let bus = EventBus::new(16);

        // Send events before subscribing.
        bus.send(make_game_state_event("before"));

        // Subscribe after the first event.
        let mut sub = bus.subscribe();

        // Send another event.
        let after = make_game_state_event("after");
        bus.send(after.clone());

        // The subscriber should only see "after".
        let received = sub.recv().await;
        assert_eq!(received, Some(after));
        Ok(())
    }

    #[tokio::test]
    async fn test_multiple_dynamic_subscribers_at_different_times() -> TestResult {
        let bus = EventBus::new(16);

        let mut sub1 = bus.subscribe();

        let event1 = make_game_state_event("first");
        bus.send(event1.clone());

        let mut sub2 = bus.subscribe();

        let event2 = make_session_event("second");
        bus.send(event2.clone());

        // sub1 should see both events.
        assert_eq!(sub1.recv().await, Some(event1));
        assert_eq!(sub1.recv().await, Some(event2.clone()));

        // sub2 should only see the second event.
        assert_eq!(sub2.recv().await, Some(event2));
        Ok(())
    }

    // -- Debug ---------------------------------------------------------------

    #[test]
    fn test_event_bus_debug_format() {
        let bus = EventBus::new(16);
        let _sub = bus.subscribe();
        let debug = format!("{bus:?}");
        assert!(debug.contains("EventBus"));
        assert!(debug.contains("subscriber_count"));
    }

    #[test]
    fn test_subscriber_debug_format() {
        let bus = EventBus::new(16);
        let sub = bus.subscribe();
        let debug = format!("{sub:?}");
        assert!(debug.contains("Subscriber"));
    }

    // -- edge cases ----------------------------------------------------------

    #[tokio::test]
    async fn test_recv_waits_for_event() -> TestResult {
        let bus = EventBus::new(16);
        let mut sub = bus.subscribe();

        // Spawn a task that sends an event after a short delay.
        let bus_clone_sender = bus.sender.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            let _ = bus_clone_sender.send(make_game_state_event("delayed"));
        });

        let received = tokio::time::timeout(std::time::Duration::from_secs(2), sub.recv()).await?;
        assert!(received.is_some());
        Ok(())
    }

    #[test]
    fn test_send_returns_zero_after_all_subscribers_dropped() {
        let bus = EventBus::new(16);
        let sub = bus.subscribe();
        drop(sub);
        let count = bus.send(make_game_state_event("test"));
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn test_subscriber_receives_after_other_subscriber_dropped() -> TestResult {
        let bus = EventBus::new(16);
        let sub1 = bus.subscribe();
        let mut sub2 = bus.subscribe();

        // Drop sub1; sub2 should still work.
        drop(sub1);

        let event = make_game_state_event("after-drop");
        bus.send(event.clone());

        assert_eq!(sub2.recv().await, Some(event));
        Ok(())
    }
}
