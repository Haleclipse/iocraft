use crate::{
    ComponentUpdater, FullscreenMouseEvent, Hook, Hooks, KeyCode, KeyEventKind, KeyModifiers,
    PropagatedTerminalEvent, TerminalEvent, TerminalEvents,
};
use core::{
    pin::Pin,
    task::{Context, Poll},
};
use taffy::{Point, Size};

mod private {
    pub trait Sealed {}
    impl Sealed for crate::Hooks<'_, '_> {}
}

/// `UseTerminalEvents` is a hook that allows you to listen for user input such as key strokes.
///
/// # Example
///
/// ```
/// # use iocraft::prelude::*;
/// # use unicode_width::UnicodeWidthStr;
/// const AREA_WIDTH: u32 = 80;
/// const AREA_HEIGHT: u32 = 11;
/// const FACE: &str = "👾";
///
/// #[component]
/// fn Example(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
///     let mut system = hooks.use_context_mut::<SystemContext>();
///     let mut x = hooks.use_state(|| 0);
///     let mut y = hooks.use_state(|| 0);
///     let mut should_exit = hooks.use_state(|| false);
///
///     hooks.use_terminal_events({
///         move |event| match event {
///             TerminalEvent::Key(KeyEvent { code, kind, .. }) if kind != KeyEventKind::Release => {
///                 match code {
///                     KeyCode::Char('q') => should_exit.set(true),
///                     KeyCode::Up => y.set((y.get() as i32 - 1).max(0) as _),
///                     KeyCode::Down => y.set((y.get() + 1).min(AREA_HEIGHT - 1)),
///                     KeyCode::Left => x.set((x.get() as i32 - 1).max(0) as _),
///                     KeyCode::Right => x.set((x.get() + 1).min(AREA_WIDTH - FACE.width() as u32)),
///                     _ => {}
///                 }
///             }
///             _ => {}
///         }
///     });
///
///     if should_exit.get() {
///         system.exit();
///     }
///
///     element! {
///         View(
///             flex_direction: FlexDirection::Column,
///             padding: 2,
///             align_items: AlignItems::CENTER
///         ) {
///             Text(content: "Use arrow keys to move. Press \"q\" to exit.")
///             View(
///                 border_style: BorderStyle::Round,
///                 border_color: Color::Green,
///                 height: AREA_HEIGHT + 2,
///                 width: AREA_WIDTH + 2,
///             ) {
///                 #(if should_exit.get() {
///                     element! {
///                         View(
///                             width: 100pct,
///                             height: 100pct,
///                             justify_content: JustifyContent::CENTER,
///                             align_items: AlignItems::CENTER,
///                         ) {
///                             Text(content: format!("Goodbye! {}", FACE))
///                         }
///                     }
///                 } else {
///                     element! {
///                         View(
///                             padding_left: x.get(),
///                             padding_top: y.get(),
///                         ) {
///                             Text(content: FACE)
///                         }
///                     }
///                 })
///             }
///         }
///     }
/// }
/// ```
pub trait UseTerminalEvents: private::Sealed {
    /// Defines a callback to be invoked whenever a terminal event occurs.
    ///
    /// This hook will be called for all terminal events, including those that occur outside of the
    /// component. If you only want to listen for events within the component, use
    /// [`Self::use_local_terminal_events`] instead.
    ///
    /// Callbacks registered this way observe **every** event, even ones consumed via
    /// [`PropagatedTerminalEvent::stop_propagation`] — they are bypass listeners,
    /// suitable for global concerns such as "press q to quit".
    fn use_terminal_events<F>(&mut self, f: F)
    where
        F: FnMut(TerminalEvent) + Send + 'static;

    /// Defines a callback to be invoked whenever a terminal event occurs within a component.
    ///
    /// Unlike [`Self::use_terminal_events`], this hook will not be called for events such as mouse
    /// events that occur outside of the component. Furthermore, coordinates will be translated to
    /// component-local coordinates.
    fn use_local_terminal_events<F>(&mut self, f: F)
    where
        F: FnMut(TerminalEvent) + Send + 'static;

