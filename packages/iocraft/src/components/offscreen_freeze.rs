use crate::{AnyElement, Component, ComponentUpdater, Hooks, Props};

/// Context marker that disables [`OffscreenFreeze`] freezing inside virtualized
/// in-app lists.
///
/// This mirrors Claude Code's CC Ink `InVirtualListContext`: virtual lists clip
/// content inside an app-owned scroll container rather than using native
/// terminal scrollback, so stale offscreen element caching would block normal
/// interactions such as expand/collapse.
#[derive(Clone, Copy, Debug, Default)]
pub struct InVirtualListContext;

/// Props for [`OffscreenFreeze`].
#[non_exhaustive]
#[derive(Default, Props)]
pub struct OffscreenFreezeProps<'a> {
    /// The subtree to freeze when it moves above the terminal viewport.
    pub children: Vec<AnyElement<'a>>,

    /// Optional terminal-row override. When `None`, the current terminal size is
    /// used. This is mainly useful for deterministic tests/examples or custom
    /// viewport owners.
    pub terminal_rows: Option<u16>,
}

/// Freezes a subtree once its rows have moved into native terminal scrollback.
///
/// This is iocraft's counterpart to the CC Ink fork's `<OffscreenFreeze>`:
/// while the wrapper is visible, children update normally; after the wrapper is
/// detected outside the live viewport, the previous child component tree is
/// retained and reused without applying new child props. If an
/// [`InVirtualListContext`] is present, freezing is bypassed because virtual
/// lists clip inside an app viewport rather than native scrollback. The latest
/// visibility value comes from [`use_terminal_viewport`](crate::hooks::UseTerminalViewport)
/// and does not by itself schedule extra renders.
#[derive(Default)]
pub struct OffscreenFreeze;

impl Component for OffscreenFreeze {
    type Props<'a> = OffscreenFreezeProps<'a>;

    fn new(_props: &Self::Props<'_>) -> Self {
        Self
    }

    fn update(
        &mut self,
        props: &mut Self::Props<'_>,
        mut hooks: Hooks,
        updater: &mut ComponentUpdater,
    ) {
        let (_, actual_rows) = crate::hooks::UseTerminalSize::use_terminal_size(&mut hooks);
        let rows = props.terminal_rows.unwrap_or(actual_rows);
        let entry =
            crate::hooks::UseTerminalViewport::use_terminal_viewport_with_rows(&mut hooks, rows);
        let in_virtual_list = updater.get_context::<InVirtualListContext>().is_some();

        if entry.is_visible || in_virtual_list {
            updater.update_children(props.children.iter_mut(), None);
        }
        // When offscreen, intentionally do not call update_children. The
        // previously instantiated children remain attached to this wrapper's
        // layout node and draw unchanged, mirroring CC Ink's cached element ref.
    }
}

#[cfg(test)]
mod tests {
    use crate::prelude::*;
    use futures::StreamExt;

    #[component]
    fn CountingChild(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut renders = hooks.use_state(|| 0u32);
        renders += 1;
        element!(Text(content: format!("child renders={}", renders.get())))
    }

    #[component]
    fn FreezeApp(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let mut tick = hooks.use_state(|| 0u8);
        if tick.get() < 3 {
            tick += 1;
        } else {
            system.exit();
        }

        element! {
            View(flex_direction: FlexDirection::Column) {
                OffscreenFreeze(terminal_rows: Some(3)) {
                    CountingChild
                }
                Text(content: "row 1")
                Text(content: "row 2")
                Text(content: "row 3")
                Text(content: "row 4")
                Text(content: "row 5")
                Text(content: "row 6")
                Text(content: "row 7")
            }
        }
    }

    #[test]
    fn test_offscreen_freeze_reuses_previous_child_tree() {
        let canvases: Vec<_> = smol::block_on(
            element!(FreezeApp)
                .mock_terminal_render_loop(MockTerminalConfig::default())
                .collect(),
        );
        let rendered = canvases.last().unwrap().to_string();
        assert!(
            rendered.starts_with("child renders=1"),
            "offscreen child should stay frozen after first visible render: {rendered:?}"
        );
    }

    #[component]
    fn VirtualFreezeApp(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let mut tick = hooks.use_state(|| 0u8);
        if tick.get() < 3 {
            tick += 1;
        } else {
            system.exit();
        }

        element! {
            View(flex_direction: FlexDirection::Column) {
                ContextProvider(value: Context::owned(InVirtualListContext)) {
                    OffscreenFreeze(terminal_rows: Some(3)) {
                        CountingChild
                    }
                }
                Text(content: "row 1")
                Text(content: "row 2")
                Text(content: "row 3")
                Text(content: "row 4")
                Text(content: "row 5")
                Text(content: "row 6")
                Text(content: "row 7")
            }
        }
    }

    #[test]
    fn test_offscreen_freeze_bypasses_cache_inside_virtual_list_context() {
        let canvases: Vec<_> = smol::block_on(
            element!(VirtualFreezeApp)
                .mock_terminal_render_loop(MockTerminalConfig::default())
                .collect(),
        );
        let rendered = canvases.last().unwrap().to_string();
        assert!(
            rendered.starts_with("child renders=4"),
            "virtual-list context should keep updating offscreen children: {rendered:?}"
        );
    }
}
