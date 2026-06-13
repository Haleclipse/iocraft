//! Demonstrates OSC 8 hyperlinks and terminal title (OSC 0).
//!
//! Links are clickable in supporting terminals (kitty, iTerm2, WezTerm,
//! Windows Terminal) — try Cmd+click or Ctrl+click. Unsupported terminals
//! display the text normally.
//!
//! The terminal tab/window title is also set via OSC 0.
//! Press Esc to quit.

use iocraft::prelude::*;

#[component]
fn App(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let mut system = hooks.use_context_mut::<SystemContext>();
    let mut should_exit = hooks.use_state(|| false);

    system.set_terminal_title("iocraft hyperlink demo");

    hooks.use_terminal_events(move |e| {
        if let TerminalEvent::Key(KeyEvent {
            code: KeyCode::Esc,
            kind: KeyEventKind::Press,
            ..
        }) = e
        {
            should_exit.set(true);
        }
    });

    if should_exit.get() {
        system.exit();
    }

    element! {
        View(flex_direction: FlexDirection::Column, padding: 1) {
            Text(content: "Hyperlink Demo", weight: Weight::Bold, color: Color::Cyan)
            Text(content: "(Cmd+click or Ctrl+click to open links)", color: Color::Grey)
            View(margin_top: 1, flex_direction: FlexDirection::Column) {
                Text(
                    content: "iocraft on GitHub",
                    color: Color::Blue,
                    href: "https://github.com/ccbrown/iocraft".to_string(),
                )
                Text(
                    content: "Rust documentation",
                    color: Color::Blue,
                    href: "https://docs.rs/iocraft".to_string(),
                )
                Text(
                    content: "This text has no link",
                    color: Color::White,
                )
            }
            View(margin_top: 1) {
                Text(content: "Press Esc to quit", color: Color::Grey)
            }
        }
    }
}

fn main() {
    smol::block_on(element!(App).render_loop()).unwrap();
}
