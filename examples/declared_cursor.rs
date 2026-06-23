//! Demonstrates declaring the physical terminal cursor from a custom component.
//!
//! CC Ink exposes `useDeclaredCursor(...)` so focused inputs can park the native
//! cursor at the caret for IME preedit text and accessibility tools. iocraft's
//! `use_declared_cursor` hook provides the same component-relative declaration.

use iocraft::prelude::*;
use std::io;

#[component]
fn CursorDemo(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    // Put the terminal cursor on the caret after "input: ". The declaration is
    // relative to this component's canvas rect and is clamped by the canvas.
    hooks.use_declared_cursor(0, 7, true);
    element!(Text(content: "input: | custom declared cursor"))
}

fn main() -> io::Result<()> {
    let mut app = element!(CursorDemo);
    let canvas = app.render(Some(40));
    println!("declared cursor={:?}", canvas.cursor_declaration());
    canvas.write(&mut io::stdout())?;
    Ok(())
}
