use crate::{AnyElement, Canvas, Component, ComponentDrawer, ComponentUpdater, Hook, Hooks, Props};

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

    /// Marks the restored cached region damaged. Defaults to `false` to keep
    /// native-scrollback prompt-only frames on the sparse terminal diff path.
    pub damage_on_restore: Option<bool>,

    /// Skips polling the retained child subtree while frozen. Defaults to
    /// `false`, preserving existing child subscription/timer polling unless the
    /// caller explicitly opts into a harder freeze.
    pub skip_poll: Option<bool>,
}

struct OffscreenFreezeSnapshot {
    width: u16,
    height: u16,
    canvas: Canvas,
}

fn is_drawer_visible_in_terminal_viewport(
    drawer: &mut ComponentDrawer,
    terminal_rows: u16,
) -> bool {
    let rows = terminal_rows as isize;
    if rows <= 0 {
        return true;
    }

    let position = drawer.canvas_position();
    let size = drawer.size();
    let absolute_top = position.y as isize;
    let bottom = absolute_top + size.height as isize;
    let screen_height = drawer.root_canvas_mut().height() as isize;
    let cursor_restore_scroll = if screen_height > rows { 1 } else { 0 };
    let viewport_top = (screen_height - rows).max(0) + cursor_restore_scroll;
    let viewport_bottom = viewport_top + rows;
    bottom > viewport_top && absolute_top < viewport_bottom
}

#[derive(Default)]
struct OffscreenFreezeDrawCache {
    frozen: bool,
    bypassed: bool,
    damage_on_restore: bool,
    terminal_rows: u16,
    snapshot: Option<OffscreenFreezeSnapshot>,
}

impl Hook for OffscreenFreezeDrawCache {
    fn pre_component_draw(&mut self, drawer: &mut ComponentDrawer) {
        self.frozen =
            !self.bypassed && !is_drawer_visible_in_terminal_viewport(drawer, self.terminal_rows);
        if !self.frozen {
            return;
        }

        let size = drawer.size();
        let Some(snapshot) = &self.snapshot else {
            return;
        };
        if snapshot.width != size.width || snapshot.height != size.height {
            return;
        }

        if self.damage_on_restore {
            drawer.canvas().blit_region_from(
                &snapshot.canvas,
                0,
                0,
                0,
                0,
                size.width as usize,
                size.height as usize,
            );
        } else {
            drawer.canvas().blit_region_from_clean(
                &snapshot.canvas,
                0,
                0,
                0,
                0,
                size.width as usize,
                size.height as usize,
            );
        }
        drawer.skip_children();
    }

    fn post_component_draw(&mut self, drawer: &mut ComponentDrawer) {
        if self.bypassed {
            self.snapshot = None;
            return;
        }
        if self.frozen {
            return;
        }

        let size = drawer.size();
        let pos = drawer.canvas_position();
        if size.width == 0 || size.height == 0 || pos.x < 0 || pos.y < 0 {
            self.snapshot = None;
            return;
        }
        let (x, y) = (pos.x as usize, pos.y as usize);
        let root = drawer.root_canvas_mut();
        if x.saturating_add(size.width as usize) > root.width()
            || y.saturating_add(size.height as usize) > root.height()
        {
            self.snapshot = None;
            return;
        }

        self.snapshot = Some(OffscreenFreezeSnapshot {
            width: size.width,
            height: size.height,
            canvas: root.copy_region(x, y, size.width as usize, size.height as usize),
        });
    }
}

/// Freezes a subtree once its rows have moved into native terminal scrollback.
///
/// This is iocraft's counterpart to the CC Ink fork's `<OffscreenFreeze>`:
/// while the wrapper is visible, children update normally; after the wrapper is
/// detected outside the live viewport, the previous child component tree is
/// retained and reused without applying new child props. If an
/// [`InVirtualListContext`] is present, freezing is bypassed because virtual
/// lists clip inside an app viewport rather than native scrollback. Draw-time
/// viewport checks restore the last visible canvas snapshot without scheduling
/// an extra render.
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
        let in_virtual_list = updater.get_context::<InVirtualListContext>().is_some();
        let draw_cache = hooks.use_hook(OffscreenFreezeDrawCache::default);
        draw_cache.terminal_rows = props.terminal_rows.unwrap_or(actual_rows);
        draw_cache.bypassed = in_virtual_list;
        draw_cache.damage_on_restore = props.damage_on_restore.unwrap_or(false);
        let frozen = draw_cache.frozen && !in_virtual_list;
        updater.set_skip_child_poll(frozen && props.skip_poll.unwrap_or(false));

        if !frozen {
            updater.update_children(props.children.iter_mut(), None);
        }
        // When offscreen, intentionally do not call update_children. The
        // previously instantiated children remain attached to this wrapper's
        // layout node and draw unchanged, mirroring CC Ink's cached element ref.
        // The draw hook restores the last visible canvas snapshot and skips
        // child drawing so static scrollback rows do not redraw on prompt-only frames.
    }
}

#[cfg(test)]
mod tests {
    use crate::prelude::*;
    use core::{
        pin::Pin,
        task::{Context as TaskContext, Poll},
    };
    use futures::StreamExt;
    use std::sync::atomic::{AtomicUsize, Ordering};

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

