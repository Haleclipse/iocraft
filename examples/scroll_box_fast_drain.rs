//! Demonstrates opt-in CC Ink-style wheel scroll draining on `ScrollBox`.
//!
//! The default iocraft `ScrollBox` applies wheel deltas eagerly. Setting
//! `scroll_drain_mode` accumulates bursty wheel input and drains it over
//! animation frames, mirroring the CC Ink fork's `pendingScrollDelta` behavior
//! without making it a renderer-global default.

use iocraft::prelude::*;
use std::io;

#[component]
fn FastDrainDemo(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let mut app = hooks.use_app();
    let handle = hooks.use_ref_default::<ScrollBoxHandle>();

    hooks.use_keybinding("q", move || app.exit());

    let rows = (0..120)
        .map(|i| format!("Fast-drain transcript row {i:03}"))
        .collect::<Vec<_>>()
        .join("\n");
    let status = {
        let handle = handle.read();
        format!(
            "wheel = drained over frames · q quit · top={} pending={}",
            handle.get_scroll_top(),
            handle.get_pending_delta()
        )
    };

    element! {
        View(width: 84, height: 16, flex_direction: FlexDirection::Column) {
            Text(content: "ScrollBox wheel drain", weight: Weight::Bold, color: Color::Cyan)
            Text(content: status, color: Color::Grey)
            View(height: 13, margin_top: 1) {
                ScrollBox(
                    handle,
                    sticky_scroll: false,
                    scroll_step: Some(24),
                    scroll_drain_mode: Some(ScrollDrainMode::Native),
                ) {
                    Text(content: rows)
                }
            }
        }
    }
}

fn main() -> io::Result<()> {
    let mut app = element!(FastDrainDemo);
    smol::block_on(app.fullscreen())
}
