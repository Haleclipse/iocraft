//! Demonstrates CC Ink-style clamping for absolute overlays above the viewport.
//!
//! When an absolute-positioned menu/tooltip computes a negative screen-space Y,
//! iocraft mirrors CC Ink's `render-node-to-output.ts` and starts rendering it at
//! the top of the canvas instead of clipping away its first rows.

use iocraft::prelude::*;

fn main() {
    element! {
        View(width: 32, height: 6, margin_top: 1, border_style: BorderStyle::Round) {
            Text(content: "The overlay starts above this box.")
            View(
                position: Position::Absolute,
                top: -3,
                left: 2,
                width: 22,
                padding_x: 1,
                background_color: Color::Reset,
            ) {
                Text(content: "menu top\nstays visible", wrap: TextWrap::NoWrap)
            }
        }
    }
    .print();
}
