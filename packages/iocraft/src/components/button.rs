use crate::{
    component,
    components::{View, ViewClickEvent, ViewKeyboardEvent},
    element,
    hooks::{Ref, UseInterval, UseState, UseTerminalEvents},
    AnyElement, HandlerMut, Hooks, KeyCode, KeyEvent, KeyEventKind, Props, TerminalEvent,
};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

/// Interactive state exposed by [`Button`].
///
/// This mirrors CC Ink's `ButtonState` render-prop shape. Rust callers can pass
/// a [`Ref<ButtonState>`](crate::hooks::Ref) via [`ButtonProps::state`] and use
/// the value on subsequent renders to style their children.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ButtonState {
    /// Whether this button currently owns keyboard focus.
    pub focused: bool,
    /// Whether the mouse is currently inside the button's rendered rect.
    pub hovered: bool,
    /// Whether the button is in CC Ink's transient keyboard-active state.
    pub active: bool,
}

/// The props which can be passed to the [`Button`] component.
#[non_exhaustive]
#[derive(Default, Props)]
pub struct ButtonProps<'a> {
    /// The children of the component. Exactly one child is expected.
    pub children: Vec<AnyElement<'a>>,

    /// The handler to invoke when the button is triggered.
    ///
    /// The button can be triggered two ways:
    ///
    /// - By clicking on it with the mouse while in fullscreen mode.
    /// - By pressing the Enter or Space key while [`has_focus`](Self::has_focus) is `true`,
    ///   or while the button is registered with a [`FocusScope`](super::FocusScope)
    ///   via [`focusable`](Self::focusable).
    pub handler: HandlerMut<'static, ()>,

    /// CC Ink-style alias for [`Self::handler`]. If both handlers are set, both
    /// are invoked with `handler` first.
    pub on_action: HandlerMut<'static, ()>,

    /// True if the button has focus and should process keyboard input.
    pub has_focus: bool,

    /// If true and the button is inside a [`FocusScope`](super::FocusScope), it
    /// registers itself for Tab traversal and keyboard activation.
    ///
    /// This legacy iocraft flag is kept for compatibility. New CC Ink-style
    /// code can use [`tab_index`](Self::tab_index); when neither is provided,
    /// `Button` defaults to `tabIndex=0` like CC Ink.
    pub focusable: bool,

    /// CC Ink-style tab order index. Defaults to `Some(0)`, so buttons inside
    /// a [`FocusScope`](super::FocusScope) participate in Tab traversal unless
    /// set to `Some(-1)`.
    pub tab_index: Option<i32>,

    /// Requests focus on first mount when the button is focusable/tabbable.
    pub auto_focus: bool,

    /// Whether the button should react to keyboard/mouse activation. Defaults
    /// to `true`.
    pub is_active: Option<bool>,

    /// Optional external state ref for styling children based on focused,
    /// hovered, and active state.
    pub state: Option<Ref<ButtonState>>,
}

