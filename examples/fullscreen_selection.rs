//! Interactive fullscreen selection + screen-buffer demo.
//!
//! Run with:
//!
//! ```text
//! cargo run --example fullscreen_selection
//! ```
//!
//! This combines retained screen-buffer primitives with a real fullscreen
//! mouse/keyboard event loop: selection overlays, search highlight, noSelect
//! gutters, soft-wrap copy, hyperlink fallback, and copy/copy-on-select status
//! prompts.
//!
//! By default clipboard writes are simulated so the example does not modify your
//! clipboard. Set `IOCRAFT_EXAMPLE_WRITE_CLIPBOARD=1` to enable OSC 52 writes.

mod selection_demo;

fn main() {
    selection_demo::main();
}
