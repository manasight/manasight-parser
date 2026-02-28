# Manasight Parser — Repo Instructions

> Project-level context (overview, policies, workflow) is in the parent `../CLAUDE.md`.

## 1. Purpose

Open-source Rust library crate for parsing MTG Arena's `Player.log` into typed game events. Consumed by `manasight-desktop` as a git dependency. No Tauri dependency, no runtime initialization — runs on the caller's async runtime.

---

## 2. Quick Reference

**Common Commands:**
```bash
# Fast compile check (use this for iteration, NOT cargo build)
cargo check

# Run all tests
cargo test --all-features

# Lint (REQUIRED before commit)
cargo clippy --all-targets --all-features -- -D warnings

# Format (REQUIRED before commit)
cargo fmt --all              # Auto-format
cargo fmt --all -- --check   # Check only

# Coverage
cargo tarpaulin --all-features --ignore-tests

# Build
cargo build
```

**Key Files:**
- `src/lib.rs` — Public API surface, module declarations
- `src/events.rs` — Public event type enums and structs
- `src/event_bus.rs` — Async broadcast channel
- `src/router.rs` — Raw entry to parser dispatch
- `src/util.rs` — Shared `pub(crate)` helper functions
- `src/log/` — Log file discovery, tailing, entry parsing, timestamps
- `src/parsers/` — One parser per event category
- `tests/fixtures/` — Sanitized Player.log snippets for integration tests

---

## 3. Pre-Commit Checklist (CRITICAL)

- [ ] Code formatted: `cargo fmt --all`
- [ ] Format verified: `cargo fmt --all -- --check`
- [ ] Clippy clean: `cargo clippy --all-targets --all-features -- -D warnings`
- [ ] Tests pass: `cargo test --all-features`
- [ ] No `.unwrap()` in production code
- [ ] No `dbg!()`, `todo!()`, or `println!()` in production code
- [ ] **New/updated tests** for every code change
- [ ] All files staged: `git add :/ && git status`

---

## 4. Coding Conventions

**Rust conventions**: See `../manasight-docs/docs/conventions/rust.md` for full details.

Critical build-breaker reminders (enforced by clippy deny lints):
- **No** `.unwrap()`, `.expect()`, `panic!()`, `todo!()`, `dbg!()`, `println!()` in production code
- **No** `#[allow(clippy::...)]` lint suppressions — fix the code instead

### Logging
- Follow the Rust logging policy: `../manasight-docs/docs/conventions/rust-logging.md`
- **Library crate rule**: Must NOT initialize a logger — use `log` facade only
- Internal code must use `::log` (e.g., `::log::info!()`) because `pub mod log` shadows the crate

### Shared Helpers
- Place `pub(crate)` utility functions in `src/util.rs` — do not duplicate helpers across modules

### Library Crate Rules
- Must NOT depend on Tauri or any desktop-specific crates
- Must NOT start a Tokio runtime — runs on the caller's runtime
- All public items need `///` doc comments
- Each module needs `//!` module-level doc comment

---

## 5. Testing Policy

### Running Tests
```bash
cargo test --all-features        # All tests
cargo test test_name              # Single test
cargo test module::               # All tests in module
```

### Test Organization
- **Unit tests**: In-module `#[cfg(test)] mod tests` blocks in each source file
- **Integration tests**: `tests/` directory for cross-module tests
- **Test fixtures**: `tests/fixtures/` for sanitized Player.log snippets
- **Test naming**: `test_<function>_<scenario>_<expected>`

### Coverage
- `cargo tarpaulin --all-features --ignore-tests` — **80% minimum**
- Test behavior, not implementation

---

## 6. Architecture References

- Feature spec: `../manasight-docs/docs/requirements/feature-specs/log-file-parser.md`
- Crate splitting research: `../manasight-docs/docs/research/2026-02-23_crate-splitting-strategies.md`
- System architecture: `../manasight-docs/docs/architecture/overview.md`
- Coding standards: `../manasight-docs/docs/conventions/coding-standards.md`
