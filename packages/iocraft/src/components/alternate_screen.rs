use crate::{
    component, components::View, element, hooks::UseContext, AnyElement, Hooks, Props,
    SystemContext,
};

/// Props for [`AlternateScreen`].
#[non_exhaustive]
#[derive(Default, Props)]
pub struct AlternateScreenProps<'a> {
    /// The fullscreen children to render while alternate-screen mode is active.
    pub children: Vec<AnyElement<'a>>,

    /// Enable SGR mouse tracking while mounted. Defaults to `true`.
    pub mouse_tracking: Option<bool>,
}

/// Runs children in the terminal alternate screen while mounted.
///
/// This is the iocraft counterpart to CC Ink's `<AlternateScreen>` component.
/// It requests DEC 1049 alternate-screen mode during the update phase so the
/// renderer enters/clears/homes the alternate buffer before laying out and
/// painting the frame. The root canvas is then constrained to the terminal
/// viewport, preserving the main screen and native scrollback for when this
/// component unmounts.
#[component]
pub fn AlternateScreen<'a>(
    hooks: Hooks,
    props: &mut AlternateScreenProps<'a>,
) -> impl Into<AnyElement<'a>> {
    hooks
        .use_context_mut::<SystemContext>()
        .request_alternate_screen(props.mouse_tracking.unwrap_or(true));

    element! {
        View(width: 100pct, height: 100pct, flex_shrink: 0.0) {
            #(props.children.iter_mut())
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::prelude::*;
    use futures::{stream, StreamExt};

    #[component]
    fn AlternateScreenProbe(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let mut show_alt = hooks.use_state(|| true);
        hooks.use_terminal_events(move |event| {
            if matches!(
                event,
                TerminalEvent::Key(KeyEvent {
                    code: KeyCode::Char('x'),
                    kind: KeyEventKind::Press,
                    ..
                })
            ) {
                show_alt.set(false);
            }
        });

        if show_alt.get() {
            element! {
                AlternateScreen {
                    View(width: 100pct, height: 100pct) {
                        Text(content: "alt")
                    }
                }
            }
            .into_any()
        } else {
            system.exit();
            element!(Text(content: "main")).into_any()
        }
    }

    #[test]
    fn test_alternate_screen_constrains_canvas_and_unmounts_to_main_screen() {
        let canvases: Vec<_> = smol::block_on(
            element!(AlternateScreenProbe)
                .mock_terminal_render_loop(
                    MockTerminalConfig::with_events(stream::iter(vec![TerminalEvent::Key(
                        KeyEvent::new(KeyEventKind::Press, KeyCode::Char('x')),
                    )]))
                    .with_size(10, 4),
                )
                .collect(),
        );

        assert_eq!(canvases[0].width(), 10);
        assert_eq!(canvases[0].height(), 4);
        assert!(canvases[0].to_string().contains("alt"));
        assert_eq!(canvases.last().unwrap().to_string(), "main\n");
    }
}
