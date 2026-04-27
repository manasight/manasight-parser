//! Public event type enums and structs for parsed MTG Arena log events.
//!
//! These types represent the structured output of the parser and form the
//! contract between the parser library and its consumers. Each event
//! corresponds to a category in the
//! [Event-to-Class Mapping][spec].
//!
//! [spec]: https://github.com/manasight/manasight-docs/blob/main/docs/requirements/feature-specs/log-file-parser.md#event-to-class-mapping

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

// ---------------------------------------------------------------------------
// Serde helper modules
// ---------------------------------------------------------------------------

/// Serialize `Vec<u8>` as a base64 string (RFC 4648 standard alphabet).
mod base64_serde {
    use base64::prelude::{Engine as _, BASE64_STANDARD};
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&BASE64_STANDARD.encode(bytes))
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        BASE64_STANDARD.decode(&s).map_err(serde::de::Error::custom)
    }
}

/// Serialize `[u8; 32]` as a 64-character lowercase hex string.
///
/// Serialize-only: `EventMetadata` has a custom `Deserialize` impl that
/// ignores `raw_bytes_hash` (it is always recomputed from `raw_bytes`), so
/// no `deserialize` function is needed. If `#[derive(Deserialize)]` is
/// ever added to `EventMetadata`, add a `deserialize` function here.
mod hex_serde {
    use serde::Serializer;
    use std::fmt::Write as _;

    pub fn serialize<S>(bytes: &[u8; 32], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let hex = bytes.iter().fold(String::with_capacity(64), |mut acc, b| {
            // write! to String is infallible.
            let _ = write!(acc, "{b:02x}");
            acc
        });
        serializer.serialize_str(&hex)
    }
}

// ---------------------------------------------------------------------------
// Macros
// ---------------------------------------------------------------------------

