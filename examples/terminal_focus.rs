//! Demonstrates terminal focus tracking.
//!
//! iocraft enables DECSET 1004 focus reporting while in raw mode and exposes
//! the current state with `use_terminal_focus_state()` / `use_terminal_focus()`.
//! Like CC Ink's `TerminalFocusContext`, late-mounted consumers see the latest
//! render-loop focus state instead of reverting to `Unknown`.
//! This example uses mock terminal event streams so it is deterministic.

use futures::{stream, StreamExt};
use iocraft::prelude::*;
use std::io;

#[component]
fn FocusProbe(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let mut system = hooks.use_context_mut::<SystemContext>();
    let state = hooks.use_terminal_focus_state();
    if state != TerminalFocusState::Unknown {
        system.exit();
    }
    element!(Text(content: format!(
        "terminal focused={} state={state:?}",
        state.is_focused()
    )))
}

fn render_with(event: TerminalEvent) -> Vec<String> {
    smol::block_on(
        element!(FocusProbe)
            .mock_terminal_render_loop(MockTerminalConfig::with_events(stream::iter(vec![event])))
            .map(|canvas| canvas.to_string())
            .collect(),
    )
}

fn main() -> io::Result<()> {
    println!("Focus lost frames:");
    for frame in render_with(TerminalEvent::FocusLost) {
        print!("{frame}");
    }
    println!("Focus gained frames:");
    for frame in render_with(TerminalEvent::FocusGained) {
        print!("{frame}");
    }
    Ok(())
}
