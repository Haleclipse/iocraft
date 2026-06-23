//! Demonstrates the `NoSelect` wrapper for fullscreen selection metadata.
//!
//! Gutters such as line numbers and diff sigils stay visible, but selection
//! copy/highlight/search skip them so copied text is clean.

use iocraft::prelude::*;
use std::io;

fn main() -> io::Result<()> {
    let mut row = element! {
        View(flex_direction: FlexDirection::Row) {
            NoSelect(from_left_edge: true) {
                Text(color: Color::DarkGrey, content: " 42 + ")
            }
            Text(content: "let answer = 42;")
        }
    };
    let canvas = row.render(Some(40));
    let selected = canvas.selected_text(SelectionRange::new(
        SelectionPoint { col: 0, row: 0 },
        SelectionPoint { col: 39, row: 0 },
    ));

    println!("Rendered row:");
    canvas.write(&mut io::stdout())?;
    println!("\nselected text skips NoSelect gutter: {selected:?}");
    println!(
        "noSelect columns 0..6: {:?}",
        (0..6)
            .map(|col| canvas.is_no_select(col, 0))
            .collect::<Vec<_>>()
    );
    Ok(())
}