/// Generates a category-specific event struct with `metadata` and `payload`
/// fields plus public accessor methods.
///
/// When a new event category is added, create a new invocation of this
/// macro rather than writing the struct and impl by hand.
macro_rules! define_event {
    (
        $(#[$attr:meta])*
        $name:ident
    ) => {
        $(#[$attr])*
        #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
        pub struct $name {
            /// Shared event metadata (timestamp, raw bytes, hash).
            metadata: EventMetadata,
            /// The parsed JSON payload.
            payload: serde_json::Value,
        }

        impl $name {
            /// Constructs a new event with the given metadata and payload.
            pub fn new(
                metadata: EventMetadata,
                payload: serde_json::Value,
            ) -> Self {
                Self { metadata, payload }
            }

            /// Returns the shared event metadata.
            pub fn metadata(&self) -> &EventMetadata {
                &self.metadata
            }

            /// Returns the parsed JSON payload.
            pub fn payload(&self) -> &serde_json::Value {
                &self.payload
            }
        }
    };
}

/// Dispatches a field accessor across all `GameEvent` variants.
///
/// When a new variant is added to `GameEvent`, add it here too.
/// `$method` must be a `&self` no-arg method present on all inner types.
macro_rules! delegate_to_inner {
    ($self:expr, $method:ident) => {
        match $self {
            Self::GameState(e) => e.$method(),
            Self::ClientAction(e) => e.$method(),
            Self::MatchState(e) => e.$method(),
            Self::DraftBot(e) => e.$method(),
            Self::DraftHuman(e) => e.$method(),
            Self::DraftComplete(e) => e.$method(),
            Self::EventLifecycle(e) => e.$method(),
            Self::Session(e) => e.$method(),
            Self::Rank(e) => e.$method(),
            Self::Inventory(e) => e.$method(),
            Self::GameResult(e) => e.$method(),
            Self::LogFileRotated(e) => e.$method(),
            Self::DetailedLoggingStatus(e) => e.$method(),
            Self::MatchConnectionState(e) => e.$method(),
            Self::TcpConnectionClose(e) => e.$method(),
            Self::WebSocketClosed(e) => e.$method(),
            Self::ConnectionError(e) => e.$method(),
        }
    };
}

// ---------------------------------------------------------------------------
// GameEvent enum
// ---------------------------------------------------------------------------

/// A parsed MTG Arena log event.
///
/// Each variant wraps a category-specific struct containing parsed fields,
/// the original raw log bytes, and a precomputed payload hash. Consumers
/// subscribe to the event bus and pattern-match on this enum.
///
/// Marked `#[non_exhaustive]` so that new event categories can be added
/// in future releases without a breaking change for downstream consumers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum GameEvent {
    /// GRE-to-client messages: `GameStateMessage`, `ConnectResp`,
    /// `QueuedGameStateMessage`. Class 1 — interactive dispatch.
    GameState(GameStateEvent),

    /// Client-to-GRE messages: `SelectNResp`, `SubmitDeckResp`,
    /// `MulliganResp`. Class 1 — interactive dispatch.
    ClientAction(ClientActionEvent),

    /// Match room state changes (`matchGameRoomStateChangedEvent`).
    /// Class 1 — interactive dispatch.
    MatchState(MatchStateEvent),

    /// Bot draft events (`<== BotDraftDraftStatus`, `<== BotDraftDraftPick`,
    /// `==> BotDraftDraftPick`).
    /// Class 2 — durable per-event.
    DraftBot(DraftBotEvent),

    /// Human draft picks (`Draft.Notify`, `EventPlayerDraftMakePick`).
    /// Class 2 — durable per-event.
    DraftHuman(DraftHumanEvent),

    /// Draft completion (`Draft_CompleteDraft`).
    /// Class 2 — durable per-event.
    DraftComplete(DraftCompleteEvent),

    /// Event lifecycle: `==> EventJoin`, `==> EventClaimPrize`,
    /// `==> EventEnterPairing`. Class 2 — durable per-event.
    EventLifecycle(EventLifecycleEvent),

    /// Session: login, account identity, logout.
    /// Class 2 — durable per-event.
    Session(SessionEvent),

    /// Rank snapshot (`<== RankGetCombinedRankInfo`).
    /// Class 2 — durable per-event.
    Rank(RankEvent),

    /// Inventory snapshot (`<== StartHook` with `InventoryInfo`):
    /// currency, wildcards, etc. Class 2 — durable per-event.
    Inventory(InventoryEvent),

    /// Game result (`GameStage_GameOver` from GRE `GameStateMessage`).
    /// Class 3 — triggers post-game batch assembly.
    GameResult(GameResultEvent),

    /// Log file rotation detected — `Player.log` was replaced (MTGA restart).
    ///
    /// Emitted by the file tailer when it detects that the log file at the
    /// monitored path has been replaced (file size shrinkage or mtime jump).
    /// Downstream consumers should reset their state for a new session.
    /// Class 1 — interactive dispatch (local reset signal).
    LogFileRotated(LogFileRotatedEvent),

    /// Detailed logging status change detected.
    ///
    /// Emitted by the file tailer when it determines whether Arena's
    /// "Detailed Logs (Plugin Support)" setting is enabled. `enabled: false`
    /// is emitted after 30 seconds of observed log writes without any
    /// `[UnityCrossThreadLogger]` or `[Client GRE]` headers. `enabled: true`
    /// is emitted if structured headers are later detected (user enabled the
    /// setting and restarted Arena).
    /// Class 1 — interactive dispatch (local status signal).
    DetailedLoggingStatus(DetailedLoggingStatusEvent),

    /// Match connection state machine transition (`STATE CHANGED`).
    ///
    /// Parsed from `[UnityCrossThreadLogger]STATE CHANGED {"old":"...","new":"..."}`
    /// entries. Payload is `{"old": "<state>", "new": "<state>"}`. Drives the
    /// connection health indicator (AC-DET-1) — the definitive signal for
    /// local-client disconnect detection.
    /// Class 1 — interactive dispatch.
    MatchConnectionState(MatchConnectionStateEvent),

    /// TCP connection close event (`Client.TcpConnection.Close`).
    ///
    /// Parsed from `[UnityCrossThreadLogger]Client.TcpConnection.Close {...}`
    /// entries. The payload is the full parsed JSON from the log line,
    /// preserving `status`, `reason`, and abnormal-close-only fields
    /// (`function`, `description`, `exception`). Feeds the desktop
    /// connection health monitor (AC-DET-2); the parser is agnostic to
    /// `status` semantics (per ADR-011).
    /// Class 1 — interactive dispatch.
    TcpConnectionClose(TcpConnectionCloseEvent),

    /// WebSocket close event (`GREConnection.HandleWebSocketClosed`).
    ///
    /// Parsed from
    /// `[UnityCrossThreadLogger]GREConnection.HandleWebSocketClosed {...}`
    /// entries. The payload is the full parsed JSON from the log line,
    /// which always includes `closeType`, `reason`, and a nested `tcpConn`
    /// object snapshot of the paired TCP connection. Feeds the desktop
    /// connection health monitor (AC-DET-3); the parser is agnostic to
    /// `closeType` semantics (per ADR-011).
    /// Class 1 — interactive dispatch.
    WebSocketClosed(WebSocketClosedEvent),

    /// Connection error event (error-path markers).
    ///
    /// Parsed from four JSON-bearing markers under `[UnityCrossThreadLogger]`:
    /// `TcpConnection.ProcessRead.Exception`,
    /// `Client.TcpConnection.ProcessFailure`,
    /// `GREConnection.MatchDoorConnectionError`, and
    /// `TcpConnection.Close.Exception`. Each variant is discriminated by a
    /// stable `error_type` string and wraps the full parsed JSON under a
    /// `payload` key. Feeds the desktop connection health monitor (AC-DET-5);
    /// the parser is agnostic to inner error-code semantics (per ADR-011).
    /// Class 1 — interactive dispatch.
    ConnectionError(ConnectionErrorEvent),
}

impl GameEvent {
    /// Returns the performance class for this event.
    ///
    /// - Class 1: interactive dispatch (local, ≤ 100 ms)
    /// - Class 2: durable per-event upload
    /// - Class 3: post-game batch upload trigger
    pub fn performance_class(&self) -> PerformanceClass {
        match self {
            Self::GameState(_)
            | Self::ClientAction(_)
            | Self::MatchState(_)
            | Self::LogFileRotated(_)
            | Self::DetailedLoggingStatus(_)
            | Self::MatchConnectionState(_)
            | Self::TcpConnectionClose(_)
            | Self::WebSocketClosed(_)
            | Self::ConnectionError(_) => PerformanceClass::InteractiveDispatch,
            Self::DraftBot(_)
            | Self::DraftHuman(_)
            | Self::DraftComplete(_)
            | Self::EventLifecycle(_)
            | Self::Session(_)
            | Self::Rank(_)
            | Self::Inventory(_) => PerformanceClass::DurablePerEvent,
            Self::GameResult(_) => PerformanceClass::PostGameBatch,
        }
    }

    /// Returns the shared metadata common to all events.
    pub fn metadata(&self) -> &EventMetadata {
        delegate_to_inner!(self, metadata)
    }

    /// Returns the parsed JSON payload of the event.
    pub fn payload(&self) -> &serde_json::Value {
        delegate_to_inner!(self, payload)
    }
}

// ---------------------------------------------------------------------------
// PerformanceClass
// ---------------------------------------------------------------------------

/// Performance class determining latency target and delivery path.
///
/// See the [feature spec performance classes][spec] for details.
///
/// [spec]: https://github.com/manasight/manasight-docs/blob/main/docs/requirements/feature-specs/log-file-parser.md#performance-classes
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub enum PerformanceClass {
    /// Class 1: local-only, ≤ 100 ms latency. Also accumulated for Class 3.
    InteractiveDispatch,
    /// Class 2: persisted to disk queue, uploaded individually, ≤ 1 s.
    DurablePerEvent,
    /// Class 3: triggers assembly and upload of the complete game batch.
    PostGameBatch,
}

impl PerformanceClass {
    /// Returns the numeric class identifier (1, 2, or 3).
    ///
    /// Useful for logging, metrics, and wire-format tagging where a compact
    /// integer representation is preferred over the enum variant name.
    pub fn as_class_number(&self) -> u8 {
        match self {
            Self::InteractiveDispatch => 1,
            Self::DurablePerEvent => 2,
            Self::PostGameBatch => 3,
        }
    }

    /// Returns `true` if events in this class must be persisted to durable
    /// storage (disk queue or disk-backed buffer) before being considered
    /// processed.
    ///
    /// Class 2 events are individually persisted to a disk queue for
    /// per-event upload. Class 3 events trigger batch assembly from a
    /// disk-backed game buffer. Class 1 events are local-only and do not
    /// require durable storage (though they are also accumulated into the
    /// Class 3 buffer asynchronously).
    pub fn requires_durable_storage(&self) -> bool {
        match self {
            Self::InteractiveDispatch => false,
            Self::DurablePerEvent | Self::PostGameBatch => true,
        }
    }

    /// Returns `true` if this class triggers post-game batch assembly.
    ///
    /// Only Class 3 (`PostGameBatch`) triggers the assembly and upload of
    /// the accumulated game buffer. Downstream consumers use this to know
    /// when to finalize and ship the game record.
    pub fn is_batch_trigger(&self) -> bool {
        matches!(self, Self::PostGameBatch)
    }
}

// ---------------------------------------------------------------------------
// EventMetadata
// ---------------------------------------------------------------------------

/// Fields shared by every event: timestamp, raw bytes, and raw-bytes hash.
///
/// Constructed via [`EventMetadata::new`], which computes the `raw_bytes_hash`
/// from `raw_bytes` to enforce the invariant that the hash always matches.
/// This is critical for server-side deduplication via event fingerprints.
///
/// The `timestamp` is `Option<DateTime<Utc>>` because some log entries lack
/// a parseable timestamp in the header. `None` means "no timestamp found in
/// the log entry" — downstream consumers must handle this explicitly rather
/// than receiving a synthetic `Utc::now()` that would break fingerprinting
/// and chronological ordering.
///
/// All fields are private to protect the hash invariant. Use the accessor
/// methods to read them.
///
/// Deserialization also enforces this invariant: the hash is recomputed from
/// `raw_bytes` during deserialization rather than trusting the serialized value.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EventMetadata {
    /// UTC timestamp parsed from the log entry header, or `None` if the
    /// entry did not contain a parseable timestamp.
    timestamp: Option<DateTime<Utc>>,

    /// Original log entry bytes, serialized as base64. Private to prevent
    /// mutation that would break the `raw_bytes_hash` invariant.
    #[serde(with = "base64_serde")]
    raw_bytes: Vec<u8>,

    /// SHA-256 hash of `raw_bytes`, serialized as lowercase hex.
    /// Precomputed at construction time. Used as part of the event
    /// fingerprint for server-side deduplication.
    #[serde(with = "hex_serde")]
    raw_bytes_hash: [u8; 32],
}

