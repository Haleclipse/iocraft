//! Demonstrates CC Ink-style selection tracking during sticky follow-scroll.
//!
//! When a sticky `ScrollBox` grows at the bottom, the visible content moves up.
//! Passing `selection: Some(selection)` lets iocraft capture rows that leave the
//! viewport and shift the selection so copying remains anchored to the original
//! text, matching CC Ink's `consumeFollowScroll()` path.

use futures::StreamExt;
use iocraft::prelude::*;

#[component]
fn SelectionFollowScroll(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let mut system = hooks.use_context_mut::<SystemContext>();
    let selection = create_selection_context(&mut hooks);
    let mut rows = hooks.use_state(|| 3usize);
    let copied = hooks.use_state(String::new);

    if !selection.has_selection() && copied.read().is_empty() {
        let mut controller = SelectionController::new();
        controller.selection_mut().start(0, 0);
        controller.selection_mut().update(3, 2);
        controller.selection_mut().finish();
        selection.set_controller(controller);
    }

    let row_count = rows.get();
    if row_count == 3 {
        rows.set(4);
    }

    let mut copied_for_callback = copied;
    hooks.use_copy_on_select_text(selection, row_count > 3, move |text| {
        copied_for_callback.set(text);
    });

    if !copied.read().is_empty() && !selection.copy_on_select_would_mutate() {
        system.exit();
    }

    let lines = (0..row_count)
        .map(|i| format!("row{i}"))
        .collect::<Vec<_>>()
        .join("\n");

    element! {
        View(flex_direction: FlexDirection::Column) {
            View(width: 6, height: 3) {
                ScrollBox(sticky_scroll: true, selection: Some(selection)) {
                    Text(content: lines)
                }
            }
            Text(content: format!("copied={:?}", &*copied.read()))
        }
    }
}

fn main() {
    let canvases: Vec<_> = smol::block_on(
        element!(SelectionFollowScroll)
            .mock_terminal_render_loop(MockTerminalConfig::default())
            .collect(),
    );
    println!("{}", canvases.last().unwrap().to_string().trim_end());
}
