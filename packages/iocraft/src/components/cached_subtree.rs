use crate::{
    component, components::View, element, AnyElement, Canvas, ComponentDrawer, Hook, Hooks, Props,
};

/// Props for [`CachedSubtree`].
#[non_exhaustive]
#[derive(Default, Props)]
pub struct CachedSubtreeProps<'a> {
    /// The subtree to render or restore from cache.
    pub children: Vec<AnyElement<'a>>,

    /// Application-defined cache key. Change this whenever the subtree's visual
    /// output should be recomputed.
    pub cache_key: String,

    /// Enables the cache. Defaults to `true`.
    pub enabled: Option<bool>,
}

struct CachedSubtreeSnapshot {
    key: String,
    width: u16,
    height: u16,
    canvas: Canvas,
}

#[derive(Default)]
struct CachedSubtreeHook {
    key: String,
    enabled: bool,
    snapshot: Option<CachedSubtreeSnapshot>,
}

impl Hook for CachedSubtreeHook {
    fn pre_component_draw(&mut self, drawer: &mut ComponentDrawer) {
        if !self.enabled {
            return;
        }
        let size = drawer.size();
        if size.width == 0 || size.height == 0 {
            return;
        }
        let Some(snapshot) = &self.snapshot else {
            return;
        };
        if snapshot.key != self.key
            || snapshot.width != size.width
            || snapshot.height != size.height
        {
            return;
        }

        drawer.canvas().blit_region_from(
            &snapshot.canvas,
            0,
            0,
            0,
            0,
            size.width as usize,
            size.height as usize,
        );
        drawer.skip_children();
    }

