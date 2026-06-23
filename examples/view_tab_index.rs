//! Demonstrates CC Ink-style `View(tab_index: ...)` focus behavior.
//!
//! The left panel uses `tab_index: Some(-1)`: it can be focused by `auto_focus`
//! or mouse click, but Tab skips it. The right panel uses `focusable: true`, the
//! iocraft shorthand for `tabIndex={0}`. Press Tab/Shift+Tab and click panels;
//! press `q` to quit.

use iocraft::prelude::*;

#[component]
fn ViewTabIndexDemo(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let mut app = hooks.use_app();
    let mut focused = hooks.use_state(|| "programmatic".to_string());
    let mut programmatic_keys = hooks.use_state(|| 0usize);
    let mut tabbable_keys = hooks.use_state(|| 0usize);

    hooks.use_keybinding("q", move || app.exit());

    element! {
        AlternateScreen(mouse_tracking: Some(true)) {
            View(
                width: 100pct,
                height: 100pct,
                flex_direction: FlexDirection::Column,
                justify_content: JustifyContent::CENTER,
                align_items: AlignItems::CENTER,
            ) {
                Text(content: "View tab_index demo · Tab skips the left panel · q quits")
                Newline
                FocusScope(trap_keys: Some(true)) {
                    View(flex_direction: FlexDirection::Row, column_gap: 2) {
                        View(
                            tab_index: Some(-1),
                            auto_focus: true,
                            width: 32,
                            height: 5,
                            border_style: BorderStyle::Round,
                            border_color: if focused.read().as_str() == "programmatic" { Color::Green } else { Color::DarkGrey },
                            padding: 1,
                            on_focus: move |_| focused.set("programmatic".to_string()),
                            on_key_down: move |key: ViewKeyboardEvent| {
                                if matches!(key.code, KeyCode::Char(_)) {
                                    programmatic_keys += 1;
                                }
                            },
                        ) {
                            Text(content: "tab_index = -1")
                            Text(content: format!("keys while focused: {}", programmatic_keys.get()))
                        }
                        View(
                            focusable: true,
                            width: 32,
                            height: 5,
                            border_style: BorderStyle::Round,
                            border_color: if focused.read().as_str() == "tabbable" { Color::Green } else { Color::DarkGrey },
                            padding: 1,
                            on_focus: move |_| focused.set("tabbable".to_string()),
                            on_key_down: move |key: ViewKeyboardEvent| {
                                if matches!(key.code, KeyCode::Char(_)) {
                                    tabbable_keys += 1;
                                }
                            },
                        ) {
                            Text(content: "focusable = true")
                            Text(content: format!("keys while focused: {}", tabbable_keys.get()))
                        }
                    }
                }
            }
        }
    }
}

fn main() -> std::io::Result<()> {
    let mut app = element!(ViewTabIndexDemo);
    smol::block_on(app.render_loop())
}
