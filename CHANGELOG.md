# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

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
