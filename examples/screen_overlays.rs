//! Demonstrates CC Ink-style app-level overlay ordering.
//!
//! The combined `use_screen_overlays` hook paints fullscreen text selection
//! first, then rendered-screen search highlights, and finally the positioned
//! current match. This mirrors `Ink.render()` in the CC Ink fork and keeps
//! overlapping selection/search cells deterministic.

use iocraft::prelude::*;
use std::io;

#[component]
fn ScreenOverlays(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let selection = create_selection_context(&mut hooks);
    let highlight = create_search_highlight_context(&mut hooks);

    if !selection.has_selection() {
        let mut controller = SelectionController::new();
        controller.selection_mut().start(4, 1);
        controller.selection_mut().update(7, 1);
        controller.selection_mut().finish();
        selection.set_controller(controller);
        selection.set_selection_bg_color(Color::Blue);
    }

    highlight.set_query("lazy");
    highlight.set_positions(
        vec![
            TextMatchPosition {
                row: 1,
                col: 4,
                len: 4,
            },
            TextMatchPosition {
                row: 2,
                col: 12,
                len: 4,
            },
        ],
        0,
        1,
    );

    hooks.use_screen_overlays(selection, highlight);

    element! {
        View(flex_direction: FlexDirection::Column) {
            Text(content: "Screen overlay order demo")
            Text(content: "the lazy fox")
            Text(content: "jumped over lazy dogs")
            Text(content: "first lazy is selected + searched; second is current")
        }
    }
}

fn main() -> io::Result<()> {
    let mut app = element!(ScreenOverlays);
    let canvas = app.render(Some(64));
    println!("ANSI dump (selection -> search -> current):\n");
    canvas.write_ansi(&mut io::stdout())?;
    Ok(())
}
