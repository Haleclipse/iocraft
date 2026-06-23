//! Demonstrates the opt-in raw terminal byte stream adapter.
//!
//! `TerminalRawInputEventStream` does not enable raw mode or read from stdin by
//! itself. Wrap it around a caller-owned byte source when you need CC Ink-style
//! byte tokenization, paste grouping, terminal-response parsing, mouse parsing,
//! and incomplete-sequence flush timing.

use futures::{stream, StreamExt};
use iocraft::prelude::*;

fn main() {
    let byte_chunks = stream::iter(vec![
        b"a\x1b[".to_vec(),
        b"A\x1b[200~pasted".to_vec(),
        b" text\x1b[201~".to_vec(),
    ]);

    let events = smol::block_on(TerminalRawInputEventStream::new(byte_chunks).collect::<Vec<_>>());
    for event in events {
        println!("{event:?}");
    }
}
