//! Demonstrates the opt-in async-reader raw input bridge.
//!
//! The bridge is deliberately not wired to real stdin by default: applications
//! own raw-mode setup and the async reader, then pass byte chunks through the CC
//! Ink-compatible tokenizer/parser. The session event stream shown below scopes
//! terminal-side mode cleanup around the reader while keeping OS raw mode opt-in.

use futures::StreamExt;
use iocraft::prelude::*;

fn main() -> std::io::Result<()> {
    smol::block_on(async {
        let reader = futures::io::Cursor::new(b"hi\x1b[A".to_vec());
        let mut events =
            TerminalRawInputFallibleEventStream::from_reader_with_chunk_size(reader, 2);

        while let Some(event) = events.next().await {
            println!("{:?}", event?);
        }

        let session_options = TerminalRawInputSessionOptions {
            // Keep this example safe to run in any shell. A real caller-owned
            // stdin backend may set this to true while it owns stdin.
            enable_os_raw_mode: false,
            ..Default::default()
        };
        let reader = futures::io::Cursor::new(b"session\x1b[B".to_vec());
        let mut session = TerminalRawInputSessionEventStream::from_reader_with_chunk_size(
            Vec::new(),
            reader,
            3,
            session_options,
        )?;
        while let Some(event) = session.next().await {
            println!("session: {:?}", event?);
        }
        let session_bytes = session.exit()?;
        println!(
            "session guard bytes: {:?}",
            String::from_utf8_lossy(&session_bytes)
        );

        Ok(())
    })
}