    fn post_component_draw(&mut self, drawer: &mut ComponentDrawer) {
        if !self.enabled {
            self.snapshot = None;
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
        let canvas = root.copy_region(x, y, size.width as usize, size.height as usize);
        self.snapshot = Some(CachedSubtreeSnapshot {
            key: self.key.clone(),
            width: size.width,
            height: size.height,
            canvas,
        });
    }
}

/// Explicit clean-subtree canvas cache.
///
/// This component provides an iocraft building block for the CC Ink fork's
/// clean-subtree `nodeCache` / `screen.blitRegion(...)` fast path. The first
/// frame renders children normally and captures the component's canvas region.
/// Later frames with the same `cache_key` and layout size restore that retained
/// canvas region and skip child drawing. Change `cache_key` whenever the
/// subtree's visible output should be recomputed.
///
/// The cache stores the retained screen-buffer metadata as well as text cells,
/// so `noSelect`, `softWrap`, hyperlinks, overlays, and wide-character boundary
/// repair survive the blit path.
#[component]
pub fn CachedSubtree<'a>(
    mut hooks: Hooks,
    props: &mut CachedSubtreeProps<'a>,
) -> impl Into<AnyElement<'a>> {
    let hook = hooks.use_hook(CachedSubtreeHook::default);
    hook.key = props.cache_key.clone();
    hook.enabled = props.enabled.unwrap_or(true);

    element! {
        View(width: 100pct) {
            #(props.children.iter_mut())
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::prelude::*;
    use crossterm::style::Colored;
    use futures::{stream, StreamExt};

    #[component]
    fn CachedSubtreeProbe(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let mut phase = hooks.use_state(|| 0u8);
        hooks.use_terminal_events(move |event| {
            if matches!(
                event,
                TerminalEvent::Key(KeyEvent {
                    code: KeyCode::Char('x'),
                    kind: KeyEventKind::Press,
                    ..
                })
            ) {
                phase.set(1);
            }
        });

        if phase.get() == 1 {
            system.exit();
        }

        element! {
            View(width: 10) {
                CachedSubtree(cache_key: "stable".to_string()) {
                    Text(content: if phase.get() == 0 { "first" } else { "changed" })
                }
            }
        }
    }

    #[test]
    fn test_cached_subtree_blits_and_skips_child_draw_when_key_is_stable() {
        let canvases: Vec<_> = smol::block_on(
            element!(CachedSubtreeProbe)
                .mock_terminal_render_loop(MockTerminalConfig::with_events(stream::iter(vec![
                    TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Char('x'))),
                ])))
                .collect(),
        );
        assert_eq!(canvases.last().unwrap().to_string(), "first\n");
        assert!(
            canvases.last().unwrap().has_damage(),
            "cached blit should mark the restored region damaged"
        );
    }

    #[component]
    fn CachedSubtreeInvalidationProbe(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let mut phase = hooks.use_state(|| 0u8);
        hooks.use_terminal_events(move |event| {
            if matches!(
                event,
                TerminalEvent::Key(KeyEvent {
                    code: KeyCode::Char('x'),
                    kind: KeyEventKind::Press,
                    ..
                })
            ) {
                phase.set(1);
            }
        });

        if phase.get() == 1 {
            system.exit();
        }

        element! {
            View(width: 10) {
                CachedSubtree(cache_key: format!("phase-{}", phase.get())) {
                    Text(content: if phase.get() == 0 { "first" } else { "changed" })
                }
            }
        }
    }

    #[test]
    fn test_cached_subtree_rerenders_when_key_changes() {
        let canvases: Vec<_> = smol::block_on(
            element!(CachedSubtreeInvalidationProbe)
                .mock_terminal_render_loop(MockTerminalConfig::with_events(stream::iter(vec![
                    TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Char('x'))),
                ])))
                .collect(),
        );
        assert_eq!(canvases.last().unwrap().to_string(), "changed\n");
    }

    #[component]
    fn CachedSubtreeNoSelectParentProbe(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let mut fenced = hooks.use_state(|| false);
        hooks.use_terminal_events(move |event| {
            if matches!(
                event,
                TerminalEvent::Key(KeyEvent {
                    code: KeyCode::Char('x'),
                    kind: KeyEventKind::Press,
                    ..
                })
            ) {
                fenced.set(true);
            }
        });

        if fenced.get() {
            system.exit();
        }

        element! {
            View(width: 4, no_select: fenced.get()) {
                CachedSubtree(cache_key: "stable".to_string()) {
                    Text(content: "abcd")
                }
            }
        }
    }

    #[test]
    fn test_parent_no_select_replays_after_cached_child_blit_like_cc_ink() {
        let canvases: Vec<_> = smol::block_on(
            element!(CachedSubtreeNoSelectParentProbe)
                .mock_terminal_render_loop(MockTerminalConfig::with_events(stream::iter(vec![
                    TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Char('x'))),
                ])))
                .collect(),
        );
        let canvas = canvases.last().unwrap();
        assert_eq!(canvas.to_string(), "abcd\n");
        for col in 0..4 {
            assert!(
                canvas.is_no_select(col, 0),
                "parent noSelect should win over a cached child blit at col {col}"
            );
        }
    }

    #[component]
    fn CachedSubtreeNoSelectOverlayProbe(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let mut fenced = hooks.use_state(|| false);
        let selection = create_selection_context(&mut hooks);
        let search = create_search_highlight_context(&mut hooks);

        hooks.use_terminal_events(move |event| {
            if matches!(
                event,
                TerminalEvent::Key(KeyEvent {
                    code: KeyCode::Char('x'),
                    kind: KeyEventKind::Press,
                    ..
                })
            ) {
                fenced.set(true);
            }
        });

        if fenced.get() && !selection.has_selection() {
            let mut controller = SelectionController::new();
            controller.selection_mut().start(0, 0);
            controller.selection_mut().update(3, 0);
            controller.selection_mut().finish();
            selection.set_controller(controller);
        }
        hooks.use_screen_overlays(selection, search);

        if fenced.get() {
            system.exit();
        }

        element! {
            View(width: 4, no_select: fenced.get()) {
                CachedSubtree(cache_key: "stable".to_string()) {
                    Text(content: "abcd")
                }
            }
        }
    }

    #[test]
    fn test_no_select_replay_precedes_selection_overlay_like_cc_ink_output_get() {
        let canvases: Vec<_> = smol::block_on(
            element!(CachedSubtreeNoSelectOverlayProbe)
                .mock_terminal_render_loop(MockTerminalConfig::with_events(stream::iter(vec![
                    TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Char('x'))),
                ])))
                .collect(),
        );
        let canvas = canvases.last().unwrap();
        assert_eq!(canvas.to_string(), "abcd\n");
        for col in 0..4 {
            assert!(canvas.is_no_select(col, 0));
        }

        let mut ansi = Vec::new();
        canvas.write_ansi(&mut ansi).unwrap();
        let ansi = String::from_utf8_lossy(&ansi);
        let blue_bg = format!("{}", Colored::BackgroundColor(Color::Blue));
        assert!(
            !ansi.contains(&blue_bg),
            "selection overlay must see replayed noSelect metadata before painting: {ansi:?}"
        );
    }
}
