//! Demonstrates CC Ink-style selection tracking across scroll jumps.
//!
//! `ScrollBox(selection: Some(...))` keeps fullscreen text selections anchored
//! to the text for keyboard scroll jumps. Rows that leave the viewport are
//! captured before the scroll, so copying still includes text that just moved
//! into scrollback. Wheel scrolling clears selection, matching CC Ink's
//! `ScrollKeybindingHandler`.

use futures::{stream, StreamExt};
use iocraft::prelude::*;

#[component]
fn SelectionScrollTracking(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let mut system = hooks.use_context_mut::<SystemContext>();
    let selection = create_selection_context(&mut hooks);
    let mut page_seen = hooks.use_state(|| false);
    let copied = hooks.use_state(String::new);

    hooks.use_terminal_events(move |event| {
        if matches!(
            event,
            TerminalEvent::Key(KeyEvent {
                code: KeyCode::PageDown,
                kind: KeyEventKind::Press,
                ..
            })
        ) {
            page_seen.set(true);
        }
    });

    if !selection.has_selection() && copied.read().is_empty() {
        let mut controller = SelectionController::new();
        controller.selection_mut().start(0, 0);
        controller.selection_mut().update(3, 2);
        controller.selection_mut().finish();
        selection.set_controller(controller);
    }

    let mut copied_for_callback = copied;
    hooks.use_copy_on_select_text(selection, page_seen.get(), move |text| {
        copied_for_callback.set(text);
    });

    if !copied.read().is_empty() && !selection.copy_on_select_would_mutate() {
        system.exit();
    }

    let lines = (0..5)
        .map(|i| format!("row{i}"))
        .collect::<Vec<_>>()
        .join("\n");

    element! {
        View(flex_direction: FlexDirection::Column) {
            View(width: 6, height: 3) {
                ScrollBox(selection: Some(selection), keyboard_scroll: Some(true)) {
                    Text(content: lines)
                }
            }
            Text(content: format!("copied={:?}", &*copied.read()))
        }
    }
}

fn main() {
    let canvases: Vec<_> = smol::block_on(
        element!(SelectionScrollTracking)
            .mock_terminal_render_loop(MockTerminalConfig::with_events(stream::iter(vec![
                TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::PageDown)),
            ])))
            .collect(),
    );
    println!("{}", canvases.last().unwrap().to_string().trim_end());
}
