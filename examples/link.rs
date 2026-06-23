//! Demonstrates OSC 8 hyperlink metadata with the `Link` component.
//!
//! Terminals that support OSC 8 can open the rendered label. Fullscreen mouse
//! click handling can also read the same target from the retained screen buffer.
//! `Link` auto-detects support by default; this demo forces the first link on so
//! the retained metadata is visible in deterministic example output.

use iocraft::prelude::*;
use std::io;

fn main() -> io::Result<()> {
    let mut app = element! {
        View(flex_direction: FlexDirection::Column) {
            Link(
                url: "https://example.com/docs".to_string(),
                label: Some("Open docs".to_string()),
                enabled: Some(true), // force OSC 8 metadata for this deterministic demo
            )
            Link(
                url: "https://example.com/plain".to_string(),
                label: Some("Disabled OSC8".to_string()),
                fallback: Some("Plain fallback".to_string()),
                enabled: Some(false),
            )
        }
    };
    let canvas = app.render(Some(40));
    println!("hyperlink at row 0 col 1 = {:?}", canvas.hyperlink_at(1, 0));
    println!("hyperlink at row 1 col 1 = {:?}", canvas.hyperlink_at(1, 1));
    println!("ANSI dump:\n");
    canvas.write_ansi(&mut io::stdout())?;
    Ok(())
}
