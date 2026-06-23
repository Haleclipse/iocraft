//! Demonstrates terminal-viewport visibility tracking.
//!
//! CC Ink uses `useTerminalViewport()` to let components pause expensive work
//! after their rows have moved into native terminal scrollback. iocraft exposes
//! the same idea with `use_terminal_viewport()`. `InVirtualListContext` mirrors
//! Claude Code's virtual-list bypass for `OffscreenFreeze`. This deterministic
//! example uses an explicit three-row viewport so it can be run without a real terminal.

use futures::StreamExt;
use iocraft::prelude::*;
use std::io;

#[derive(Default, Props)]
struct CounterProps {
    label: &'static str,
}

#[component]
fn Counter(mut hooks: Hooks, props: &CounterProps) -> impl Into<AnyElement<'static>> {
    let mut renders = hooks.use_state(|| 0u32);
    renders += 1;
    element!(Text(content: format!(
        "{} counter renders={}",
        props.label,
        renders.get()
    )))
}

#[component]
fn TopProbe(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let entry = hooks.use_terminal_viewport_with_rows(3);
    element!(Text(content: format!("top probe visible={}", entry.is_visible)))
}

#[component]
fn BottomProbe(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let entry = hooks.use_terminal_viewport_with_rows(3);
    element!(Text(content: format!(
        "bottom probe visible={}",
        entry.is_visible
    )))
}

#[component]
fn TerminalViewportDemo(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let mut system = hooks.use_context_mut::<SystemContext>();
    let mut tick = hooks.use_state(|| 0u8);

    // The hook learns geometry during draw and reports it on the next render,
    // matching CC Ink's no-extra-render viewport entry semantics.
    if tick.get() < 3 {
        tick += 1;
    } else {
        system.exit();
    }

    element! {
        View(flex_direction: FlexDirection::Column) {
            OffscreenFreeze(terminal_rows: Some(3)) {
                Counter(label: "frozen")
            }
            ContextProvider(value: Context::owned(InVirtualListContext)) {
                OffscreenFreeze(terminal_rows: Some(3)) {
                    Counter(label: "virtual-list bypass")
                }
            }
            TopProbe
            Text(content: "row 1")
            Text(content: "row 2")
            Text(content: "row 3")
            Text(content: "row 4")
            Text(content: "row 5")
            BottomProbe
            Text(content: "row 7")
        }
    }
}

fn main() -> io::Result<()> {
    let canvases: Vec<_> = smol::block_on(
        element!(TerminalViewportDemo)
            .mock_terminal_render_loop(MockTerminalConfig::default())
            .collect(),
    );
    let canvas = canvases
        .last()
        .expect("example should render at least once");
    println!("Three-row viewport simulation:\n");
    canvas.write(&mut io::stdout())?;
    Ok(())
}
