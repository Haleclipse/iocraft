//! Demonstrates CC Ink-style focus-stack restoration.
//!
//! Tab to the middle panel, press `x` to remove it, and focus returns to the
//! previously focused mounted panel instead of falling through to the next
//! sibling. Press `q` to quit.

use iocraft::prelude::*;

#[component]
fn FocusRestoreDemo(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let mut app = hooks.use_app();
    let mut show_middle = hooks.use_state(|| true);
    let mut focused = hooks.use_state(|| "none".to_string());

    hooks.use_keybinding("q", move || app.exit());

    let focused_label = focused.read().clone();
    element! {
        View(width: 68, flex_direction: FlexDirection::Column, row_gap: 1) {
            Text(content: "Focus restore demo · Tab to Middle · x removes it · q quits")
            Text(content: format!("focused: {focused_label}"), color: Color::Grey)
            FocusScope(trap_keys: Some(true)) {
                View(flex_direction: FlexDirection::Column, row_gap: 1) {
                    View(
                        focusable: true,
                        auto_focus: true,
                        border_style: BorderStyle::Round,
                        border_color: if focused.read().as_str() == "left" { Color::Green } else { Color::DarkGrey },
                        padding_left: 1,
                        padding_right: 1,
                        on_focus: move |_| focused.set("left".to_string()),
                    ) {
                        Text(content: "Left / previous focus target")
                    }
                    #(show_middle.get().then(|| element! {
                        View(
                            focusable: true,
                            border_style: BorderStyle::Round,
                            border_color: if focused.read().as_str() == "middle" { Color::Yellow } else { Color::DarkGrey },
                            padding_left: 1,
                            padding_right: 1,
                            on_focus: move |_| focused.set("middle".to_string()),
                            on_key_down: move |event: ViewKeyboardEvent| {
                                if event.code == KeyCode::Char('x') {
                                    show_middle.set(false);
                                    event.prevent_default();
                                }
                            },
                        ) {
                            Text(content: "Middle / press x to unmount")
                        }
                    }))
                    View(
                        focusable: true,
                        border_style: BorderStyle::Round,
                        border_color: if focused.read().as_str() == "right" { Color::Green } else { Color::DarkGrey },
                        padding_left: 1,
                        padding_right: 1,
                        on_focus: move |_| focused.set("right".to_string()),
                    ) {
                        Text(content: "Right / next sibling")
                    }
                }
            }
        }
    }
}

fn main() -> std::io::Result<()> {
    let mut app = element!(FocusRestoreDemo);
    smol::block_on(app.render_loop())
}
