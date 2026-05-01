//! Demonstrates the iocraft-router crate: declarative page routing with
//! history stack and keyboard navigation.
//!
//! Tab / Shift+Tab to switch pages, Backspace to go back, Ctrl+C to quit.

use iocraft::prelude::*;
use iocraft_router::*;
use std::sync::Arc;

#[derive(Default, Props)]
struct TabProps {
    label: String,
    active: bool,
}

#[component]
fn Tab(props: &TabProps) -> impl Into<AnyElement<'static>> {
    if props.active {
        element! {
            View(background_color: Color::Cyan, ) {
                Text(content: format!(" {} ", props.label), color: Color::Black, weight: Weight::Bold)
            }
        }
    } else {
        element! {
            View() {
                Text(content: format!(" {} ", props.label), color: Color::DarkGrey)
            }
        }
    }
}

#[component]
fn NavBar(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let router = use_router(&mut hooks);
    let current = router.current_route_id();

    let tabs = [("home", "Home"), ("about", "About"), ("settings", "Settings")];

    element! {
        View(width: 100pct, padding_top: 1, padding_bottom: 1) {
            #(tabs.iter().enumerate().flat_map(|(i, (id, label))| {
                let mut items: Vec<AnyElement<'static>> = Vec::new();
                if i > 0 {
                    items.push(element! { Text(content: " | ", color: Color::DarkGrey) }.into_any());
                }
                items.push(element! { Tab(label: label.to_string(), active: current.as_ref() == *id) }.into_any());
                items
            }))
        }
    }
}

fn home_page(_hooks: Hooks) -> AnyElement<'static> {
    element! {
        View(flex_direction: FlexDirection::Column, padding: 1) {
            Text(content: "Home", weight: Weight::Bold, color: Color::Green)
            Text(content: "Welcome to the iocraft router demo.")
            Text(content: "Tab / Shift+Tab to switch pages.", color: Color::Grey)
        }
    }
    .into()
}

fn about_page(_hooks: Hooks) -> AnyElement<'static> {
    element! {
        View(flex_direction: FlexDirection::Column, padding: 1) {
            Text(content: "About", weight: Weight::Bold, color: Color::Blue)
            Text(content: "iocraft-router provides declarative page routing")
            Text(content: "with a history stack, builder API, and Context integration.", color: Color::Grey)
        }
    }
    .into()
}

fn settings_page(_hooks: Hooks) -> AnyElement<'static> {
    element! {
        View(flex_direction: FlexDirection::Column, padding: 1) {
            Text(content: "Settings", weight: Weight::Bold, color: Color::Magenta)
            Text(content: "Nothing to configure yet.")
            Text(content: "Press Backspace to go back.", color: Color::Grey)
        }
    }
    .into()
}

#[component]
fn App(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let app = hooks.use_memo(
        || {
            Arc::new(
                UIRouterBuilder::new()
                    .route("home", "Home", home_page)
                    .route("about", "About", about_page)
                    .route("settings", "Settings", settings_page)
                    .default("home")
                    .build()
                    .expect("router build"),
            )
        },
        (),
    );

    let router_handle =
        ReactiveRouterHandle::new_with_hooks(&mut hooks, app.config.clone())
            .expect("router init");

    let route_ids: Vec<RouteId> = vec!["home".into(), "about".into(), "settings".into()];
    let mut handle = router_handle.clone();
    hooks.use_terminal_events(move |event| {
        if let TerminalEvent::Key(KeyEvent {
            code,
            kind: KeyEventKind::Press,
            modifiers,
            ..
        }) = event
        {
            match code {
                KeyCode::Tab if modifiers.contains(KeyModifiers::SHIFT) => {
                    let cur = handle.current_route_id();
                    let idx = route_ids.iter().position(|id| *id == cur).unwrap_or(0);
                    let prev = (idx + route_ids.len() - 1) % route_ids.len();
                    let _ = handle.navigate(route_ids[prev].clone());
                }
                KeyCode::Tab | KeyCode::BackTab => {
                    let cur = handle.current_route_id();
                    let idx = route_ids.iter().position(|id| *id == cur).unwrap_or(0);
                    let next = (idx + 1) % route_ids.len();
                    let _ = handle.navigate(route_ids[next].clone());
                }
                KeyCode::Backspace => { handle.go_back(); }
                _ => {}
            }
        }
    });

    element! {
        ContextProvider(value: Context::owned(RouterContext { handle: router_handle })) {
            View(flex_direction: FlexDirection::Column, width: 60) {
                NavBar
                UIRouter(app)
            }
        }
    }
}

fn main() {
    smol::block_on(element!(App).render_loop()).unwrap();
}
