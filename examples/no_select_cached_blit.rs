//! Demonstrates that `NoSelect` metadata is replayed after retained subtree blits.
//!
//! The first frame caches a child subtree without any noSelect metadata. The
//! second frame wraps that cached child in a noSelect parent and applies a
//! selection overlay. CC Ink applies noSelect after writes/blits but before
//! selection/search overlays; iocraft mirrors that so the selection overlay
//! skips the cached cells.

use futures::{stream, StreamExt};
use iocraft::prelude::*;
use std::io;

#[component]
fn Demo(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let mut system = hooks.use_context_mut::<SystemContext>();
    let mut fenced = hooks.use_state(|| false);
    let selection = create_selection_context(&mut hooks);
    let search = create_search_highlight_context(&mut hooks);

    hooks.use_terminal_events(move |event| {
        if matches!(
            event,
            TerminalEvent::Key(KeyEvent {
                code: KeyCode::Char('x'),
                kind: KeyEventKind::Press,
                ..
            })
        ) {
            fenced.set(true);
        }
    });

    if fenced.get() && !selection.has_selection() {
        let mut controller = SelectionController::new();
        controller.selection_mut().start(0, 0);
        controller.selection_mut().update(3, 0);
        controller.selection_mut().finish();
        selection.set_controller(controller);
    }
    hooks.use_screen_overlays(selection, search);

    if fenced.get() {
        system.exit();
    }

    element! {
        View(width: 4, no_select: fenced.get()) {
            CachedSubtree(cache_key: "stable".to_string()) {
                Text(content: "abcd")
            }
        }
    }
}

fn main() -> io::Result<()> {
    let canvases: Vec<_> = smol::block_on(
        element!(Demo)
            .mock_terminal_render_loop(MockTerminalConfig::with_events(stream::iter(vec![
                TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Char('x'))),
            ])))
            .collect(),
    );
    let canvas = canvases.last().expect("demo renders at least one frame");

    println!("Rendered cached row:");
    canvas.write(&mut io::stdout())?;
    println!(
        "\nnoSelect columns: {:?}",
        (0..4)
            .map(|col| canvas.is_no_select(col, 0))
            .collect::<Vec<_>>()
    );

    let mut ansi = Vec::new();
    canvas.write_ansi(&mut ansi)?;
    println!(
        "selection overlay painted blue background: {}",
        String::from_utf8_lossy(&ansi).contains("\x1b[44m")
    );
    Ok(())
}
