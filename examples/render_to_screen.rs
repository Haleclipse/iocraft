//! Demonstrates CC Ink-style off-terminal `render_to_screen(...)`.
//!
//! This is useful for side-rendered search/highlight work: render a subtree at a
//! fixed terminal width, inspect its natural height, and scan exact terminal-cell
//! match positions without entering fullscreen or writing terminal modes.

use iocraft::prelude::*;

fn main() {
    let mut message = element! {
        View(flex_direction: FlexDirection::Column) {
            Text(content: "The quick brown fox")
            Text(content: "jumps over the lazy dog")
        }
    };

    let screen = message.render_to_screen(28);
    println!(
        "height={} matches={:?}",
        screen.height,
        screen.scan_positions("lazy")
    );
    print!("{}", screen.canvas);
}