    /// Defines a propagation-aware callback for terminal events within a component.
    ///
    /// Mouse coordinates are translated to component-local coordinates as with
    /// [`Self::use_local_terminal_events`]. Keyboard, resize, and paste events are
    /// delivered unchanged. The callback may call
    /// [`PropagatedTerminalEvent::stop_propagation`] to hide handled events from
    /// ancestor propagation-aware subscribers.
    fn use_local_propagated_terminal_events<F>(&mut self, f: F)
    where
        F: FnMut(&PropagatedTerminalEvent) + Send + 'static;

    /// Defines a propagation-aware callback for terminal events.
    ///
    /// Unlike [`Self::use_terminal_events`], events that were consumed via
    /// [`PropagatedTerminalEvent::stop_propagation`] by an earlier subscriber are
    /// skipped, and the callback itself may consume events to hide them from
    /// later propagation-aware subscribers.
    ///
    /// Hooks are polled depth-first — a component's children are polled before its own
    /// hooks — so the deepest interested component sees each event first and ancestors
    /// only see it if no descendant consumed it. This mirrors DOM-style event bubbling:
    /// a nested component (e.g. a modal's [`FocusScope`](crate::components::FocusScope))
    /// can capture Tab without its ancestors also acting on it.
    fn use_propagated_terminal_events<F>(&mut self, f: F)
    where
        F: FnMut(&PropagatedTerminalEvent) + Send + 'static;
}

pub(crate) trait UseTerminalDefaultEvents: private::Sealed {
    /// Defines a framework default-action callback.
    ///
    /// Unlike propagation-aware component listeners, default-action callbacks
    /// still observe events after `stop_propagation()` so they can mirror DOM
    /// behavior: propagation and default prevention are independent, and only
    /// `prevent_default()` should block built-in defaults such as FocusScope Tab
    /// traversal.
    fn use_terminal_default_events<F>(&mut self, f: F)
    where
        F: FnMut(&PropagatedTerminalEvent) + Send + 'static;
}

impl UseTerminalEvents for Hooks<'_, '_> {
    fn use_terminal_events<F>(&mut self, mut f: F)
    where
        F: FnMut(TerminalEvent) + Send + 'static,
    {
        let h = self.use_hook(move || UseTerminalEventsImpl {
            events: None,
            component_location: Default::default(),
            in_component: false,
            propagation_aware: false,
            observe_stopped: false,
            f: None,
        });
        h.f = Some(Box::new(move |event: &PropagatedTerminalEvent| {
            f(event.event().clone())
        }));
    }

    fn use_local_terminal_events<F>(&mut self, mut f: F)
    where
        F: FnMut(TerminalEvent) + Send + 'static,
    {
        let h = self.use_hook(move || UseTerminalEventsImpl {
            events: None,
            component_location: Default::default(),
            in_component: true,
            propagation_aware: false,
            observe_stopped: false,
            f: None,
        });
        h.f = Some(Box::new(move |event: &PropagatedTerminalEvent| {
            f(event.event().clone())
        }));
    }

    fn use_local_propagated_terminal_events<F>(&mut self, f: F)
    where
        F: FnMut(&PropagatedTerminalEvent) + Send + 'static,
    {
        let h = self.use_hook(move || UseTerminalEventsImpl {
            events: None,
            component_location: Default::default(),
            in_component: true,
            propagation_aware: true,
            observe_stopped: false,
            f: None,
        });
        h.f = Some(Box::new(f));
    }

    fn use_propagated_terminal_events<F>(&mut self, f: F)
    where
        F: FnMut(&PropagatedTerminalEvent) + Send + 'static,
    {
        let h = self.use_hook(move || UseTerminalEventsImpl {
            events: None,
            component_location: Default::default(),
            in_component: false,
            propagation_aware: true,
            observe_stopped: false,
            f: None,
        });
        h.f = Some(Box::new(f));
    }
}

