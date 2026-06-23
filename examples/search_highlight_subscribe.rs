//! Demonstrates `SearchHighlightContext::subscribe`.
//!
//! The subscription is an external-store style push signal for search query,
//! current-match position, and overlay theme mutations.

use iocraft::prelude::*;
use std::sync::{Arc, Mutex};

#[component]
fn SearchHighlightSubscribeDemo(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let mut app = hooks.use_app();
    let highlight = create_search_highlight_context(&mut hooks);
    let mut phase = hooks.use_state(|| 0u8);
    let notifications = hooks.use_state(Vec::<String>::new);
    let subscription: Arc<Mutex<Option<SearchHighlightSubscription>>> = hooks.use_const_default();

    if subscription.lock().unwrap().is_none() {
        let mut notifications_for_listener = notifications;
        *subscription.lock().unwrap() = Some(highlight.subscribe(move || {
            let mut next = notifications_for_listener.read().clone();
            next.push("search highlight changed".to_string());
            notifications_for_listener.set(next);
        }));
    }

    match phase.get() {
        0 => {
            highlight.set_query("lazy");
            phase.set(1);
        }
        1 => {
            highlight.set_positions(
                vec![TextMatchPosition {
                    row: 0,
                    col: 0,
                    len: 4,
                }],
                0,
                0,
            );
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
    let mut app = element!(SearchHighlightSubscribeDemo);
    smol::block_on(app.render_loop())
}
