//! Demonstrates CC Ink-style protection against zero-height sibling ghost text.
//!
//! If layout squeezes a child to height 0 and the next sibling lands on the same
//! row, iocraft skips painting the hidden child so a longer hidden line can't
//! leave stale tail glyphs behind the visible sibling.

use iocraft::prelude::*;

fn main() {
    element! {
        View(width: 12, height: 1, flex_direction: FlexDirection::Column) {
            View(height: 0) {
                Text(content: "hidden-tail")
            }
            Text(content: "visible")
        }
    }
    .print();
}
