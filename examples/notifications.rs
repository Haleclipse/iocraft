//! Demonstrates CC-style toast notifications for iocraft.
//!
//! This extracts the app-level `useNotifications()` pattern into reusable
//! `NotificationProvider`, `NotificationViewport`, and `use_notifications()`.

use iocraft::prelude::*;
use std::{io, time::Duration};

#[component]
fn NotificationDemo(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let mut app = hooks.use_app();
    let notifications = hooks.use_notifications();
    let mut counter = hooks.use_state(|| 0usize);

    hooks.use_keybinding("q", move || app.exit());
    hooks.use_keybinding("i", {
        let notifications = notifications;
        move || {
            notifications.add_notification(
                Notification::immediate("instant", "immediate toast preempted the queue")
                    .with_color(Color::Yellow)
                    .with_timeout(Duration::from_millis(2000)),
            );
        }
    });
    hooks.use_keybinding("l", {
        let notifications = notifications;
        move || {
            notifications.add_notification(
                Notification::new(
                    "low",
                    "low priority queued toast",
                    NotificationPriority::Low,
                )
                .with_color(Color::Grey),
            );
        }
    });
    hooks.use_keybinding("h", {
        let notifications = notifications;
        move || {
            notifications.add_notification(
                Notification::new("high", "high priority toast", NotificationPriority::High)
                    .with_color(Color::Cyan),
            );
        }
    });
    hooks.use_keybinding("n", {
        let notifications = notifications;
        move || {
            counter += 1;
            notifications.add_notification(
                Notification::medium(
                    format!("note-{}", counter.get()),
                    format!("note #{}", counter.get()),
                )
                .with_timeout(Duration::from_millis(3000)),
            );
        }
    });
    hooks.use_keybinding("r", {
        let notifications = notifications;
        move || notifications.remove_notification("low")
    });

    element! {
        View(width: 84, flex_direction: FlexDirection::Column, padding: 1) {
            Text(content: "Notifications", weight: Weight::Bold, color: Color::Cyan)
            Text(content: "i immediate · h high · l low · n unique medium · r remove low · q quit", color: Color::Grey)
            View(margin_top: 1, height: 3, border_style: BorderStyle::Round, border_color: Color::DarkGrey) {
                NotificationViewport(prefix: Some("toast: ".to_string()))
            }
            Text(content: format!("queued={}", notifications.queued_len()))
        }
    }
}

fn main() -> io::Result<()> {
    smol::block_on(
        element! {
            NotificationProvider {
                NotificationDemo
            }
        }
        .render_loop(),
    )
}
