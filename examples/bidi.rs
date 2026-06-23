//! Demonstrates CC Ink-style software bidi fallback.
//!
//! On terminals that usually lack native bidirectional rendering (Windows,
//! Windows Terminal/WSL, and VS Code's xterm.js), iocraft reorders RTL/LTR
//! grapheme clusters before writing them to the retained screen buffer. On
//! terminals with native bidi support this remains a no-op to avoid applying the
//! Unicode Bidi Algorithm twice.

use iocraft::prelude::*;
use std::io;

fn main() -> io::Result<()> {
    let mut app = element! {
        View(flex_direction: FlexDirection::Column, width: 40) {
            Text(content: "Plain mixed text:".to_string(), color: Color::Cyan)
            Text(content: "אבגabc".to_string())
            Text(content: "Styled mixed text:".to_string(), color: Color::Cyan)
            MixedText(contents: vec![
                MixedTextContent::new("אבג").color(Color::Red).weight(Weight::Bold),
                MixedTextContent::new("abc").color(Color::Green),
            ])
            Text(content: "ANSI mixed text:".to_string(), color: Color::Cyan)
            Ansi(content: "\x1b[31mאבג\x1b[0mabc".to_string())
        }
    };

    app.print();
    Ok(())
}
