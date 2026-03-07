> **This project is in active development.** APIs may change without notice.

# manasight-parser

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
