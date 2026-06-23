//! Demonstrates CC Ink-style layout primitives and style shorthands.
//!
//! This example covers `Spacer`, `Newline`, `padding_x`/`padding_y`,
//! `margin_x`/`margin_y`, `gap` with axis overrides, and per-item
//! `align_self`.

use iocraft::prelude::*;

fn main() {
    element! {
        View(width: 40, flex_direction: FlexDirection::Column, padding_x: 1, padding_y: 1) {
            View(width: 100pct, flex_direction: FlexDirection::Row) {
                Text(content: "left")
                Spacer
                Text(content: "right")
            }
            Newline
            View(width: 100pct, flex_direction: FlexDirection::Row) {
                Text(content: "status")
                Spacer
                Text(content: "ready")
            }
            View(width: 100pct, flex_direction: FlexDirection::Row, gap: 4, column_gap: 2) {
                Text(content: "A")
                Text(content: "B")
                Text(content: "C")
            }
            View(
                width: 100pct,
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::FLEX_START,
                margin_y: 1,
            ) {
                View(width: 12, align_self: AlignSelf::FLEX_END) {
                    Text(content: "aligned")
                }
            }
        }
    }
    .print();
}
