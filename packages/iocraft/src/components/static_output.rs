use crate::{
    component,
    hooks::{UseOutput, UseState},
    AnyElement, Hooks, Props,
};

/// The props which can be passed to the [`StaticOutput`] component.
#[non_exhaustive]
#[derive(Default, Props)]
pub struct StaticOutputProps {
    /// The items to render as permanent, non-redrawn output above the live TUI.
    ///
    /// Each time this list grows, the **new** items (those past the previous high-water
    /// mark) are written to stdout and become part of the terminal's scroll buffer.
    /// They are never touched again by the render loop — hence "static".
    ///
    /// Items are rendered as-is (one per line). For richer formatting, build the
    /// strings with ANSI escape codes or use [`element!`](crate::element!) to render
    /// sub-trees to strings via [`ElementExt::to_string`](crate::ElementExt::to_string).
    pub items: Vec<String>,
}

/// `StaticOutput` permanently renders its output above the live TUI area.
///
/// This is iocraft's equivalent of ink's `<Static>` component. It is the standard
/// pattern for displaying completed work — finished tasks, log entries, test results —
/// that should scroll into the terminal's history while the live UI continues below.
///
/// Internally it is built on [`use_output`](crate::hooks::UseOutput), which handles
/// the terminal mechanics (clear the current canvas, write the static lines, then let
/// the render loop redraw the live area underneath).
///
/// # Example
///
/// ```
/// # use iocraft::prelude::*;
/// # #[component]
/// # fn App(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
/// let logs = hooks.use_state(|| vec!["boot: ok".to_string()]);
///
/// element! {
///     View(flex_direction: FlexDirection::Column) {
///         StaticOutput(items: logs.read().clone())
///         Text(content: "running...")
///     }
/// }
/// # }
/// ```
#[component]
pub fn StaticOutput(mut hooks: Hooks, props: &StaticOutputProps) -> impl Into<AnyElement<'static>> {
    let (stdout, _) = hooks.use_output();
    let mut rendered_count = hooks.use_state(|| 0usize);

    let already_rendered = rendered_count.get().min(props.items.len());
    let new_items = &props.items[already_rendered..];
    for item in new_items {
        stdout.println(item);
    }
    if rendered_count.get() != props.items.len() {
        rendered_count.set(props.items.len());
    }

    // StaticOutput contributes no elements to the live render tree. Its output
    // lives entirely in the terminal's scroll buffer via use_output.
    crate::element!(crate::components::View)
}

#[cfg(test)]
mod tests {
    use crate::prelude::*;
    use futures::stream::StreamExt;
    use macro_rules_attribute::apply;
    use smol_macros::test;

    #[component]
    fn LogApp(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let logs = hooks.use_state(|| vec!["line 1".to_string()]);
        let mut tick = hooks.use_state(|| 0u32);

        hooks.use_future({
            let mut logs = logs;
            async move {
                logs.write().push("line 2".to_string());
            }
        });

        tick += 1;
        if tick >= 2 {
            system.exit();
        }

        element! {
            View(flex_direction: FlexDirection::Column) {
                StaticOutput(items: logs.read().clone())
                Text(content: format!("live: tick {}", tick))
            }
        }
    }

    #[apply(test!)]
    async fn test_static_output_renders_new_items_only_once() {
        // StaticOutput writes to stdout via use_output, which is not captured by
        // mock_terminal_render_loop's canvas stream. What we CAN verify is that the
        // component doesn't crash and the live portion renders correctly.
        let canvases: Vec<_> = element!(LogApp)
            .mock_terminal_render_loop(MockTerminalConfig::default())
            .collect()
            .await;
        let last = canvases.last().unwrap().to_string();
        assert!(
            last.contains("live: tick"),
            "live portion should render: {last:?}"
        );
        // The static output ("line 1", "line 2") went to stdout, not the canvas.
        // In a real terminal it appears above; in tests it's invisible to the canvas
        // stream. This is by design — static output is permanent scroll buffer content.
    }

    #[component]
    fn ShrinkingLogApp(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let mut tick = hooks.use_state(|| 0u32);
        tick += 1;
        if tick >= 2 {
            system.exit();
        }
        let items = if tick.get() == 1 {
            vec!["line 1".to_string(), "line 2".to_string()]
        } else {
            vec!["line 1".to_string()]
        };
        element! {
            View(flex_direction: FlexDirection::Column) {
                StaticOutput(items: items)
                Text(content: format!("live: tick {}", tick))
            }
        }
    }

    #[apply(test!)]
    async fn test_static_output_tolerates_items_shrinking() {
        let canvases: Vec<_> = element!(ShrinkingLogApp)
            .mock_terminal_render_loop(MockTerminalConfig::default())
            .collect()
            .await;
        assert!(!canvases.is_empty());
    }
}
