//! Demonstrates app-level rendered-screen search highlighting.
//!
//! CC Ink stores search-highlight query/current-match positions on the Ink
//! instance, then paints overlays onto the finished screen buffer before
//! diffing. iocraft exposes the same shape with `SearchHighlightContext` and
//! `use_search_highlight_overlay`.

use iocraft::prelude::*;
use std::io;

#[component]
fn SearchHighlightOverlay(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let highlight = create_search_highlight_context(&mut hooks);
    highlight.set_query("lazy");

    // Pretend these positions came from scanning a message/subtree. The query
    // overlay highlights every visible "lazy"; the positioned overlay marks the
    // current match with a solid yellow background + bold + underline.
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
    hooks.use_search_highlight_overlay(highlight);

    element! {
        ContextProvider(value: Context::owned(highlight)) {
            View(flex_direction: FlexDirection::Column) {
                Text(content: "SearchHighlightContext demo")
                Text(content: "the lazy fox")
                Text(content: "jumped over lazy dogs")
                Text(content: format!("visible matches: {}", highlight.query()))
            }
        }
    }
}

fn main() -> io::Result<()> {
    let mut app = element!(SearchHighlightOverlay);
    let canvas = app.render(Some(40));
    println!("search highlight damage={:?}", canvas.damage_region());
    println!(
        "region scan rows 1..3 for lazy={:?}",
        canvas.scan_text_positions_region(0, 1, 40, 2, "lazy")
    );
    println!("ANSI dump (all matches inverted; current match is yellow):\n");
    canvas.write_ansi(&mut io::stdout())?;
    Ok(())
}
