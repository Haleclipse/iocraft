//! Demonstrates the opt-in raw-stdin frontend bridge.
//!
//! `TerminalRawInputFrontend` is for custom frontends that own raw stdin. It
//! does not enable raw mode or replace iocraft's default crossterm backend; it
//! only turns raw byte chunks into parsed CC Ink-style input plus iocraft
//! `TerminalEvent`s and tells the caller when to flush incomplete ESC/CSI input.

use iocraft::prelude::*;

fn main() {
    let mut frontend = TerminalRawInputFrontend::new();

    let output = frontend.feed_bytes(b"typed\x1b[?2026;1$y\x1b[200~paste\x1b[201~\x1b[<64;12;3M");
    for parsed in &output.parsed {
        println!("parsed: {parsed:?}");
    }
    for event in &output.events {
        println!("iocraft event: {event:?}");
    }

    let output = frontend.feed("\x1b[");
    println!("pending flush timeout: {:?}", output.pending_flush_timeout);

    // In a real backend, pass `true` if the raw input source reports queued
    // bytes. CC Ink re-arms the timer in that case to avoid splitting delayed
    // mouse/CSI sequences.
    if let Some(flushed) = frontend.flush_if_due(false) {
        println!("flushed incomplete input: {:?}", flushed.parsed);
    }
}
