//! Demonstrates CC Ink-style visual-only virtual scroll clamp bounds.
//!
//! Press `j` to set a mounted-range clamp and jump far down the content. The
//! logical `get_scroll_top()` target can run ahead, while the rendered viewport
//! stays at the clamp edge so a virtual list would have time to mount more rows.

use iocraft::prelude::*;
use std::io;

#[component]
fn VisualClampDemo(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let mut app = hooks.use_app();
    let handle = hooks.use_ref_default::<ScrollBoxHandle>();
    let mut clamped = hooks.use_state(|| false);

    hooks.use_keybinding("q", move || app.exit());
    hooks.use_terminal_events({
        let mut handle = handle;
        move |event| match event {
            TerminalEvent::Key(KeyEvent {
                code: KeyCode::Char('j'),
                kind: KeyEventKind::Press,
                ..
            }) => {
                let mut handle = handle.write();
                handle.set_clamp_bounds(Some(8), Some(12));
                handle.scroll_by(99);
                clamped.set(true);
            }
            TerminalEvent::Key(KeyEvent {
                code: KeyCode::Char('c'),
                kind: KeyEventKind::Press,
                ..
            }) => {
                handle.write().clear_clamp_bounds();
                clamped.set(false);
            }
            _ => {}
        }
    });

    let rows = (0..60)
        .map(|i| format!("Virtual row {i:02}"))
        .collect::<Vec<_>>()
        .join("\n");
    let status = {
        let handle = handle.read();
        format!(
            "j clamp+jump · c clear · q quit · logical_top={} clamp={}",
            handle.get_scroll_top(),
            if clamped.get() { "8..12" } else { "off" }
        )
    };

    element! {
        View(width: 72, height: 13, flex_direction: FlexDirection::Column) {
            Text(content: "ScrollBox visual clamp", weight: Weight::Bold, color: Color::Cyan)
            Text(content: status, color: Color::Grey)
            View(height: 10, margin_top: 1) {
                ScrollBox(handle, sticky_scroll: false, scrollbar: Some(false)) {
                    Text(content: rows)
                }
            }
        }
    }
}

fn main() -> io::Result<()> {
    let mut app = element!(VisualClampDemo);
    smol::block_on(app.fullscreen())
}
