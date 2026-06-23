use super::view::ViewFocusParentContext;
use crate::{AnyElement, Component, ComponentUpdater, Context, Hooks, Props};

/// The props which can be passed to the [`Fragment`] component.
#[non_exhaustive]
#[derive(Default, Props)]
pub struct FragmentProps<'a> {
    /// The children of the component.
    pub children: Vec<AnyElement<'a>>,
}

/// `Fragment` is a component which allows you to group elements without impacting the resulting
/// layout.
///
/// This is typically used when you want to create a component that returns multiple elements.
///
/// # Example
///
/// ```
/// # use iocraft::prelude::*;
/// #[component]
/// fn TextLines() -> impl Into<AnyElement<'static>> {
///     element! {
///         Fragment {
///             Text(content: "Line 1")
///             Text(content: "Line 2")
///         }
///     }
/// }
///
/// fn MyComponent() -> impl Into<AnyElement<'static>> {
///     element! {
///         View(flex_direction: FlexDirection::Column) {
///             TextLines
///         }
///     }
/// }
/// ```
#[derive(Default)]
pub struct Fragment;

impl Component for Fragment {
    type Props<'a> = FragmentProps<'a>;

    fn new(_props: &Self::Props<'_>) -> Self {
        Self
    }

    fn update(
        &mut self,
        props: &mut Self::Props<'_>,
        _hooks: Hooks,
        updater: &mut ComponentUpdater,
    ) {
        updater.set_transparent_layout(true);
        let context = updater
            .get_context::<ViewFocusParentContext>()
            .is_none()
            .then(|| Context::owned(ViewFocusParentContext::shared_root()));
        updater.update_children(props.children.iter_mut(), context);
    }
}

#[cfg(test)]
mod tests {
    use crate::prelude::*;
    use futures::StreamExt;

    #[component]
    fn TextLines() -> impl Into<AnyElement<'static>> {
        element! {
            Fragment {
                Text(content: "Line 1")
                Text(content: "Line 2")
            }
        }
    }

    #[component]
    fn MyComponent() -> impl Into<AnyElement<'static>> {
        element! {
            View(flex_direction: FlexDirection::Column) {
                TextLines
            }
        }
    }

    #[test]
    fn test_fragment() {
        assert_eq!(element!(MyComponent).to_string(), "Line 1\nLine 2\n");
    }

    #[component]
    fn FragmentRootHitTestApp(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let mut first_clicks = hooks.use_state(|| 0usize);
        let mut second_clicks = hooks.use_state(|| 0usize);
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

        let first = first_clicks.get();
        let second = second_clicks.get();
        element! {
            Fragment {
                View(
                    width: 8,
                    height: 1,
                    on_click: move |_| first_clicks += 1,
                ) {
                    Text(content: "first")
                }
                View(
                    width: 8,
                    height: 1,
                    position: Position::Absolute,
                    top: 0,
                    left: 0,
                    on_click: move |_| second_clicks += 1,
                ) {
                    Text(content: format!("{first}/{second} top"))
                }
            }
        }
    }

    #[test]
    fn test_fragment_shares_root_event_context_for_topmost_hit_test() {
        let canvases: Vec<_> = smol::block_on(
            element!(FragmentRootHitTestApp)
                .mock_terminal_render_loop(MockTerminalConfig::with_events(futures::stream::iter(
                    vec![
                        TerminalEvent::FullscreenMouse(FullscreenMouseEvent::new(
                            MouseEventKind::Down(MouseButton::Left),
                            1,
                            0,
                        )),
                        TerminalEvent::FullscreenMouse(FullscreenMouseEvent::new(
                            MouseEventKind::Up(MouseButton::Left),
                            1,
                            0,
                        )),
                    ],
                )))
                .collect(),
        );
        assert_eq!(canvases.last().unwrap().to_string(), "0/1 top\n");
    }

    #[component]
    fn FragmentRootHoverApp(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let mut first_enters = hooks.use_state(|| 0usize);
        let mut second_enters = hooks.use_state(|| 0usize);
        let mut moves = hooks.use_state(|| 0usize);

        hooks.use_terminal_events(move |event| {
            if matches!(
                event,
                TerminalEvent::FullscreenMouse(FullscreenMouseEvent {
                    kind: MouseEventKind::Moved,
                    ..
                })
            ) {
                moves += 1;
            }
        });
        if moves.get() > 0 {
            system.exit();
        }

        let first = first_enters.get();
        let second = second_enters.get();
        element! {
            Fragment {
                View(
                    width: 8,
                    height: 1,
                    on_mouse_enter: move |_| first_enters += 1,
                ) {
                    Text(content: "first")
                }
                View(
                    width: 8,
                    height: 1,
                    position: Position::Absolute,
                    top: 0,
                    left: 0,
                    on_mouse_enter: move |_| second_enters += 1,
                ) {
                    Text(content: format!("{first}/{second} top"))
                }
            }
        }
    }

    #[test]
    fn test_fragment_shares_root_hover_state_for_topmost_hit_test() {
        let canvases: Vec<_> =
            smol::block_on(
                element!(FragmentRootHoverApp)
                    .mock_terminal_render_loop(MockTerminalConfig::with_events(
                        futures::stream::iter(vec![TerminalEvent::FullscreenMouse(
                            FullscreenMouseEvent::new(MouseEventKind::Moved, 1, 0),
                        )]),
                    ))
                    .collect(),
            );
        assert_eq!(canvases.last().unwrap().to_string(), "0/1 top\n");
    }
}
