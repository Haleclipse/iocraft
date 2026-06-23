//! Demonstrates CC Ink transcript/modal pager keys on `ScrollBox`.
//!
//! `modal_pager_keys` is opt-in because it consumes printable keys that would
//! conflict with a prompt. It mirrors the extracted `ScrollKeybindingHandler`
//! pager behavior used by transcript/copy-mode style views. It also enables
//! the opt-in CC Ink-style wheel acceleration ramp.

use iocraft::prelude::*;
use std::io;

#[component]
fn ModalPagerDemo(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let mut app = hooks.use_app();
    let handle = hooks.use_ref_default::<ScrollBoxHandle>();

    hooks.use_keybinding("q", move || app.exit());

    let lines = (0..60)
        .map(|i| format!("Transcript row {i:02}"))
        .collect::<Vec<_>>()
        .join("\n");
    let status = {
        let handle = handle.read();
        format!(
            "j/k line · Space/b page · Ctrl+U/D half · Ctrl+B/F full · g/G bounds · accelerated wheel · q quit · top={}",
            handle.get_scroll_top()
        )
    };

    element! {
        View(width: 92, height: 14, flex_direction: FlexDirection::Column) {
            Text(content: "ScrollBox modal pager keys", weight: Weight::Bold, color: Color::Cyan)
            Text(content: status, color: Color::Grey)
            View(height: 11, margin_top: 1) {
                ScrollBox(handle, modal_pager_keys: Some(true), wheel_acceleration: Some(true), sticky_scroll: false) {
                    Text(content: lines)
                }
            }
        }
    }
}

fn main() -> io::Result<()> {
    let mut app = element!(ModalPagerDemo);
    smol::block_on(app.fullscreen())
}
