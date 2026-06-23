//! Demonstrates declarative terminal title updates.
//!
//! `use_terminal_title` strips ANSI escape sequences before setting OSC 0, just
//! like the CC Ink fork's `useTerminalTitle(...)` hook.

use iocraft::prelude::*;

#[component]
fn TitleDemo(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    hooks.use_terminal_title("\x1b[32miocraft\x1b[0m title demo");
    let mut system = hooks.use_context_mut::<SystemContext>();
    system.exit();
    element!(Text(content: "terminal title requested: iocraft title demo"))
}

fn main() -> std::io::Result<()> {
    let mut app = element!(TitleDemo);
    smol::block_on(app.render_loop())
}