impl UseTerminalDefaultEvents for Hooks<'_, '_> {
    fn use_terminal_default_events<F>(&mut self, f: F)
    where
        F: FnMut(&PropagatedTerminalEvent) + Send + 'static,
    {
        let h = self.use_hook(move || UseTerminalEventsImpl {
            events: None,
            component_location: Default::default(),
            in_component: false,
            propagation_aware: true,
            observe_stopped: true,
            f: None,
        });
        h.f = Some(Box::new(f));
    }
}

type EventCallback = Box<dyn FnMut(&PropagatedTerminalEvent) + Send + 'static>;

struct UseTerminalEventsImpl {
    events: Option<TerminalEvents>,
    component_location: (Point<i16>, Size<u16>),
    in_component: bool,
    propagation_aware: bool,
    observe_stopped: bool,
    f: Option<EventCallback>,
}

impl Hook for UseTerminalEventsImpl {
    fn poll_change(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let mut processed_ctrl_c = false;
        while let Some(Poll::Ready(Some((event, state)))) = self
            .events
            .as_mut()
            .map(|events| events.poll_next_shared(cx))
        {
            processed_ctrl_c |= matches!(
                &event,
                TerminalEvent::Key(key)
                    if key.code == KeyCode::Char('c')
                        && key.kind == KeyEventKind::Press
                        && key.modifiers.contains(KeyModifiers::CONTROL)
                        && !key.modifiers.intersects(KeyModifiers::ALT | KeyModifiers::SUPER)
            );
            // Propagation-aware subscribers skip events already consumed by an
            // earlier (deeper) subscriber. Plain subscribers observe everything.
            if self.propagation_aware
                && ((!self.observe_stopped && state.is_propagation_stopped())
                    || (self.observe_stopped && state.is_default_propagation_stopped()))
            {
                continue;
            }
            if self.in_component {
                let (location, size) = self.component_location;
                match event {
                    TerminalEvent::FullscreenMouse(event) => {
                        if event.row as i16 >= location.y && event.column as i16 >= location.x {
                            let row = (event.row as i16 - location.y) as u16;
                            let column = (event.column as i16 - location.x) as u16;
                            if row < size.height && column < size.width {
                                if let Some(f) = &mut self.f {
                                    f(&PropagatedTerminalEvent::new(
                                        TerminalEvent::FullscreenMouse(FullscreenMouseEvent {
                                            row,
                                            column,
                                            ..event
                                        }),
                                        state,
                                    ));
                                }
                            }
                        }
                    }
                    TerminalEvent::Key(_)
                    | TerminalEvent::Resize(..)
                    | TerminalEvent::FocusGained
                    | TerminalEvent::FocusLost
                    | TerminalEvent::Paste(_)
                    | TerminalEvent::Response(_) => {
                        if let Some(f) = &mut self.f {
                            f(&PropagatedTerminalEvent::new(event, state));
                        }
                    }
                }
            } else if let Some(f) = &mut self.f {
                f(&PropagatedTerminalEvent::new(event, state));
            }
        }
        if processed_ctrl_c {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }

    fn post_component_update(&mut self, updater: &mut ComponentUpdater) {
        if self.events.is_none() {
            self.events = updater.terminal_events();
        }
    }

    fn post_component_draw(&mut self, drawer: &mut crate::ComponentDrawer) {
        self.component_location = (drawer.canvas_position(), drawer.size());
    }
}

#[cfg(test)]
mod tests {
    use crate::prelude::*;
    use crossterm::event::MouseButton;
    use futures::stream::{self, StreamExt};
    use macro_rules_attribute::apply;
    use smol_macros::test;

    #[component]
    fn MyComponent(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let mut should_exit = hooks.use_state(|| false);
        hooks.use_terminal_events(move |_event| {
            should_exit.set(true);
        });

        if should_exit.get() {
            system.exit();
            element!(Text(content:"received event")).into_any()
        } else {
            element!(View).into_any()
        }
    }

    #[apply(test!)]
    async fn test_use_terminal_events() {
        let canvases: Vec<_> = element!(MyComponent)
            .mock_terminal_render_loop(MockTerminalConfig::with_events(stream::iter(vec![
                TerminalEvent::Key(KeyEvent {
                    code: KeyCode::Char('f'),
                    modifiers: KeyModifiers::empty(),
                    kind: KeyEventKind::Press,
                }),
            ])))
            .collect()
            .await;
        let actual = canvases.iter().map(|c| c.to_string()).collect::<Vec<_>>();
        let expected = vec!["", "received event\n"];
        assert_eq!(actual, expected);
    }

