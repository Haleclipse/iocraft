//! Demonstrates CC Ink-style ANSI parsing into styled text segments.

use iocraft::prelude::*;

fn main() {
    element! {
        View(width: 36, flex_direction: FlexDirection::Column) {
            Text(content: "ANSI component", weight: Weight::Bold)
            Ansi(content: "\x1b[31;1mred bold\x1b[0m plain \x1b[3mitalic\x1b[0m")
            View(width: 20) {
                Ansi(content: "\x1b[32mwrapped ansi text keeps styles\x1b[0m")
            }
            Ansi(content: "\x1b[38:2::255:128:0mcolon truecolor\x1b[0m")
            Ansi(content: "DCS \x1bPnot rendered\x1b\\strings are skipped")
            Ansi(content: "\x1b]8;;https://example.com\x07\x1b[34mOSC 8\x1b[0m link survives reset\x1b]8;;\x07".to_string())
        }
    }
    .print();
}
