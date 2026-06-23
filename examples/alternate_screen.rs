//! Demonstrates dynamic CC Ink-style `AlternateScreen`.
//!
//! The app starts on the main screen. Press `f` to enter the alternate screen,
//! `Esc` to return to the main screen, or `q` to quit.

use iocraft::prelude::*;

#[component]
fn AlternateScreenDemo(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let mut app = hooks.use_app();
    let mut fullscreen = hooks.use_state(|| false);

    hooks.use_input(move |input, key| {
        if input == "q" {
            app.exit();
        } else if input == "f" {
            fullscreen.set(true);
        } else if key.escape {
            fullscreen.set(false);
        }
    });

    if fullscreen.get() {
        element! {
            AlternateScreen {
                View(
                    width: 100pct,
                    height: 100pct,
                    flex_direction: FlexDirection::Column,
                    justify_content: JustifyContent::CENTER,
                    align_items: AlignItems::CENTER,
                    border_style: BorderStyle::Double,
                    border_color: Color::Blue,
                ) {
                    Text(content: "Alternate screen is active")
                    Newline
                    Text(content: "Press Esc to restore the main screen")
                }
            }
        }
        .into_any()
    } else {
        element! {
            View(flex_direction: FlexDirection::Column) {
                Text(content: "Main screen: native scrollback is preserved.")
                Text(content: "Press f for AlternateScreen, q to quit.")
            }
        }
        .into_any()
    }
}

fn main() -> std::io::Result<()> {
    let mut app = element!(AlternateScreenDemo);
    smol::block_on(app.render_loop())
}
