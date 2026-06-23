//! Demonstrates `SelectionContext::subscribe`.
//!
//! The subscription is an external-store style push signal: listeners run after
//! selection controller/theme mutations and are removed when the returned
//! `SelectionSubscription` is dropped.

use iocraft::prelude::*;
use std::sync::{Arc, Mutex};

fn set_demo_selection(selection: SelectionContext) {
    let mut controller = SelectionController::new();
    controller.selection_mut().start(1, 0);
    controller.selection_mut().update(4, 0);
    controller.selection_mut().finish();
    selection.set_controller(controller);
}

#[component]
fn SelectionSubscribeDemo(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let mut app = hooks.use_app();
    let selection = create_selection_context(&mut hooks);
    let mut phase = hooks.use_state(|| 0u8);
    let notifications = hooks.use_state(Vec::<String>::new);
    let subscription: Arc<Mutex<Option<SelectionSubscription>>> = hooks.use_const_default();

    if subscription.lock().unwrap().is_none() {
        let mut notifications_for_listener = notifications;
        *subscription.lock().unwrap() = Some(selection.subscribe(move || {
            let mut next = notifications_for_listener.read().clone();
            next.push("selection changed".to_string());
            notifications_for_listener.set(next);
        }));
    }

    match phase.get() {
        0 => {
            set_demo_selection(selection);
            phase.set(1);
        }
        1 => {
            selection.clear_selection();
            phase.set(2);
        }
        _ => app.exit(),
    }

    let rows = notifications.read().clone();
    element! {
        View(flex_direction: FlexDirection::Column) {
            Text(content: format!("notifications={}", rows.len()))
            #(rows.into_iter().map(|row| element!(Text(content: row))))
        }
    }
}

fn main() -> std::io::Result<()> {
    let mut app = element!(SelectionSubscribeDemo);
    smol::block_on(app.render_loop())
}
