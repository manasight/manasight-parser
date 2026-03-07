# Rust Coding Conventions

Applies to all Rust code in this repository.

## Formatting

- **rustfmt** with default settings
- Run `cargo fmt --all` before committing; verify with `cargo fmt --all -- --check`

## Linting

Clippy pedantic enabled as warnings, with key restriction lints **denied** (build failures). See `Cargo.toml` `[lints.clippy]` for the full configuration.

### Lint Suppression Policy

- **Never** add `#[allow(clippy::...)]` to suppress warnings — fix the underlying code
- `#[allow(unused)]` is acceptable only during active development and must be removed before PR merge

## Naming

Follow the [Rust API Guidelines](https://rust-lang.github.io/api-guidelines/naming.html):

| Element | Convention | Example |
|---------|-----------|---------|
| Functions, methods, variables | `snake_case` | `find_log_file` |
| Types, structs, enums, traits | `PascalCase` | `GameEvent`, `ParseError` |
| Constants | `SCREAMING_SNAKE_CASE` | `MAX_LINE_LENGTH` |
| Modules | `snake_case` | `game_state` |

## Error Handling

- Use `thiserror` for typed error enums
- **Never** use `.unwrap()` or `.expect()` in production code — use `?` operator
- `.unwrap()` is acceptable only in tests and test helpers
- Log errors at the point of recovery; propagate with `?` otherwise

## Imports

Group imports in this order, with a blank line between groups, alphabetized within each:

```rust
// 1. Standard library
use std::collections::HashMap;

// 2. External crates
use serde::{Deserialize, Serialize};

// 3. Local modules
use crate::events::GameEvent;
```

## Logging

This is a **library crate** — it must NOT initialize a logger. Use the `log` facade only; the consuming binary provides the subscriber.

Internal code must use `::log` (e.g., `::log::info!()`) because `pub mod log` shadows the `log` crate.

### Log Levels

| Level | Use When | Examples |
|-------|----------|----------|
| **ERROR** | Unrecoverable failure within the parser | Corrupt state, invariant violation |
| **WARN** | Something is wrong but parsing continues | Malformed entry skipped, unknown format |
| **INFO** | Lifecycle events | Game started/ended, log file found/rotated |
| **DEBUG** | Detail for troubleshooting | State machine transitions, parse attempts |
| **TRACE** | Firehose, development only | Raw log lines, individual field parsing |

### Never Log

At any level: auth tokens, credentials, session IDs.

At INFO+: raw `account_id` (use hash or omit), unsanitized file paths (replace home dir with `~`), raw Arena log lines (may contain tokens).

### Message Format

Human-readable phrase + `key=value` pairs:

```rust
::log::info!("Game ended: match_id={match_id}, turns={turns}, won={won}");
::log::warn!("Entry skipped: reason={reason}, line={line_num}");
```

## Documentation

- Use `///` doc comments for all public items
- Use `//!` module-level docs at the top of each file

## Testing

- **Unit tests**: In-module `#[cfg(test)] mod tests` blocks
- **Integration tests**: `tests/` directory
- **Test fixtures**: `tests/fixtures/` for sanitized Player.log snippets
- **Naming**: `test_<function>_<scenario>_<expected>`
- **Coverage**: 80% minimum via `cargo tarpaulin --all-features --ignore-tests`
- Test behavior, not implementation
