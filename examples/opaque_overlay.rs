//! Demonstrates CC Ink-style `opaque` overlays.
//!
//! `View(opaque: true)` clears the view's region with blank cells before
//! rendering children, hiding underlying siblings without changing the terminal
//! background color. This is useful for absolute-positioned menus/tooltips whose
//! padding and gaps should not be transparent.

use iocraft::prelude::*;

fn main() {
    element! {
        View(width: 48, height: 5) {
            Text(content: "This text is behind an opaque floating panel.")
            View(
                position: Position::Absolute,
                top: 1,
                left: 4,
                width: 32,
                height: 3,
                padding: 1,
                opaque: true,
                border_style: BorderStyle::Round,
            ) {
                Text(content: "Opaque overlay")
            }
        }
    }
    .print();
}
