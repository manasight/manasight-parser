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
            Self::Collection(e) => e.$method(),
            Self::Inventory(e) => e.$method(),
            Self::GameResult(e) => e.$method(),
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

    /// Bot draft picks (`DraftStatus: "PickNext"`, `BotDraft_DraftPick`).
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

    /// Card collection snapshot (`<== StartHook` with `PlayerCards`).
    /// Class 2 — durable per-event.
    Collection(CollectionEvent),

    /// Inventory snapshot (`<== StartHook` with `InventoryInfo`):
    /// currency, wildcards, etc. Class 2 — durable per-event.
    Inventory(InventoryEvent),

    /// Game result (`GameStage_GameOver` from GRE `GameStateMessage`).
    /// Class 3 — triggers post-game batch assembly.
    GameResult(GameResultEvent),
}

impl GameEvent {
    /// Returns the performance class for this event.
    ///
    /// - Class 1: interactive dispatch (local, ≤ 100 ms)
    /// - Class 2: durable per-event upload
    /// - Class 3: post-game batch upload trigger
    pub fn performance_class(&self) -> PerformanceClass {
        match self {
            Self::GameState(_) | Self::ClientAction(_) | Self::MatchState(_) => {
                PerformanceClass::InteractiveDispatch
            }
            Self::DraftBot(_)
            | Self::DraftHuman(_)
            | Self::DraftComplete(_)
            | Self::EventLifecycle(_)
            | Self::Session(_)
            | Self::Rank(_)
            | Self::Collection(_)
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

// ---------------------------------------------------------------------------
// EventMetadata
// ---------------------------------------------------------------------------

/// Fields shared by every event: timestamp, raw bytes, and raw-bytes hash.
///
/// Constructed via [`EventMetadata::new`], which computes the `raw_bytes_hash`
/// from `raw_bytes` to enforce the invariant that the hash always matches.
/// This is critical for server-side deduplication via event fingerprints.
///
/// All fields are private to protect the hash invariant. Use the accessor
/// methods to read them.
///
/// Deserialization also enforces this invariant: the hash is recomputed from
/// `raw_bytes` during deserialization rather than trusting the serialized value.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EventMetadata {
    /// UTC timestamp parsed from the log entry header.
    timestamp: DateTime<Utc>,

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
    pub fn new(timestamp: DateTime<Utc>, raw_bytes: Vec<u8>) -> Self {
        let raw_bytes_hash: [u8; 32] = Sha256::digest(&raw_bytes).into();
        Self {
            timestamp,
            raw_bytes,
            raw_bytes_hash,
        }
    }

    /// Returns the UTC timestamp parsed from the log entry header.
    pub fn timestamp(&self) -> DateTime<Utc> {
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
            timestamp: DateTime<Utc>,
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
    /// Bot draft pick events.
    ///
    /// Parsed from `DraftStatus: "PickNext"` and `BotDraft_DraftPick` entries.
    /// Each pick is independently valuable and must survive crashes.
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
    /// Card collection snapshot.
    ///
    /// Parsed from `<== StartHook` responses containing `PlayerCards`.
    /// Enables future deck building features. Best-effort collection.
    CollectionEvent
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
        EventMetadata::new(timestamp, raw.to_vec())
    }

    /// Helper: build all 12 `GameEvent` variants for exhaustive testing.
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
            GameEvent::Collection(CollectionEvent::new(meta.clone(), payload.clone())),
            GameEvent::Inventory(InventoryEvent::new(meta.clone(), payload.clone())),
            GameEvent::GameResult(GameResultEvent::new(meta.clone(), payload.clone())),
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
        assert_eq!(meta.timestamp().year(), 2026);
        assert_eq!(meta.timestamp().month(), 2);
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
        assert_eq!(ts.year(), 2026);
        assert_eq!(ts.month(), 2);
        assert_eq!(ts.day(), 25);
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
    fn test_collection_event_field_access() {
        let event = CollectionEvent::new(
            make_metadata(b"collection"),
            serde_json::json!({"12345": 4, "67890": 2}),
        );
        assert_eq!(event.payload()["12345"], 4);
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
            PerformanceClass::DurablePerEvent,     // Collection
            PerformanceClass::DurablePerEvent,     // Inventory
            PerformanceClass::PostGameBatch,       // GameResult
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
}
