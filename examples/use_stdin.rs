//! Demonstrates the CC Ink-style `use_stdin` hook.
//!
//! iocraft owns the terminal input stream internally, but the hook exposes the
//! raw-mode status/control surface that Ink apps commonly use, plus the
//! CC Ink-style `exit_on_ctrl_c` flag. Press `r` to request raw mode on again,
//! or `q` to quit.

use iocraft::prelude::*;

#[component]
fn UseStdinDemo(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let mut app = hooks.use_app();
    let mut stdin = hooks.use_stdin();
    let mut requests = hooks.use_state(|| 0usize);

    hooks.use_keybinding("q", move || app.exit());
    hooks.use_keybinding("r", move || {
        stdin.set_raw_mode(true);
        requests += 1;
    });

    element! {
        View(flex_direction: FlexDirection::Column) {
            Text(content: "use_stdin demo · press r to request raw mode · q quits")
            Text(content: format!(
                "raw supported={} enabled={} exit_on_ctrl_c={} last_request={:?} request_count={}",
                stdin.is_raw_mode_supported(),
                stdin.is_raw_mode_enabled(),
                stdin.exit_on_ctrl_c(),
                stdin.requested_raw_mode(),
                requests.get(),
            ))
        }
    }
}

fn main() -> std::io::Result<()> {
    let mut app = element!(UseStdinDemo);
    smol::block_on(app.render_loop())
}
