//! Demonstrates CC Ink-style focus/key/paste handlers on `View`.
//!
//! Press Tab/Shift+Tab to move focus, any printable key or paste text to update
//! the focused panel, `s` to stop ancestor bubbling from target capture while
//! still running the target bubble handler, `d` to call `prevent_default()` while
//! still bubbling to the ancestor, and `q` while a panel is focused to quit. The
//! footer shows CC Ink-style `event_type`/bubbles/cancelable metadata.

use iocraft::prelude::*;

#[component]
fn FocusablePanelDemo(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let mut app = hooks.use_app();
    let mut focused = hooks.use_state(|| "left".to_string());
    let mut left_count = hooks.use_state(|| 0usize);
    let mut left_captures = hooks.use_state(|| 0usize);
    let mut left_pastes = hooks.use_state(|| 0usize);
    let mut right_count = hooks.use_state(|| 0usize);
    let mut right_captures = hooks.use_state(|| 0usize);
    let mut right_pastes = hooks.use_state(|| 0usize);
    let mut root_bubbles = hooks.use_state(|| 0usize);
    let mut root_prevented_bubbles = hooks.use_state(|| 0usize);
    let last_event = hooks.use_state(|| "last event: none".to_string());

    let mut last_left_focus = last_event;
    let mut last_left_key = last_event;
    let mut last_left_paste = last_event;
    let mut last_right_focus = last_event;
    let mut last_right_key = last_event;
    let mut last_right_paste = last_event;

    element! {
        View(
            width: 64,
            flex_direction: FlexDirection::Column,
            on_key_down: move |key: ViewKeyboardEvent| {
                if matches!(key.code, KeyCode::Char(_)) {
                    root_bubbles += 1;
                    if key.default_prevented() {
                        root_prevented_bubbles += 1;
                    }
                }
            },
        ) {
            Text(content: "View focus/key/paste demo · Tab moves focus · s stops parent bubble · d prevents default · q quits")
            Text(content: format!("ancestor key bubbles={} defaultPrevented={}", root_bubbles.get(), root_prevented_bubbles.get()))
            Text(content: last_event.read().clone(), color: Color::Grey)
            FocusScope(trap_keys: Some(true)) {
                View(flex_direction: FlexDirection::Row, column_gap: 2) {
                    View(
                        focusable: true,
                        auto_focus: true,
                        width: 28,
                        border_style: BorderStyle::Round,
                        border_color: if focused.read().as_str() == "left" { Color::Green } else { Color::DarkGrey },
                        padding: 1,
                        on_focus: move |event: ViewFocusEvent| {
                            focused.set("left".to_string());
                            last_left_focus.set(format!("last event: {} bubbles={} cancelable={}", event.event_type, event.bubbles, event.cancelable));
                        },
                        on_key_down_capture: move |key: ViewKeyboardEvent| {
                            if key.phase == ViewEventPhase::AtTarget && matches!(key.code, KeyCode::Char(_)) {
                                if key.code == KeyCode::Char('s') {
                                    key.stop_propagation();
                                }
                                if key.code == KeyCode::Char('d') {
                                    key.prevent_default();
                                }
                                left_captures += 1;
                            }
                        },
                        on_key_down: move |key: ViewKeyboardEvent| {
                            last_left_key.set(format!("last event: {} bubbles={} cancelable={} defaultPrevented={}", key.event_type, key.bubbles, key.cancelable, key.default_prevented()));
                            if key.code == KeyCode::Char('q') {
                                app.exit();
                            } else if matches!(key.code, KeyCode::Char(_)) {
                                left_count += 1;
                            }
                        },
                        on_paste: move |event: ViewPasteEvent| {
                            last_left_paste.set(format!("last event: {} bubbles={} cancelable={}", event.event_type, event.bubbles, event.cancelable));
                            left_pastes += event.text.chars().count();
                        },
                    ) {
                        Text(content: format!("left cap={} keys={} paste_chars={}", left_captures.get(), left_count.get(), left_pastes.get()))
                    }
                    View(
                        focusable: true,
                        width: 28,
                        border_style: BorderStyle::Round,
                        border_color: if focused.read().as_str() == "right" { Color::Green } else { Color::DarkGrey },
                        padding: 1,
                        on_focus: move |event: ViewFocusEvent| {
                            focused.set("right".to_string());
                            last_right_focus.set(format!("last event: {} bubbles={} cancelable={}", event.event_type, event.bubbles, event.cancelable));
                        },
                        on_key_down_capture: move |key: ViewKeyboardEvent| {
                            if key.phase == ViewEventPhase::AtTarget && matches!(key.code, KeyCode::Char(_)) {
                                if key.code == KeyCode::Char('s') {
                                    key.stop_propagation();
                                }
                                if key.code == KeyCode::Char('d') {
                                    key.prevent_default();
                                }
                                right_captures += 1;
                            }
                        },
                        on_key_down: move |key: ViewKeyboardEvent| {
                            last_right_key.set(format!("last event: {} bubbles={} cancelable={} defaultPrevented={}", key.event_type, key.bubbles, key.cancelable, key.default_prevented()));
                            if key.code == KeyCode::Char('q') {
                                app.exit();
                            } else if matches!(key.code, KeyCode::Char(_)) {
                                right_count += 1;
                            }
                        },
                        on_paste: move |event: ViewPasteEvent| {
                            last_right_paste.set(format!("last event: {} bubbles={} cancelable={}", event.event_type, event.bubbles, event.cancelable));
                            right_pastes += event.text.chars().count();
                        },
                    ) {
                        Text(content: format!("right cap={} keys={} paste_chars={}", right_captures.get(), right_count.get(), right_pastes.get()))
                    }
                }
            }
        }
    }
}

fn main() -> std::io::Result<()> {
    let mut app = element!(FocusablePanelDemo);
    smol::block_on(app.render_loop())
}
