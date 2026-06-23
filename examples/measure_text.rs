//! Demonstrates CC Ink-style text measurement.
//!
//! `expand_tabs`, `line_width`, `widest_line`, and `measure_text` mirror the
//! fork's `tabstops.ts`, `line-width-cache.ts`, `widest-line.ts`, and
//! `measure-text.ts`: they use terminal display width for tabs, ANSI/control
//! sequences, wide graphemes, and wrapping while preserving trailing newline rows.

use iocraft::prelude::*;

fn main() {
    println!("expanded tabs: {:?}", expand_tabs("a\tb\n\tindented"));
    println!("line width: {}", line_width("a\tb\x1b[31mred\x1b[0m"));
    println!("widest line: {}", widest_line("short\n中中\n"));
    println!();

    for (label, text, max_width) in [
        ("plain", "hello", None),
        ("trailing newline", "hello\n", None),
        ("wrapped wide", "中中a", Some(3)),
        ("tabs + ANSI", "a\tb\x1b[31mred\x1b[0m", Some(5)),
    ] {
        let measured = measure_text(text, max_width);
        println!(
            "{label:16} max_width={max_width:?} -> width={} height={}",
            measured.width, measured.height
        );
    }
}
