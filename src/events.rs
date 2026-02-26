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

    /// Event lifecycle: join, set deck, get courses, claim prize.
    /// Class 2 — durable per-event.
    EventLifecycle(EventLifecycleEvent),

    /// Session: login, account identity, logout.
    /// Class 2 — durable per-event.
    Session(SessionEvent),

    /// Rank snapshot (`Rank_GetCombinedRankInfo`).
    /// Class 2 — durable per-event.
    Rank(RankEvent),

    /// Card collection snapshot (`PlayerInventory.GetPlayerCardsV3`).
    /// Class 2 — durable per-event.
    Collection(CollectionEvent),

    /// Inventory snapshot (`DTO_InventoryInfo`): currency, wildcards, etc.
    /// Class 2 — durable per-event.
    Inventory(InventoryEvent),

    /// Game result (`LogBusinessEvents` with `WinningType`).
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
        match self {
            Self::GameState(e) => &e.metadata,
            Self::ClientAction(e) => &e.metadata,
            Self::MatchState(e) => &e.metadata,
            Self::DraftBot(e) => &e.metadata,
            Self::DraftHuman(e) => &e.metadata,
            Self::DraftComplete(e) => &e.metadata,
            Self::EventLifecycle(e) => &e.metadata,
            Self::Session(e) => &e.metadata,
            Self::Rank(e) => &e.metadata,
            Self::Collection(e) => &e.metadata,
            Self::Inventory(e) => &e.metadata,
            Self::GameResult(e) => &e.metadata,
        }
    }

    /// Returns the parsed JSON payload of the event.
    pub fn payload(&self) -> &serde_json::Value {
        match self {
            Self::GameState(e) => &e.payload,
            Self::ClientAction(e) => &e.payload,
            Self::MatchState(e) => &e.payload,
            Self::DraftBot(e) => &e.payload,
            Self::DraftHuman(e) => &e.payload,
            Self::DraftComplete(e) => &e.payload,
            Self::EventLifecycle(e) => &e.payload,
            Self::Session(e) => &e.payload,
            Self::Rank(e) => &e.payload,
            Self::Collection(e) => &e.payload,
            Self::Inventory(e) => &e.payload,
            Self::GameResult(e) => &e.payload,
        }
    }
}

/// Performance class determining latency target and delivery path.
///
/// See the [feature spec performance classes][spec] for details.
///
/// [spec]: https://github.com/manasight/manasight-docs/blob/main/docs/requirements/feature-specs/log-file-parser.md#performance-classes
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PerformanceClass {
    /// Class 1: local-only, ≤ 100 ms latency. Also accumulated for Class 3.
    InteractiveDispatch,
    /// Class 2: persisted to disk queue, uploaded individually, ≤ 1 s.
    DurablePerEvent,
    /// Class 3: triggers assembly and upload of the complete game batch.
    PostGameBatch,
}

/// Fields shared by every event: timestamp, raw bytes, and payload hash.
///
/// Constructed via [`EventMetadata::new`], which computes the `payload_hash`
/// from `raw_bytes` to enforce the invariant that the hash always matches.
/// This is critical for server-side deduplication via event fingerprints.
///
/// Deserialization also enforces this invariant: the hash is recomputed from
/// `raw_bytes` during deserialization rather than trusting the serialized value.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EventMetadata {
    /// UTC timestamp parsed from the log entry header.
    pub timestamp: DateTime<Utc>,

    /// Original log entry bytes. Needed by the game accumulator for disk
    /// storage and by the raw-log backup pipeline. Private to prevent
    /// mutation that would break the `payload_hash` invariant.
    raw_bytes: Vec<u8>,

    /// SHA-256 hash of `raw_bytes`, precomputed at construction time.
    /// Used as part of the event fingerprint for server-side deduplication:
    /// `sha256(event_type + '\0' + match_id + '\0' + timestamp + '\0' + payload_hash)`.
    payload_hash: [u8; 32],
}

impl EventMetadata {
    /// Creates a new `EventMetadata`, computing `payload_hash` as the
    /// SHA-256 digest of `raw_bytes`.
    pub fn new(timestamp: DateTime<Utc>, raw_bytes: Vec<u8>) -> Self {
        let payload_hash: [u8; 32] = Sha256::digest(&raw_bytes).into();
        Self {
            timestamp,
            raw_bytes,
            payload_hash,
        }
    }

