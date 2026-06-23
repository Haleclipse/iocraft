/// Child metadata for [`plan_scroll_viewport_child_render`].
///
/// This models the inputs CC Ink's `renderScrolledChildren(...)` reads from a
/// DOM child and `nodeCache`: current content-local layout, previous cached top
/// and height, and the child's dirty flag.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScrollViewportChildInput<K> {
    /// Caller-owned child identifier returned in the corresponding decision.
    pub key: K,
    /// Current content-local top row from the latest layout pass.
    pub top: i32,
    /// Current child height in rows from the latest layout pass.
    pub height: i32,
    /// Previous cached content-local top row, if available.
    pub cached_top: Option<i32>,
    /// Previous cached height, if available.
    pub cached_height: Option<i32>,
    /// Whether the child was dirty before rendering this frame.
    pub dirty: bool,
}

/// Per-child scroll viewport rendering decision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScrollViewportChildDecision<K> {
    /// Caller-owned child identifier from [`ScrollViewportChildInput::key`].
    pub key: K,
    /// Whether the child intersects the scroll viewport and should be rendered.
    pub visible: bool,
    /// Top row used for the visibility decision.
    pub top: i32,
    /// Height used for the visibility decision.
    pub height: i32,
    /// Whether `top`/`height` came from cached layout instead of current layout.
    pub used_cached_layout: bool,
    /// Whether a visible child render may receive the previous retained screen.
    pub allow_previous_screen: bool,
    /// Cached top value the caller should write back when current layout was read.
    pub refresh_cached_top: Option<i32>,
    /// Whether the caller should drop this culled child's subtree cache.
    pub drop_subtree_cache: bool,
}

/// Plans scroll viewport child culling and cache refreshes.
///
/// This is a mode-neutral counterpart to CC Ink's
/// `renderScrolledChildren(...)` helper. Clean children with a cached `top` and
/// no cumulative dirty-height shift can be culled using cached layout without a
/// fresh layout read; dirty children update the cumulative height shift using
/// their previous cached height; culled children request subtree-cache drops
/// unless `preserve_culled_cache` is set for the DECSTBM/blit fast path.
///
/// The helper only computes metadata. It does not mutate [`RendererNodeCache`],
/// inspect iocraft's component tree, draw to a [`Canvas`], write terminal output,
/// or change screen mode.
pub fn plan_scroll_viewport_child_render<K>(
    scroll_top: i32,
    scroll_bottom: i32,
    preserve_culled_cache: bool,
    has_removed_child: bool,
    children: impl IntoIterator<Item = ScrollViewportChildInput<K>>,
) -> Vec<ScrollViewportChildDecision<K>> {
    let visible_top = i64::from(scroll_top);
    let visible_bottom = i64::from(scroll_bottom);
    let mut cumulative_height_shift = 0i64;
    let mut seen_dirty_rendered_child = false;
    let mut decisions = Vec::new();

    for child in children {
        let has_cached_layout = child.cached_top.is_some() || child.cached_height.is_some();
        let can_use_cached = child.cached_top.is_some()
            && child.cached_height.is_some()
            && !child.dirty
            && cumulative_height_shift == 0;

        let (top, height, used_cached_layout, refresh_cached_top) = if can_use_cached {
            (
                child.cached_top.unwrap_or(child.top),
                child.cached_height.unwrap_or(child.height),
                true,
                None,
            )
        } else {
            if child.dirty {
                cumulative_height_shift +=
                    i64::from(child.height) - i64::from(child.cached_height.unwrap_or(0));
            }
            (
                child.top,
                child.height,
                false,
                has_cached_layout.then_some(child.top),
            )
        };

        let bottom = i64::from(top) + i64::from(height);
        let visible = !(bottom <= visible_top || i64::from(top) >= visible_bottom);
        let allow_previous_screen = visible && !has_removed_child && !seen_dirty_rendered_child;
        let dirty = child.dirty;

        decisions.push(ScrollViewportChildDecision {
            key: child.key,
            visible,
            top,
            height,
            used_cached_layout,
            allow_previous_screen,
            refresh_cached_top,
            drop_subtree_cache: !visible && !preserve_culled_cache,
        });

        if visible && dirty {
            seen_dirty_rendered_child = true;
        }
    }

    decisions
}
