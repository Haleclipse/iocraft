//! Demonstrates rendering pre-wrapped ANSI text with `RawAnsi`.
//!
//! This mirrors CC Ink's `<RawAnsi>` optimization for external renderers that
//! already produced terminal-ready ANSI lines. SGR and OSC 8 metadata are parsed
//! into the retained screen buffer.

use iocraft::prelude::*;
use std::io;

fn main() -> io::Result<()> {
    let mut app = element! {
        RawAnsi(
            width: 32usize,
            lines: vec![
                "\x1b[31;1mred bold\x1b[0m plain".to_string(),
                "\x1b[48;5;4mblue background\x1b[0m".to_string(),
                "\x1b[4:3mcurly underline\x1b[24m and \x1b[21mdouble\x1b[24m".to_string(),
                "\x1b[4;58:2::255:128:0mcolored underline\x1b[59;24m".to_string(),
                "\x1b[5mblink\x1b[25m and \x1b[8mhidden text\x1b[28m metadata".to_string(),
                "controls: a\x08b \x1b[2Kcursor clear skipped".to_string(),
                "\x1b]8;;https://example.com\x07linked label\x1b]8;;\x07".to_string(),
            ],
        )
    };
    let canvas = app.render(Some(32));
    println!(
        "raw ansi hyperlink at row 6 col 1 = {:?}",
        canvas.hyperlink_at(1, 6)
    );
    println!("ANSI dump:\n");
    canvas.write_ansi(&mut io::stdout())?;
    Ok(())
}
