# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [0.2.2] - 2026-05-07

### Changed

- `Makefile` added with `precommit`, `precommit-trivial`, `coverage`, and `fmt` targets
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
