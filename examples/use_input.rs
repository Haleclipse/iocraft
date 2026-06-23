//! Demonstrates the `use_input` convenience hook.
//!
//! It maps terminal key/paste events into `(input, key)` callbacks, similar to
//! CC Ink's `useInput(...)` hook. The render loop opts out of the default
//! Ctrl+C exit so the hook can demonstrate custom Ctrl+C handling. It also
//! opts into CC Ink/Claude Code-style Ctrl+Z suspension; generic iocraft apps
//! receive Ctrl+Z as ordinary input unless they enable that policy.

use iocraft::prelude::*;

#[component]
fn UseInputDemo(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let app = hooks.use_app();
    let text = hooks.use_state(String::new);
    let text_for_handler = text;

    hooks.use_input(move |input, key| {
        let mut next = text_for_handler.read().clone();
        if key.escape || (key.ctrl && input.eq_ignore_ascii_case("c")) {
            let mut app = app;
            app.exit();
        } else if key.ctrl && input == " " {
            next.push_str("<ctrl-space>");
        } else if key.backspace {
            next.pop();
        } else if key.paste {
            next.push_str(&format!("[paste:{input}]"));
        } else if key.wheel_up {
            next.push_str("<wheel-up>");
        } else if key.wheel_down {
            next.push_str("<wheel-down>");
        } else if key.left_arrow {
            next.push_str("<left>");
        } else if key.return_key {
            next.push_str("<enter>");
        } else {
            next.push_str(&input);
        }
        let mut text = text_for_handler;
        text.set(next);
    });

    element! {
        View(flex_direction: FlexDirection::Column) {
            Text(content: "Type text, paste, scroll, use left arrow/Enter, Ctrl+Space; Esc/Ctrl+C exits; Ctrl+Z suspends on Unix.")
            Text(content: format!("input: {}", &*text.read()))
        }
    }
}

fn main() -> std::io::Result<()> {
    let mut app = element!(UseInputDemo);
    smol::block_on(app.render_loop().ignore_ctrl_c().suspend_on_ctrl_z())
}
