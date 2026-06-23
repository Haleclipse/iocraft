use crate::{Hooks, SystemContext};

use super::{State, UseContext, UseState};

mod private {
    pub trait Sealed {}
    impl Sealed for crate::Hooks<'_, '_> {}
}

/// Handle returned by [`UseApp::use_app`].
#[derive(Clone, Copy)]
pub struct AppHandle {
    should_exit: State<bool>,
}

impl AppHandle {
    /// Requests that the render loop exits after the next render pass.
    ///
    /// The handle is copyable and can be moved into event handlers/futures,
    /// matching CC Ink's `useApp().exit()` use case.
    pub fn exit(&mut self) {
        self.should_exit.set(true);
    }

    /// Returns whether exit has been requested through this handle.
    pub fn should_exit(&self) -> bool {
        self.should_exit.get()
    }
}

/// Access to app-level lifecycle controls.
///
/// This is the iocraft counterpart to CC Ink's `useApp()` hook. It exposes an
/// exit handle that can be moved into callbacks instead of requiring callers to
/// thread their own `should_exit` state and call [`SystemContext::exit`] during
/// render.
pub trait UseApp: private::Sealed {
    /// Returns a handle for requesting app exit.
    fn use_app(&mut self) -> AppHandle;
}

impl UseApp for Hooks<'_, '_> {
    fn use_app(&mut self) -> AppHandle {
        let should_exit = self.use_state(|| false);
        if should_exit.get() {
            self.use_context_mut::<SystemContext>().exit();
        }
        AppHandle { should_exit }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prelude::*;
    use futures::StreamExt;

    #[component]
    fn ExitFromFutureApp(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut app = hooks.use_app();
        let exiting = app.should_exit();
        hooks.use_future(async move {
            app.exit();
        });
        element!(Text(content: format!("exiting={exiting}")))
    }

    #[test]
    fn test_use_app_exit_handle_can_exit_from_future() {
        let canvases: Vec<_> = smol::block_on(
            element!(ExitFromFutureApp)
                .mock_terminal_render_loop(MockTerminalConfig::default())
                .collect(),
        );

        assert_eq!(
            canvases.iter().map(Canvas::to_string).collect::<Vec<_>>(),
            vec!["exiting=false\n".to_string(), "exiting=true\n".to_string()]
        );
    }
}
