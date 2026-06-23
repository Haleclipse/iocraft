//! Demonstrates app-level selection context overlay rendering.
//!
//! The CC Ink fork applies text-selection styling after the component tree has
//! rendered, directly onto the retained screen buffer before diffing. iocraft's
//! `use_selection_overlay` hook provides the same layering: components render
//! normally, then the selection context paints a solid background and damage
//! metadata on top of the finished canvas.
//!
//! This example also uses `use_copy_on_select_text` to demonstrate
//! copy-on-select semantics without writing to the user's real clipboard.

use futures::StreamExt;
use iocraft::prelude::*;
use std::io;

#[component]
fn SelectionContextOverlay(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let mut system = hooks.use_context_mut::<SystemContext>();
    let selection = create_selection_context(&mut hooks);
    let last_copy = hooks.use_state(String::new);
    hooks.use_selection_bg_color(selection, Color::DarkBlue);

    if !selection.has_selection() && last_copy.read().is_empty() {
        let mut controller = SelectionController::new();
        controller.selection_mut().start(7, 1);
        controller.selection_mut().update(24, 1);
        controller.selection_mut().finish();
        selection.set_controller(controller);
    }

    // Paint the overlay after this component and its children draw, mirroring
    // CC Ink's applySelectionOverlay(frame.screen, selection, ...).
    hooks.use_selection_overlay(selection);

    // Demonstrate automatic copy-on-select without touching the real clipboard.
    // Applications that want OSC52 clipboard transport can use
    // hooks.use_copy_on_select(selection, stdout, true) instead.
    let mut last_copy_for_callback = last_copy;
    hooks.use_copy_on_select_text(selection, true, move |text| {
        last_copy_for_callback.set(text);
    });

    if !last_copy.read().is_empty() && !selection.copy_on_select_would_mutate() {
        system.exit();
    }

    element! {
        ContextProvider(value: Context::owned(selection)) {
            View(flex_direction: FlexDirection::Column) {
                Text(content: "SelectionContext overlay demo")
                Text(content: "Painted after child draw")
                View(no_select: true) {
                    Text(content: "noSelect gutter is skipped")
                }
                Text(content: format!("simulated copy-on-select: {:?}", &*last_copy.read()))
            }
        }
    }
}

fn main() -> io::Result<()> {
    let canvases: Vec<_> = smol::block_on(
        element!(SelectionContextOverlay)
            .mock_terminal_render_loop(MockTerminalConfig::default().with_size(40, 6))
            .collect(),
    );
    let canvas = canvases
        .last()
        .expect("example should render at least once");
    println!("selection overlay damage={:?}", canvas.damage_region());
    println!("ANSI dump (selected cells use a dark-blue background):\n");
    canvas.write_ansi(&mut io::stdout())?;
    Ok(())
}