    /// Returns the original log entry bytes.
    pub fn raw_bytes(&self) -> &[u8] {
        &self.raw_bytes
    }

    /// Returns the SHA-256 hash of `raw_bytes`.
    pub fn payload_hash(&self) -> &[u8; 32] {
        &self.payload_hash
    }
}

/// Custom `Deserialize` that recomputes `payload_hash` from `raw_bytes`,
/// ensuring the hash invariant survives serialization round-trips.
impl<'de> Deserialize<'de> for EventMetadata {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        /// Wire format for deserializing `EventMetadata`. The `payload_hash`
        /// field is read but discarded — the real hash is recomputed.
        #[derive(Deserialize)]
        struct EventMetadataWire {
            timestamp: DateTime<Utc>,
            raw_bytes: Vec<u8>,
            // Underscore prefix marks the field intentionally unused (hash is
            // recomputed); serde(rename) keeps the JSON key as "payload_hash".
            #[serde(rename = "payload_hash")]
            _payload_hash: [u8; 32],
        }

        let wire = EventMetadataWire::deserialize(deserializer)?;
        Ok(EventMetadata::new(wire.timestamp, wire.raw_bytes))
    }
}

// ---------------------------------------------------------------------------
// Class 1: Interactive Dispatch
// ---------------------------------------------------------------------------

/// GRE-to-client game state messages.
///
/// Covers `GameStateMessage`, `ConnectResp`, and `QueuedGameStateMessage`
/// payloads from `greToClientEvent` entries.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GameStateEvent {
    /// Shared event metadata (timestamp, raw bytes, payload hash).
    pub(crate) metadata: EventMetadata,

    /// The parsed JSON payload from the GRE message.
    pub(crate) payload: serde_json::Value,
}

impl GameStateEvent {
    /// Returns the parsed JSON payload.
    pub fn payload(&self) -> &serde_json::Value {
        &self.payload
    }
}

/// Client-to-GRE player actions.
///
/// Covers `SelectNResp`, `SubmitDeckResp`, `MulliganResp`, and other
/// `ClientToGREMessage` payloads.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClientActionEvent {
    /// Shared event metadata (timestamp, raw bytes, payload hash).
    pub(crate) metadata: EventMetadata,

    /// The parsed JSON payload from the client message.
    pub(crate) payload: serde_json::Value,
}

impl ClientActionEvent {
    /// Returns the parsed JSON payload.
    pub fn payload(&self) -> &serde_json::Value {
        &self.payload
    }
}

/// Match room state transitions.
///
/// Parsed from `matchGameRoomStateChangedEvent` entries. Signals match
/// start/end and triggers overlay state transitions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MatchStateEvent {
    /// Shared event metadata (timestamp, raw bytes, payload hash).
    pub(crate) metadata: EventMetadata,

    /// The parsed JSON payload of the match state change.
    pub(crate) payload: serde_json::Value,
}

impl MatchStateEvent {
    /// Returns the parsed JSON payload.
    pub fn payload(&self) -> &serde_json::Value {
        &self.payload
    }
}

// ---------------------------------------------------------------------------
// Class 2: Durable Per-Event
// ---------------------------------------------------------------------------

/// Bot draft pick events.
///
/// Parsed from `DraftStatus: "PickNext"` and `BotDraft_DraftPick` entries.
/// Each pick is independently valuable and must survive crashes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DraftBotEvent {
    /// Shared event metadata (timestamp, raw bytes, payload hash).
    pub(crate) metadata: EventMetadata,

    /// The parsed JSON payload of the draft pick.
    pub(crate) payload: serde_json::Value,
}

impl DraftBotEvent {
    /// Returns the parsed JSON payload.
    pub fn payload(&self) -> &serde_json::Value {
        &self.payload
    }
}

/// Human draft pick events.
///
/// Parsed from `Draft.Notify`, `EventPlayerDraftMakePick`, and
/// `LogBusinessEvents` entries containing `PickGrpId`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DraftHumanEvent {
    /// Shared event metadata (timestamp, raw bytes, payload hash).
    pub(crate) metadata: EventMetadata,

    /// The parsed JSON payload of the draft pick.
    pub(crate) payload: serde_json::Value,
}

impl DraftHumanEvent {
    /// Returns the parsed JSON payload.
    pub fn payload(&self) -> &serde_json::Value {
        &self.payload
    }
}

