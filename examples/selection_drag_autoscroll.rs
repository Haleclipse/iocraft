//! Demonstrates CC Ink-style selection drag-to-scroll autoscroll.
//!
//! A fullscreen drag whose focus leaves a `ScrollBox` viewport scrolls the box
//! every 50ms, captures rows that leave the viewport, and shifts the selection
//! anchor so copied text stays coherent. This example seeds such a drag in a
//! mock render loop and exits once the scroll box has moved.

use futures::StreamExt;
use iocraft::prelude::*;

#[component]
fn SelectionDragAutoscroll(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let mut system = hooks.use_context_mut::<SystemContext>();
    let selection = create_selection_context(&mut hooks);
    let handle = hooks.use_ref_default::<ScrollBoxHandle>();

    if !selection.controller_snapshot().selection().is_dragging() {
        let mut controller = SelectionController::new();
        controller.selection_mut().start(0, 1);
        controller.selection_mut().update(3, 3); // focus below viewport bottom
        selection.set_controller(controller);
    }

    if handle.read().get_scroll_top() > 0 {
        system.exit();
    }

    let lines = (0..5)
        .map(|i| format!("row{i}"))
        .collect::<Vec<_>>()
        .join("\n");

    element! {
        View(flex_direction: FlexDirection::Column) {
            View(width: 6, height: 3) {
                ScrollBox(handle, selection: Some(selection)) {
                    Text(content: lines)
                }
            }
            Text(content: format!("scroll_top={}", handle.read().get_scroll_top()))
        }
    }
}

fn main() {
    let canvases: Vec<_> = smol::block_on(
        element!(SelectionDragAutoscroll)
            .mock_terminal_render_loop(MockTerminalConfig::default())
            .collect(),
    );
    println!("{}", canvases.last().unwrap().to_string().trim_end());
}
