//! Demonstrates `ErrorBoundary` — catching a child panic without crashing the TUI.
//!
//! Press 1 to render a safe component, 2 to render one that panics. The error
//! boundary catches the panic and shows an error message; the rest of the UI
//! keeps working. Press Esc to quit.

use iocraft::prelude::*;

#[component]
fn Safe() -> impl Into<AnyElement<'static>> {
    element! {
        View(border_style: BorderStyle::Round, border_color: Color::Green, padding: 1) {
            Text(content: "This component is healthy.", color: Color::Green)
        }
    }
}

#[component]
fn Risky() -> impl Into<AnyElement<'static>> {
    // Simulate a bug: this component always panics.
    if true {
        panic!("something went wrong in Risky component!");
    }
    element!(View)
}

#[component]
fn App(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let mut system = hooks.use_context_mut::<SystemContext>();
    let show_risky = hooks.use_state(|| false);
    let mut should_exit = hooks.use_state(|| false);

    hooks.use_keybinding("1", {
        let mut show_risky = show_risky;
        move || show_risky.set(false)
    });
    hooks.use_keybinding("2", {
        let mut show_risky = show_risky;
        move || show_risky.set(true)
    });
    hooks.use_keybinding("esc", move || should_exit.set(true));

    if should_exit.get() {
        system.exit();
    }

    element! {
        View(flex_direction: FlexDirection::Column, padding: 1) {
            Text(content: "ErrorBoundary Demo", weight: Weight::Bold)
            Text(content: "1 = safe component | 2 = panicking component | Esc = quit", color: Color::Grey)
            View(margin_top: 1) {
                ErrorBoundary {
                    #(if show_risky.get() {
                        element!(Risky).into_any()
                    } else {
                        element!(Safe).into_any()
                    })
                }
            }
            View(margin_top: 1) {
                Text(content: "This text proves the rest of the UI survives.", color: Color::DarkGrey)
            }
        }
    }
}

fn main() {
    smol::block_on(element!(App).render_loop()).unwrap();
}
