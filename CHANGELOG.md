# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [0.5.3] - 2026-06-19

### Fixed

- **`parse_whole_log_js` (WASM) now serializes event payloads as plain JS
  objects instead of `Map` instances.** The wasm-bindgen export used the default
  `serde_wasm_bindgen` serializer, which emits dynamic `serde_json::Value`
  objects — every `payload` and nested dynamic object — as JS `Map`s, so JS/TS
  consumers reading fields by property access (`payload.match_id`) got
  `undefined` and had to fall back to `.get(...)`. This diverged from the native
  `serde_json` output. The serializer is now configured with
  `serialize_maps_as_objects(true)`, so WASM output matches the native
  plain-object shape and the documented externally-tagged event contract. Only
  WASM consumers were affected; native Rust output is unchanged (#243, #244).

## [0.5.2] - 2026-06-19

### Fixed

- **`parse_whole_log` now parses logs from the newer MTGA Mac client** that
  prefixes every line with a Unity frame counter
  (`[<frame>] [UnityCrossThreadLogger]…`, the uploaded `UTC_Log` archive
  variant). Header and metadata detection is anchored at byte 0, so these
  otherwise-valid logs produced **0 events**. `LineBuffer::push_line` now strips
  an optional leading `[<digits>] ` prefix before detection — a single chokepoint
  covering every byte-0-anchored detector. Output is byte-identical for
  unprefixed input across the full corpus, so the live `Player.log` path is
  unaffected (#240, #241).

## [0.5.1] - 2026-06-17

### Added

- **`GameEvent::DeckSubmission(DeckSubmissionEvent)`** surfacing the submitted
  deck's registered `Format` and `DeckId` from `EventSetDeckV2` / `EventSetDeckV3`
  request lines. A new family-matched parser claims the bare `==> EventSetDeck`
  prefix — covering V2, V3, and any future `Vn` without panicking on an
  unrecognized version — decodes the double-encoded `request`, and emits a
  `{ metadata, payload }` event whose payload carries
  `{ deck_id, deck_format, event_name, is_singleton }`. This gives the format
  model a precise fallback for generic / bot-match queues whose event name is not
  format-bearing. `GameEvent` is `#[non_exhaustive]`, so the new variant is
  additive (#232, #237).

## [0.5.0] - 2026-06-16

> Released directly from `0.3.0`. Versions `0.4.0` and `0.5.0` were bumped in
> `Cargo.toml` during feature work but never published; `0.4.0` is intentionally
> skipped on crates.io and its changes are folded into this entry.

### Added

- **WebAssembly build target and synchronous whole-log parser.** New
  `parse_whole_log(input: &str) -> Vec<GameEvent>` — a pure, synchronous, I/O-free
  entry point that reuses the same `LineBuffer` + `Router` core as the async
  desktop path. New `wasm` cargo feature exports `parseWholeLog` via `wasm-bindgen`
  (`src/wasm.rs`) for a browser/Node parse worker; build with
  `wasm-pack build --target web --no-default-features --features wasm`. The crate
  now builds as both `cdylib` and `rlib` (#224, #230).
- **`tailer` cargo feature (default-on)** gating the async stack — `tokio`,
  `known-folders`, the file tailer, log discovery, `event_bus`, and `stream`.
  Building with `--no-default-features` yields a pure-sync, WASM-compatible subset
  that still exposes `parse_whole_log` without pulling in `tokio` or filesystem
  access. The default desktop build (`brace_depth_flush` + `tailer`) is unchanged
  (#224).
- **`ScrubOptions` and `scrub_raw_log_with`.** New
  `ScrubOptions { keep_player_names: bool }` (defaults to `false`) and
  `scrub_raw_log_with(input, &ScrubOptions)`; `scrub_raw_log` keeps its existing
  signature and behavior, now delegating with default options. Setting
  `keep_player_names = true` retains in-game screen/player names (needed for seat
  attribution) while still redacting everything else. Email, IPv4, and IPv6
  addresses are now scrubbed as well (#225).
- **`GameEvent::Truncation(TruncationEvent)`** surfacing Arena's
  `[Message summarized … exceeded the 50 GameObject or 50 Annotation limit.]`
  marker that replaces an oversized GSM body. The payload carries
  `{ object_count, annotation_count }`; a new `EntryHeader::TruncationMarker`
  (classified `MultiLine`) accumulates the marker and its follow-on count lines.
  `GameEvent` is `#[non_exhaustive]`, so the new variant is additive (#200, #201).
- **`prev_game_state_id` on game-state-message payloads**, read from inside the
  `gameStateMessage` sub-object. Always present — serializes to `null` on Full
  GSMs that omit it — giving consumers a contiguous Diff-sequence signal (#201).
- **ChoiceResult and LinkInfo annotation details.** `AnnotationType_ChoiceResult`
  and `AnnotationType_LinkInfo` previously fell through the catch-all arm and
  dropped all detail. The parser now emits `choice_value` (plus optional
  `choice_domain` / `choice_sentiment`) and `choose_link_type` /
  `source_ability_grp_id` / `link_type`, restoring the chosen card name for
  "name a card" effects (e.g. Pithing Needle, Meddling Mage) that previously
  reached consumers with base fields only (#221).
- **Linux (Steam/Proton + Lutris) `Player.log` auto-discovery** in
  `discover_log_file()`. Parses
  `~/.local/share/Steam/steamapps/libraryfolders.vdf` to find the Steam library
  containing MTGA (app id 2141910) across any configured disk, with a Lutris prefix
  fallback. On Linux, `UnsupportedPlatform` is no longer returned; instead a
  `LogFileMissing` error (matching the Windows/macOS absent-file contract) is
  returned when no candidate location contains a `Player.log` (#212, #213).

### Changed

- **`SelectN` client-action payload now reads `selectNResp.ids`** and emits a
  single `selected_ids: Vec<i64>` field. The previous `selected_option_ids` /
  `selected_object_ids` fields are **removed** — neither name appears in real MTGA
  logs (zero corpus hits) and no downstream consumer read them. The optional
  `useArbitrary` ordering tag remains reachable through `raw_client_action`
  (#206, #208).

### Fixed

- GameOver `GameStateMessage`s carrying annotations (e.g. the lethal combat damage that ends the match) no longer drop them silently. When `gameInfo.stage == GameStage_GameOver` and the GSM has a non-empty `annotations` or `persistentAnnotations` array, the parser now emits a `GameEvent::GameState` carrying both arrays immediately before the existing `GameEvent::GameResult`. Per-producer ordering on the broadcast channel guarantees annotation-walker consumers see the killing-blow data before the result payload. GameOver GSMs with empty annotation arrays continue to emit only the `GameResult` event; the `MatchState_MatchComplete` suppression branch is unchanged (#196, #197).

### Removed

- **Dead `EntryHeader::ClientGre` variant.** `[Client GRE]` never appears in real
  MTGA logs (zero occurrences across 44 sessions / 606k lines); the variant was
  speculative scaffolding. Removed from the `EntryHeader` enum and all five match
  arms (#207, #209).
- **Three zero-corpus-hit parser markers and their dead code paths**:
  `Updated account. DisplayName:` account-update parsing (`authenticateResponse`
  is the real identity source), `LogBusinessEvents`, and `PickGrpId`. All had zero
  occurrences across the corpus (#205).

## [0.3.0] - 2026-05-13

### Changed

- **`LineBuffer` now flushes multi-line entries on JSON brace-balance** instead of waiting for the next header (#193, #194). Entries containing a `{` are emitted the moment their JSON body's depth returns to 0, dropping draft-event and other interactive-flow latency from seconds-to-minutes to one polling cycle (~50 ms). Public method signatures are unchanged — but downstream consumers that previously batched entries between headers will now receive them sooner.
- Non-JSON multi-line entries (`[Message summarized…]` GRE markers, `true`-bodied REST responses) keep the original "flush on next header" behavior as a deterministic fallback.

### Added

- New `brace_depth_flush` cargo feature, **default-on**, gating the new flush trigger. Disabling the feature reverts to the original next-header flush behavior — kept as a one-flip rollback in case a live-Arena edge case surfaces.
- New `BraceState` internal struct with a string-literal + escape-aware state machine; handles nested-JSON-in-string values, escaped quotes, escaped backslashes, and brace noise inside string literals.
- New `test-no-default-features` CI job (`.github/workflows/ci.yml`) so the rollback path cannot bit-rot.
- `proptest = "1"` added to `[dev-dependencies]` for state-machine property tests (3 generators + 6 corpus-derived regression cases).

### Fixed

- Smoke-test CI step now uses `set -o pipefail` so failures inside the piped `cargo test … | tee` no longer silently pass (#191, #192).

## [0.2.2] - 2026-05-07

### Added

- New `game_state_type` field on `game_state_message` and `queued_game_state_message` payloads, sourced from the inner `gameStateMessage.type` (`GameStateType_Full` / `GameStateType_Diff`). Field is always present — `None` serializes to JSON `null` — so the schema contract is unambiguous (#182, #183).

### Changed

- `Makefile` added with `precommit`, `precommit-trivial`, `coverage`, and `fmt` targets (#186)
- CI `check` job updated to call `make precommit` instead of inlining gate steps
- CLAUDE.md pre-commit checklist updated to reference `make precommit`

## [0.2.1] - 2026-04-30

### Added

- `DeckCollection` event variant for `StartHook` deck snapshots, correlating `DeckSummaries` metadata with `Decks` payloads by `DeckId`
- Ordered emit for `ConnectResp` GRE messages
- `SubmitDeckResp` Bo3 round-trip support
- Typed `Designation` annotation extractor
- Typed `Shuffle` annotation extractor

### Fixed

- Restored `BotDraft` parsing for modern API log signatures

## [0.2.0] - 2026-04-25

### Changed

- **Breaking:** `LineBuffer::push_line` now returns `Vec<LogEntry>` instead of `Option<LogEntry>`, so single-line entries can flush in the same call that produced them when a prior multi-line entry also needs to flush

### Removed

- **Breaking:** Dead `StartHook` `PlayerCards` collection-event path; inventory is the only supported `StartHook` snapshot

### Fixed

- Silenced routine post-flush "headerless line" warnings; the warning now fires only for true file-start / post-rotation anomalies

## [0.1.2] - 2026-04-15

### Added

- Installation instructions in README for crates.io consumers

## [0.1.1] - 2026-04-15

### Added

- **Parsers** for all Arena log event categories: session, match state, GRE (connect, game state messages, annotations), client actions, game results, bot draft, human draft, rank, inventory, collection, event lifecycle
- **Async event bus** with broadcast fan-out for real-time event delivery
- **File tailer** with polling-based log following
- **Multi-platform log discovery** for Windows and macOS
- **Timestamp parser** with Arena-specific format handling
- **Line buffer** with log entry header detection and reassembly
- **Performance-class router** for fast dispatch to the correct parser