impl EventMetadata {
    /// Creates a new `EventMetadata`, computing `raw_bytes_hash` as the
    /// SHA-256 digest of `raw_bytes`.
    ///
    /// `timestamp` is `None` when the log entry header did not contain a
    /// parseable timestamp. This preserves the distinction between "real
    /// timestamp from the log" and "no timestamp available" for downstream
    /// consumers.
    pub fn new(timestamp: Option<DateTime<Utc>>, raw_bytes: Vec<u8>) -> Self {
        let raw_bytes_hash: [u8; 32] = Sha256::digest(&raw_bytes).into();
        Self {
            timestamp,
            raw_bytes,
            raw_bytes_hash,
        }
    }

    /// Returns the UTC timestamp parsed from the log entry header, or
    /// `None` if the entry did not contain a parseable timestamp.
    pub fn timestamp(&self) -> Option<DateTime<Utc>> {
        self.timestamp
    }

    /// Returns the original log entry bytes.
    pub fn raw_bytes(&self) -> &[u8] {
        &self.raw_bytes
    }

    /// Returns the SHA-256 hash of `raw_bytes`.
    pub fn raw_bytes_hash(&self) -> &[u8; 32] {
        &self.raw_bytes_hash
    }
}

/// Custom `Deserialize` that recomputes `raw_bytes_hash` from `raw_bytes`,
/// ensuring the hash invariant survives serialization round-trips.
impl<'de> Deserialize<'de> for EventMetadata {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        /// Wire format for deserializing `EventMetadata`. The
        /// `raw_bytes_hash` field is optional and discarded — the real
        /// hash is always recomputed from `raw_bytes`.
        #[derive(Deserialize)]
        struct EventMetadataWire {
            timestamp: Option<DateTime<Utc>>,
            #[serde(with = "base64_serde")]
            raw_bytes: Vec<u8>,
            // Accepts any format (hex string, integer array) or absence.
            // The value is discarded — hash is always recomputed.
            #[serde(default, rename = "raw_bytes_hash")]
            _raw_bytes_hash: serde::de::IgnoredAny,
        }

        let wire = EventMetadataWire::deserialize(deserializer)?;
        Ok(EventMetadata::new(wire.timestamp, wire.raw_bytes))
    }
}

// ---------------------------------------------------------------------------
// Class 1: Interactive Dispatch
// ---------------------------------------------------------------------------

define_event! {
    /// GRE-to-client game state messages.
    ///
    /// Covers `GameStateMessage`, `ConnectResp`, and `QueuedGameStateMessage`
    /// payloads from `greToClientEvent` entries.
    GameStateEvent
}

define_event! {
    /// Client-to-GRE player actions.
    ///
    /// Covers `SelectNResp`, `SubmitDeckResp`, `MulliganResp`, and other
    /// `ClientToGREMessage` payloads.
    ClientActionEvent
}

define_event! {
    /// Match room state transitions.
    ///
    /// Parsed from `matchGameRoomStateChangedEvent` entries. Signals match
    /// start/end and triggers overlay state transitions.
    MatchStateEvent
}

// ---------------------------------------------------------------------------
// Class 2: Durable Per-Event
// ---------------------------------------------------------------------------

define_event! {
    /// Bot draft events.
    ///
    /// Parsed from `BotDraftDraftStatus` and `BotDraftDraftPick` request and
    /// response entries. Each pick is independently valuable and must survive
    /// crashes.
    DraftBotEvent
}

define_event! {
    /// Human draft pick events.
    ///
    /// Parsed from `Draft.Notify`, `EventPlayerDraftMakePick`, and
    /// `LogBusinessEvents` entries containing `PickGrpId`.
    DraftHumanEvent
}

define_event! {
    /// Draft completion event.
    ///
    /// Parsed from `Draft_CompleteDraft`. Links the draft ID to the event
    /// and marks the draft as finished.
    DraftCompleteEvent
}

