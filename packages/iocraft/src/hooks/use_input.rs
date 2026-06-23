use crate::{
    context::ExitOnCtrlCContext, Hooks, KeyCode, KeyEvent, KeyEventKind, KeyModifiers,
    MouseEventKind, PropagatedTerminalEvent, TerminalEvent,
};

use super::{UseContext, UseTerminalEvents};

mod private {
    pub trait Sealed {}
    impl Sealed for crate::Hooks<'_, '_> {}
}

/// Convenience key description passed to [`UseInput`] handlers.
///
/// This mirrors the CC Ink fork's `useInput((input, key) => ...)` shape while
/// keeping the original terminal concepts explicit for Rust callers.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InputKey {
    /// Original key code for key events, or `None` for paste events.
    pub code: Option<KeyCode>,
    /// Whether Ctrl/Control was held.
    pub ctrl: bool,
    /// Whether Alt/Option was held.
    pub alt: bool,
    /// CC Ink-style alias for Alt/Option.
    pub meta: bool,
    /// Whether Shift was held.
    pub shift: bool,
    /// Whether Super/Cmd/Meta was held.
    pub super_key: bool,
    /// Function key (`F1`–`F24`).
    pub fn_key: bool,
    /// Left arrow key.
    pub left_arrow: bool,
    /// Right arrow key.
    pub right_arrow: bool,
    /// Up arrow key.
    pub up_arrow: bool,
    /// Down arrow key.
    pub down_arrow: bool,
    /// Page Up key.
    pub page_up: bool,
    /// Page Down key.
    pub page_down: bool,
    /// Mouse wheel up event.
    pub wheel_up: bool,
    /// Mouse wheel down event.
    pub wheel_down: bool,
    /// Home key.
    pub home: bool,
    /// End key.
    pub end: bool,
    /// Enter/Return key.
    pub return_key: bool,
    /// Escape key.
    pub escape: bool,
    /// Backspace key.
    pub backspace: bool,
    /// Delete key.
    pub delete: bool,
    /// Tab or BackTab key.
    pub tab: bool,
    /// Bracketed paste event.
    pub paste: bool,
}

impl InputKey {
    fn from_key_event(event: &KeyEvent) -> Self {
        Self {
            code: Some(event.code),
            ctrl: event.modifiers.contains(KeyModifiers::CONTROL),
            alt: event.modifiers.contains(KeyModifiers::ALT),
            // CC Ink's InputEvent keeps the historical `meta` behavior where
            // Escape itself sets meta=true, in addition to Alt/Option-modified
            // keys. DOM-style ViewKeyboardEvent keeps meta == Alt/Option.
            meta: event.modifiers.contains(KeyModifiers::ALT) || event.code == KeyCode::Esc,
            // CC Ink also infers Shift for uppercase printable letters even
            // when the terminal/parser reports only the character.
            shift: event.modifiers.contains(KeyModifiers::SHIFT)
                || matches!(event.code, KeyCode::Char(ch) if ch.is_ascii_uppercase()),
            super_key: event.modifiers.contains(KeyModifiers::SUPER),
            fn_key: matches!(event.code, KeyCode::F(_)),
            left_arrow: event.code == KeyCode::Left,
            right_arrow: event.code == KeyCode::Right,
            up_arrow: event.code == KeyCode::Up,
            down_arrow: event.code == KeyCode::Down,
            page_up: event.code == KeyCode::PageUp,
            page_down: event.code == KeyCode::PageDown,
            wheel_up: false,
            wheel_down: false,
            home: event.code == KeyCode::Home,
            end: event.code == KeyCode::End,
            return_key: event.code == KeyCode::Enter,
            escape: event.code == KeyCode::Esc,
            backspace: event.code == KeyCode::Backspace,
            delete: event.code == KeyCode::Delete,
            tab: matches!(event.code, KeyCode::Tab | KeyCode::BackTab),
            paste: false,
        }
    }

    fn paste() -> Self {
        Self {
            paste: true,
            ..Default::default()
        }
    }

