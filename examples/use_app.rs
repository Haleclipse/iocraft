//! Demonstrates the `use_app` lifecycle hook.
//!
//! The returned handle can be moved into callbacks/futures and used to request
//! app exit, mirroring CC Ink's `useApp().exit()`.

use iocraft::prelude::*;

#[component]
fn UseAppDemo(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let mut app = hooks.use_app();
    let exiting = app.should_exit();

    hooks.use_future(async move {
        app.exit();
    });

    element!(Text(content: format!("exit requested={exiting}")))
}

fn main() -> std::io::Result<()> {
    let mut app = element!(UseAppDemo);
    smol::block_on(app.render_loop())
}