define_event! {
    /// Event lifecycle transitions.
    ///
    /// Covers `==> EventJoin`, `==> EventClaimPrize`, and
    /// `==> EventEnterPairing`. Each is independently meaningful.
    EventLifecycleEvent
}

define_event! {
    /// Session identity and connection events.
    ///
    /// Covers `Updated account. DisplayName:`, `authenticateResponse`,
    /// and `FrontDoorConnection.Close`. Needed to tag all subsequent events
    /// with player identity.
    SessionEvent
}

define_event! {
    /// Rank snapshot.
    ///
    /// Parsed from `<== RankGetCombinedRankInfo`. Infrequent, small,
    /// independently useful.
    RankEvent
}

define_event! {
    /// Inventory snapshot.
    ///
    /// Parsed from `<== StartHook` responses containing `InventoryInfo`.
    /// Contains currency, wildcards, boosters, and vault progress.
    InventoryEvent
}

// ---------------------------------------------------------------------------
// Class 3: Post-Game Batch
// ---------------------------------------------------------------------------

define_event! {
    /// Game result event — triggers post-game batch assembly.
    ///
    /// Parsed from `LogBusinessEvents` with `WinningType` and
    /// `GameStage_GameOver`. When this event fires, the desktop app
    /// serializes the disk-backed game buffer into a single compressed
    /// payload and uploads it.
    GameResultEvent
}

// ---------------------------------------------------------------------------
// Infrastructure events
// ---------------------------------------------------------------------------

define_event! {
    /// Log file rotation event.
    ///
    /// Emitted when the file tailer detects that `Player.log` was replaced
    /// (MTGA restart). The payload contains `previous_file_size` — the byte
    /// offset in the old file at the time rotation was detected.
    ///
    /// Unlike parsed log events, `raw_bytes` in the metadata is empty and
    /// the timestamp reflects when the rotation was detected (wall-clock),
    /// not a timestamp parsed from the log.
    LogFileRotatedEvent
}

impl LogFileRotatedEvent {
    /// Creates a rotation event with the given detection timestamp and the
    /// byte offset in the old file.
    pub fn for_rotation(timestamp: DateTime<Utc>, previous_file_size: u64) -> Self {
        let metadata = EventMetadata::new(Some(timestamp), Vec::new());
        let payload = serde_json::json!({ "previous_file_size": previous_file_size });
        Self::new(metadata, payload)
    }

    /// Returns the byte offset in the old file when rotation was detected.
    ///
    /// Returns `None` only if the payload was manually constructed without
    /// the `previous_file_size` field (not expected in normal usage).
    pub fn previous_file_size(&self) -> Option<u64> {
        self.payload()["previous_file_size"].as_u64()
    }
}

define_event! {
    /// Detailed logging status event.
    ///
    /// Emitted when the file tailer detects whether Arena's "Detailed Logs
    /// (Plugin Support)" setting is enabled. The payload contains `enabled`
    /// — `false` after 30 seconds of log writes without structured headers,
    /// `true` when structured headers are subsequently detected.
    ///
    /// Like `LogFileRotatedEvent`, `raw_bytes` in the metadata is empty and
    /// the timestamp reflects wall-clock detection time.
    DetailedLoggingStatusEvent
}

impl DetailedLoggingStatusEvent {
    /// Creates a detailed logging status event.
    pub fn new_status(timestamp: DateTime<Utc>, enabled: bool) -> Self {
        let metadata = EventMetadata::new(Some(timestamp), Vec::new());
        let payload = serde_json::json!({ "enabled": enabled });
        Self::new(metadata, payload)
    }

    /// Returns whether detailed logging is enabled.
    ///
    /// Returns `None` only if the payload was manually constructed without
    /// the `enabled` field (not expected in normal usage).
    pub fn enabled(&self) -> Option<bool> {
        self.payload()["enabled"].as_bool()
    }
}

define_event! {
    /// Match connection state machine transition event.
    ///
    /// Parsed from `[UnityCrossThreadLogger]STATE CHANGED {...}` entries.
    /// The payload is the JSON object `{"old": "<state>", "new": "<state>"}`
    /// where each state is one of the values observed in the MTGA match
    /// connection state machine (e.g., `None`, `ConnectedToMatchDoor`,
    /// `ConnectedToMatchDoor_ConnectingToGRE`,
    /// `ConnectedToMatchDoor_ConnectedToGRE_Waiting`, `Playing`,
    /// `MatchCompleted`, `Disconnected`).
    ///
    /// Feeds the desktop connection health monitor; see feature spec
    /// `connection-health-indicator.md` **AC-DET-1**.
    MatchConnectionStateEvent
}

define_event! {
    /// TCP connection close event.
    ///
    /// Parsed from `[UnityCrossThreadLogger]Client.TcpConnection.Close {...}`
    /// entries. The payload is the full parsed JSON from the log line and
    /// carries at minimum `status` and `reason`; abnormal closes also
    /// include `function`, `description`, and a nested `exception` tree
    /// (with `InnerException.NativeErrorCode` on Windows/macOS).
    ///
    /// The parser is agnostic to `status` semantics — downstream consumers
    /// classify close types per ADR-011. Bare-marker entries (no JSON
    /// payload) do not produce this event.
    ///
    /// Feeds the desktop connection health monitor; see feature spec
    /// `connection-health-indicator.md` **AC-DET-2**.
    TcpConnectionCloseEvent
}

define_event! {
    /// WebSocket close event.
    ///
    /// Parsed from
    /// `[UnityCrossThreadLogger]GREConnection.HandleWebSocketClosed {...}`
    /// entries. The payload is the full parsed JSON from the log line and
    /// always includes `closeType`, `reason`, and a nested `tcpConn`
    /// object snapshot of the paired TCP connection (host/port/timing/ping
    /// stats).
    ///
    /// The parser is agnostic to `closeType` semantics — downstream
    /// consumers classify close types per ADR-011.
    ///
    /// Feeds the desktop connection health monitor; see feature spec
    /// `connection-health-indicator.md` **AC-DET-3**.
    WebSocketClosedEvent
}

