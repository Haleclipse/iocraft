use crate::{
    component,
    components::Text,
    element,
    hooks::{UseState, UseTerminalEvents},
    AnyElement, Color, HandlerMut, Hooks, KeyCode, KeyEvent, KeyEventKind, MouseEventKind, Props,
    TerminalEvent,
};

/// The props which can be passed to the [`Checkbox`] component.
#[non_exhaustive]
#[derive(Default, Props)]
pub struct CheckboxProps {
    /// Whether the checkbox is checked.
    ///
    /// This is a controlled prop: the checkbox renders whatever you pass and reports
    /// toggles via [`on_change`](Self::on_change), it does not keep internal state.
    pub checked: bool,

    /// True if the checkbox has focus and should process keyboard input.
    pub has_focus: bool,

    /// The handler to invoke with the new value when the checkbox is toggled.
    ///
    /// The checkbox can be toggled two ways:
    ///
    /// - By clicking on it with the mouse while in fullscreen mode.
    /// - By pressing the Enter or Space key while [`has_focus`](Self::has_focus) is `true`.
    pub on_change: HandlerMut<'static, bool>,

    /// The color of the checkbox glyphs. Defaults to the terminal's foreground color.
    pub color: Option<Color>,

    /// The glyph to render when checked. Defaults to `"[x]"`.
    pub checked_symbol: Option<String>,

    /// The glyph to render when unchecked. Defaults to `"[ ]"`.
    pub unchecked_symbol: Option<String>,
}

/// `Checkbox` is a togglable component that invokes a handler when clicked or when the
/// Enter or Space key is pressed while it has focus.
///
/// It renders as a textual glyph (`[x]` / `[ ]` by default) and, like
/// [`TextInput`](crate::components::TextInput), shows focus via style inversion.
///
/// # Example
///
/// ```
/// # use iocraft::prelude::*;
/// # #[component]
/// # fn FormField(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
/// let mut agreed = hooks.use_state(|| false);
///
/// element! {
///     View {
///         Checkbox(
///             checked: agreed.get(),
///             has_focus: true,
///             on_change: move |value| agreed.set(value),
///         )
///         Text(content: " I agree to the terms")
///     }
/// }
/// # }
/// ```
#[component]
pub fn Checkbox(mut hooks: Hooks, props: &mut CheckboxProps) -> impl Into<AnyElement<'static>> {
    let has_focus = props.has_focus;
    let checked = props.checked;

    // Track toggle requests from the (Send + 'static) event closure. The closure can't
    // capture `props.checked` across renders, so it flips a counter and we derive the
    // new value from the latest prop here.
    let mut toggle_requests = hooks.use_state(|| 0u64);
    let mut handled_requests = hooks.use_state(|| 0u64);

    hooks.use_local_propagated_terminal_events(move |event| {
        let handled = match event.event() {
            TerminalEvent::FullscreenMouse(event)
                if event.kind == MouseEventKind::Down(crossterm::event::MouseButton::Left) =>
            {
                true
            }
            TerminalEvent::Key(KeyEvent { code, kind, .. })
                if has_focus
                    && *kind == KeyEventKind::Press
                    && (*code == KeyCode::Enter || *code == KeyCode::Char(' ')) =>
            {
                true
            }
            _ => false,
        };
        if handled {
            toggle_requests += 1;
            event.stop_propagation();
        }
    });

    if toggle_requests.get() > handled_requests.get() {
        handled_requests.set(toggle_requests.get());
        let mut on_change = props.on_change.take();
        on_change(!checked);
    }

    let symbol = if checked {
        props
            .checked_symbol
            .clone()
            .unwrap_or_else(|| "[x]".to_string())
    } else {
        props
            .unchecked_symbol
            .clone()
            .unwrap_or_else(|| "[ ]".to_string())
    };

    element! {
        Text(content: symbol, color: props.color, invert: has_focus)
    }
}

#[cfg(test)]
mod tests {
    use crate::prelude::*;
    use futures::stream::StreamExt;
    use macro_rules_attribute::apply;
    use smol_macros::test;

    #[component]
    fn MyComponent(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let mut checked = hooks.use_state(|| false);

        // Exit once the toggle has taken effect, so the final (checked) frame is
        // still rendered before the loop stops.
        if checked.get() {
            system.exit();
        }

        element! {
            View {
                Checkbox(
                    checked: checked.get(),
                    has_focus: true,
                    on_change: move |value| checked.set(value),
                )
                Text(content: if checked.get() { " on" } else { " off" })
            }
        }
    }

    #[apply(test!)]
    async fn test_checkbox_toggles_with_space() {
        let actual = element!(MyComponent)
            .mock_terminal_render_loop(MockTerminalConfig::with_events(futures::stream::iter(
                vec![TerminalEvent::Key(KeyEvent::new(
                    KeyEventKind::Press,
                    KeyCode::Char(' '),
                ))],
            )))
            .map(|c| c.to_string())
            .collect::<Vec<_>>()
            .await;
        // Initial frame: unchecked. The Space press toggles, and the re-render shows
        // the checked state. (The intermediate frame where on_change fires renders
        // identically to the first and is deduplicated by the render loop.)
        assert_eq!(actual, vec!["[ ] off\n", "[x] on\n"]);
    }

    #[component]
    fn UnfocusedComponent(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let mut checked = hooks.use_state(|| false);
        let mut key_presses = hooks.use_state(|| 0);

        hooks.use_terminal_events(move |event| {
            if let TerminalEvent::Key(KeyEvent {
                kind: KeyEventKind::Press,
                ..
            }) = event
            {
                key_presses += 1;
            }
        });

        if key_presses.get() >= 1 {
            system.exit();
        }

        element! {
            Checkbox(
                checked: checked.get(),
                has_focus: false,
                on_change: move |value| checked.set(value),
            )
        }
    }

    #[apply(test!)]
    async fn test_checkbox_ignores_keys_without_focus() {
        let actual = element!(UnfocusedComponent)
            .mock_terminal_render_loop(MockTerminalConfig::with_events(futures::stream::iter(
                vec![TerminalEvent::Key(KeyEvent::new(
                    KeyEventKind::Press,
                    KeyCode::Char(' '),
                ))],
            )))
            .map(|c| c.to_string())
            .collect::<Vec<_>>()
            .await;
        // The checkbox must not toggle: still unchecked in every frame.
        assert_eq!(actual, vec!["[ ]\n"]);
    }

    #[component]
    fn RepeatComponent(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let mut checked = hooks.use_state(|| false);
        let mut repeats = hooks.use_state(|| 0);

        hooks.use_terminal_events(move |event| {
            if let TerminalEvent::Key(KeyEvent {
                kind: KeyEventKind::Repeat,
                ..
            }) = event
            {
                repeats += 1;
            }
        });

        if repeats.get() >= 1 {
            system.exit();
        }

        element! {
            Checkbox(
                checked: checked.get(),
                has_focus: true,
                on_change: move |value| checked.set(value),
            )
        }
    }

    #[apply(test!)]
    async fn test_checkbox_ignores_repeat_events() {
        let actual = element!(RepeatComponent)
            .mock_terminal_render_loop(MockTerminalConfig::with_events(futures::stream::iter(
                vec![TerminalEvent::Key(KeyEvent::new(
                    KeyEventKind::Repeat,
                    KeyCode::Char(' '),
                ))],
            )))
            .map(|c| c.to_string())
            .collect::<Vec<_>>()
            .await;
        assert_eq!(actual, vec!["[ ]\n"]);
    }
}
