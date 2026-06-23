//! Demonstrates CC Ink-style `Button` helpers.
//!
//! The button defaults to CC Ink-style `tabIndex=0`, supports `auto_focus`,
//! exposes a `ButtonState` ref, and calls `on_action` when Enter/Space or mouse click
//! activates it. Press `q` to quit.

use iocraft::prelude::*;

#[component]
fn ButtonDemo(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let mut app = hooks.use_app();
    let mut presses = hooks.use_state(|| 0usize);
    let button_state = hooks.use_ref_default::<ButtonState>();

    hooks.use_keybinding("q", move || app.exit());

    let state = *button_state.read();
    element! {
        View(width: 56, flex_direction: FlexDirection::Column) {
            Text(content: "Focused button: Enter/Space/click activates · q quits")
            FocusScope(trap_keys: Some(true)) {
                Button(
                    auto_focus: true,
                    state: Some(button_state),
                    on_action: move |_| presses += 1,
                ) {
                    View(
                        border_style: BorderStyle::Round,
                        border_color: if state.focused { Color::Green } else { Color::DarkGrey },
                        padding_left: 1,
                        padding_right: 1,
                    ) {
                        Text(content: format!("presses={}", presses.get()))
                    }
                }
            }
            Text(content: format!(
                "ButtonState focused={} hovered={} active={}",
                state.focused,
                state.hovered,
                state.active,
            ))
        }
    }
}

fn main() -> std::io::Result<()> {
    let mut app = element!(ButtonDemo);
    smol::block_on(app.render_loop())
}
