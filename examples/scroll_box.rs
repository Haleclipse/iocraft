//! Demonstrates the CC Ink-style `ScrollBox` compatibility wrapper.
//!
//! `ScrollBox` is backed by iocraft's `ScrollView`, but exposes the CC Ink
//! naming and handle helpers such as `get_scroll_top()` / `is_sticky()`.

use iocraft::prelude::*;
use std::{
    io,
    sync::{Arc, Mutex},
};

#[component]
fn ScrollBoxDemo(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let mut app = hooks.use_app();
    let handle = hooks.use_ref_default::<ScrollBoxHandle>();
    let notifications = hooks.use_state(|| 0usize);
    let subscription: Arc<Mutex<Option<ScrollBoxSubscription>>> = hooks.use_const_default();

    hooks.use_keybinding("q", move || app.exit());
    hooks.use_keybinding("j", {
        let mut handle = handle;
        let subscription = subscription.clone();
        let mut notifications_for_listener = notifications;
        move || {
            if subscription.lock().unwrap().is_none() {
                *subscription.lock().unwrap() = Some(handle.read().subscribe(move || {
                    notifications_for_listener += 1;
                }));
            }
            handle.write().scroll_by(1);
        }
    });
    hooks.use_keybinding("k", {
        let mut handle = handle;
        move || handle.write().scroll_by(-1)
    });
    hooks.use_keybinding("end", {
        let mut handle = handle;
        move || handle.write().scroll_to_bottom()
    });
    hooks.use_keybinding("c", {
        let mut handle = handle;
        move || {
            let mut handle = handle.write();
            handle.set_clamp_bounds(Some(5), Some(10));
            handle.scroll_to(99);
        }
    });
    hooks.use_keybinding("u", {
        let mut handle = handle;
        move || handle.write().clear_clamp_bounds()
    });

    let lines = (0..30)
        .map(|i| format!("Line {i:02}"))
        .collect::<Vec<_>>()
        .join("\n");
    let status = {
        let handle = handle.read();
        format!(
            "j/k scroll · c clamp 5..10 · u unclamp · End bottom · q quit · top={} height={} viewport={} sticky={} notifications={}",
            handle.get_scroll_top(),
            handle.get_scroll_height(),
            handle.get_viewport_height(),
            handle.is_sticky(),
            notifications.get(),
        )
    };

    element! {
        View(width: 88, height: 10, flex_direction: FlexDirection::Column) {
            Text(content: status)
            View(height: 1, background_color: Color::DarkGrey) { Text(content: " ") }
            View(height: 8) {
                ScrollBox(handle, sticky_scroll: false) {
                    Text(content: lines)
                }
            }
        }
    }
}

fn main() -> io::Result<()> {
    let mut app = element!(ScrollBoxDemo);
    smol::block_on(app.fullscreen())
}
