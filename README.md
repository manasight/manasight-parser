> **This project is in active development.** APIs may change without notice.

# manasight-parser

**manasight-parser** is the log parsing engine behind [Manasight](https://manasight.gg), an MTG Arena companion app.

MTG Arena log file parser — a Rust library crate that reads Arena's `Player.log` and emits typed game events. It runs **natively** — tailing a live log through an async event bus — or compiles to **WebAssembly** to parse a whole log in the browser or in Node.

## Installation

```sh
cargo add manasight-parser
```

Or in `Cargo.toml`:

```toml
[dependencies]
manasight-parser = "0.6"
```

Requires Rust 1.93.0 or later.

## Architecture

```text
Player.log → File Tailer → Entry Buffer → Router → Parsers → Event Bus
```

- **`log`** — file discovery, polling tailer, entry accumulation, timestamps
- **`router`** — dispatches raw entries to the correct category parser
- **`parsers`** — one sub-module per event category
- **`events`** — public event type enums/structs (the parser's output contract)
- **`event_bus`** — `tokio::broadcast` channel for fan-out to subscribers
- **`stream`** — public entry point (`MtgaEventStream`)
- **`sanitize`** — privacy scrubber for redacting PII from raw log text
- **`util`** — pipeline utilities (gzip compression, content hashing)
- **`wasm`** — `wasm-bindgen` export of the synchronous parser (enabled by the `wasm` feature)

The async pipeline above powers the live desktop overlay. The synchronous [`parse_whole_log`](#whole-log-parsing-synchronous) entry point — and its `wasm` export — reuse the same **Router → Parsers** core directly, bypassing the tailer and event bus (no `tokio`, no filesystem).

## Usage

manasight-parser has two entry points: a **streaming** tailer for live logs, and a **synchronous whole-log** parser (the basis for the WASM build).

### Streaming (live `Player.log`)

`MtgaEventStream` (the default `tailer` feature) tails a live log and fans out events to subscribers as they arrive:

```rust
use std::path::Path;
use manasight_parser::MtgaEventStream;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (stream, mut subscriber) = MtgaEventStream::start(Path::new("Player.log")).await?;

    while let Some(event) = subscriber.recv().await {
        println!("got event: {event:?}");
    }

    Ok(())
}
```

### Whole-log parsing (synchronous)

`parse_whole_log` parses an entire `Player.log` string in one synchronous call — no async runtime, no filesystem. It is always available (independent of the `tailer` feature) and is the exact entry point the [WASM build](#wasm--wasm-bindgen-build) exposes to JavaScript:

```rust
use manasight_parser::parse_whole_log;

fn main() -> std::io::Result<()> {
    let text = std::fs::read_to_string("Player.log")?;
    let events = parse_whole_log(&text);
    println!("parsed {} events", events.len());
    Ok(())
}
```

## Cargo Features

| Feature | Default | Description |
|---------|---------|-------------|
| `brace_depth_flush` | ✓ | Flush multi-line entries on JSON brace-balance instead of waiting for the next header. Disable as a rollback mechanism if a regression is detected. |
| `tailer` | ✓ | File tailer, log discovery, async event stream (`MtgaEventStream`), and event bus. Pulls in `tokio` and `known-folders`. Disable to build the pure-sync WASM-compatible subset. |
| `wasm` | | wasm-bindgen export of `parseWholeLog` and `StreamingParser` for use in a browser or Node Parse Worker. Implies `brace_depth_flush` and `lean` (omits `raw_bytes` + heavy payload fields such as annotations, timers, zones, and deck lists to reduce WASM memory pressure). Does **not** pull in `tailer` (no tokio/known-folders). |

### WASM / wasm-bindgen build

The `wasm` feature wraps the synchronous [`parse_whole_log`](#whole-log-parsing-synchronous) in a [`wasm-bindgen`](https://rustwasm.github.io/wasm-bindgen/) export, producing a loadable `.wasm` artifact plus JS/TypeScript bindings via [`wasm-pack`](https://rustwasm.github.io/wasm-pack/). The *same* parser core then runs in a browser or in Node — no `tokio`, no filesystem, no async runtime.

```bash
# Install wasm-pack (once) — pin the version for reproducible builds
cargo install wasm-pack --locked --version 0.15.0

# Build for a bundler (webpack / Vite) — PRIMARY/recommended target:
wasm-pack build --target bundler --no-default-features --features wasm

# For a Node.js consumer:
wasm-pack build --target nodejs --no-default-features --features wasm

# For direct <script type="module"> usage (no bundler):
wasm-pack build --target web --no-default-features --features wasm
```

Consume the generated bindings from JavaScript / TypeScript:

```js
import init, { parseWholeLog } from "./pkg/manasight_parser.js";

await init();                       // instantiate the .wasm module (web / bundler targets)
const text = await readPlayerLog(); // the full Player.log contents, as a string

let events;
try {
  events = parseWholeLog(text);     // GameEvent[] — see "Event Types" below
} catch (err) {
  // parseWholeLog throws only if serialisation to JS fails (extremely unlikely)
  console.error("parse failed:", err);
}
```

**Return type:** at runtime `parseWholeLog` returns a `GameEvent[]` — a live JS object graph (via [`serde-wasm-bindgen`](https://github.com/RReverser/serde-wasm-bindgen)), **not** a JSON string, so there's no second `JSON.parse`. Each element is an externally-tagged enum, e.g. `{ "GameState": { … } }` (see [Event Types](#event-types)). Because the wrapper hands back a `JsValue`, the generated `.d.ts` types the return as `any` and the call can throw — TypeScript consumers will likely want to layer their own types and a `try`/`catch`.

**Distribution:** npm publishing / packaging is intentionally out of scope for this library. The consuming application (e.g. a Parse Worker) vendors or packages the built `pkg/` artifact.

## Log Sanitization

The `sanitize` module strips PII and credentials from raw `Player.log` text before it leaves the user's machine. It redacts auth tokens, bearer tokens, account IDs, display names, session identifiers, OS user paths, and hardware fingerprints. Free-text deck names are pseudonymized by default to a deterministic `Deck-<8hex>` label derived from the deck's ID (opt out per-call via `ScrubOptions::keep_deck_names`), preserving deck structure while keeping the same deck correlatable across a session.

```rust
use manasight_parser::sanitize::scrub_raw_log;

let raw = std::fs::read_to_string("Player.log").unwrap();
let clean = scrub_raw_log(&raw);
// clean contains no tokens, account IDs, or user paths
```

Pipeline utilities for compression and content-addressable storage:

```rust
use manasight_parser::util::{compress_log, content_hash};

let compressed = compress_log(&clean).unwrap();
let hash = content_hash(&compressed); // 64-char hex SHA-256
```

### CLI

The `scrub` binary reads stdin and writes sanitized output to stdout:

```sh
cargo run --bin scrub < Player.log > Player-sanitized.log
```

## Event Types

| Event | Description | Class |
|-------|-------------|-------|
| `GameState` | GRE-to-client messages (zones, game objects, turns) | Interactive |
| `ClientAction` | Client-to-GRE messages (mulligan, select, deck submit) | Interactive |
| `MatchState` | Match room state changes (start, end, player seats) | Interactive |
| `DraftBot` | Bot draft picks (Quick Draft) | Durable |
| `DraftHuman` | Human draft picks (Premier/Traditional Draft) | Durable |
| `DraftComplete` | Draft completion signal | Durable |
| `EventLifecycle` | Event join, claim prize, enter pairing | Durable |
| `Session` | Login, account identity, logout | Durable |
| `Rank` | Constructed and limited rank snapshots | Durable |
| `DeckCollection` | Deck collection snapshots with correlated decklists | Durable |
| `Inventory` | Currency, wildcards, boosters, vault progress | Durable |
| `DeckSubmission` | Submitted deck Format + DeckId from `EventSetDeck` requests | Durable |
| `CourseDeck` | Registered deck identity (name, format, maindeck hash) per active event Course | Durable |
| `GameResult` | Game result / batch trigger | Post-game |

### Performance Classes

- **Interactive** (Class 1): local-only processing, ≤100ms latency target
- **Durable** (Class 2): persisted to disk queue, ≤1s latency target
- **Post-game** (Class 3): triggers assembly and upload of game batch

## Minimum Supported Rust Version

MSRV is 1.93.0.

## Contributing

Contributions are welcome! See [CONTRIBUTING.md](CONTRIBUTING.md) for guidelines on reporting bugs, submitting pull requests, and setting up a development environment.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or <http://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or <http://opensource.org/licenses/MIT>)

at your option.

> This project is not affiliated with, endorsed by, or associated with Wizards of the Coast, Hasbro, or Magic: The Gathering Arena. All trademarks are the property of their respective owners.