define_event! {
    /// Connection error event (error-path markers).
    ///
    /// Parsed from four JSON-bearing error markers under
    /// `[UnityCrossThreadLogger]`:
    ///
    /// | Marker | `error_type` |
    /// |--------|--------------|
    /// | `TcpConnection.ProcessRead.Exception` | `tcp_process_read_exception` |
    /// | `Client.TcpConnection.ProcessFailure` | `tcp_process_failure_socket_error` |
    /// | `GREConnection.MatchDoorConnectionError` | `gre_match_door_connection_error` |
    /// | `TcpConnection.Close.Exception` | `tcp_close_exception` |
    ///
    /// The payload shape is
    /// `{"error_type": "<discriminant>", "payload": <parsed>}`, where
    /// `<parsed>` is the full parsed JSON from the log line preserved
    /// unchanged. Bare-marker entries (no JSON payload) do not produce this
    /// event; the paired JSON line on a subsequent entry emits it.
    ///
    /// The parser is agnostic to inner error-code semantics — downstream
    /// consumers match on `error_type` per ADR-011.
    ///
    /// Feeds the desktop connection health monitor; see feature spec
    /// `connection-health-indicator.md` **AC-DET-5**.
    ConnectionErrorEvent
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::prelude::{Engine as _, BASE64_STANDARD};
    use chrono::{Datelike, TimeZone};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// Helper: build an `EventMetadata` with a fixed timestamp and the
    /// given raw bytes.
    ///
    /// UTC datetimes are never ambiguous so `single()` always returns
    /// `Some`. Uses `unwrap_or_default()` because `clippy::expect_used`
    /// is denied in `Cargo.toml [lints.clippy]` — verified: this applies
    /// crate-wide including `#[cfg(test)]` code under `--all-targets`.
    /// The epoch fallback (1970-01-01) would visibly fail any timestamp
    /// assertion rather than passing silently.
    fn make_metadata(raw: &[u8]) -> EventMetadata {
        let timestamp = Utc
            .with_ymd_and_hms(2026, 2, 25, 12, 0, 0)
            .single()
            .unwrap_or_default();
        EventMetadata::new(Some(timestamp), raw.to_vec())
    }

    /// Helper: build all `GameEvent` variants for exhaustive testing.
    ///
    /// Must stay in sync with `GameEvent` variants. Compile-time
    /// exhaustiveness is enforced by `performance_class()` and
    /// `delegate_to_inner!`; this array is the test-only counterpart.
    fn all_variants() -> Vec<GameEvent> {
        let meta = make_metadata(b"test");
        let payload = serde_json::json!({});
        vec![
            GameEvent::GameState(GameStateEvent::new(meta.clone(), payload.clone())),
            GameEvent::ClientAction(ClientActionEvent::new(meta.clone(), payload.clone())),
            GameEvent::MatchState(MatchStateEvent::new(meta.clone(), payload.clone())),
            GameEvent::DraftBot(DraftBotEvent::new(meta.clone(), payload.clone())),
            GameEvent::DraftHuman(DraftHumanEvent::new(meta.clone(), payload.clone())),
            GameEvent::DraftComplete(DraftCompleteEvent::new(meta.clone(), payload.clone())),
            GameEvent::EventLifecycle(EventLifecycleEvent::new(meta.clone(), payload.clone())),
            GameEvent::Session(SessionEvent::new(meta.clone(), payload.clone())),
            GameEvent::Rank(RankEvent::new(meta.clone(), payload.clone())),
            GameEvent::Inventory(InventoryEvent::new(meta.clone(), payload.clone())),
            GameEvent::GameResult(GameResultEvent::new(meta.clone(), payload.clone())),
            GameEvent::LogFileRotated(LogFileRotatedEvent::new(meta.clone(), payload.clone())),
            GameEvent::DetailedLoggingStatus(DetailedLoggingStatusEvent::new(
                meta.clone(),
                payload.clone(),
            )),
            GameEvent::MatchConnectionState(MatchConnectionStateEvent::new(
                meta.clone(),
                payload.clone(),
            )),
            GameEvent::TcpConnectionClose(TcpConnectionCloseEvent::new(
                meta.clone(),
                payload.clone(),
            )),
            GameEvent::WebSocketClosed(WebSocketClosedEvent::new(meta.clone(), payload.clone())),
            GameEvent::ConnectionError(ConnectionErrorEvent::new(meta.clone(), payload.clone())),
        ]
    }

    // -- EventMetadata construction --

    #[test]
    fn test_event_metadata_new_stores_raw_bytes() {
        let raw = b"[UnityCrossThreadLogger]some log line";
        let meta = make_metadata(raw);
        assert_eq!(meta.raw_bytes(), raw);
    }

    #[test]
    fn test_event_metadata_new_computes_raw_bytes_hash() {
        let raw = b"test payload";
        let meta = make_metadata(raw);
        let expected: [u8; 32] = Sha256::digest(raw).into();
        assert_eq!(*meta.raw_bytes_hash(), expected);
    }

    #[test]
    fn test_event_metadata_new_stores_timestamp() {
        let meta = make_metadata(b"data");
        let ts = meta.timestamp();
        assert!(ts.is_some());
        let ts = ts.unwrap_or_default();
        assert_eq!(ts.year(), 2026);
        assert_eq!(ts.month(), 2);
    }

    #[test]
    fn test_event_metadata_new_enforces_hash_invariant() {
        let raw = b"important data";
        let meta = make_metadata(raw);
        let expected: [u8; 32] = Sha256::digest(raw).into();
        assert_eq!(
            *meta.raw_bytes_hash(),
            expected,
            "raw_bytes_hash must always be SHA-256 of raw_bytes"
        );
    }

    // -- EventMetadata properties --

    #[test]
    fn test_different_raw_bytes_produce_different_hashes() {
        let meta1 = make_metadata(b"payload one");
        let meta2 = make_metadata(b"payload two");
        assert_ne!(meta1.raw_bytes_hash(), meta2.raw_bytes_hash());
    }

    #[test]
    fn test_identical_raw_bytes_produce_same_hash() {
        let meta1 = make_metadata(b"same payload");
        let meta2 = make_metadata(b"same payload");
        assert_eq!(meta1.raw_bytes_hash(), meta2.raw_bytes_hash());
    }

    #[test]
    fn test_empty_raw_bytes_valid() {
        let meta = make_metadata(b"");
        assert!(meta.raw_bytes().is_empty());
        let expected: [u8; 32] = Sha256::digest(b"").into();
        assert_eq!(*meta.raw_bytes_hash(), expected);
    }

    #[test]
    fn test_event_metadata_clone_is_equal() {
        let meta = make_metadata(b"original");
        let cloned = meta.clone();
        assert_eq!(meta, cloned);
    }

    #[test]
    fn test_event_metadata_timestamp_getter() {
        let meta = make_metadata(b"data");
        let ts = meta.timestamp();
        assert!(ts.is_some());
        let ts = ts.unwrap_or_default();
        assert_eq!(ts.year(), 2026);
        assert_eq!(ts.month(), 2);
        assert_eq!(ts.day(), 25);
    }

    #[test]
    fn test_event_metadata_none_timestamp() {
        let meta = EventMetadata::new(None, b"data".to_vec());
        assert!(meta.timestamp().is_none());
    }

    // -- Per-category struct field access (via accessors) --

    #[test]
    fn test_game_state_event_field_access() {
        let event = GameStateEvent::new(
            make_metadata(b"gre payload"),
            serde_json::json!({"type": "GameStateMessage"}),
        );
        assert_eq!(event.payload()["type"], "GameStateMessage");
        assert_eq!(event.metadata().raw_bytes(), b"gre payload");
    }

    #[test]
    fn test_client_action_event_field_access() {
        let event = ClientActionEvent::new(
            make_metadata(b"client action"),
            serde_json::json!({"type": "MulliganResp"}),
        );
        assert_eq!(event.payload()["type"], "MulliganResp");
    }

    #[test]
    fn test_match_state_event_field_access() {
        let event = MatchStateEvent::new(
            make_metadata(b"match state"),
            serde_json::json!(
                {"matchGameRoomStateChangedEvent": {}}
            ),
        );
        assert!(event.payload()["matchGameRoomStateChangedEvent"].is_object());
    }

    #[test]
    fn test_draft_bot_event_field_access() {
        let event = DraftBotEvent::new(
            make_metadata(b"bot draft"),
            serde_json::json!({"DraftStatus": "PickNext"}),
        );
        assert_eq!(event.payload()["DraftStatus"], "PickNext");
    }

    #[test]
    fn test_draft_human_event_field_access() {
        let event = DraftHumanEvent::new(
            make_metadata(b"human draft"),
            serde_json::json!({"PickGrpId": 12345}),
        );
        assert_eq!(event.payload()["PickGrpId"], 12345);
    }

    #[test]
    fn test_draft_complete_event_field_access() {
        let event = DraftCompleteEvent::new(
            make_metadata(b"draft complete"),
            serde_json::json!({"Draft_CompleteDraft": true}),
        );
        assert_eq!(
            event.payload()["Draft_CompleteDraft"],
            serde_json::json!(true)
        );
    }

    #[test]
    fn test_event_lifecycle_event_field_access() {
        let event = EventLifecycleEvent::new(
            make_metadata(b"event lifecycle"),
            serde_json::json!({"action": "Event_Join"}),
        );
        assert_eq!(event.payload()["action"], "Event_Join");
    }

    #[test]
    fn test_session_event_field_access() {
        let event = SessionEvent::new(
            make_metadata(b"session data"),
            serde_json::json!({"DisplayName": "Player"}),
        );
        assert_eq!(event.payload()["DisplayName"], "Player");
    }

    #[test]
    fn test_rank_event_field_access() {
        let event = RankEvent::new(
            make_metadata(b"rank data"),
            serde_json::json!(
                {"constructedClass": "Gold", "constructedLevel": 2}
            ),
        );
        assert_eq!(event.payload()["constructedClass"], "Gold");
    }

    #[test]
    fn test_inventory_event_field_access() {
        let event = InventoryEvent::new(
            make_metadata(b"inventory"),
            serde_json::json!(
                {"gold": 5000, "gems": 200, "wcCommon": 10}
            ),
        );
        assert_eq!(event.payload()["gold"], 5000);
    }

    #[test]
    fn test_game_result_event_field_access() {
        let event = GameResultEvent::new(
            make_metadata(b"game result"),
            serde_json::json!(
                {"WinningType": "Win", "GameStage": "GameOver"}
            ),
        );
        assert_eq!(event.payload()["WinningType"], "Win");
    }

    // -- GameEvent enum --

    #[test]
    fn test_game_event_all_variants_have_correct_performance_class() {
        let events = all_variants();

        let expected_classes = [
            PerformanceClass::InteractiveDispatch, // GameState
            PerformanceClass::InteractiveDispatch, // ClientAction
            PerformanceClass::InteractiveDispatch, // MatchState
            PerformanceClass::DurablePerEvent,     // DraftBot
            PerformanceClass::DurablePerEvent,     // DraftHuman
            PerformanceClass::DurablePerEvent,     // DraftComplete
            PerformanceClass::DurablePerEvent,     // EventLifecycle
            PerformanceClass::DurablePerEvent,     // Session
            PerformanceClass::DurablePerEvent,     // Rank
            PerformanceClass::DurablePerEvent,     // Inventory
            PerformanceClass::PostGameBatch,       // GameResult
            PerformanceClass::InteractiveDispatch, // LogFileRotated
            PerformanceClass::InteractiveDispatch, // DetailedLoggingStatus
            PerformanceClass::InteractiveDispatch, // MatchConnectionState
            PerformanceClass::InteractiveDispatch, // TcpConnectionClose
            PerformanceClass::InteractiveDispatch, // WebSocketClosed
            PerformanceClass::InteractiveDispatch, // ConnectionError
        ];

        assert_eq!(
            events.len(),
            expected_classes.len(),
            "all_variants() and expected_classes must have the same length"
        );
        for (event, expected) in events.iter().zip(expected_classes.iter()) {
            assert_eq!(&event.performance_class(), expected);
        }
    }

    #[test]
    fn test_game_event_metadata_accessor_all_variants() {
        let raw = b"test";
        let events = all_variants();
        for event in &events {
            assert_eq!(event.metadata().raw_bytes(), raw);
        }
    }

    #[test]
    fn test_game_event_payload_accessor_all_variants() {
        let events = all_variants();
        let expected = serde_json::json!({});
        for event in &events {
            assert_eq!(*event.payload(), expected);
        }
    }

    // -- PerformanceClass --

    #[test]
    fn test_performance_class_equality() {
        assert_eq!(
            PerformanceClass::InteractiveDispatch,
            PerformanceClass::InteractiveDispatch
        );
        assert_ne!(
            PerformanceClass::InteractiveDispatch,
            PerformanceClass::DurablePerEvent
        );
        assert_ne!(
            PerformanceClass::DurablePerEvent,
            PerformanceClass::PostGameBatch
        );
    }

    #[test]
    fn test_performance_class_as_class_number_interactive_dispatch_returns_1() {
        assert_eq!(PerformanceClass::InteractiveDispatch.as_class_number(), 1);
    }

    #[test]
    fn test_performance_class_as_class_number_durable_per_event_returns_2() {
        assert_eq!(PerformanceClass::DurablePerEvent.as_class_number(), 2);
    }

    #[test]
    fn test_performance_class_as_class_number_post_game_batch_returns_3() {
        assert_eq!(PerformanceClass::PostGameBatch.as_class_number(), 3);
    }

    #[test]
    fn test_performance_class_requires_durable_storage_class1_false() {
        assert!(!PerformanceClass::InteractiveDispatch.requires_durable_storage());
    }

    #[test]
    fn test_performance_class_requires_durable_storage_class2_true() {
        assert!(PerformanceClass::DurablePerEvent.requires_durable_storage());
    }

    #[test]
    fn test_performance_class_requires_durable_storage_class3_true() {
        assert!(PerformanceClass::PostGameBatch.requires_durable_storage());
    }

    #[test]
    fn test_performance_class_is_batch_trigger_class1_false() {
        assert!(!PerformanceClass::InteractiveDispatch.is_batch_trigger());
    }

    #[test]
    fn test_performance_class_is_batch_trigger_class2_false() {
        assert!(!PerformanceClass::DurablePerEvent.is_batch_trigger());
    }

    #[test]
    fn test_performance_class_is_batch_trigger_class3_true() {
        assert!(PerformanceClass::PostGameBatch.is_batch_trigger());
    }

    #[test]
    fn test_performance_class_class_number_matches_event_mapping() {
        // Verify the class numbers align with the event-to-class mapping:
        // Class 1 events map to InteractiveDispatch (number 1)
        // Class 2 events map to DurablePerEvent (number 2)
        // Class 3 events map to PostGameBatch (number 3)
        let events = all_variants();
        let expected_numbers: Vec<u8> = vec![
            1, // GameState
            1, // ClientAction
            1, // MatchState
            2, // DraftBot
            2, // DraftHuman
            2, // DraftComplete
            2, // EventLifecycle
            2, // Session
            2, // Rank
            2, // Inventory
            3, // GameResult
            1, // LogFileRotated
            1, // DetailedLoggingStatus
            1, // MatchConnectionState
            1, // TcpConnectionClose
            1, // WebSocketClosed
            1, // ConnectionError
        ];
        assert_eq!(events.len(), expected_numbers.len());
        for (event, expected_num) in events.iter().zip(expected_numbers.iter()) {
            assert_eq!(event.performance_class().as_class_number(), *expected_num);
        }
    }

    // -- Serialization round-trip --

    #[test]
    fn test_game_event_serde_round_trip_all_variants() -> TestResult {
        for event in all_variants() {
            let serialized = serde_json::to_string(&event)?;
            let deserialized: GameEvent = serde_json::from_str(&serialized)?;
            assert_eq!(deserialized, event);
        }
        Ok(())
    }

    #[test]
    fn test_event_metadata_serde_round_trip() -> TestResult {
        let meta = make_metadata(b"round trip test");
        let serialized = serde_json::to_string(&meta)?;
        let deserialized: EventMetadata = serde_json::from_str(&serialized)?;

        assert_eq!(deserialized, meta);
        Ok(())
    }

    #[test]
    fn test_event_metadata_deserialize_recomputes_hash() -> TestResult {
        let meta = make_metadata(b"test data");
        let mut serialized: serde_json::Value = serde_json::to_value(&meta)?;

        // Tamper with the serialized raw_bytes_hash (now a hex string)
        serialized["raw_bytes_hash"] = serde_json::json!("00".repeat(32));

        let deserialized: EventMetadata = serde_json::from_value(serialized)?;

        // Hash should be recomputed from raw_bytes, not the tampered
        // value
        assert_eq!(*deserialized.raw_bytes_hash(), *meta.raw_bytes_hash());
        assert_eq!(deserialized.raw_bytes(), meta.raw_bytes());
        Ok(())
    }

    #[test]
    fn test_performance_class_serde_round_trip() -> TestResult {
        for class in [
            PerformanceClass::InteractiveDispatch,
            PerformanceClass::DurablePerEvent,
            PerformanceClass::PostGameBatch,
        ] {
            let serialized = serde_json::to_string(&class)?;
            let deserialized: PerformanceClass = serde_json::from_str(&serialized)?;
            assert_eq!(deserialized, class);
        }
        Ok(())
    }

    // -- Wire format --

    #[test]
    fn test_event_metadata_serializes_raw_bytes_as_base64() -> TestResult {
        let meta = make_metadata(b"hello world");
        let serialized: serde_json::Value = serde_json::to_value(&meta)?;
        assert_eq!(serialized["raw_bytes"], "aGVsbG8gd29ybGQ=");
        Ok(())
    }

    #[test]
    fn test_event_metadata_serializes_raw_bytes_hash_as_hex() -> TestResult {
        let meta = make_metadata(b"hello world");
        let serialized: serde_json::Value = serde_json::to_value(&meta)?;
        let hash_str = serialized["raw_bytes_hash"]
            .as_str()
            .ok_or("raw_bytes_hash should be a string")?;
        // Known SHA-256 of "hello world"
        assert_eq!(
            hash_str,
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
        Ok(())
    }

    #[test]
    fn test_event_metadata_deserialize_missing_raw_bytes_hash() -> TestResult {
        // Forward-compatibility: raw_bytes_hash absent from wire format
        let json = serde_json::json!({
            "timestamp": "2026-02-25T12:00:00Z",
            "raw_bytes": BASE64_STANDARD.encode(b"test data"),
        });
        let meta: EventMetadata = serde_json::from_value(json)?;
        let expected: [u8; 32] = Sha256::digest(b"test data").into();
        assert_eq!(*meta.raw_bytes_hash(), expected);
        assert_eq!(meta.raw_bytes(), b"test data");
        Ok(())
    }

    #[test]
    fn test_event_metadata_deserialize_integer_array_raw_bytes_hash() -> TestResult {
        // Backward-compatibility: raw_bytes_hash in old integer array
        // format
        let json = serde_json::json!({
            "timestamp": "2026-02-25T12:00:00Z",
            "raw_bytes": BASE64_STANDARD.encode(b"data"),
            "raw_bytes_hash": vec![0; 32],
        });
        let meta: EventMetadata = serde_json::from_value(json)?;
        // Hash is recomputed, not taken from wire
        let expected: [u8; 32] = Sha256::digest(b"data").into();
        assert_eq!(*meta.raw_bytes_hash(), expected);
        Ok(())
    }

    #[test]
    fn test_event_metadata_none_timestamp_serde_round_trip() -> TestResult {
        let meta = EventMetadata::new(None, b"no timestamp".to_vec());
        let serialized = serde_json::to_string(&meta)?;
        let deserialized: EventMetadata = serde_json::from_str(&serialized)?;
        assert_eq!(deserialized, meta);
        assert!(deserialized.timestamp().is_none());
        Ok(())
    }

    #[test]
    fn test_event_metadata_deserialize_null_timestamp() -> TestResult {
        let json = serde_json::json!({
            "timestamp": null,
            "raw_bytes": BASE64_STANDARD.encode(b"data"),
        });
        let meta: EventMetadata = serde_json::from_value(json)?;
        assert!(meta.timestamp().is_none());
        assert_eq!(meta.raw_bytes(), b"data");
        Ok(())
    }

    // -- LogFileRotatedEvent --

    #[test]
    fn test_log_file_rotated_for_rotation_stores_previous_file_size() {
        let ts = Utc
            .with_ymd_and_hms(2026, 3, 7, 10, 0, 0)
            .single()
            .unwrap_or_default();
        let event = LogFileRotatedEvent::for_rotation(ts, 42_000);
        assert_eq!(event.previous_file_size(), Some(42_000));
    }

    #[test]
    fn test_log_file_rotated_for_rotation_stores_timestamp() {
        let ts = Utc
            .with_ymd_and_hms(2026, 3, 7, 10, 0, 0)
            .single()
            .unwrap_or_default();
        let event = LogFileRotatedEvent::for_rotation(ts, 1000);
        assert_eq!(event.metadata().timestamp(), Some(ts));
    }

    #[test]
    fn test_log_file_rotated_has_empty_raw_bytes() {
        let ts = Utc
            .with_ymd_and_hms(2026, 3, 7, 10, 0, 0)
            .single()
            .unwrap_or_default();
        let event = LogFileRotatedEvent::for_rotation(ts, 500);
        assert!(event.metadata().raw_bytes().is_empty());
    }

    #[test]
    fn test_log_file_rotated_serde_round_trip() -> TestResult {
        let ts = Utc
            .with_ymd_and_hms(2026, 3, 7, 10, 0, 0)
            .single()
            .unwrap_or_default();
        let event = GameEvent::LogFileRotated(LogFileRotatedEvent::for_rotation(ts, 12345));
        let serialized = serde_json::to_string(&event)?;
        let deserialized: GameEvent = serde_json::from_str(&serialized)?;
        assert_eq!(deserialized, event);
        Ok(())
    }

    #[test]
    fn test_log_file_rotated_performance_class_is_interactive() {
        let ts = Utc
            .with_ymd_and_hms(2026, 3, 7, 10, 0, 0)
            .single()
            .unwrap_or_default();
        let event = GameEvent::LogFileRotated(LogFileRotatedEvent::for_rotation(ts, 0));
        assert_eq!(
            event.performance_class(),
            PerformanceClass::InteractiveDispatch
        );
    }

    // -- DetailedLoggingStatusEvent --

    #[test]
    fn test_detailed_logging_status_new_status_stores_enabled_true() {
        let ts = Utc
            .with_ymd_and_hms(2026, 3, 15, 10, 0, 0)
            .single()
            .unwrap_or_default();
        let event = DetailedLoggingStatusEvent::new_status(ts, true);
        assert_eq!(event.enabled(), Some(true));
    }

    #[test]
    fn test_detailed_logging_status_new_status_stores_enabled_false() {
        let ts = Utc
            .with_ymd_and_hms(2026, 3, 15, 10, 0, 0)
            .single()
            .unwrap_or_default();
        let event = DetailedLoggingStatusEvent::new_status(ts, false);
        assert_eq!(event.enabled(), Some(false));
    }

    #[test]
    fn test_detailed_logging_status_stores_timestamp() {
        let ts = Utc
            .with_ymd_and_hms(2026, 3, 15, 10, 0, 0)
            .single()
            .unwrap_or_default();
        let event = DetailedLoggingStatusEvent::new_status(ts, true);
        assert_eq!(event.metadata().timestamp(), Some(ts));
    }

    #[test]
    fn test_detailed_logging_status_has_empty_raw_bytes() {
        let ts = Utc
            .with_ymd_and_hms(2026, 3, 15, 10, 0, 0)
            .single()
            .unwrap_or_default();
        let event = DetailedLoggingStatusEvent::new_status(ts, false);
        assert!(event.metadata().raw_bytes().is_empty());
    }

    #[test]
    fn test_detailed_logging_status_serde_round_trip() -> TestResult {
        let ts = Utc
            .with_ymd_and_hms(2026, 3, 15, 10, 0, 0)
            .single()
            .unwrap_or_default();
        let event =
            GameEvent::DetailedLoggingStatus(DetailedLoggingStatusEvent::new_status(ts, false));
        let serialized = serde_json::to_string(&event)?;
        let deserialized: GameEvent = serde_json::from_str(&serialized)?;
        assert_eq!(deserialized, event);
        Ok(())
    }

    #[test]
    fn test_detailed_logging_status_performance_class_is_interactive() {
        let ts = Utc
            .with_ymd_and_hms(2026, 3, 15, 10, 0, 0)
            .single()
            .unwrap_or_default();
        let event =
            GameEvent::DetailedLoggingStatus(DetailedLoggingStatusEvent::new_status(ts, true));
        assert_eq!(
            event.performance_class(),
            PerformanceClass::InteractiveDispatch
        );
    }
}
