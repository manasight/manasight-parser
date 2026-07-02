# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Added

- **Richer deck identity in the event stream.** `GameEvent::DeckSubmission`
  now surfaces `name` (the deck's registered name) and `maindeck_hash` (a
  canonical SHA-256 digest of the submitted `MainDeck`) alongside the
  existing `deck_id`/`deck_format`/`event_name`/`is_singleton` fields.
- **`GameEvent::CourseDeck`** — a new event parsed from `<== EventGetCoursesV2`
  responses, emitting one event per active Course carrying a registered
  deck: `{ type, deck_id, name, format, maindeck_hash, internal_event_name,
  course_id }`. Covers limited runs and app-started-mid-event recovery where
  no deck-submission request exists in the current log session. Courses with
  no registered deck yet (mid-draft/pre-deckbuild) are skipped.
- `maindeck_hash` is a cross-event contract: both `DeckSubmission` and
  `CourseDeck` compute it via the same canonicalization (entries sorted by
  `cardId` ascending, serialized `cardId:quantity` joined with `;`, SHA-256
  hex) so downstream consumers can compare deck identity across the two
  event types.

  This adds a variant to the lean output → consumers re-parse.
- **`ScrubOptions::keep_deck_names`** — new toggle (default `false`) that
  controls whether free-text deck names in scrubbed log output are
  pseudonymized. By default, deck `Name` fields are replaced with a
  deterministic `Deck-<first 8 hex of DeckId>` label, preserving
  `DeckId`/`Format`/`Attributes`/card-list structure so parsing and corpus
  greps still work; the same `DeckId` always maps to the same label, so
  rename chains and cross-carrier occurrences of one deck stay correlatable
  after scrubbing. Set `keep_deck_names: true` to pass deck names through
  untouched (all other patterns still apply). Mirrors the existing
  `keep_player_names` toggle precedent.

### Changed

- **BREAKING:** `ScrubOptions` gains a new public field
  (`keep_deck_names: bool`). Since `ScrubOptions` is not `#[non_exhaustive]`,
  any exhaustive struct literal (`ScrubOptions { keep_player_names: ... }`
  without `..Default::default()`) no longer compiles and must add the new
  field, or switch to `..Default::default()`.

### Fixed

- **Scrubber: CRLF-terminated lines bypassing `$`-anchored scrub patterns.**
  The pet-diagnostic deck-name line and the macOS Metal enumerated-device
  hardware-fingerprint line anchored on a bare `\)$`; on `\r\n`-terminated
  log lines (standard for Windows-written `Player.log` files) the `\r`
  before the newline broke the match, leaving the field unscrubbed under
  default options. Both patterns now capture-and-re-emit an optional
  trailing `\r` so CRLF lines are scrubbed without normalizing the line
  ending to LF-only. A repo-wide sweep confirmed these were the only two
  `$`-anchored scrub patterns exposed to this issue. Verified against
  the `manasight-corpus` real logs (41/55 files are CRLF-terminated;
  198 pet-diagnostic deck-name lines leaked before this change; 0 after).

## [0.6.3] - 2026-06-26

### Fixed

