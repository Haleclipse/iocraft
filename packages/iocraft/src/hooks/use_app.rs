use crate::{context::TerminalHandoffRequest, Hooks, SystemContext};
use std::io;

use super::{State, UseContext, UseState};

mod private {
    pub trait Sealed {}
    impl Sealed for crate::Hooks<'_, '_> {}
}

/// Handle returned by [`UseApp::use_app`].
#[derive(Clone, Copy)]
pub struct AppHandle {
    should_exit: State<bool>,
    terminal_handoff: State<Option<TerminalHandoffRequest>>,
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

    /// Releases terminal ownership to a synchronous closure, then restores the
    /// TUI after it returns.
    ///
    /// The render loop:
    /// 1. disables raw/input modes and drops the event stream
    /// 2. runs `f` while the terminal is in cooked mode
    /// 3. reacquires modes, rebuilds the event stream, and forces a full repaint
    ///
    /// This is a terminal-ownership primitive, not a process launcher. The
    /// closure decides what to do with the terminal — open `$EDITOR`, print a
    /// pager, prompt for a password, etc.
    ///
    /// If releasing the terminal fails, `f` is not invoked and the receiver
    /// yields the release error.
    pub fn suspend_terminal<T, F>(
        &mut self,
        f: F,
    ) -> futures::channel::oneshot::Receiver<io::Result<T>>
    where
        T: Send + 'static,
        F: FnOnce() -> T + Send + 'static,
    {
        let (sender, receiver) = futures::channel::oneshot::channel();
        let job = Box::new(move |release: io::Result<()>| {
            let result = match release {
                Ok(()) => Ok(f()),
                Err(error) => Err(error),
            };
            let _ = sender.send(result);
        });
        self.terminal_handoff
            .set(Some(TerminalHandoffRequest::new(job)));
        receiver
    }
}

/// Access to app-level lifecycle controls.
///
/// This is the iocraft counterpart to CC Ink's `useApp()` hook. It exposes an
/// exit handle that can be moved into callbacks instead of requiring callers to
/// thread their own `should_exit` state and call [`SystemContext::exit`] during
/// render.
pub trait UseApp: private::Sealed {
    /// Returns a handle for requesting app exit and temporary terminal handoff.
    fn use_app(&mut self) -> AppHandle;
}

impl UseApp for Hooks<'_, '_> {
    fn use_app(&mut self) -> AppHandle {
        let should_exit = self.use_state(|| false);
        let mut terminal_handoff = self.use_state(|| None::<TerminalHandoffRequest>);
        let pending_handoff = terminal_handoff.read().clone();
        if should_exit.get() {
            self.use_context_mut::<SystemContext>().exit();
        }
        if let Some(request) = pending_handoff {
            terminal_handoff.set(None);
            self.use_context_mut::<SystemContext>()
                .request_terminal_handoff(request);
        }
        AppHandle {
            should_exit,
            terminal_handoff,
        }
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

    #[cfg(unix)]
    #[component]
    fn SuspendTerminalApp(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut app = hooks.use_app();
        let mut result = hooks.use_state(|| None::<i32>);
        let display = result
            .read()
            .map_or_else(|| "pending".to_string(), |code| format!("code={code}"));
        hooks.use_future(async move {
            let receiver = app.suspend_terminal(|| {
                std::process::Command::new("sh")
                    .args(["-c", "exit 7"])
                    .status()
                    .ok()
                    .and_then(|status| status.code())
            });
            if let Ok(Ok(Some(code))) = receiver.await {
                result.set(Some(code));
                app.exit();
            }
        });
        element!(Text(content: display))
    }

    #[cfg(unix)]
    #[test]
    fn test_suspend_terminal_runs_closure_and_restores() {
        let canvases: Vec<_> = smol::block_on(
            element!(SuspendTerminalApp)
                .mock_terminal_render_loop(MockTerminalConfig::default())
                .collect(),
        );
        assert!(canvases
            .iter()
            .any(|canvas| canvas.to_string().contains("code=7")));
    }
}
