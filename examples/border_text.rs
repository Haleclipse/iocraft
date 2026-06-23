//! Demonstrates CC Ink-style border text labels.

use iocraft::prelude::*;

fn main() {
    element! {
        View(width: 42, flex_direction: FlexDirection::Column, row_gap: 1) {
            View(
                width: 100pct,
                border_style: BorderStyle::Round,
                border_top_color: Some(Color::Cyan),
                border_bottom_color: Some(Color::DarkCyan),
                border_left_dim_color: Some(true),
                border_right_dim_color: Some(true),
                border_text: Some(BorderText {
                    content: "\x1b[1mStatus\x1b[0m".to_string(),
                    position: BorderTextPosition::Top,
                    align: BorderTextAlign::Center,
                    offset: 0,
                }),
                padding: 1,
            ) {
                Text(content: "Border labels match CC Ink borderText.")
            }
            View(
                width: 100pct,
                border_style: BorderStyle::Dashed,
                border_top: false,
                border_text: Some(BorderText {
                    content: "ready".to_string(),
                    position: BorderTextPosition::Bottom,
                    align: BorderTextAlign::End,
                    offset: 2,
                }),
                padding: 1,
            ) {
                Text(content: "Bottom/end aligned labels are supported too.")
            }
        }
    }
    .print();
}
