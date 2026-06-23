//! Demonstrates the opt-in `FastScrollBox` convenience wrapper.
//!
//! Unlike the default `ScrollBox`, this wrapper enables CC Ink-style wheel
//! acceleration and render-time scroll draining by default. The drain strategy
//! is selected from the current terminal host (`xterm.js` adaptive vs native
//! proportional), but the component remains opt-in.

use iocraft::prelude::*;
use std::io;

#[component]
fn FastScrollBoxDemo(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let mut app = hooks.use_app();
    let handle = hooks.use_ref_default::<ScrollBoxHandle>();

    hooks.use_keybinding("q", move || app.exit());

    let rows = (0..160)
        .map(|i| format!("FastScrollBox transcript row {i:03}"))
        .collect::<Vec<_>>()
        .join("\n");
    let status = {
        let handle = handle.read();
        format!(
            "q quit · top={} pending={} mode={:?}",
            handle.get_scroll_top(),
            handle.get_pending_delta(),
            ScrollDrainMode::for_current_terminal()
        )
    };

    element! {
        View(width: 86, height: 16, flex_direction: FlexDirection::Column) {
            Text(content: "FastScrollBox", weight: Weight::Bold, color: Color::Cyan)
            Text(content: status, color: Color::Grey)
            View(height: 13, margin_top: 1) {
                FastScrollBox(handle, sticky_scroll: false) {
                    Text(content: rows)
                }
            }
        }
    }
}

fn main() -> io::Result<()> {
    let mut app = element!(FastScrollBoxDemo);
    smol::block_on(app.fullscreen())
}
