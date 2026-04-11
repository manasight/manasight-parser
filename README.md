> **This project is in active development.** APIs may change without notice.

# manasight-parser

**manasight-parser** is the log parsing engine behind [Manasight](https://manasight.gg), an MTG Arena companion app.

MTG Arena log file parser — a Rust library crate that reads Arena's `Player.log` and emits typed game events via an async event bus.

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

## Usage

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

## Log Sanitization

The `sanitize` module strips PII and credentials from raw `Player.log` text before it leaves the user's machine. It redacts auth tokens, bearer tokens, account IDs, display names, session identifiers, OS user paths, and hardware fingerprints.

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
| `Collection` | Card collection snapshot | Durable |
| `Inventory` | Currency, wildcards, boosters, vault progress | Durable |
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
