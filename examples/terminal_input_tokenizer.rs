//! Demonstrates the raw terminal input tokenizer and CC Ink-style parser.
//!
//! This is useful for custom frontends that read raw stdin themselves and need
//! to separate terminal query replies, bracketed paste payloads, mouse reports,
//! and key/input events before forwarding them into iocraft. Use `feed(...)`
//! when you want exact CC Ink-style parsed input, or `feed_events(...)` when
//! you want crossterm-like `TerminalEvent`s for iocraft hooks.

use iocraft::prelude::*;

fn main() {
    let mut parser = TerminalInputParser::new();
    let chunks = [
        "typed ",
        "\x1b[?2026;1$y",
        "\x1b[200~pasted\x1b[31m text\x1b[201~",
        "\x1b[<0;12;3M",
        "\x1b[<64;12;3M",
        "\x1b[13;2u",
        "\x1bP>|xterm.js(5.5.0)\x1b\\",
    ];

    for chunk in chunks {
        for input in parser.feed(chunk) {
            match input {
                TerminalParsedInput::Text(text) => println!("text: {text:?}"),
                TerminalParsedInput::Sequence(sequence) => {
                    let event = parse_terminal_input_event(&sequence);
                    println!("escape sequence: {sequence:?} parsed-input-event={event:?}")
                }
                TerminalParsedInput::Key(event) => println!("key: {event:?}"),
                TerminalParsedInput::Paste(text) => println!("paste: {text:?}"),
                TerminalParsedInput::Mouse(mouse) => println!("mouse: {mouse:?}"),
                TerminalParsedInput::Response(response) => {
                    println!("terminal response: {response:?}")
                }
            }
        }
    }

    for token in parser.flush() {
        println!("flushed: {token:?}");
    }

    let mut event_parser = TerminalInputParser::new();
    for event in event_parser.feed_events("hi\x1b[A\x1b[200~paste\x1b[201~") {
        println!("iocraft event: {event:?}");
    }

    // CC Ink's Buffer path treats a single high-bit byte as an ESC-prefixed
    // Meta key. 0xe1 becomes ESC + 'a'.
    let mut byte_parser = TerminalInputParser::new();
    for input in byte_parser.feed_bytes(&[0xe1]) {
        println!("byte input: {input:?}");
    }

    let mut incomplete = TerminalInputParser::new();
    incomplete.feed("\x1b[");
    println!(
        "pending flush timeout: {:?}, flush now: {}",
        incomplete.pending_flush_timeout(),
        incomplete.should_flush_incomplete(false),
    );
}
