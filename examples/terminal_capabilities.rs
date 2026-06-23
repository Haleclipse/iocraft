//! Prints terminal capability gates shared with the CC Ink fork.
//!
//! These are intentionally small, synchronous checks. Higher-level components
//! use them to avoid terminal-specific rendering pitfalls such as Windows
//! cursor-up viewport yanks and to gate side-band progress/tab-status escapes.

use iocraft::prelude::*;

fn main() {
    println!(
        "cursor-up viewport yank bug: {}",
        has_cursor_up_viewport_yank_bug()
    );
    println!("OSC 8 hyperlinks supported: {}", supports_hyperlinks());
    println!(
        "DEC 2026 synchronized output supported: {}",
        is_synchronized_output_supported()
    );
    println!("extended keys supported: {}", supports_extended_keys());
    println!(
        "OSC 9;4 progress reporting available: {}",
        is_progress_reporting_available()
    );
    println!("OSC 21337 tab status enabled: {}", supports_tab_status());
    println!(
        "clear terminal sequence (escaped): {}",
        clear_terminal_sequence().escape_debug()
    );
    println!("xterm.js host: {}", is_xterm_js());
    println!(
        "XTVERSION query sequence (escaped): {}",
        xtversion_query_sequence().escape_debug()
    );
    println!(
        "OSC 11 color query sequence (escaped): {}",
        osc_color_query_sequence(11).escape_debug()
    );

    // TerminalQuerier mirrors CC Ink's timeout-free query batching: write the
    // query, then a DA1 flush sentinel. Parsed responses are fed back via
    // on_response; unanswered queries before the sentinel resolve as unsupported.
    // In a live render loop, Terminal::send_terminal_query starts the backend
    // event stream automatically; backends/custom frontends that surface
    // TerminalEvent::Response can resolve the same queue without a timeout.
    let mut querier = TerminalQuerier::new(Vec::new());
    let _pending_xtversion = querier.send(TerminalQuery::xtversion()).unwrap();
    let _flush = querier.flush().unwrap();
    println!(
        "querier batch (escaped): {}",
        String::from_utf8_lossy(querier.output_ref()).escape_debug()
    );
    println!(
        "sample parsed response: {:?}",
        parse_terminal_response("\x1bP>|xterm.js(5.5.0)\x1b\\")
    );

    let mut parser = TerminalResponseParser::new();
    let mut parsed = parser.feed("\x1bP>|xterm");
    parsed.extend(parser.feed(".js(5.5.0)\x1b\\"));
    println!("chunked response parser output: {parsed:?}");
}
