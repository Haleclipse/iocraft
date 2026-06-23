use crate::{context::ExitOnCtrlCContext, ComponentUpdater, Hook, Hooks};

use super::{State, UseContext, UseState};

mod private {
    pub trait Sealed {}
    impl Sealed for crate::Hooks<'_, '_> {}
}

/// Handle returned by [`UseStdin::use_stdin`].
///
/// This is the Rust counterpart to CC Ink's `useStdin()` context. iocraft does
/// not expose a raw `ReadStream`; the render loop owns stdin so it can multiplex
/// terminal events, Ctrl+C handling, bracketed paste, and focus reporting. This
/// handle exposes the same application-level capabilities that are safe in that
/// model: raw-mode support/status and a render-loop-mediated raw-mode request.
#[derive(Clone, Copy)]
pub struct StdinHandle {
    raw_mode_supported: State<bool>,
    raw_mode_enabled: State<bool>,
    requested_raw_mode: State<Option<bool>>,
    exit_on_ctrl_c: bool,
}

impl StdinHandle {
    /// Returns whether the current terminal input supports raw mode.
    pub fn is_raw_mode_supported(&self) -> bool {
        self.raw_mode_supported.get()
    }

    /// Returns whether the render loop currently believes raw mode is enabled.
    pub fn is_raw_mode_enabled(&self) -> bool {
        self.raw_mode_enabled.get()
    }

    /// Requests raw mode to be enabled or disabled during the next render update.
    ///
    /// The request is routed through iocraft's terminal owner instead of calling
    /// platform raw-mode APIs directly, mirroring Ink's guidance to use
    /// `setRawMode` from `StdinContext` rather than `process.stdin.setRawMode`.
    pub fn set_raw_mode(&mut self, enabled: bool) {
        self.requested_raw_mode.set(Some(enabled));
    }

    /// Returns the last raw-mode request made through this handle, if any.
    pub fn requested_raw_mode(&self) -> Option<bool> {
        self.requested_raw_mode.get()
    }

    /// Returns whether the render loop will perform the default Ctrl+C exit.
    ///
    /// This mirrors CC Ink's internal `exitOnCtrlC` value exposed through
    /// `useStdin()`: when this is `true`, [`UseInput`](crate::hooks::UseInput)
    /// suppresses ordinary Ctrl+C callbacks because the framework will exit by
    /// default. Apps that call
    /// [`RenderLoopFuture::ignore_ctrl_c`](crate::RenderLoopFuture::ignore_ctrl_c)
    /// can observe `false` here and handle Ctrl+C themselves.
    pub fn exit_on_ctrl_c(&self) -> bool {
        self.exit_on_ctrl_c
    }
}

/// Access to stdin/raw-mode status and controls.
///
/// This mirrors CC Ink's `useStdin()` at the safe Rust abstraction level. Use
/// [`UseInput`](crate::hooks::UseInput) for ordinary key/paste input callbacks;
/// use this hook when a component needs to check raw-mode availability or make
/// an explicit raw-mode request.
pub trait UseStdin: private::Sealed {
    /// Returns a copyable stdin handle.
    fn use_stdin(&mut self) -> StdinHandle;
}

impl UseStdin for Hooks<'_, '_> {
    fn use_stdin(&mut self) -> StdinHandle {
        let raw_mode_supported = self.use_state(|| false);
        let raw_mode_enabled = self.use_state(|| false);
        let requested_raw_mode = self.use_state(|| None::<bool>);
        let exit_on_ctrl_c = self
            .try_use_context::<ExitOnCtrlCContext>()
            .map(|context| context.0)
            .unwrap_or(true);

        let h = self.use_hook(|| UseStdinImpl {
            raw_mode_supported,
            raw_mode_enabled,
            requested_raw_mode,
        });
        h.raw_mode_supported = raw_mode_supported;
        h.raw_mode_enabled = raw_mode_enabled;
        h.requested_raw_mode = requested_raw_mode;

        StdinHandle {
            raw_mode_supported,
            raw_mode_enabled,
            requested_raw_mode,
            exit_on_ctrl_c,
        }
    }
}

struct UseStdinImpl {
    raw_mode_supported: State<bool>,
    raw_mode_enabled: State<bool>,
    requested_raw_mode: State<Option<bool>>,
}

impl Hook for UseStdinImpl {
    fn post_component_update(&mut self, updater: &mut ComponentUpdater) {
        if let Some(requested) = self.requested_raw_mode.get() {
            if let Some(terminal) = updater.terminal_mut() {
                let _ = terminal.set_raw_mode_enabled(requested);
            }
        }

        let supported = updater.is_terminal_raw_mode_supported();
        let enabled = updater.is_terminal_raw_mode_enabled();
        if self.raw_mode_supported.get() != supported {
            self.raw_mode_supported.set(supported);
        }
        if self.raw_mode_enabled.get() != enabled {
            self.raw_mode_enabled.set(enabled);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prelude::*;
    use futures::StreamExt;

    #[component]
    fn StdinStatusApp(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let stdin = hooks.use_stdin();

        if stdin.is_raw_mode_supported() && stdin.is_raw_mode_enabled() {
            system.exit();
        }

        element!(Text(content: format!(
            "supported={} enabled={} exit_on_ctrl_c={}",
            stdin.is_raw_mode_supported(),
            stdin.is_raw_mode_enabled(),
            stdin.exit_on_ctrl_c()
        )))
    }

    #[test]
    fn test_use_stdin_reports_mock_raw_mode_status() {
        let canvases: Vec<_> = smol::block_on(
            element!(StdinStatusApp)
                .mock_terminal_render_loop(MockTerminalConfig::default())
                .collect(),
        );

        assert_eq!(
            canvases.iter().map(Canvas::to_string).collect::<Vec<_>>(),
            vec![
                "supported=false enabled=false exit_on_ctrl_c=true\n".to_string(),
                "supported=true enabled=true exit_on_ctrl_c=true\n".to_string(),
            ]
        );
    }

    #[test]
    fn test_use_stdin_reports_disabled_default_ctrl_c_exit() {
        let canvases: Vec<_> = smol::block_on(
            element!(StdinStatusApp)
                .mock_terminal_render_loop(MockTerminalConfig::default().with_ignore_ctrl_c(true))
                .collect(),
        );

        assert_eq!(
            canvases.iter().map(Canvas::to_string).collect::<Vec<_>>(),
            vec![
                "supported=false enabled=false exit_on_ctrl_c=false\n".to_string(),
                "supported=true enabled=true exit_on_ctrl_c=false\n".to_string(),
            ]
        );
    }

    #[component]
    fn StdinRequestApp(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let mut stdin = hooks.use_stdin();
        let mut requested = hooks.use_state(|| false);

        if !requested.get() {
            stdin.set_raw_mode(false);
            requested.set(true);
        } else if stdin.requested_raw_mode() == Some(false) {
            system.exit();
        }

        element!(Text(content: format!(
            "requested={:?}",
            stdin.requested_raw_mode()
        )))
    }

    #[test]
    fn test_use_stdin_raw_mode_request_is_retained() {
        let canvases: Vec<_> = smol::block_on(
            element!(StdinRequestApp)
                .mock_terminal_render_loop(MockTerminalConfig::default())
                .collect(),
        );

        assert_eq!(
            canvases.last().unwrap().to_string(),
            "requested=Some(false)\n"
        );
    }
}
