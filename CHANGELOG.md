# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [0.1.1] - 2026-04-15

### Added

- **Parsers** for all Arena log event categories: session, match state, GRE (connect, game state messages, annotations), client actions, game results, bot draft, human draft, rank, inventory, collection, event lifecycle
- **Async event bus** with broadcast fan-out for real-time event delivery
- **File tailer** with polling-based log following
- **Multi-platform log discovery** for Windows and macOS
- **Timestamp parser** with Arena-specific format handling
- **Line buffer** with log entry header detection and reassembly
- **Performance-class router** for fast dispatch to the correct parser