/// `Button` is a component that invokes a handler when clicked or when the Enter or Space key is pressed while it has focus.
///
/// # Example
///
/// ```
/// # use iocraft::prelude::*;
/// # fn foo() -> impl Into<AnyElement<'static>> {
/// element! {
///     Button(handler: |_| { /* do something */ }, has_focus: true) {
///         View(border_style: BorderStyle::Round, border_color: Color::Blue) {
///             Text(content: "Click me!")
///         }
///     }
/// }
/// # }
/// ```
#[component]
pub fn Button<'a>(mut hooks: Hooks, props: &mut ButtonProps<'a>) -> impl Into<AnyElement<'a>> {
    let enabled = props.is_active.unwrap_or(true);
    let hovered = hooks.use_state(|| false);
    let focused_state = hooks.use_state(|| props.has_focus);
    let active = hooks.use_state(|| false);
    let focused = props.has_focus || focused_state.get();

    hooks.use_interval(
        {
            let mut active = active;
            move || active.set(false)
        },
        active.get().then_some(Duration::from_millis(100)),
    );

    let handler = Arc::new(Mutex::new(props.handler.take()));
    let on_action = Arc::new(Mutex::new(props.on_action.take()));

    let handle_view_keys = !props.has_focus;
    hooks.use_propagated_terminal_events({
        let handler = handler.clone();
        let on_action = on_action.clone();
        let mut active = active;
        let legacy_has_focus = props.has_focus;
        move |event| {
            if !enabled || !legacy_has_focus {
                return;
            }
            let TerminalEvent::Key(KeyEvent { code, kind, .. }) = event.event() else {
                return;
            };
            if *kind == KeyEventKind::Press
                && (*code == KeyCode::Enter || *code == KeyCode::Char(' '))
            {
                active.set(true);
                handler
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())(());
                on_action
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())(());
                event.prevent_default();
                event.stop_propagation();
            }
        }
    });

    let button_state = ButtonState {
        focused,
        hovered: hovered.get(),
        active: active.get(),
    };
    if let Some(mut state) = props.state {
        state.set(button_state);
    }

    let mut hover_enter = hovered;
    let mut hover_leave = hovered;
    let mut focus_enter = focused_state;
    let mut focus_leave = focused_state;
    let click_handler = handler.clone();
    let click_on_action = on_action.clone();
    let mut click_focus = focused_state;
    let key_handler = handler.clone();
    let key_on_action = on_action.clone();
    let mut key_active = active;
    let tab_index = enabled.then_some(props.tab_index.unwrap_or(0));
    let focusable = props.focusable && enabled;
    let focus_target = focusable || tab_index.is_some() || (props.auto_focus && enabled);
    let auto_focus = props.auto_focus && enabled;

    element! {
        View(
            focusable: focusable,
            tab_index: tab_index,
            auto_focus: auto_focus,
            on_focus: move |_| focus_enter.set(true),
            on_blur: move |_| focus_leave.set(false),
            on_mouse_enter: move |_| hover_enter.set(true),
            on_mouse_leave: move |_| hover_leave.set(false),
            on_key_down: move |event: ViewKeyboardEvent| {
                if enabled
                    && handle_view_keys
                    && (event.code == KeyCode::Enter || event.code == KeyCode::Char(' '))
                {
                    event.prevent_default();
                    key_active.set(true);
                    key_handler
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())(());
                    key_on_action
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())(());
                }
            },
            on_click: move |_event: ViewClickEvent| {
                if enabled {
                    if focus_target {
                        click_focus.set(true);
                    }
                    click_handler
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())(());
                    click_on_action
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())(());
                }
            },
        ) {
            #(props.children.iter_mut())
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::prelude::*;
    use crossterm::event::MouseButton;
    use futures::stream::StreamExt;
    use macro_rules_attribute::apply;
    use smol_macros::test;

    #[component]
    fn MyComponent(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let mut should_exit = hooks.use_state(|| false);

        if should_exit.get() {
            system.exit();
        }

        element! {
            Button(handler: move |_| should_exit.set(true), has_focus: true) {
                Text(content: "Exit")
            }
        }
    }

    #[apply(test!)]
    async fn test_button_click() {
        let actual = element!(MyComponent)
            .mock_terminal_render_loop(MockTerminalConfig::with_events(futures::stream::iter(
                vec![
                    TerminalEvent::FullscreenMouse(FullscreenMouseEvent::new(
                        MouseEventKind::Down(MouseButton::Left),
                        2,
                        0,
                    )),
                    TerminalEvent::FullscreenMouse(FullscreenMouseEvent::new(
                        MouseEventKind::Up(MouseButton::Left),
                        2,
                        0,
                    )),
                ],
            )))
            .map(|c| c.to_string())
            .collect::<Vec<_>>()
            .await;
        let expected = vec!["Exit\n"];
        assert_eq!(actual, expected);
    }

    #[component]
    fn ButtonClickCountComponent(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let mut clicks = hooks.use_state(|| 0usize);
        let mut releases = hooks.use_state(|| 0usize);
        hooks.use_terminal_events(move |event| {
            if matches!(
                event,
                TerminalEvent::FullscreenMouse(FullscreenMouseEvent {
                    kind: MouseEventKind::Up(MouseButton::Left),
                    ..
                })
            ) {
                releases += 1;
            }
        });
        if releases.get() > 0 {
            system.exit();
        }
        element! {
            Button(handler: move |_| clicks += 1) {
                Text(content: format!("clicks={}", clicks.get()))
            }
        }
    }

    #[apply(test!)]
    async fn test_button_click_fires_on_release_like_ink_click() {
        let actual = element!(ButtonClickCountComponent)
            .mock_terminal_render_loop(MockTerminalConfig::with_events(futures::stream::iter(
                vec![
                    TerminalEvent::FullscreenMouse(FullscreenMouseEvent::new(
                        MouseEventKind::Down(MouseButton::Left),
                        2,
                        0,
                    )),
                    TerminalEvent::FullscreenMouse(FullscreenMouseEvent::new(
                        MouseEventKind::Up(MouseButton::Left),
                        2,
                        0,
                    )),
                ],
            )))
            .map(|c| c.to_string())
            .collect::<Vec<_>>()
            .await;
        assert_eq!(actual.last().map(String::as_str), Some("clicks=1\n"));
    }

    #[apply(test!)]
    async fn test_button_click_ignores_drag_like_ink_click() {
        let actual = element!(ButtonClickCountComponent)
            .mock_terminal_render_loop(MockTerminalConfig::with_events(futures::stream::iter(
                vec![
                    TerminalEvent::FullscreenMouse(FullscreenMouseEvent::new(
                        MouseEventKind::Down(MouseButton::Left),
                        2,
                        0,
                    )),
                    TerminalEvent::FullscreenMouse(FullscreenMouseEvent::new(
                        MouseEventKind::Drag(MouseButton::Left),
                        3,
                        0,
                    )),
                    TerminalEvent::FullscreenMouse(FullscreenMouseEvent::new(
                        MouseEventKind::Up(MouseButton::Left),
                        3,
                        0,
                    )),
                ],
            )))
            .map(|c| c.to_string())
            .collect::<Vec<_>>()
            .await;
        assert_eq!(actual, vec!["clicks=0\n"]);
    }

    #[apply(test!)]
    async fn test_button_key_input() {
        let actual = element!(MyComponent)
            .mock_terminal_render_loop(MockTerminalConfig::with_events(futures::stream::once(
                async { TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Enter)) },
            )))
            .map(|c| c.to_string())
            .collect::<Vec<_>>()
            .await;
        let expected = vec!["Exit\n"];
        assert_eq!(actual, expected);
    }

    #[component]
    fn RepeatComponent(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let mut triggered = hooks.use_state(|| false);
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
            Button(handler: move |_| triggered.set(true), has_focus: true) {
                Text(content: if triggered.get() { "triggered" } else { "idle" })
            }
        }
    }

    #[apply(test!)]
    async fn test_button_ignores_repeat_events() {
        let actual = element!(RepeatComponent)
            .mock_terminal_render_loop(MockTerminalConfig::with_events(futures::stream::iter(
                vec![TerminalEvent::Key(KeyEvent::new(
                    KeyEventKind::Repeat,
                    KeyCode::Enter,
                ))],
            )))
            .map(|c| c.to_string())
            .collect::<Vec<_>>()
            .await;
        assert_eq!(actual, vec!["idle\n"]);
    }

    #[component]
    fn AutoFocusButtonComponent(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let mut triggered = hooks.use_state(|| false);
        if triggered.get() {
            system.exit();
        }

        element! {
            FocusScope {
                Button(
                    auto_focus: true,
                    on_action: move |_| triggered.set(true),
                ) {
                    Text(content: if triggered.get() { "triggered" } else { "idle" })
                }
            }
        }
    }

    #[apply(test!)]
    async fn test_button_defaults_to_tab_index_zero_like_ink() {
        let actual = element!(AutoFocusButtonComponent)
            .mock_terminal_render_loop(MockTerminalConfig::with_events(futures::stream::iter(
                vec![TerminalEvent::Key(KeyEvent::new(
                    KeyEventKind::Press,
                    KeyCode::Enter,
                ))],
            )))
            .map(|c| c.to_string())
            .collect::<Vec<_>>()
            .await;
        assert_eq!(actual.last().map(String::as_str), Some("triggered\n"));
    }

    #[component]
    fn FocusableButtonComponent(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let mut triggered = hooks.use_state(|| false);
        if triggered.get() {
            system.exit();
        }

        element! {
            FocusScope {
                Button(
                    focusable: true,
                    auto_focus: true,
                    on_action: move |_| triggered.set(true),
                ) {
                    Text(content: if triggered.get() { "triggered" } else { "idle" })
                }
            }
        }
    }

    #[apply(test!)]
    async fn test_button_can_register_with_focus_scope_and_on_action_alias() {
        let actual = element!(FocusableButtonComponent)
            .mock_terminal_render_loop(MockTerminalConfig::with_events(futures::stream::iter(
                vec![TerminalEvent::Key(KeyEvent::new(
                    KeyEventKind::Press,
                    KeyCode::Enter,
                ))],
            )))
            .map(|c| c.to_string())
            .collect::<Vec<_>>()
            .await;
        assert_eq!(actual.last().unwrap(), "triggered\n");
    }
}
