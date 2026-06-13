//! Demonstrates `use_keybinding` — declarative keyboard shortcuts.
//!
//! Press Ctrl+S to save, Ctrl+Z to undo, Ctrl+R to reset. Esc to quit.

use iocraft::prelude::*;

#[component]
fn App(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let mut system = hooks.use_context_mut::<SystemContext>();
    let mut log = hooks.use_state(|| vec!["Ready. Try Ctrl+S, Ctrl+Z, Ctrl+R.".to_string()]);
    let mut should_exit = hooks.use_state(|| false);

    hooks.use_keybinding("ctrl+s", {
        let mut log = log;
        move || log.write().push("Saved!".to_string())
    });
    hooks.use_keybinding("ctrl+z", {
        let mut log = log;
        move || log.write().push("Undo!".to_string())
    });
    hooks.use_keybinding("ctrl+r", {
        let mut log = log;
        move || {
            let mut l = log.write();
            l.clear();
            l.push("Reset.".to_string());
        }
    });
    hooks.use_keybinding("esc", move || should_exit.set(true));

    if should_exit.get() {
        system.exit();
    }

    element! {
        View(flex_direction: FlexDirection::Column, padding: 1) {
            Text(content: "Keybinding Demo", weight: Weight::Bold, color: Color::Cyan)
            Text(content: "Ctrl+S save | Ctrl+Z undo | Ctrl+R reset | Esc quit", color: Color::Grey)
            View(
                border_style: BorderStyle::Round,
                border_color: Color::DarkGrey,
                flex_direction: FlexDirection::Column,
                margin_top: 1,
                width: 50,
                height: 8,
            ) {
                #(log.read().iter().rev().take(6).rev().map(|l| {
                    element! { Text(content: format!("  {l}")) }
                }))
            }
        }
    }
}

fn main() {
    smol::block_on(element!(App).render_loop()).unwrap();
}