/// Draft completion event.
///
/// Parsed from `Draft_CompleteDraft`. Links the draft ID to the event
/// and marks the draft as finished.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DraftCompleteEvent {
    /// Shared event metadata (timestamp, raw bytes, payload hash).
    pub(crate) metadata: EventMetadata,

    /// The parsed JSON payload of the draft completion.
    pub(crate) payload: serde_json::Value,
}

impl DraftCompleteEvent {
    /// Returns the parsed JSON payload.
    pub fn payload(&self) -> &serde_json::Value {
        &self.payload
    }
}

/// Event lifecycle transitions.
///
/// Covers `Event_Join`, `Event_SetDeck`, `Event_GetCourses`, and
/// `Event_ClaimPrize`. Each is independently meaningful.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventLifecycleEvent {
    /// Shared event metadata (timestamp, raw bytes, payload hash).
    pub(crate) metadata: EventMetadata,

    /// The parsed JSON payload of the event lifecycle action.
    pub(crate) payload: serde_json::Value,
}

impl EventLifecycleEvent {
    /// Returns the parsed JSON payload.
    pub fn payload(&self) -> &serde_json::Value {
        &self.payload
    }
}

/// Session identity and connection events.
///
/// Covers `Updated account. DisplayName:`, `authenticateResponse`,
/// and `FrontDoorConnection.Close`. Needed to tag all subsequent events
/// with player identity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionEvent {
    /// Shared event metadata (timestamp, raw bytes, payload hash).
    pub(crate) metadata: EventMetadata,

    /// The parsed JSON payload of the session event.
    pub(crate) payload: serde_json::Value,
}

impl SessionEvent {
    /// Returns the parsed JSON payload.
    pub fn payload(&self) -> &serde_json::Value {
        &self.payload
    }
}

/// Rank snapshot.
///
/// Parsed from `Rank_GetCombinedRankInfo`. Infrequent, small,
/// independently useful.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RankEvent {
    /// Shared event metadata (timestamp, raw bytes, payload hash).
    pub(crate) metadata: EventMetadata,

    /// The parsed JSON payload of the rank information.
    pub(crate) payload: serde_json::Value,
}

impl RankEvent {
    /// Returns the parsed JSON payload.
    pub fn payload(&self) -> &serde_json::Value {
        &self.payload
    }
}

/// Card collection snapshot.
///
/// Parsed from `PlayerInventory.GetPlayerCardsV3`. Enables future
/// deck building features. Best-effort collection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CollectionEvent {
    /// Shared event metadata (timestamp, raw bytes, payload hash).
    pub(crate) metadata: EventMetadata,

    /// The parsed JSON payload mapping card IDs to quantities.
    pub(crate) payload: serde_json::Value,
}

impl CollectionEvent {
    /// Returns the parsed JSON payload.
    pub fn payload(&self) -> &serde_json::Value {
        &self.payload
    }
}

/// Inventory snapshot.
///
/// Parsed from `DTO_InventoryInfo`. Contains currency, wildcards,
/// boosters, and vault progress.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InventoryEvent {
    /// Shared event metadata (timestamp, raw bytes, payload hash).
    pub(crate) metadata: EventMetadata,

    /// The parsed JSON payload of the inventory state.
    pub(crate) payload: serde_json::Value,
}

impl InventoryEvent {
    /// Returns the parsed JSON payload.
    pub fn payload(&self) -> &serde_json::Value {
        &self.payload
    }
}

// ---------------------------------------------------------------------------
// Class 3: Post-Game Batch
// ---------------------------------------------------------------------------

/// Game result event — triggers post-game batch assembly.
///
/// Parsed from `LogBusinessEvents` with `WinningType` and
/// `GameStage_GameOver`. When this event fires, the desktop app
/// serializes the disk-backed game buffer into a single compressed
/// payload and uploads it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GameResultEvent {
    /// Shared event metadata (timestamp, raw bytes, payload hash).
    pub(crate) metadata: EventMetadata,

    /// The parsed JSON payload of the game result.
    pub(crate) payload: serde_json::Value,
}

