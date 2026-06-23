//! Demonstrates CC Ink-compatible text styling primitives.
//!
//! iocraft supports foreground color, bold/dim weights, underline, italic,
//! strikethrough, overline, inverse video, mixed styled runs, and OSC 8 hyperlinks.
//! In tmux, RGB colors are automatically clamped to ANSI-256 unless
//! `CLAUDE_CODE_TMUX_TRUECOLOR` is set, matching the CC Ink fork.

use iocraft::prelude::*;

fn main() {
    element! {
        View(flex_direction: FlexDirection::Column, width: 72, padding: 1) {
            Text(content: "Text styles", bold: true, color: Color::Cyan)
            Text(content: "bold", bold: true)
            Text(content: "dim", dim: true)
            Text(content: "background", background_color: Some(Color::DarkBlue), color: Color::White)
            Text(content: "rgb truecolor / tmux-safe", color: Color::Rgb { r: 215, g: 119, b: 87 })
            Text(content: "emoji width compensation: ☀️ 🩷")
            View(width: 2) {
                Text(content: "☀️x")
            }
            Text(content: "underline", underline: true)
            Text(content: "italic", italic: true)
            Text(content: "strikethrough", strikethrough: true)
            Text(content: "overline / SGR 53 parity", overline: true)
            Text(content: "inverse", inverse: true)
            Text(content: "tab\tstops\talign like terminals")
            View(width: 20) {
                Text(content: "truncate from the middle", wrap: TextWrap::TruncateMiddle)
            }
            Link(label: Some("OSC 8 hyperlink".to_string()), url: "https://example.com".to_string())
            MixedText(contents: vec![
                MixedTextContent::new("mixed ").color(Color::Green),
                MixedTextContent::new("bg ").background_color(Color::DarkBlue).color(Color::White),
                MixedTextContent::new("strike ").strikethrough(),
                MixedTextContent::new("over ").overline(),
                MixedTextContent::new("italic").italic().color(Color::Magenta),
            ])
        }
    }
    .print();
}