    #[component]
    fn MyClickableComponent(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let mut should_exit = hooks.use_state(|| false);
        hooks.use_local_terminal_events(move |event| {
            if let TerminalEvent::FullscreenMouse(FullscreenMouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                row,
                column,
                ..
            }) = event
            {
                assert_eq!(row, 8);
                assert_eq!(column, 8);
                should_exit.set(true);
            }
        });

        if should_exit.get() {
            system.exit();
            element!(Text(content:"received click")).into_any()
        } else {
            element!(View(width: 10, height: 10)).into_any()
        }
    }

    #[apply(test!)]
    async fn test_use_local_terminal_events() {
        let canvases: Vec<_> = element! {
            View(padding: 2) {
                MyClickableComponent
            }
        }
        .mock_terminal_render_loop(MockTerminalConfig::with_events(stream::iter(vec![
            TerminalEvent::FullscreenMouse(FullscreenMouseEvent::new(
                MouseEventKind::Down(MouseButton::Left),
                10,
                10,
            )),
        ])))
        .collect()
        .await;
        let actual = canvases
            .iter()
            .map(|c| c.to_string().trim().to_string())
            .collect::<Vec<_>>();
        assert_eq!(actual, vec!["", "received click"]);
    }

    #[component]
    fn CtrlCObserverComponent(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        hooks.use_terminal_events(|_event| {});
        element!(View)
    }

    #[component]
    fn CtrlCInterceptComponent(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let mut caught = hooks.use_state(|| false);
        hooks.use_propagated_terminal_events(move |event| {
            if matches!(
                event.event(),
                TerminalEvent::Key(KeyEvent {
                    code: KeyCode::Char('c'),
                    modifiers: KeyModifiers::CONTROL,
                    kind: KeyEventKind::Press,
                    ..
                })
            ) {
                caught.set(true);
                event.stop_propagation();
            }
        });

        if caught.get() {
            system.exit();
            element!(Text(content:"caught ctrl-c")).into_any()
        } else {
            element!(View).into_any()
        }
    }

    #[apply(test!)]
    async fn test_ctrl_shift_c_wakes_for_default_exit_with_subscriber() {
        let events = stream::once(async {
            TerminalEvent::Key(KeyEvent {
                code: KeyCode::Char('c'),
                modifiers: KeyModifiers::CONTROL | KeyModifiers::SHIFT,
                kind: KeyEventKind::Press,
            })
        })
        .chain(stream::pending());

        let mut root = element!(CtrlCObserverComponent);
        let render_loop = root
            .mock_terminal_render_loop(MockTerminalConfig::with_events(events))
            .collect::<Vec<_>>();
        let timeout = futures_timer::Delay::new(std::time::Duration::from_secs(1));
        let result = futures::future::select(Box::pin(render_loop), Box::pin(timeout)).await;
        let futures::future::Either::Left((canvases, _)) = result else {
            panic!("render loop did not wake to resolve pending Ctrl+C");
        };

        let actual = canvases.iter().map(|c| c.to_string()).collect::<Vec<_>>();
        assert_eq!(actual, vec![""]);
    }

    #[apply(test!)]
    async fn test_ctrl_c_can_be_consumed_before_default_exit() {
        let canvases: Vec<_> = element!(CtrlCInterceptComponent)
            .mock_terminal_render_loop(MockTerminalConfig::with_events(stream::iter(vec![
                TerminalEvent::Key(KeyEvent {
                    code: KeyCode::Char('c'),
                    modifiers: KeyModifiers::CONTROL,
                    kind: KeyEventKind::Press,
                }),
            ])))
            .collect()
            .await;
        let actual = canvases.iter().map(|c| c.to_string()).collect::<Vec<_>>();
        assert_eq!(actual, vec!["", "caught ctrl-c\n"]);
    }
}
