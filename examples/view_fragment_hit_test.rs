//! Demonstrates CC Ink-style root hit-testing across Fragment siblings.
//!
//! The blue overlay is a later Fragment child painted over the gray base view.
//! Click the overlap: only the topmost sibling should receive the click. Press
//! `q` to quit.

use iocraft::prelude::*;

#[component]
fn FragmentHitTestDemo(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let mut app = hooks.use_app();
    let mut base_clicks = hooks.use_state(|| 0usize);
    let mut overlay_clicks = hooks.use_state(|| 0usize);

    hooks.use_keybinding("q", move || app.exit());

    element! {
        AlternateScreen {
            View(width: 100pct, height: 100pct, padding: 2) {
                Text(content: "Fragment root hit-test demo · click overlay · q quits")
                Newline
                Fragment {
                    View(
                        width: 36,
                        height: 5,
                        border_style: BorderStyle::Round,
                        border_color: Color::DarkGrey,
                        padding: 1,
                        on_click: move |_| base_clicks += 1,
                    ) {
                        Text(content: format!("base clicks={}", base_clicks.get()))
                    }
                    View(
                        width: 28,
                        height: 3,
                        position: Position::Absolute,
                        top: 4,
                        left: 6,
                        border_style: BorderStyle::Round,
                        border_color: Color::Blue,
                        background_color: Some(Color::DarkBlue),
                        padding_left: 1,
                        padding_right: 1,
                        on_click: move |_| overlay_clicks += 1,
                    ) {
                        Text(content: format!("overlay clicks={}", overlay_clicks.get()))
                    }
                }
            }
        }
    }
}

fn main() -> std::io::Result<()> {
    let mut app = element!(FragmentHitTestDemo);
    smol::block_on(app.render_loop())
}