- **Scrubber: redact hardware fingerprints** previously left in sanitized
  output. The Unity `SystemInfo` hardware block (`graphicsDeviceName`,
  `graphicsDeviceVendor`, `graphicsDeviceVersion`, `deviceModel`,
  `operatingSystem`, `processorType`) on both macOS and Windows, plus the macOS
  Metal `GfxDevice` block, are now redacted. Found and verified against the
  `manasight-corpus` real logs (20/55 files leaked GPU/hardware identifiers
  before this change; 0 after) ([#259], [#260]).

## [0.6.2] - 2026-06-24

### Added

- **`GameEvent::LocalSeat`** — a new event that surfaces the local player's
  seat from the wrapper `systemSeatIds` of a client-directed GRE message (a
  singleton `[N]`). The local seat was previously recoverable only via
  `connect_resp.system_seat_ids`; when the client summarizes a `ConnectResp`
  (a game state exceeding its internal GameObject limit), no `connect_resp`
  event is emitted for the match and that seat channel is lost — leaving a
  match with an authoritative outcome unresolvable. The new event recovers the
  seat from the surrounding GRE envelopes, which still carry it (valid
  envelopes flank the `Truncation` marker). Payload: `{"system_seat_id": N}`.
  Emitted (idempotently) per client-directed entry carrying a singleton
  wrapper seat — the parser is stateless per entry, so consumers associate it
  to a match via event ordering relative to `match_started` /
  `match_completed`, exactly as they already do for the seatless
  `connect_resp` channel. Existing event variants are unchanged.

  This adds a variant to the lean output → consumers re-parse.

## [0.6.1] - 2026-06-22

### Added

- **`StreamingParser`** (`wasm` feature) — an incremental parse API
  (`pushChunk` / `finish`) so consumers can stream very large logs at bounded
  memory instead of materializing the whole log at once. Backed by a
  host-testable `StreamingParserCore`; feeding a log in arbitrary byte chunks
  yields event-for-event equality with `parse_whole_log` (`pushChunk` replicates
  `str::lines()` exactly, including CRLF and split-boundary handling)
  ([#254], [#255]).
- **`lean` Cargo feature** — omits `raw_bytes` from `EventMetadata` (retaining
  `raw_bytes_hash` for server-side deduplication) and drops never-read payload
  fields (GameState `annotations` / `game_objects` / `zones` / `timers` /
  `persistent_annotations`, the `DeckCollection` deck lists, and the inner
  payload of never-consumed event variants) to minimize WASM memory and
  bandwidth. Automatically enabled by the `wasm` feature; the default/native
  build is byte-identical ([#254], [#255]).

### Changed

- Documentation: reconciled the WASM build command to `wasm-pack build
  --target bundler` and documented the `lean` payload omissions across the
  module, parser, and `README` docs ([#254], [#255]).
- Internal dependency and CI bumps: `wasm-bindgen-test` 0.3.63 → 0.3.75
  ([#250]), `log` 0.4.32 → 0.4.33 ([#249]), `actions/checkout` 4 → 7 ([#247]),
  and `rust-lang/crates-io-auth-action` 1.0.4 → 1.0.5.

### Notes

- This release is **additive and backward-compatible**. The default (non-`lean`)
  serialized output is unchanged; `lean` field omission and the `StreamingParser`
  API are opt-in via the `wasm`/`lean` features.

[#247]: https://github.com/manasight/manasight-parser/pull/247
[#249]: https://github.com/manasight/manasight-parser/pull/249
[#250]: https://github.com/manasight/manasight-parser/pull/250
[#254]: https://github.com/manasight/manasight-parser/issues/254
[#255]: https://github.com/manasight/manasight-parser/pull/255

## [0.6.0] - 2026-06-20

### Added

- **`EventMetadata::instant_utc()`** — returns the true UTC instant for events
  carrying an embedded epoch-millisecond `"timestamp"` (currently
  match-lifecycle events from `matchGameRoomStateChangedEvent`). Unlike the
  log-header timestamp, this is timezone-independent and correct regardless of
  the host's local zone (including the `wasm`/headless build). Returns `None`
  for events without an embedded epoch-ms field ([#251], [#252]).
- **`EventMetadata::local_timestamp()`** — returns the log-header timestamp as a
  zone-less `chrono::NaiveDateTime`, the honest representation of MTGA's local
  wall-clock header value ([#251], [#252]).
- **`EventMetadata::with_instant()`** — constructor that accepts both the header
  timestamp and an `instant_utc` value ([#251], [#252]).

### Changed

- **`EventMetadata::timestamp()` is now `#[deprecated]`.** MTGA log-header
  timestamps are local wall-clock time mislabeled as UTC (the inner
  `NaiveDateTime` is promoted with `.and_utc()`). Use `instant_utc()` for the
  absolute UTC instant, or `local_timestamp()` for the honest zone-less local
  value. The accessor still works unchanged; this release only adds the
  deprecation warning ([#251], [#252]).
- Corrected the `src/log/timestamp.rs` documentation, which previously claimed
  log-header timestamps were UTC ([#251], [#252]).

### Notes

- This release is **additive and backward-compatible**. Serialized payloads
  round-trip with and without the new `instant_utc` field (`#[serde(default)]`).
  Wiring `instant_utc` for `.NET-ticks` events (e.g. `client_actions`) is
  deferred to a future release.

[#251]: https://github.com/manasight/manasight-parser/issues/251
[#252]: https://github.com/manasight/manasight-parser/pull/252

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
