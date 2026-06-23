//! Demonstrates CC Ink-style terminal size context syncing.
//!
//! `use_terminal_size` reports the size owned by the render loop, not just
//! resize events, so fullscreen/layout code sees the same columns/rows that the
//! renderer uses for the current frame.

use iocraft::prelude::*;

#[component]
fn TerminalSizeDemo(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let mut app = hooks.use_app();
    let (width, height) = hooks.use_terminal_size();

    hooks.use_input(move |input, key| {
        if input == "q" || key.escape {
            app.exit();
        }
    });

    element! {
        View(flex_direction: FlexDirection::Column, padding: 1) {
            Text(content: "Terminal size", weight: Weight::Bold)
            Text(content: format!("columns={width} rows={height}"))
            Text(content: "Press q or Esc to exit", color: Color::DarkGrey)
        }
    }
}

fn main() -> std::io::Result<()> {
    let mut app = element!(TerminalSizeDemo);
    smol::block_on(app.render_loop())
}