    fn wheel_up(modifiers: KeyModifiers) -> Self {
        Self {
            ctrl: modifiers.contains(KeyModifiers::CONTROL),
            alt: modifiers.contains(KeyModifiers::ALT),
            meta: modifiers.contains(KeyModifiers::ALT),
            shift: modifiers.contains(KeyModifiers::SHIFT),
            super_key: modifiers.contains(KeyModifiers::SUPER),
            wheel_up: true,
            ..Default::default()
        }
    }

    fn wheel_down(modifiers: KeyModifiers) -> Self {
        Self {
            ctrl: modifiers.contains(KeyModifiers::CONTROL),
            alt: modifiers.contains(KeyModifiers::ALT),
            meta: modifiers.contains(KeyModifiers::ALT),
            shift: modifiers.contains(KeyModifiers::SHIFT),
            super_key: modifiers.contains(KeyModifiers::SUPER),
            wheel_down: true,
            ..Default::default()
        }
    }
}

/// Input event handle passed to [`UseInput::use_input_event`] handlers.
///
/// This mirrors CC Ink's third `event` argument enough for layered input
/// handlers to coordinate propagation. Call [`Self::stop_propagation`] from a
/// focused child to prevent ancestor/global propagation-aware input listeners
/// from handling the same terminal event.
pub struct InputEvent<'a> {
    propagated: &'a PropagatedTerminalEvent,
}

impl InputEvent<'_> {
    /// Returns the underlying terminal event.
    pub fn terminal_event(&self) -> &TerminalEvent {
        self.propagated.event()
    }

    /// Stops delivery to later propagation-aware listeners.
    pub fn stop_propagation(&self) {
        self.propagated.stop_propagation();
    }

    /// Returns whether propagation has already been stopped.
    pub fn is_propagation_stopped(&self) -> bool {
        self.propagated.is_propagation_stopped()
    }

    /// Prevents the framework-level default action for this input event, such
    /// as [`FocusScope`](crate::components::FocusScope)'s Tab traversal.
    pub fn prevent_default(&self) {
        self.propagated.prevent_default();
    }

    /// Returns whether default handling has already been prevented.
    pub fn is_default_prevented(&self) -> bool {
        self.propagated.is_default_prevented()
    }
}

/// Options for [`UseInput::use_input_with_options`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UseInputOptions {
    /// Whether the handler is active. Defaults to `true`.
    pub active: bool,
}

impl Default for UseInputOptions {
    fn default() -> Self {
        Self { active: true }
    }
}

fn input_for_key_event(event: &KeyEvent) -> String {
    match event.code {
        KeyCode::Char(ch) => ch.to_string(),
        // Crossterm reports Ctrl+Space as `Null` on some terminals. CC Ink
        // normalizes ctrl+space's `keypress.name === "space"` to a literal
        // space input instead of leaking the word "space" or an empty string.
        KeyCode::Null if event.modifiers.contains(KeyModifiers::CONTROL) => " ".to_string(),
        // Match CC Ink's InputEvent: non-alphanumeric/special keys communicate
        // through the `key` booleans, not through raw control bytes in `input`.
        // This prevents Enter/Tab/Escape/Backspace from leaking terminal control
        // characters into text handlers that simply append `input`.
        KeyCode::Enter | KeyCode::Tab | KeyCode::BackTab | KeyCode::Backspace | KeyCode::Esc => {
            String::new()
        }
        _ => String::new(),
    }
}

/// Convenience hook for handling keyboard input.
///
/// This is the iocraft counterpart to CC Ink's `useInput(...)`. It sits on top
/// of [`UseTerminalEvents`] so raw-mode setup, bracketed paste, focus reporting,
/// and event propagation stay centralized.
pub trait UseInput: private::Sealed {
    /// Registers an active input handler.
    fn use_input<F>(&mut self, handler: F)
    where
        F: FnMut(String, InputKey) + Send + 'static;

    /// Registers an input handler with explicit active/inactive options.
    fn use_input_with_options<F>(&mut self, options: UseInputOptions, handler: F)
    where
        F: FnMut(String, InputKey) + Send + 'static;

    /// Registers an active input handler that receives a propagation handle.
    fn use_input_event<F>(&mut self, handler: F)
    where
        F: for<'event> FnMut(String, InputKey, InputEvent<'event>) + Send + 'static;

    /// Registers an input-event handler with explicit active/inactive options.
    fn use_input_event_with_options<F>(&mut self, options: UseInputOptions, handler: F)
    where
        F: for<'event> FnMut(String, InputKey, InputEvent<'event>) + Send + 'static;
}

