//! CLI tool that reads raw MTGA log text from stdin, redacts PII, and writes
//! the sanitized output to stdout.
//!
//! # Usage
//!
//! ```sh
//! cargo run --bin scrub < Player.log > Player-sanitized.log
//! ```

use std::error::Error;
use std::io::{self, Read, Write};

fn main() -> Result<(), Box<dyn Error>> {
    let mut input = String::new();
    io::stdin().read_to_string(&mut input)?;

    let output = manasight_parser::scrub_raw_log(&input);
    io::stdout().write_all(output.as_bytes())?;

    Ok(())
}
