//! Demonstrates CC-style selection copy notifications.
//!
//! Press `s` to seed a settled fullscreen-style selection over `abcdef`. The
//! `SelectionClipboardNotifications` wrapper copies it via OSC 52 without
//! clearing the highlight and enqueues the same kind of toast that Claude
//! Code's `ScrollKeybindingHandler` shows after copy-on-select.

use iocraft::prelude::*;
use std::io;

#[component]
fn SelectionCopyNotificationsDemo(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let mut app = hooks.use_app();
    let selection = create_selection_context(&mut hooks);
    let notifications = hooks.use_notifications();

    hooks.use_keybinding("q", move || app.exit());
    hooks.use_keybinding("s", move || {
        let mut controller = SelectionController::new();
        controller.selection_mut().start(1, 2);
        controller.selection_mut().update(3, 2);
        controller.selection_mut().finish();
        selection.set_controller(controller);
    });

    hooks.use_selection_bg_color(selection, Color::DarkBlue);
    hooks.use_selection_overlay(selection);

    element! {
        ContextProvider(value: Context::owned(selection)) {
            SelectionClipboardNotifications(selection: Some(selection), clipboard_path: Some(SelectionClipboardPath::Osc52)) {
                View(width: 84, flex_direction: FlexDirection::Column, padding: 1) {
                    Text(content: "Selection copy notifications", weight: Weight::Bold, color: Color::Cyan)
                    Text(content: "press s to copy selected 'bcd' · Ctrl+C copies+clears active selection · q quit", color: Color::Grey)
                    Text(content: "abcdef")
                    View(margin_top: 1, height: 1) {
                        NotificationViewport(prefix: Some("toast: ".to_string()))
                    }
                    Text(content: format!("queued={} has_selection={}", notifications.queued_len(), selection.has_selection()))
                }
            }
        }
    }
}

fn main() -> io::Result<()> {
    smol::block_on(
        element! {
            NotificationProvider {
                SelectionCopyNotificationsDemo
            }
        }
        .render_loop(),
    )
}