impl UseInput for Hooks<'_, '_> {
    fn use_input<F>(&mut self, handler: F)
    where
        F: FnMut(String, InputKey) + Send + 'static,
    {
        self.use_input_with_options(UseInputOptions::default(), handler);
    }

    fn use_input_with_options<F>(&mut self, options: UseInputOptions, mut handler: F)
    where
        F: FnMut(String, InputKey) + Send + 'static,
    {
        self.use_input_event_with_options(options, move |input, key, _event| {
            handler(input, key);
        });
    }

    fn use_input_event<F>(&mut self, handler: F)
    where
        F: for<'event> FnMut(String, InputKey, InputEvent<'event>) + Send + 'static,
    {
        self.use_input_event_with_options(UseInputOptions::default(), handler);
    }

    fn use_input_event_with_options<F>(&mut self, options: UseInputOptions, mut handler: F)
    where
        F: for<'event> FnMut(String, InputKey, InputEvent<'event>) + Send + 'static,
    {
        let exit_on_ctrl_c = self
            .try_use_context::<ExitOnCtrlCContext>()
            .map(|context| context.0)
            .unwrap_or(true);
        self.use_propagated_terminal_events(move |event| {
            if !options.active {
                return;
            }
            let input_event = InputEvent { propagated: event };
            match event.event() {
                TerminalEvent::Key(event) if event.kind != KeyEventKind::Release => {
                    let input = input_for_key_event(event);
                    let key = InputKey::from_key_event(event);
                    if exit_on_ctrl_c && key.ctrl && input.eq_ignore_ascii_case("c") {
                        return;
                    }
                    handler(input, key, input_event);
                }
                TerminalEvent::Paste(text) => {
                    handler(text.clone(), InputKey::paste(), input_event);
                }
                TerminalEvent::FullscreenMouse(event) => match event.kind {
                    MouseEventKind::ScrollUp => {
                        handler(
                            String::new(),
                            InputKey::wheel_up(event.modifiers),
                            input_event,
                        );
                    }
                    MouseEventKind::ScrollDown => {
                        handler(
                            String::new(),
                            InputKey::wheel_down(event.modifiers),
                            input_event,
                        );
                    }
                    _ => {}
                },
                _ => {}
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prelude::*;
    use futures::{stream, StreamExt};

    #[component]
    fn InputProbe(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let seen = hooks.use_state(String::new);
        let seen_for_handler = seen;
        hooks.use_input(move |input, key| {
            let mut next = seen_for_handler.read().clone();
            if key.paste {
                next.push_str(&format!("paste:{input}"));
            } else if key.wheel_down {
                next.push_str("<wheel-down>");
            } else if key.fn_key {
                next.push_str("<fn>");
            } else if key.left_arrow {
                next.push_str("<left>");
            } else if key.return_key {
                next.push_str("<enter>");
            } else {
                next.push_str(&input);
            }
            let mut seen = seen_for_handler;
            seen.set(next);
        });

        if seen.read().contains("paste:xy") {
            system.exit();
        }

        element!(Text(content: seen.read().clone()))
    }

    #[test]
    fn test_use_input_maps_keys_and_paste() {
        let canvases: Vec<_> = smol::block_on(
            element!(InputProbe)
                .mock_terminal_render_loop(MockTerminalConfig::with_events(stream::iter(vec![
                    TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Char('a'))),
                    TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Left)),
                    TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Enter)),
                    TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::F(2))),
                    TerminalEvent::FullscreenMouse(FullscreenMouseEvent::new(
                        MouseEventKind::ScrollDown,
                        0,
                        0,
                    )),
                    TerminalEvent::Paste("xy".to_string()),
                ])))
                .collect(),
        );
        assert_eq!(
            canvases.last().unwrap().to_string(),
            "a<left><enter><fn><wheel-down>paste:xy\n"
        );
    }

    #[component]
    fn SpecialInputProbe(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let entries = hooks.use_state(Vec::<String>::new);
        let entries_for_handler = entries;
        hooks.use_input(move |input, key| {
            let name = if key.return_key {
                "enter"
            } else if key.tab {
                "tab"
            } else if key.backspace {
                "backspace"
            } else if key.escape {
                "escape"
            } else {
                "other"
            };
            let mut next = entries_for_handler.read().clone();
            next.push(format!(
                "{name}:{input:?}:meta={}:shift={}",
                key.meta, key.shift
            ));
            let mut entries = entries_for_handler;
            entries.set(next);
        });

        if entries.read().len() >= 5 {
            system.exit();
        }

        element!(Text(content: entries.read().join("|")))
    }

    #[test]
    fn test_use_input_matches_cc_special_key_input_and_escape_meta() {
        let canvases: Vec<_> = smol::block_on(
            element!(SpecialInputProbe)
                .mock_terminal_render_loop(MockTerminalConfig::with_events(stream::iter(vec![
                    TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Char('A'))),
                    TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Enter)),
                    TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Tab)),
                    TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Backspace)),
                    TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Esc)),
                ])))
                .collect(),
        );
        assert_eq!(
            canvases.last().unwrap().to_string(),
            "other:\"A\":meta=false:shift=true|enter:\"\":meta=false:shift=false|tab:\"\":meta=false:shift=false|backspace:\"\":meta=false:shift=false|escape:\"\":meta=true:shift=false\n"
        );
    }

    #[component]
    fn CtrlZInputProbe(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut app = hooks.use_app();
        let seen = hooks.use_state(String::new);
        let seen_for_handler = seen;
        hooks.use_input(move |input, key| {
            if key.ctrl && input.eq_ignore_ascii_case("z") {
                let mut seen = seen_for_handler;
                seen.set("ctrl-z".to_string());
            } else if input == "q" {
                app.exit();
            }
        });
        element!(Text(content: format!("seen={}", seen.read().clone())))
    }

    #[test]
    fn test_use_input_receives_ctrl_z_by_default() {
        let canvases: Vec<_> = smol::block_on(
            element!(CtrlZInputProbe)
                .mock_terminal_render_loop(MockTerminalConfig::with_events(stream::iter(vec![
                    TerminalEvent::Key(KeyEvent {
                        code: KeyCode::Char('z'),
                        modifiers: KeyModifiers::CONTROL,
                        kind: KeyEventKind::Press,
                    }),
                    TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Char('q'))),
                ])))
                .collect(),
        );
        assert_eq!(canvases.last().unwrap().to_string(), "seen=ctrl-z\n");
    }

    #[test]
    fn test_use_input_does_not_receive_ctrl_z_when_suspend_is_opted_in() {
        let canvases: Vec<_> = smol::block_on(
            element!(CtrlZInputProbe)
                .mock_terminal_render_loop(
                    MockTerminalConfig::with_events(stream::iter(vec![
                        TerminalEvent::Key(KeyEvent {
                            code: KeyCode::Char('z'),
                            modifiers: KeyModifiers::CONTROL,
                            kind: KeyEventKind::Press,
                        }),
                        TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Char('q'))),
                    ]))
                    .with_suspend_on_ctrl_z(true),
                )
                .collect(),
        );
        assert_eq!(canvases.last().unwrap().to_string(), "seen=\n");
    }

    #[component]
    fn CtrlSpaceProbe(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let seen = hooks.use_state(String::new);
        let seen_for_handler = seen;
        hooks.use_input(move |input, key| {
            if key.ctrl {
                let mut seen = seen_for_handler;
                seen.set(input);
            }
        });
        if *seen.read() == " " {
            system.exit();
        }
        element!(Text(content: format!("input={:?}", seen.read().clone())))
    }

    #[test]
    fn test_use_input_normalizes_ctrl_space_like_cc_ink() {
        let canvases: Vec<_> = smol::block_on(
            element!(CtrlSpaceProbe)
                .mock_terminal_render_loop(MockTerminalConfig::with_events(stream::iter(vec![
                    TerminalEvent::Key(KeyEvent {
                        code: KeyCode::Null,
                        modifiers: KeyModifiers::CONTROL,
                        kind: KeyEventKind::Press,
                    }),
                ])))
                .collect(),
        );
        assert_eq!(canvases.last().unwrap().to_string(), "input=\" \"\n");
    }

    #[component]
    fn CtrlCInputProbe(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut seen = hooks.use_state(|| false);
        hooks.use_input(move |input, key| {
            if key.ctrl && input.eq_ignore_ascii_case("c") {
                seen.set(true);
            }
        });
        if seen.get() {
            hooks.use_context_mut::<SystemContext>().exit();
        }

        element!(Text(content: format!("seen={}", seen.get())))
    }

    #[test]
    fn test_use_input_suppresses_ctrl_c_when_default_exit_is_enabled_like_cc_ink() {
        let canvases: Vec<_> = smol::block_on(
            element!(CtrlCInputProbe)
                .mock_terminal_render_loop(MockTerminalConfig::with_events(stream::iter(vec![
                    TerminalEvent::Key(KeyEvent {
                        code: KeyCode::Char('c'),
                        modifiers: KeyModifiers::CONTROL,
                        kind: KeyEventKind::Press,
                    }),
                ])))
                .collect(),
        );
        assert_eq!(canvases.last().unwrap().to_string(), "seen=false\n");
    }

    #[test]
    fn test_use_input_allows_ctrl_c_when_default_exit_is_disabled_like_cc_ink() {
        let canvases: Vec<_> = smol::block_on(
            element!(CtrlCInputProbe)
                .mock_terminal_render_loop(
                    MockTerminalConfig::with_events(stream::iter(vec![TerminalEvent::Key(
                        KeyEvent {
                            code: KeyCode::Char('c'),
                            modifiers: KeyModifiers::CONTROL,
                            kind: KeyEventKind::Press,
                        },
                    )]))
                    .with_ignore_ctrl_c(true),
                )
                .collect(),
        );
        assert_eq!(canvases.last().unwrap().to_string(), "seen=true\n");
    }

    #[component]
    fn InactiveInputProbe(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let mut tick = hooks.use_state(|| 0u8);
        let mut seen = hooks.use_state(|| false);
        hooks.use_input_with_options(UseInputOptions { active: false }, move |_input, _key| {
            seen.set(true);
        });
        tick += 1;
        if tick.get() >= 2 {
            system.exit();
        }
        element!(Text(content: format!("seen={}", seen.get())))
    }

    #[test]
    fn test_use_input_inactive_ignores_events() {
        let canvases: Vec<_> = smol::block_on(
            element!(InactiveInputProbe)
                .mock_terminal_render_loop(MockTerminalConfig::with_events(stream::iter(vec![
                    TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Char('a'))),
                ])))
                .collect(),
        );
        assert_eq!(canvases.last().unwrap().to_string(), "seen=false\n");
    }

    #[derive(Default, Props)]
    struct StopChildProps {
        handler: HandlerMut<'static, String>,
    }

    #[component]
    fn StopChild(mut hooks: Hooks, props: &mut StopChildProps) -> impl Into<AnyElement<'static>> {
        let mut handler = props.handler.take();
        hooks.use_input_event(move |input, _key, event| {
            if input == "x" {
                handler("child".to_string());
                event.stop_propagation();
            }
        });
        element!(View)
    }

    #[component]
    fn InputPropagationProbe(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let log = hooks.use_state(String::new);
        let log_for_parent = log;
        hooks.use_input_event(move |input, _key, _event| {
            if input == "x" {
                let next = format!("{}parent", &*log_for_parent.read());
                let mut log = log_for_parent;
                log.set(next);
            }
        });

        if log.read().contains("child") {
            system.exit();
        }

        let log_for_child = log;
        let content = log.read().clone();
        element! {
            View(flex_direction: FlexDirection::Column) {
                StopChild(handler: move |message: String| {
                    let next = format!("{}{}", &*log_for_child.read(), message);
                    let mut log = log_for_child;
                    log.set(next);
                })
                Text(content)
            }
        }
    }

    #[test]
    fn test_use_input_event_can_stop_propagation_to_ancestor() {
        let canvases: Vec<_> = smol::block_on(
            element!(InputPropagationProbe)
                .mock_terminal_render_loop(MockTerminalConfig::with_events(stream::iter(vec![
                    TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Char('x'))),
                ])))
                .collect(),
        );
        assert_eq!(canvases.last().unwrap().to_string(), "child\n");
    }
}