impl GameResultEvent {
    /// Returns the parsed JSON payload.
    pub fn payload(&self) -> &serde_json::Value {
        &self.payload
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Datelike, TimeZone};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// Helper: build an `EventMetadata` with a fixed timestamp and the given raw bytes.
    ///
    /// UTC datetimes are never ambiguous so `single()` always returns `Some`.
    /// The `unwrap_or_default()` fallback returns epoch (1970-01-01) which would
    /// visibly fail any timestamp assertion rather than passing silently.
    fn make_metadata(raw: &[u8]) -> EventMetadata {
        let timestamp = Utc
            .with_ymd_and_hms(2026, 2, 25, 12, 0, 0)
            .single()
            .unwrap_or_default();
        EventMetadata::new(timestamp, raw.to_vec())
    }

    /// Helper: build all 12 `GameEvent` variants for exhaustive testing.
    fn all_variants() -> Vec<GameEvent> {
        let meta = make_metadata(b"test");
        let payload = serde_json::json!({});
        vec![
            GameEvent::GameState(GameStateEvent {
                metadata: meta.clone(),
                payload: payload.clone(),
            }),
            GameEvent::ClientAction(ClientActionEvent {
                metadata: meta.clone(),
                payload: payload.clone(),
            }),
            GameEvent::MatchState(MatchStateEvent {
                metadata: meta.clone(),
                payload: payload.clone(),
            }),
            GameEvent::DraftBot(DraftBotEvent {
                metadata: meta.clone(),
                payload: payload.clone(),
            }),
            GameEvent::DraftHuman(DraftHumanEvent {
                metadata: meta.clone(),
                payload: payload.clone(),
            }),
            GameEvent::DraftComplete(DraftCompleteEvent {
                metadata: meta.clone(),
                payload: payload.clone(),
            }),
            GameEvent::EventLifecycle(EventLifecycleEvent {
                metadata: meta.clone(),
                payload: payload.clone(),
            }),
            GameEvent::Session(SessionEvent {
                metadata: meta.clone(),
                payload: payload.clone(),
            }),
            GameEvent::Rank(RankEvent {
                metadata: meta.clone(),
                payload: payload.clone(),
            }),
            GameEvent::Collection(CollectionEvent {
                metadata: meta.clone(),
                payload: payload.clone(),
            }),
            GameEvent::Inventory(InventoryEvent {
                metadata: meta.clone(),
                payload: payload.clone(),
            }),
            GameEvent::GameResult(GameResultEvent {
                metadata: meta.clone(),
                payload: payload.clone(),
            }),
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
    fn test_event_metadata_new_computes_payload_hash() {
        let raw = b"test payload";
        let meta = make_metadata(raw);
        let expected: [u8; 32] = Sha256::digest(raw).into();
        assert_eq!(*meta.payload_hash(), expected);
    }

    #[test]
    fn test_event_metadata_new_stores_timestamp() {
        let meta = make_metadata(b"data");
        assert_eq!(meta.timestamp.year(), 2026);
        assert_eq!(meta.timestamp.month(), 2);
    }

    #[test]
    fn test_event_metadata_new_enforces_hash_invariant() {
        let raw = b"important data";
        let meta = EventMetadata::new(Utc::now(), raw.to_vec());
        let expected: [u8; 32] = Sha256::digest(raw).into();
        assert_eq!(
            *meta.payload_hash(),
            expected,
            "payload_hash must always be SHA-256 of raw_bytes"
        );
    }

    // -- EventMetadata properties --

    #[test]
    fn test_different_raw_bytes_produce_different_hashes() {
        let meta1 = make_metadata(b"payload one");
        let meta2 = make_metadata(b"payload two");
        assert_ne!(meta1.payload_hash(), meta2.payload_hash());
    }

    #[test]
    fn test_identical_raw_bytes_produce_same_hash() {
        let meta1 = make_metadata(b"same payload");
        let meta2 = make_metadata(b"same payload");
        assert_eq!(meta1.payload_hash(), meta2.payload_hash());
    }

    #[test]
    fn test_empty_raw_bytes_valid() {
        let meta = make_metadata(b"");
        assert!(meta.raw_bytes().is_empty());
        let expected: [u8; 32] = Sha256::digest(b"").into();
        assert_eq!(*meta.payload_hash(), expected);
    }

    #[test]
    fn test_event_metadata_clone_is_equal() {
        let meta = make_metadata(b"original");
        let cloned = meta.clone();
        assert_eq!(meta, cloned);
    }

    // -- Per-category struct field access (via accessors) --

    #[test]
    fn test_game_state_event_field_access() {
        let event = GameStateEvent {
            metadata: make_metadata(b"gre payload"),
            payload: serde_json::json!({"type": "GameStateMessage"}),
        };
        assert_eq!(event.payload()["type"], "GameStateMessage");
        assert_eq!(event.metadata.raw_bytes(), b"gre payload");
    }

    #[test]
    fn test_client_action_event_field_access() {
        let event = ClientActionEvent {
            metadata: make_metadata(b"client action"),
            payload: serde_json::json!({"type": "MulliganResp"}),
        };
        assert_eq!(event.payload()["type"], "MulliganResp");
    }

    #[test]
    fn test_match_state_event_field_access() {
        let event = MatchStateEvent {
            metadata: make_metadata(b"match state"),
            payload: serde_json::json!({"matchGameRoomStateChangedEvent": {}}),
        };
        assert!(event.payload()["matchGameRoomStateChangedEvent"].is_object());
    }

    #[test]
    fn test_draft_bot_event_field_access() {
        let event = DraftBotEvent {
            metadata: make_metadata(b"bot draft"),
            payload: serde_json::json!({"DraftStatus": "PickNext"}),
        };
        assert_eq!(event.payload()["DraftStatus"], "PickNext");
    }

    #[test]
    fn test_draft_human_event_field_access() {
        let event = DraftHumanEvent {
            metadata: make_metadata(b"human draft"),
            payload: serde_json::json!({"PickGrpId": 12345}),
        };
        assert_eq!(event.payload()["PickGrpId"], 12345);
    }

    #[test]
    fn test_draft_complete_event_field_access() {
        let event = DraftCompleteEvent {
            metadata: make_metadata(b"draft complete"),
            payload: serde_json::json!({"Draft_CompleteDraft": true}),
        };
        assert!(event.payload()["Draft_CompleteDraft"]
            .as_bool()
            .unwrap_or(false));
    }

    #[test]
    fn test_event_lifecycle_event_field_access() {
        let event = EventLifecycleEvent {
            metadata: make_metadata(b"event lifecycle"),
            payload: serde_json::json!({"action": "Event_Join"}),
        };
        assert_eq!(event.payload()["action"], "Event_Join");
    }

    #[test]
    fn test_session_event_field_access() {
        let event = SessionEvent {
            metadata: make_metadata(b"session data"),
            payload: serde_json::json!({"DisplayName": "Player"}),
        };
        assert_eq!(event.payload()["DisplayName"], "Player");
    }

    #[test]
    fn test_rank_event_field_access() {
        let event = RankEvent {
            metadata: make_metadata(b"rank data"),
            payload: serde_json::json!({"constructedClass": "Gold", "constructedLevel": 2}),
        };
        assert_eq!(event.payload()["constructedClass"], "Gold");
    }

    #[test]
    fn test_collection_event_field_access() {
        let event = CollectionEvent {
            metadata: make_metadata(b"collection"),
            payload: serde_json::json!({"12345": 4, "67890": 2}),
        };
        assert_eq!(event.payload()["12345"], 4);
    }

    #[test]
    fn test_inventory_event_field_access() {
        let event = InventoryEvent {
            metadata: make_metadata(b"inventory"),
            payload: serde_json::json!({"gold": 5000, "gems": 200, "wcCommon": 10}),
        };
        assert_eq!(event.payload()["gold"], 5000);
    }

    #[test]
    fn test_game_result_event_field_access() {
        let event = GameResultEvent {
            metadata: make_metadata(b"game result"),
            payload: serde_json::json!({"WinningType": "Win", "GameStage": "GameOver"}),
        };
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
    fn test_game_event_serde_round_trip() -> TestResult {
        let event = GameEvent::Session(SessionEvent {
            metadata: make_metadata(b"session data"),
            payload: serde_json::json!({"DisplayName": "Player"}),
        });

        let serialized = serde_json::to_string(&event)?;
        let deserialized: GameEvent = serde_json::from_str(&serialized)?;

        assert_eq!(deserialized, event);
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

        // Tamper with the serialized payload_hash (JSON integer array format)
        let zeroed: Vec<u8> = vec![0; 32];
        serialized["payload_hash"] = serde_json::json!(zeroed);

        let deserialized: EventMetadata = serde_json::from_value(serialized)?;

        // Hash should be recomputed from raw_bytes, not the tampered value
        assert_eq!(*deserialized.payload_hash(), *meta.payload_hash());
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
}