    static DEFAULT_POLL_CHILD_POLLS: AtomicUsize = AtomicUsize::new(0);
    static SKIP_POLL_CHILD_POLLS: AtomicUsize = AtomicUsize::new(0);
    static VIRTUAL_POLL_CHILD_POLLS: AtomicUsize = AtomicUsize::new(0);

    #[derive(Default, Props)]
    struct PollingChildProps;

    macro_rules! polling_child {
        ($name:ident, $counter:ident) => {
            #[derive(Default)]
            struct $name;

            impl Component for $name {
                type Props<'a> = PollingChildProps;

                fn new(_props: &Self::Props<'_>) -> Self {
                    Self
                }

                fn update(
                    &mut self,
                    _props: &mut Self::Props<'_>,
                    _hooks: Hooks,
                    updater: &mut ComponentUpdater,
                ) {
                    updater.update_children(
                        std::iter::once(element!(Text(content: "poll child"))),
                        None,
                    );
                }

                fn poll_change(self: Pin<&mut Self>, _cx: &mut TaskContext<'_>) -> Poll<()> {
                    let _ = self;
                    $counter.fetch_add(1, Ordering::SeqCst);
                    Poll::Pending
                }
            }
        };
    }

    polling_child!(DefaultPollingChild, DEFAULT_POLL_CHILD_POLLS);
    polling_child!(SkipPollingChild, SKIP_POLL_CHILD_POLLS);
    polling_child!(VirtualPollingChild, VIRTUAL_POLL_CHILD_POLLS);

    #[component]
    fn DefaultPollFreezeApp(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let mut tick = hooks.use_state(|| 0u8);
        if tick.get() < 4 {
            tick += 1;
        } else {
            system.exit();
        }

        element! {
            View(flex_direction: FlexDirection::Column) {
                OffscreenFreeze(terminal_rows: Some(3)) {
                    DefaultPollingChild
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

    #[component]
    fn SkipPollFreezeApp(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let mut tick = hooks.use_state(|| 0u8);
        if tick.get() < 4 {
            tick += 1;
        } else {
            system.exit();
        }

        element! {
            View(flex_direction: FlexDirection::Column) {
                OffscreenFreeze(terminal_rows: Some(3), skip_poll: true) {
                    SkipPollingChild
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
    fn test_offscreen_freeze_skip_poll_is_opt_in() {
        DEFAULT_POLL_CHILD_POLLS.store(0, Ordering::SeqCst);
        SKIP_POLL_CHILD_POLLS.store(0, Ordering::SeqCst);

        let _: Vec<_> = smol::block_on(
            element!(DefaultPollFreezeApp)
                .mock_terminal_render_loop(MockTerminalConfig::default())
                .collect(),
        );
        let default_polls = DEFAULT_POLL_CHILD_POLLS.load(Ordering::SeqCst);

        let _: Vec<_> = smol::block_on(
            element!(SkipPollFreezeApp)
                .mock_terminal_render_loop(MockTerminalConfig::default())
                .collect(),
        );
        let skip_polls = SKIP_POLL_CHILD_POLLS.load(Ordering::SeqCst);

        assert!(
            default_polls > skip_polls,
            "default frozen children should keep polling unless skip_poll is opted in: default={default_polls}, skip={skip_polls}"
        );
        assert!(
            skip_polls <= 1,
            "skip_poll should stop child polling after the wrapper has frozen: {skip_polls}"
        );
    }

    #[component]
    fn ThawingSkipPollFreezeApp(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let mut tick = hooks.use_state(|| 0u8);
        if tick.get() < 5 {
            tick += 1;
        } else {
            system.exit();
        }
        let terminal_rows = if tick.get() < 3 { 3 } else { 20 };

        element! {
            View(flex_direction: FlexDirection::Column) {
                OffscreenFreeze(terminal_rows: Some(terminal_rows), skip_poll: true) {
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
    fn test_offscreen_freeze_skip_poll_thaws_and_updates_children() {
        let canvases: Vec<_> = smol::block_on(
            element!(ThawingSkipPollFreezeApp)
                .mock_terminal_render_loop(MockTerminalConfig::default())
                .collect(),
        );
        let rendered = canvases.last().unwrap().to_string();
        assert!(
            rendered.starts_with("child renders=4"),
            "thawed skip_poll subtree should resume ordinary child updates: {rendered:?}"
        );
    }

    #[component]
    fn VirtualSkipPollFreezeApp(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let mut tick = hooks.use_state(|| 0u8);
        if tick.get() < 4 {
            tick += 1;
        } else {
            system.exit();
        }

        element! {
            View(flex_direction: FlexDirection::Column) {
                ContextProvider(value: Context::owned(InVirtualListContext)) {
                    OffscreenFreeze(terminal_rows: Some(3), skip_poll: true) {
                        VirtualPollingChild
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
    fn test_offscreen_freeze_skip_poll_keeps_virtual_list_bypass() {
        VIRTUAL_POLL_CHILD_POLLS.store(0, Ordering::SeqCst);
        let _: Vec<_> = smol::block_on(
            element!(VirtualSkipPollFreezeApp)
                .mock_terminal_render_loop(MockTerminalConfig::default())
                .collect(),
        );
        let polls = VIRTUAL_POLL_CHILD_POLLS.load(Ordering::SeqCst);
        assert!(
            polls > 1,
            "virtual-list bypass should keep polling even when skip_poll is requested: {polls}"
        );
    }
}
