use crate::{ComponentUpdater, Hook, Hooks, TerminalEvent};

use super::{State, UseState, UseTerminalEvents};

mod private {
    pub trait Sealed {}
    impl Sealed for crate::Hooks<'_, '_> {}
}

/// Terminal focus state reported by DECSET 1004 focus events.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TerminalFocusState {
    /// No focus event has been observed yet. Treat as focused by default,
    /// matching CC Ink's `useTerminalFocus()` behavior.
    #[default]
    Unknown,
    /// The terminal reported focus gained.
    Focused,
    /// The terminal reported focus lost.
    Blurred,
}

impl TerminalFocusState {
    /// Returns `true` when components should behave as if the terminal is
    /// focused. Unknown is treated as focused.
    pub fn is_focused(self) -> bool {
        !matches!(self, TerminalFocusState::Blurred)
    }
}

/// Hook for checking whether the terminal currently has focus.
pub trait UseTerminalFocus: private::Sealed {
    /// Returns the full terminal focus state.
    fn use_terminal_focus_state(&mut self) -> TerminalFocusState;

    /// Returns `true` if the terminal is focused, or if focus state is unknown.
    fn use_terminal_focus(&mut self) -> bool {
        self.use_terminal_focus_state().is_focused()
    }
}

impl UseTerminalFocus for Hooks<'_, '_> {
    fn use_terminal_focus_state(&mut self) -> TerminalFocusState {
        let mut state = self.use_state(TerminalFocusState::default);
        let hook = self.use_hook(|| UseTerminalFocusImpl { state });
        hook.state = state;
        self.use_terminal_events(move |event| match event {
            TerminalEvent::FocusGained => state.set(TerminalFocusState::Focused),
            TerminalEvent::FocusLost => state.set(TerminalFocusState::Blurred),
            _ => {}
        });
        state.get()
    }
}

struct UseTerminalFocusImpl {
    state: State<TerminalFocusState>,
}

impl Hook for UseTerminalFocusImpl {
    fn post_component_update(&mut self, updater: &mut ComponentUpdater) {
        let Some(focused) = updater
            .terminal_mut()
            .and_then(|terminal| terminal.terminal_focus_state())
        else {
            return;
        };
        let next = if focused {
            TerminalFocusState::Focused
        } else {
            TerminalFocusState::Blurred
        };
        if self.state.get() != next {
            self.state.set(next);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prelude::*;
    use futures::{stream, StreamExt};

    #[component]
    fn TerminalFocusProbe(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let state = hooks.use_terminal_focus_state();
        let focused = state.is_focused();
        if state != TerminalFocusState::Unknown {
            system.exit();
        }
        element!(Text(content: format!("focused={focused} state={state:?}")))
    }

    #[test]
    fn test_use_terminal_focus_tracks_focus_lost() {
        let canvases: Vec<_> = smol::block_on(
            element!(TerminalFocusProbe)
                .mock_terminal_render_loop(MockTerminalConfig::with_events(stream::iter(vec![
                    TerminalEvent::FocusLost,
                ])))
                .collect(),
        );

        assert_eq!(
            canvases.last().unwrap().to_string(),
            "focused=false state=Blurred\n"
        );
    }

    #[test]
    fn test_use_terminal_focus_tracks_latest_focus_event() {
        let canvases: Vec<_> = smol::block_on(
            element!(TerminalFocusProbe)
                .mock_terminal_render_loop(MockTerminalConfig::with_events(stream::iter(vec![
                    TerminalEvent::FocusLost,
                    TerminalEvent::FocusGained,
                ])))
                .collect(),
        );

        assert_eq!(
            canvases.last().unwrap().to_string(),
            "focused=true state=Focused\n"
        );
    }

    #[component]
    fn UnknownFocusProbe(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let focused = hooks.use_terminal_focus();
        let state = hooks.use_terminal_focus_state();
        system.exit();
        element!(Text(content: format!("focused={focused} state={state:?}")))
    }

    #[test]
    fn test_use_terminal_focus_treats_unknown_as_focused() {
        let canvases: Vec<_> = smol::block_on(
            element!(UnknownFocusProbe)
                .mock_terminal_render_loop(MockTerminalConfig::default())
                .collect(),
        );
        assert_eq!(
            canvases.last().unwrap().to_string(),
            "focused=true state=Unknown\n"
        );
    }

    #[component]
    fn LateFocusConsumer(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let state = hooks.use_terminal_focus_state();
        if state != TerminalFocusState::Unknown {
            system.exit();
        }
        element!(Text(content: format!("late={state:?}")))
    }

    #[component]
    fn LateFocusApp(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut saw_focus_event = hooks.use_state(|| false);
        hooks.use_terminal_events(move |event| {
            if matches!(event, TerminalEvent::FocusLost) {
                saw_focus_event.set(true);
            }
        });

        element! {
            View {
                #(if saw_focus_event.get() {
                    element!(LateFocusConsumer).into_any()
                } else {
                    element!(Text(content: "waiting")).into_any()
                })
            }
        }
    }

    #[test]
    fn test_use_terminal_focus_late_mount_reads_current_terminal_state() {
        let canvases: Vec<_> = smol::block_on(
            element!(LateFocusApp)
                .mock_terminal_render_loop(MockTerminalConfig::with_events(stream::iter(vec![
                    TerminalEvent::FocusLost,
                ])))
                .collect(),
        );

        assert_eq!(canvases.last().unwrap().to_string(), "late=Blurred\n");
    }
}
