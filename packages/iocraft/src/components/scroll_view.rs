use crate::{
    component,
    components::View,
    element,
    hooks::{
        Ref, SelectionContext, State, UseConst, UseInterval, UseRef, UseState, UseTerminalEvents,
    },
    AnyElement, Canvas, CanvasTextStyle, Color, Component, ComponentDrawer, ComponentUpdater,
    FlexDirection, Hook, Hooks, JustifyContent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers,
    MouseEventKind, Overflow, Position, Props, TerminalEvent, FRAME_INTERVAL,
};
use std::{
    collections::{HashMap, HashSet},
    hash::Hash,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

/// A handle which can be used for imperative control of a [`ScrollView`] component.
///
/// # Example
///
/// ```
/// # use iocraft::prelude::*;
/// # #[component]
/// # fn MyScrollable(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
/// let handle = hooks.use_ref_default::<ScrollViewHandle>();
///
/// hooks.use_terminal_events({
///     let mut handle = handle;
///     move |event| {
///         if let TerminalEvent::Key(KeyEvent { code: KeyCode::Home, kind: KeyEventKind::Press, .. }) = event {
///             handle.write().scroll_to_top();
///         }
///     }
/// });
///
/// element! {
///     View(width: 80, height: 20) {
///         ScrollView(handle) {
///             Text(content: "lots of content here...")
///         }
///     }
/// }
/// # }
/// ```
#[derive(Default)]
pub struct ScrollViewHandle {
    inner: Option<ScrollViewHandleInner>,
}

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
struct ScrollClampBounds {
    min: Option<i32>,
    max: Option<i32>,
}

#[derive(Clone, Default)]
struct SharedScrollViewSubscribers(Arc<Mutex<ScrollViewSubscribers>>);

type ScrollViewListener = Arc<Mutex<Box<dyn FnMut() + Send + 'static>>>;

#[derive(Default)]
struct ScrollViewSubscribers {
    next_id: u64,
    listeners: Vec<(u64, ScrollViewListener)>,
}

impl SharedScrollViewSubscribers {
    fn subscribe(&self, listener: impl FnMut() + Send + 'static) -> ScrollViewSubscription {
        let mut guard = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let id = guard.next_id;
        guard.next_id = guard.next_id.wrapping_add(1);
        guard
            .listeners
            .push((id, Arc::new(Mutex::new(Box::new(listener)))));
        ScrollViewSubscription {
            subscribers: Some(self.clone()),
            id,
        }
    }

    fn unsubscribe(&self, id: u64) {
        let mut guard = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard
            .listeners
            .retain(|(listener_id, _)| *listener_id != id);
    }

    fn notify(&self) {
        let listeners = {
            let guard = self
                .0
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            guard
                .listeners
                .iter()
                .map(|(_, listener)| listener.clone())
                .collect::<Vec<_>>()
        };
        for listener in listeners {
            let mut listener = listener
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            listener();
        }
    }
}

/// RAII subscription returned by [`ScrollViewHandle::subscribe`].
///
/// Dropping this value removes the listener.
#[derive(Default)]
pub struct ScrollViewSubscription {
    subscribers: Option<SharedScrollViewSubscribers>,
    id: u64,
}

impl Drop for ScrollViewSubscription {
    fn drop(&mut self) {
        if let Some(subscribers) = self.subscribers.take() {
            subscribers.unsubscribe(self.id);
        }
    }
}

struct ScrollViewHandleInner {
    scroll_offset: State<i32>,
    content_height: State<u16>,
    viewport_height: State<u16>,
    viewport_top: State<u16>,
    user_scrolled_up: State<bool>,
    clamp_bounds: State<ScrollClampBounds>,
    pending_scroll_delta: State<i32>,
    auto_scroll_enabled: bool,
    scroll_drain_enabled: bool,
    subscribers: SharedScrollViewSubscribers,
}

impl ScrollViewHandle {
    /// Scrolls to the top of the content. Disengages auto scroll.
    pub fn scroll_to_top(&mut self) {
        if let Some(inner) = &mut self.inner {
            let target = clamp_offset_to_scroll_range(
                0,
                inner.content_height.get(),
                inner.viewport_height.get(),
            );
            inner.scroll_offset.set(target);
            inner.pending_scroll_delta.set(0);
            Self::update_user_scrolled_up(inner);
            inner.subscribers.notify();
        }
    }

    /// Scrolls to the bottom of the content. Re-engages auto scroll.
    pub fn scroll_to_bottom(&mut self) {
        if let Some(inner) = &mut self.inner {
            let max = max_offset(inner.content_height.get(), inner.viewport_height.get());
            inner.scroll_offset.set(max);
            inner.pending_scroll_delta.set(0);
            inner.user_scrolled_up.set(false);
            inner.subscribers.notify();
        }
    }

    /// Scrolls to the given offset in lines from the top. The offset is clamped to the valid
    /// range. Disengages auto scroll if the resulting position is not at the bottom.
    pub fn scroll_to(&mut self, offset: i32) {
        if let Some(inner) = &mut self.inner {
            Self::scroll_to_inner(inner, offset);
            inner.subscribers.notify();
        }
    }

    /// Scrolls to a content-local row plus an optional offset.
    ///
    /// This is a Rust-native building block for CC Ink's `scrollToElement`:
    /// callers that already know the target's content-local top can request the
    /// same one-shot scroll without tying iocraft to a DOM node type.
    pub fn scroll_to_content_top(&mut self, content_top: i32, offset: i32) {
        self.scroll_to(content_top.saturating_add(offset));
    }

    /// Scrolls so a previously measured absolute screen rect reaches the viewport top.
    ///
    /// Pair this with [`crate::hooks::UseComponentRect::use_component_rect`].
    /// The method converts the target's absolute top row from the last completed
    /// frame into a content-local offset using the current viewport top and
    /// scroll offset, then applies `offset`. This preserves the useful
    /// semantics of CC Ink's `ScrollBoxHandle.scrollToElement(...)` while
    /// keeping the API Rust-first and independent of React/DOM objects.
    pub fn scroll_to_rect(&mut self, rect: taffy::Rect<i32>, offset: i32) {
        if let Some(inner) = &mut self.inner {
            let target = scroll_content_top_for_absolute_rect(
                Self::visual_scroll_offset(inner),
                inner.viewport_top.get() as i32,
                rect,
                offset,
            );
            Self::scroll_to_inner(inner, target);
            inner.subscribers.notify();
        }
    }

    /// CC Ink-style alias for [`Self::scroll_to_rect`].
    pub fn scroll_to_element(&mut self, rect: taffy::Rect<i32>, offset: i32) {
        self.scroll_to_rect(rect, offset);
    }

    /// Scrolls by the given number of lines (positive = down, negative = up). The resulting
    /// offset is clamped to the valid range. Disengages auto scroll if the resulting position
    /// is not at the bottom.
    ///
    /// When [`ScrollViewProps::scroll_drain_mode`] is enabled, this mirrors CC Ink's
    /// imperative `ScrollBoxHandle.scrollBy(...)`: the delta is accumulated in
    /// [`Self::get_pending_delta`] and drained over render frames. Without a drain
    /// mode, iocraft keeps its eager historical behavior.
    pub fn scroll_by(&mut self, delta: i32) {
        if let Some(inner) = &mut self.inner {
            if inner.scroll_drain_enabled {
                inner
                    .pending_scroll_delta
                    .set(inner.pending_scroll_delta.get().saturating_add(delta));
                if delta < 0 {
                    if inner.auto_scroll_enabled && !inner.user_scrolled_up.get() {
                        inner.scroll_offset.set(max_offset(
                            inner.content_height.get(),
                            inner.viewport_height.get(),
                        ));
                    }
                    inner.user_scrolled_up.set(true);
                }
                inner.subscribers.notify();
                return;
            }

            inner.scroll_offset.set(clamp_offset_to_scroll_range(
                inner.scroll_offset.get() + delta,
                inner.content_height.get(),
                inner.viewport_height.get(),
            ));
            inner.pending_scroll_delta.set(0);
            Self::update_user_scrolled_up(inner);
            inner.subscribers.notify();
        }
    }

    /// Returns the current scroll offset in lines from the top.
    pub fn scroll_offset(&self) -> i32 {
        self.inner
            .as_ref()
            .map_or(0, |inner| inner.scroll_offset.get())
    }

    /// CC Ink-style alias for [`Self::scroll_offset`].
    pub fn get_scroll_top(&self) -> i32 {
        self.scroll_offset()
    }

    /// Returns the pending wheel-scroll delta waiting for opt-in render-time drain.
    ///
    /// This is non-zero only when [`ScrollViewProps::scroll_drain_mode`] is set;
    /// ordinary iocraft scrolling applies deltas eagerly.
    pub fn get_pending_delta(&self) -> i32 {
        self.inner
            .as_ref()
            .map_or(0, |inner| inner.pending_scroll_delta.get())
    }

    /// Returns the total height of the scrollable content in lines.
    pub fn content_height(&self) -> u16 {
        self.inner
            .as_ref()
            .map_or(0, |inner| inner.content_height.get())
    }

    /// CC Ink-style alias for [`Self::content_height`].
    pub fn get_scroll_height(&self) -> u16 {
        self.content_height()
    }

    /// Returns the freshest known scroll height.
    ///
    /// iocraft measures content height during the draw phase, so the cached
    /// value is already the latest completed layout measurement.
    pub fn get_fresh_scroll_height(&self) -> u16 {
        self.content_height()
    }

    /// Returns the height of the visible viewport in lines.
    pub fn viewport_height(&self) -> u16 {
        self.inner
            .as_ref()
            .map_or(0, |inner| inner.viewport_height.get())
    }

    /// CC Ink-style alias for [`Self::viewport_height`].
    pub fn get_viewport_height(&self) -> u16 {
        self.viewport_height()
    }

    /// Returns the absolute screen-buffer row of the viewport top.
    pub fn viewport_top(&self) -> u16 {
        self.inner
            .as_ref()
            .map_or(0, |inner| inner.viewport_top.get())
    }

    /// CC Ink-style alias for [`Self::viewport_top`].
    pub fn get_viewport_top(&self) -> u16 {
        self.viewport_top()
    }

    /// Returns whether auto scroll is currently pinned to the bottom.
    pub fn is_auto_scroll_pinned(&self) -> bool {
        self.inner
            .as_ref()
            .is_some_and(|inner| inner.auto_scroll_enabled && !inner.user_scrolled_up.get())
    }

    /// CC Ink-style alias for [`Self::is_auto_scroll_pinned`].
    pub fn is_sticky(&self) -> bool {
        self.is_auto_scroll_pinned()
    }

    /// Subscribes to imperative scroll changes.
    ///
    /// This mirrors CC Ink's `ScrollBoxHandle.subscribe(...)` shape. The
    /// listener is called after `scroll_to`, `scroll_by`, `scroll_to_bottom`,
    /// keyboard/mouse scroll handling, and clamp-bound changes. Drop the
    /// returned [`ScrollViewSubscription`] to unsubscribe.
    pub fn subscribe(&self, listener: impl FnMut() + Send + 'static) -> ScrollViewSubscription {
        self.inner
            .as_ref()
            .map(|inner| inner.subscribers.subscribe(listener))
            .unwrap_or_default()
    }

    /// Sets render-time visual clamp bounds for scroll offsets.
    ///
    /// This is the iocraft counterpart to CC Ink's `setClampBounds(min, max)`:
    /// virtualized scroll owners can render at the mounted range edge while the
    /// committed logical target returned by [`Self::get_scroll_top`] continues
    /// to run ahead. Passing `None` disables either bound.
    pub fn set_clamp_bounds(&mut self, min: Option<i32>, max: Option<i32>) {
        if let Some(inner) = &mut self.inner {
            inner.clamp_bounds.set(ScrollClampBounds { min, max });
            // Clamp bounds are render-time/visual bounds for virtual scroll.
            // Keep the committed logical target intact so a fast scroll can run
            // ahead of the currently mounted range while the rendered viewport
            // stays pinned to the mounted edge, matching CC Ink.
            inner.subscribers.notify();
        }
    }

    /// Clears all clamp bounds.
    pub fn clear_clamp_bounds(&mut self) {
        self.set_clamp_bounds(None, None);
    }

    fn committed_scroll_offset(inner: &ScrollViewHandleInner) -> i32 {
        if inner.auto_scroll_enabled && !inner.user_scrolled_up.get() {
            max_offset(inner.content_height.get(), inner.viewport_height.get())
        } else {
            inner.scroll_offset.get()
        }
    }

    fn visual_scroll_offset(inner: &ScrollViewHandleInner) -> i32 {
        clamp_offset_with_bounds(
            Self::committed_scroll_offset(inner),
            inner.content_height.get(),
            inner.viewport_height.get(),
            inner.clamp_bounds.get(),
        )
    }

    fn scroll_to_inner(inner: &mut ScrollViewHandleInner, offset: i32) {
        inner.scroll_offset.set(clamp_offset_to_scroll_range(
            offset,
            inner.content_height.get(),
            inner.viewport_height.get(),
        ));
        inner.pending_scroll_delta.set(0);
        Self::update_user_scrolled_up(inner);
    }

    fn update_user_scrolled_up(inner: &mut ScrollViewHandleInner) {
        let max = max_offset(inner.content_height.get(), inner.viewport_height.get());
        inner.user_scrolled_up.set(inner.scroll_offset.get() < max);
    }
}

fn max_offset(content_height: u16, viewport_height: u16) -> i32 {
    (content_height as i32 - viewport_height as i32).max(0)
}

fn clamp_offset_to_scroll_range(offset: i32, content_height: u16, viewport_height: u16) -> i32 {
    offset.clamp(0, max_offset(content_height, viewport_height))
}

fn clamp_offset_with_bounds(
    offset: i32,
    content_height: u16,
    viewport_height: u16,
    bounds: ScrollClampBounds,
) -> i32 {
    let max_valid = max_offset(content_height, viewport_height);
    let lower = bounds.min.unwrap_or(0).clamp(0, max_valid);
    let upper = bounds.max.unwrap_or(max_valid).clamp(lower, max_valid);
    offset.clamp(lower, upper)
}

/// Converts a measured absolute rect into a content-local scroll target.
///
/// This is the pure calculation behind [`ScrollViewHandle::scroll_to_rect`]:
/// `rect.top - viewport_top` gives the target's row inside the current
/// viewport, and adding the current scroll offset recovers the target's
/// content-local row. `offset` is then applied in the same sense as CC Ink's
/// `scrollToElement(el, offset)`.
pub fn scroll_content_top_for_absolute_rect(
    current_scroll_offset: i32,
    viewport_top: i32,
    rect: taffy::Rect<i32>,
    offset: i32,
) -> i32 {
    current_scroll_offset
        .saturating_add(rect.top.saturating_sub(viewport_top))
        .saturating_add(offset)
}

/// Default estimated height, in rows, for unmeasured virtual-scroll items.
pub const VIRTUAL_SCROLL_DEFAULT_ESTIMATE_ROWS: u16 = 3;
/// Default overscan, in rows, above and below a virtual-scroll viewport.
pub const VIRTUAL_SCROLL_OVERSCAN_ROWS: u16 = 80;
/// Default tail seed count before the scroll viewport is measured.
pub const VIRTUAL_SCROLL_COLD_START_COUNT: usize = 30;
/// Default scroll snapshot quantum, in rows, used to avoid re-planning every wheel tick.
pub const VIRTUAL_SCROLL_QUANTUM_ROWS: u16 = VIRTUAL_SCROLL_OVERSCAN_ROWS / 2;
/// Default pessimistic height, in rows, for unmeasured items during coverage checks.
pub const VIRTUAL_SCROLL_PESSIMISTIC_HEIGHT_ROWS: u16 = 1;
/// Default maximum number of mounted virtual-scroll items.
pub const VIRTUAL_SCROLL_MAX_MOUNTED_ITEMS: usize = 300;
/// Default cap for newly mounted items during fast-scroll catch-up.
pub const VIRTUAL_SCROLL_SLIDE_STEP_ITEMS: usize = 25;
/// Default cap for the committed-to-pending span, in viewport heights.
pub const VIRTUAL_SCROLL_MAX_SPAN_VIEWPORTS: u16 = 3;

/// Configuration for [`plan_virtual_scroll_range`].
///
/// The defaults mirror the CC Ink fork's `useVirtualScroll` constants while
/// keeping the result as explicit Rust planning data. Applications may tune
/// these numbers for cheaper rows, denser transcripts, or non-message lists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VirtualScrollConfig {
    /// Estimated height for an unmeasured item.
    pub default_estimate_rows: u16,
    /// Extra rows to keep mounted above and below the viewport.
    pub overscan_rows: u16,
    /// Tail seed count before the viewport has reported a height.
    pub cold_start_count: usize,
    /// Quantization quantum for [`virtual_scroll_snapshot_bin`].
    pub scroll_quantum_rows: u16,
    /// Smallest possible height used when proving the mounted range covers the viewport.
    pub pessimistic_height_rows: u16,
    /// Hard cap for the mounted item count.
    pub max_mounted_items: usize,
    /// Maximum number of new items mounted per fast-scroll planning step.
    pub slide_step_items: usize,
    /// Maximum committed-to-pending span, expressed in viewport heights.
    pub max_span_viewports: u16,
}

impl Default for VirtualScrollConfig {
    fn default() -> Self {
        Self {
            default_estimate_rows: VIRTUAL_SCROLL_DEFAULT_ESTIMATE_ROWS,
            overscan_rows: VIRTUAL_SCROLL_OVERSCAN_ROWS,
            cold_start_count: VIRTUAL_SCROLL_COLD_START_COUNT,
            scroll_quantum_rows: VIRTUAL_SCROLL_QUANTUM_ROWS,
            pessimistic_height_rows: VIRTUAL_SCROLL_PESSIMISTIC_HEIGHT_ROWS,
            max_mounted_items: VIRTUAL_SCROLL_MAX_MOUNTED_ITEMS,
            slide_step_items: VIRTUAL_SCROLL_SLIDE_STEP_ITEMS,
            max_span_viewports: VIRTUAL_SCROLL_MAX_SPAN_VIEWPORTS,
        }
    }
}

/// Half-open item range returned by virtual-scroll planning.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VirtualScrollRange {
    /// First item index to render.
    pub start: usize,
    /// One-past-last item index to render.
    pub end: usize,
}

impl VirtualScrollRange {
    /// Creates a new half-open range.
    pub fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }

    /// Returns the number of items in the range.
    pub fn len(self) -> usize {
        self.end.saturating_sub(self.start)
    }

    /// Returns true when the range contains no items.
    pub fn is_empty(self) -> bool {
        self.start >= self.end
    }
}

/// Inputs for [`plan_virtual_scroll_range`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VirtualScrollInput {
    /// Current committed scroll offset, or `None` before the viewport has attached.
    pub scroll_top: Option<i32>,
    /// Pending scroll delta that has not yet drained into `scroll_top`.
    pub pending_delta: i32,
    /// Height of the visible viewport in rows.
    pub viewport_height: u16,
    /// Whether the scroll view should stay pinned to the tail.
    pub is_sticky: bool,
    /// Offset of the virtualized list inside the scroll content.
    pub list_origin: i32,
    /// Previously rendered range, used to cap fast-scroll mount churn.
    pub previous_range: Option<VirtualScrollRange>,
    /// Previously committed scroll offset, used to detect fast-scroll velocity.
    pub previous_scroll_top: Option<i32>,
}

impl Default for VirtualScrollInput {
    fn default() -> Self {
        Self {
            scroll_top: None,
            pending_delta: 0,
            viewport_height: 0,
            is_sticky: true,
            list_origin: 0,
            previous_range: None,
            previous_scroll_top: None,
        }
    }
}

/// Stateful height-cache owner for CC Ink-style virtual-scroll planning.
///
/// This mirrors the reusable, non-React parts of CC Ink's `useVirtualScroll`:
/// measured row heights are cached by stable item key, stale keys can be
/// collected explicitly, column changes scale cached heights instead of
/// dropping them, and the previous range can be frozen briefly to avoid mount
/// churn while a resize settles. It is an opt-in planning helper; it does not
/// render, allocate component nodes, or mutate a [`ScrollViewHandle`].
#[derive(Clone, Debug)]
pub struct VirtualScrollState<K> {
    heights: HashMap<K, u16>,
    columns: Option<u16>,
    previous_range: Option<VirtualScrollRange>,
    previous_scroll_top: Option<i32>,
    frozen_range: Option<VirtualScrollRange>,
    freeze_remaining: u8,
    skip_next_measurement: bool,
}

impl<K> Default for VirtualScrollState<K> {
    fn default() -> Self {
        Self {
            heights: HashMap::new(),
            columns: None,
            previous_range: None,
            previous_scroll_top: None,
            frozen_range: None,
            freeze_remaining: 0,
            skip_next_measurement: false,
        }
    }
}

impl<K> VirtualScrollState<K>
where
    K: Eq + Hash + Clone,
{
    /// Creates an empty virtual-scroll state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the number of measured item heights currently cached.
    pub fn len(&self) -> usize {
        self.heights.len()
    }

    /// Returns true if no measured heights are cached.
    pub fn is_empty(&self) -> bool {
        self.heights.is_empty()
    }

    /// Returns the cached measured height for `key`.
    pub fn height(&self, key: &K) -> Option<u16> {
        self.heights.get(key).copied()
    }

    /// Inserts or updates a measured height. Returns true when the cache changed.
    pub fn set_height(&mut self, key: K, height: u16) -> bool {
        if self.heights.get(&key).copied() == Some(height) {
            return false;
        }
        self.heights.insert(key, height);
        true
    }

    /// Removes one measured height. Returns true when an entry was present.
    pub fn remove_height(&mut self, key: &K) -> bool {
        self.heights.remove(key).is_some()
    }

    /// Clears all measured heights.
    pub fn clear_heights(&mut self) {
        self.heights.clear();
    }

    /// Retains only measured heights whose keys are still present in `keys`.
    ///
    /// This is the Rust-native counterpart to CC Ink's stale cache GC when the
    /// message key list changes after compaction, clearing, or session reload.
    pub fn retain_keys<'a>(&mut self, keys: impl IntoIterator<Item = &'a K>) -> bool
    where
        K: 'a,
    {
        let live = keys.into_iter().cloned().collect::<HashSet<_>>();
        let before = self.heights.len();
        self.heights.retain(|key, _| live.contains(key));
        before != self.heights.len()
    }

    /// Updates the terminal column count and scales cached heights on change.
    ///
    /// The scale factor is `old_columns / new_columns`, matching CC Ink's
    /// resize heuristic: widening tends to shrink wrapped row heights, while
    /// narrowing tends to grow them. When columns change, the current previous
    /// range is frozen for two planning passes and [`Self::take_skip_measurement`]
    /// returns `true` once so callers can ignore stale pre-resize measurements.
    pub fn set_columns(&mut self, columns: u16) -> bool {
        let columns = columns.max(1);
        let Some(previous) = self.columns else {
            self.columns = Some(columns);
            return false;
        };
        if previous == columns {
            return false;
        }

        let previous_u32 = u32::from(previous.max(1));
        let columns_u32 = u32::from(columns);
        for height in self.heights.values_mut() {
            let scaled = (u32::from(*height)
                .saturating_mul(previous_u32)
                .saturating_add(columns_u32 / 2))
                / columns_u32;
            *height = scaled.clamp(1, u32::from(u16::MAX)) as u16;
        }
        self.columns = Some(columns);
        self.frozen_range = self.previous_range;
        self.freeze_remaining = if self.frozen_range.is_some() { 2 } else { 0 };
        self.skip_next_measurement = true;
        true
    }

    /// Returns whether the next external measurement pass should be skipped.
    ///
    /// The flag is set by [`Self::set_columns`] when cached heights were scaled
    /// and is consumed by this method.
    pub fn take_skip_measurement(&mut self) -> bool {
        let skip = self.skip_next_measurement;
        self.skip_next_measurement = false;
        skip
    }

    /// Returns the previous planned range, if any.
    pub fn previous_range(&self) -> Option<VirtualScrollRange> {
        self.previous_range
    }

    /// Returns how many resize-freeze planning passes remain.
    pub fn freeze_remaining(&self) -> u8 {
        self.freeze_remaining
    }

    /// Clears previous range, previous scroll position, and resize-freeze state.
    pub fn clear_planning_state(&mut self) {
        self.previous_range = None;
        self.previous_scroll_top = None;
        self.frozen_range = None;
        self.freeze_remaining = 0;
        self.skip_next_measurement = false;
    }

    /// Plans a virtual-scroll range for `keys` using cached heights.
    ///
    /// `input.previous_range` and `input.previous_scroll_top` may be supplied
    /// explicitly; when omitted, the state uses the previous values recorded by
    /// earlier calls. The returned range is stored for the next call unless a
    /// resize freeze is active, matching CC Ink's “reuse the pre-resize range
    /// for two renders” behavior.
    pub fn plan(
        &mut self,
        keys: &[K],
        mut input: VirtualScrollInput,
        config: VirtualScrollConfig,
    ) -> VirtualScrollPlan {
        if input.previous_range.is_none() {
            input.previous_range = self.previous_range;
        }
        if input.previous_scroll_top.is_none() {
            input.previous_scroll_top = self.previous_scroll_top;
        }

        let heights = keys
            .iter()
            .map(|key| self.heights.get(key).copied())
            .collect::<Vec<_>>();

        let was_frozen = self.freeze_remaining > 0;
        let plan = if was_frozen {
            let frozen = self.frozen_range.unwrap_or_default();
            plan_virtual_scroll_fixed_range(&heights, input, config, frozen)
        } else {
            plan_virtual_scroll_range(&heights, input, config)
        };

        if was_frozen {
            self.freeze_remaining = self.freeze_remaining.saturating_sub(1);
            if self.freeze_remaining == 0 {
                self.frozen_range = None;
            }
        } else {
            self.previous_range = Some(plan.range);
        }
        if let Some(scroll_top) = input.scroll_top {
            self.previous_scroll_top = Some(scroll_top.max(0));
        }

        plan
    }
}

/// Pure planning result for a virtualized list inside a [`ScrollView`] or [`ScrollBox`](crate::components::ScrollBox).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VirtualScrollPlan {
    /// Half-open item range to render.
    pub range: VirtualScrollRange,
    /// Spacer rows before `range.start`.
    pub top_spacer: i32,
    /// Spacer rows after `range.end`.
    pub bottom_spacer: i32,
    /// Estimated total list height.
    pub total_height: i32,
    /// Target scroll top after adding pending delta.
    pub target_scroll_top: i32,
    /// Clamp minimum to pass to [`ScrollViewHandle::set_clamp_bounds`].
    pub clamp_min: Option<i32>,
    /// Clamp maximum to pass to [`ScrollViewHandle::set_clamp_bounds`]. `None` means unbounded.
    pub clamp_max: Option<i32>,
    /// Quantized snapshot bin for deciding whether a range recompute is needed.
    pub snapshot_bin: i32,
}

/// Returns the CC Ink-style quantized virtual-scroll snapshot bin.
///
/// The bin is computed from `scroll_top + pending_delta`, not just committed
/// `scroll_top`, so a wheel burst can mount the destination range before the
/// visual drain reaches it. Sticky mode bitwise-inverts the bin to make
/// sticky/non-sticky transitions observable even when the numeric offset stays
/// inside the same quantum.
pub fn virtual_scroll_snapshot_bin(
    scroll_top: i32,
    pending_delta: i32,
    is_sticky: bool,
    config: VirtualScrollConfig,
) -> i32 {
    let quantum = i32::from(config.scroll_quantum_rows.max(1));
    let target = scroll_top.saturating_add(pending_delta).max(0);
    let bin = target / quantum;
    if is_sticky {
        !bin
    } else {
        bin
    }
}

/// Plans the mounted range, spacers, and optional clamp bounds for a virtual list.
///
/// `item_heights` contains one entry per item: `Some(rows)` for measured rows
/// and `None` for unmeasured rows. The planner is mode-neutral and does not
/// render, read terminal input, or mutate the scroll handle; callers can feed
/// the returned `range`, `top_spacer`/`bottom_spacer`, and clamp bounds into
/// their own component.
///
/// This captures the reusable parts of CC Ink's `useVirtualScroll`: tail-first
/// cold start, sticky tail planning, range coverage across both committed and
/// pending scroll positions, coarse snapshot quantization, fast-scroll slide
/// caps, and passive clamp bounds that avoid blank spacer during catch-up.
pub fn plan_virtual_scroll_range(
    item_heights: &[Option<u16>],
    input: VirtualScrollInput,
    config: VirtualScrollConfig,
) -> VirtualScrollPlan {
    let n = item_heights.len();
    let offsets = virtual_scroll_offsets(item_heights, config.default_estimate_rows);
    let total_height = offsets.last().copied().unwrap_or(0);
    let viewport = i32::from(input.viewport_height);
    let scroll_top = input.scroll_top.unwrap_or(0).max(0);
    let target_scroll_top = scroll_top.saturating_add(input.pending_delta).max(0);
    let snapshot_bin =
        virtual_scroll_snapshot_bin(scroll_top, input.pending_delta, input.is_sticky, config);

    let mut start;
    let mut end;

    if n == 0 {
        start = 0;
        end = 0;
    } else if input.viewport_height == 0 || input.scroll_top.is_none() {
        start = n.saturating_sub(config.cold_start_count);
        end = n;
    } else if input.is_sticky {
        let budget = viewport.saturating_add(i32::from(config.overscan_rows));
        start = n;
        while start > 0 && total_height.saturating_sub(offsets[start - 1]) < budget {
            start -= 1;
        }
        end = n;
    } else {
        let (span_lo, span_hi) = virtual_scroll_effective_span(scroll_top, input, config);
        let eff_lo = span_lo.saturating_sub(input.list_origin).max(0);
        let eff_hi = span_hi.saturating_sub(input.list_origin);
        let lo = eff_lo.saturating_sub(i32::from(config.overscan_rows));
        start = virtual_scroll_find_start(&offsets, lo);

        let needed = viewport.saturating_add(i32::from(config.overscan_rows).saturating_mul(2));
        let max_end = n.min(start.saturating_add(config.max_mounted_items.max(1)));
        let mut coverage = 0i32;
        end = start;
        while end < max_end
            && (coverage < needed
                || offsets[end]
                    < eff_hi
                        .saturating_add(viewport)
                        .saturating_add(i32::from(config.overscan_rows)))
        {
            coverage = coverage.saturating_add(virtual_scroll_coverage_height(
                item_heights[end],
                config.pessimistic_height_rows,
            ));
            end += 1;
        }
    }

    let needed = viewport.saturating_add(i32::from(config.overscan_rows).saturating_mul(2));
    let min_start = end.saturating_sub(config.max_mounted_items.max(1));
    let mut coverage = virtual_scroll_coverage(item_heights, start, end, config);
    while start > min_start && coverage < needed {
        start -= 1;
        coverage = coverage.saturating_add(virtual_scroll_coverage_height(
            item_heights[start],
            config.pessimistic_height_rows,
        ));
    }

    if let (Some(prev), Some(previous_scroll_top)) =
        (input.previous_range, input.previous_scroll_top)
    {
        let velocity = scroll_top
            .saturating_sub(previous_scroll_top)
            .checked_abs()
            .unwrap_or(i32::MAX)
            .saturating_add(input.pending_delta.checked_abs().unwrap_or(i32::MAX));
        if viewport > 0 && velocity > viewport.saturating_mul(2) {
            let slide = config.slide_step_items.max(1);
            if start.saturating_add(slide) < prev.start {
                start = prev.start.saturating_sub(slide);
            }
            if end > prev.end.saturating_add(slide) {
                end = n.min(prev.end.saturating_add(slide));
            }
            if start > end {
                end = n.min(start.saturating_add(slide));
            }
        }
    }

    let max_mounted = config.max_mounted_items.max(1);
    if end.saturating_sub(start) > max_mounted {
        let mid = offsets[start]
            .saturating_add(offsets[end])
            .saturating_div(2);
        if scroll_top.saturating_sub(input.list_origin) < mid {
            end = n.min(start.saturating_add(max_mounted));
        } else {
            start = end.saturating_sub(max_mounted);
        }
    }

    start = start.min(n);
    end = end.min(n).max(start);

    build_virtual_scroll_plan_from_range(
        item_heights,
        input,
        VirtualScrollRange { start, end },
        offsets,
        total_height,
        target_scroll_top,
        snapshot_bin,
    )
}

fn plan_virtual_scroll_fixed_range(
    item_heights: &[Option<u16>],
    input: VirtualScrollInput,
    config: VirtualScrollConfig,
    range: VirtualScrollRange,
) -> VirtualScrollPlan {
    let offsets = virtual_scroll_offsets(item_heights, config.default_estimate_rows);
    let total_height = offsets.last().copied().unwrap_or(0);
    let scroll_top = input.scroll_top.unwrap_or(0).max(0);
    let target_scroll_top = scroll_top.saturating_add(input.pending_delta).max(0);
    let snapshot_bin =
        virtual_scroll_snapshot_bin(scroll_top, input.pending_delta, input.is_sticky, config);
    build_virtual_scroll_plan_from_range(
        item_heights,
        input,
        range,
        offsets,
        total_height,
        target_scroll_top,
        snapshot_bin,
    )
}

fn build_virtual_scroll_plan_from_range(
    item_heights: &[Option<u16>],
    input: VirtualScrollInput,
    range: VirtualScrollRange,
    offsets: Vec<i32>,
    total_height: i32,
    target_scroll_top: i32,
    snapshot_bin: i32,
) -> VirtualScrollPlan {
    let n = item_heights.len();
    let start = range.start.min(n);
    let end = range.end.min(n).max(start);
    let viewport = i32::from(input.viewport_height);
    let top_spacer = offsets[start];
    let bottom_spacer = total_height.saturating_sub(offsets[end]);
    let (clamp_min, clamp_max) = if input.is_sticky {
        (None, None)
    } else {
        let min = if start == 0 {
            0
        } else {
            top_spacer.saturating_add(input.list_origin)
        };
        let max = if end == n {
            None
        } else {
            Some(
                top_spacer
                    .max(offsets[end].saturating_sub(viewport))
                    .saturating_add(input.list_origin),
            )
        };
        (Some(min), max)
    };

    VirtualScrollPlan {
        range: VirtualScrollRange { start, end },
        top_spacer,
        bottom_spacer,
        total_height,
        target_scroll_top,
        clamp_min,
        clamp_max,
        snapshot_bin,
    }
}

fn virtual_scroll_offsets(item_heights: &[Option<u16>], default_estimate_rows: u16) -> Vec<i32> {
    let mut offsets = Vec::with_capacity(item_heights.len() + 1);
    offsets.push(0);
    let default_estimate = i32::from(default_estimate_rows.max(1));
    let mut total = 0i32;
    for height in item_heights {
        total = total.saturating_add(height.map(i32::from).unwrap_or(default_estimate));
        offsets.push(total);
    }
    offsets
}

fn virtual_scroll_effective_span(
    scroll_top: i32,
    input: VirtualScrollInput,
    config: VirtualScrollConfig,
) -> (i32, i32) {
    let target = scroll_top.saturating_add(input.pending_delta);
    let raw_lo = scroll_top.min(target).max(0);
    let raw_hi = scroll_top.max(target).max(0);
    let span = raw_hi.saturating_sub(raw_lo);
    let max_span = i32::from(input.viewport_height)
        .saturating_mul(i32::from(config.max_span_viewports.max(1)));
    if span > max_span {
        if input.pending_delta < 0 {
            (raw_hi.saturating_sub(max_span), raw_hi)
        } else {
            (raw_lo, raw_lo.saturating_add(max_span))
        }
    } else {
        (raw_lo, raw_hi)
    }
}

fn virtual_scroll_find_start(offsets: &[i32], lo: i32) -> usize {
    let n = offsets.len().saturating_sub(1);
    let mut left = 0usize;
    let mut right = n;
    while left < right {
        let mid = (left + right) >> 1;
        if offsets[mid + 1] <= lo {
            left = mid + 1;
        } else {
            right = mid;
        }
    }
    left
}

fn virtual_scroll_coverage(
    item_heights: &[Option<u16>],
    start: usize,
    end: usize,
    config: VirtualScrollConfig,
) -> i32 {
    let mut coverage = 0i32;
    for height in &item_heights[start.min(item_heights.len())..end.min(item_heights.len())] {
        coverage = coverage.saturating_add(virtual_scroll_coverage_height(
            *height,
            config.pessimistic_height_rows,
        ));
    }
    coverage
}

fn virtual_scroll_coverage_height(height: Option<u16>, pessimistic_height_rows: u16) -> i32 {
    height
        .map(i32::from)
        .unwrap_or_else(|| i32::from(pessimistic_height_rows.max(1)))
}

const DEFAULT_SCROLL_STEP: u16 = 3;
const SELECTION_AUTOSCROLL_LINES: i32 = crate::SELECTION_AUTOSCROLL_LINES as i32;
const SELECTION_AUTOSCROLL_INTERVAL: Duration =
    Duration::from_millis(crate::SELECTION_AUTOSCROLL_INTERVAL_MS);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ModalPagerAction {
    LineUp,
    LineDown,
    HalfPageUp,
    HalfPageDown,
    FullPageUp,
    FullPageDown,
    Top,
    Bottom,
}

fn scroll_key_delta(event: &KeyEvent, viewport_height: u16, modal_pager_keys: bool) -> Option<i32> {
    if modal_pager_keys {
        if let Some(action) = modal_pager_action(event) {
            return Some(match action {
                ModalPagerAction::LineUp => -1,
                ModalPagerAction::LineDown => 1,
                ModalPagerAction::HalfPageUp => -((viewport_height as i32 / 2).max(1)),
                ModalPagerAction::HalfPageDown => (viewport_height as i32 / 2).max(1),
                ModalPagerAction::FullPageUp => -(viewport_height as i32).max(1),
                ModalPagerAction::FullPageDown => (viewport_height as i32).max(1),
                ModalPagerAction::Top => i32::MIN / 2,
                ModalPagerAction::Bottom => i32::MAX / 2,
            });
        }
    }

    match event.code {
        KeyCode::Up => Some(-1),
        KeyCode::Down => Some(1),
        KeyCode::PageUp => Some(-(viewport_height as i32).max(1)),
        KeyCode::PageDown => Some((viewport_height as i32).max(1)),
        KeyCode::Home => Some(i32::MIN / 2),
        KeyCode::End => Some(i32::MAX / 2),
        _ => None,
    }
}

fn modal_pager_action(event: &KeyEvent) -> Option<ModalPagerAction> {
    if event
        .modifiers
        .intersects(KeyModifiers::ALT | KeyModifiers::META)
    {
        return None;
    }

    let ctrl = event.modifiers.contains(KeyModifiers::CONTROL);
    let shift = event.modifiers.contains(KeyModifiers::SHIFT);

    if !ctrl && !shift {
        match event.code {
            KeyCode::Up => return Some(ModalPagerAction::LineUp),
            KeyCode::Down => return Some(ModalPagerAction::LineDown),
            KeyCode::Home => return Some(ModalPagerAction::Top),
            KeyCode::End => return Some(ModalPagerAction::Bottom),
            _ => {}
        }
    }

    if ctrl {
        if shift {
            return None;
        }
        return match event.code {
            KeyCode::Char('u') | KeyCode::Char('U') => Some(ModalPagerAction::HalfPageUp),
            KeyCode::Char('d') | KeyCode::Char('D') => Some(ModalPagerAction::HalfPageDown),
            KeyCode::Char('b') | KeyCode::Char('B') => Some(ModalPagerAction::FullPageUp),
            KeyCode::Char('f') | KeyCode::Char('F') => Some(ModalPagerAction::FullPageDown),
            KeyCode::Char('n') | KeyCode::Char('N') => Some(ModalPagerAction::LineDown),
            KeyCode::Char('p') | KeyCode::Char('P') => Some(ModalPagerAction::LineUp),
            _ => None,
        };
    }

    match event.code {
        KeyCode::Char('G') => Some(ModalPagerAction::Bottom),
        KeyCode::Char('g') if shift => Some(ModalPagerAction::Bottom),
        KeyCode::Char(_) if shift => None,
        KeyCode::Char('g') => Some(ModalPagerAction::Top),
        KeyCode::Char('j') => Some(ModalPagerAction::LineDown),
        KeyCode::Char('k') => Some(ModalPagerAction::LineUp),
        KeyCode::Char(' ') => Some(ModalPagerAction::FullPageDown),
        KeyCode::Char('b') => Some(ModalPagerAction::FullPageUp),
        _ => None,
    }
}

const WHEEL_ACCEL_WINDOW_MS: f64 = 40.0;
const WHEEL_ACCEL_STEP: f64 = 0.3;
const WHEEL_ACCEL_MAX: f64 = 6.0;
const WHEEL_BOUNCE_GAP_MAX_MS: f64 = 200.0;
const WHEEL_MODE_STEP: f64 = 15.0;
const WHEEL_MODE_CAP: f64 = 15.0;
const WHEEL_MODE_RAMP: f64 = 3.0;
const WHEEL_MODE_IDLE_DISENGAGE_MS: f64 = 1500.0;
const WHEEL_DECAY_HALFLIFE_MS: f64 = 150.0;
const WHEEL_DECAY_STEP: f64 = 5.0;
const WHEEL_BURST_MS: f64 = 5.0;
const WHEEL_DECAY_GAP_MS: f64 = 80.0;
const WHEEL_DECAY_CAP_SLOW: f64 = 3.0;
const WHEEL_DECAY_CAP_FAST: f64 = 6.0;
const WHEEL_DECAY_IDLE_MS: f64 = 500.0;

const SCROLL_DRAIN_MIN_PER_FRAME: i32 = 4;
const SCROLL_DRAIN_XTERM_INSTANT_THRESHOLD: i32 = 5;
const SCROLL_DRAIN_XTERM_HIGH_PENDING: i32 = 12;
const SCROLL_DRAIN_XTERM_STEP_MED: i32 = 2;
const SCROLL_DRAIN_XTERM_STEP_HIGH: i32 = 3;
const SCROLL_DRAIN_XTERM_MAX_PENDING: i32 = 30;

/// Host strategy for CC Ink-style render-time scroll draining.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScrollDrainMode {
    /// Native terminals such as iTerm2/Ghostty/WezTerm where wheel events can be bursty.
    Native,
    /// xterm.js-based terminals such as VS Code/Cursor integrated terminals.
    XtermJs,
}

impl ScrollDrainMode {
    /// Selects the drain strategy for the current terminal host.
    ///
    /// Mirrors CC Ink's `isXtermJsHost()` branch in `render-node-to-output.ts`:
    /// xterm.js hosts such as VS Code/Cursor use the adaptive small-step drain,
    /// while native terminals use proportional burst draining. The result is
    /// only a strategy choice; callers still opt into draining explicitly via
    /// [`ScrollViewProps::scroll_drain_mode`] or wrapper components.
    pub fn for_current_terminal() -> Self {
        scroll_drain_mode_for_xterm_js_host(crate::terminal::is_xterm_js())
    }
}

fn scroll_drain_mode_for_xterm_js_host(xterm_js: bool) -> ScrollDrainMode {
    if xterm_js {
        ScrollDrainMode::XtermJs
    } else {
        ScrollDrainMode::Native
    }
}

/// Result of applying one CC Ink-style scroll-drain frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScrollDrainResult {
    /// Rows to apply to the visible scroll offset this frame.
    pub applied: i32,
    /// Remaining pending scroll delta after this frame.
    pub remaining: i32,
}

/// Stateful opt-in owner for render-time scroll draining.
///
/// CC Ink stores `pendingScrollDelta` on each `ScrollBox` DOM node and drains it
/// over subsequent render frames. iocraft keeps the same behavior available as
/// an explicit Rust value instead of hidden renderer-global node state. Custom
/// scroll containers can add wheel/key deltas, call [`Self::drain_frame`] while
/// rendering, and decide themselves whether/how to wake another frame when
/// [`Self::has_pending_delta`] remains true.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScrollDrainState {
    mode: ScrollDrainMode,
    pending: i32,
}

impl ScrollDrainState {
    /// Creates a drain state with no pending delta.
    pub fn new(mode: ScrollDrainMode) -> Self {
        Self { mode, pending: 0 }
    }

    /// Creates a drain state with an existing pending delta.
    pub fn with_pending(mode: ScrollDrainMode, pending: i32) -> Self {
        Self { mode, pending }
    }

    /// Returns the host strategy used for subsequent drain frames.
    pub fn mode(&self) -> ScrollDrainMode {
        self.mode
    }

    /// Changes the host strategy without changing the pending delta.
    pub fn set_mode(&mut self, mode: ScrollDrainMode) {
        self.mode = mode;
    }

    /// Returns the pending scroll delta waiting for future frames.
    pub fn pending_delta(&self) -> i32 {
        self.pending
    }

    /// Returns whether another drain frame is needed.
    pub fn has_pending_delta(&self) -> bool {
        self.pending != 0
    }

    /// Replaces the pending delta.
    pub fn set_pending_delta(&mut self, pending: i32) {
        self.pending = pending;
    }

    /// Adds a new delta to the pending accumulator.
    ///
    /// This uses saturating arithmetic so a burst of input cannot overflow the
    /// accumulator in debug or release builds.
    pub fn add_delta(&mut self, delta: i32) {
        self.pending = self.pending.saturating_add(delta);
    }

    /// Clears any pending delta.
    pub fn clear(&mut self) {
        self.pending = 0;
    }

    /// Applies one render-frame drain step and updates the pending accumulator.
    pub fn drain_frame(&mut self, viewport_height: u16) -> ScrollDrainResult {
        let result = drain_scroll_delta(self.mode, self.pending, viewport_height);
        self.pending = result.remaining;
        result
    }
}

/// Input for [`plan_scroll_box_render_offset`].
///
/// This is a pure, Rust-native description of CC Ink's render-time ScrollBox
/// offset logic in `render-node-to-output.ts`: optional one-shot anchor seek,
/// at-bottom follow, pending-scroll drain, scroll-range clamp, and virtual
/// mounted-range clamp.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScrollBoxRenderOffsetInput {
    /// Current committed scroll offset before this render pass.
    pub current_scroll_top: i32,
    /// Previous measured scroll content height.
    pub previous_scroll_height: i32,
    /// Current measured scroll content height.
    pub scroll_height: i32,
    /// Current viewport height in rows.
    pub viewport_height: i32,
    /// Whether the scroll container is sticky/pinned to bottom.
    pub sticky: bool,
    /// Pending scroll delta waiting to be drained. `None` and `Some(0)` both
    /// mean no remaining pending work in the returned plan.
    pub pending_delta: Option<i32>,
    /// One-shot content-local anchor top row, if a caller requested
    /// `scroll_to_element`-style behavior.
    pub anchor_top: Option<i32>,
    /// Additional offset applied to [`Self::anchor_top`].
    pub anchor_offset: i32,
    /// Optional virtual-scroll lower clamp bound.
    pub clamp_min: Option<i32>,
    /// Optional virtual-scroll upper clamp bound.
    pub clamp_max: Option<i32>,
    /// Drain strategy to use when [`Self::pending_delta`] is non-zero.
    /// `None` applies the pending delta eagerly.
    pub drain_mode: Option<ScrollDrainMode>,
}

/// Plan returned by [`plan_scroll_box_render_offset`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScrollBoxRenderOffsetPlan {
    /// Visual scroll offset to render this frame after the optional mounted
    /// virtual-scroll clamp is applied.
    pub scroll_top: i32,
    /// Committed logical scroll offset after anchor/follow/drain/range clamp,
    /// before the mounted virtual-scroll clamp. This mirrors CC Ink's
    /// `node.scrollTop`: it may run ahead of the currently mounted range so a
    /// virtual list can catch up on the next commit.
    pub committed_scroll_top: i32,
    /// Remaining pending delta after this frame.
    pub pending_delta: Option<i32>,
    /// Whether the container should be considered sticky after planning.
    pub sticky: bool,
    /// Whether a one-shot anchor was consumed.
    pub anchor_consumed: bool,
    /// Whether the at-bottom follow rule pinned to the current max scroll.
    pub followed_bottom: bool,
    /// Rows applied from pending delta this frame.
    pub drained_delta: i32,
    /// Whether the committed offset was clamped to `[0, max_scroll]`.
    pub clamped_to_scroll_range: bool,
    /// Whether the visual offset was clamped to the mounted virtual-scroll range.
    pub clamped_to_mounted_range: bool,
}

/// Plans CC Ink-style render-time ScrollBox offset updates.
///
/// This helper is mode-neutral and side-effect free. It exists for custom
/// virtual/retained scroll containers that want the same ordering CC Ink uses:
///
/// 1. one-shot `scrollToElement` anchor seek consumes pending drain;
/// 2. sticky/positional bottom-follow consumes non-negative pending drain;
/// 3. pending delta drains using native/xterm.js strategy;
/// 4. the committed offset is clamped to the real scroll range;
/// 5. the visual offset is passively clamped to the mounted virtual range.
///
/// Matching CC Ink, the mounted-range clamp does **not** rewrite the committed
/// offset and does **not** consume remaining drain; this lets virtualized lists
/// keep a far-ahead target while rendering at the mounted edge until the next
/// commit widens the clamp.
///
/// It does not mutate a component, wake a render loop, emit DECSTBM, or change
/// terminal mode.
pub fn plan_scroll_box_render_offset(
    input: ScrollBoxRenderOffsetInput,
) -> ScrollBoxRenderOffsetPlan {
    let viewport_height = input.viewport_height.max(0);
    let previous_max_scroll = (input.previous_scroll_height - viewport_height).max(0);
    let max_scroll = (input.scroll_height - viewport_height).max(0);
    let mut scroll_top = input.current_scroll_top.max(0);
    let mut pending_delta = input.pending_delta.filter(|delta| *delta != 0);
    let mut sticky = input.sticky;
    let mut anchor_consumed = false;
    let mut followed_bottom = false;
    let mut drained_delta = 0;
    let mut clamped_to_scroll_range = false;
    let mut clamped_to_mounted_range = false;

    if let Some(anchor_top) = input.anchor_top {
        scroll_top = anchor_top.saturating_add(input.anchor_offset).max(0);
        pending_delta = None;
        sticky = false;
        anchor_consumed = true;
    }

    let grew = input.scroll_height >= input.previous_scroll_height;
    let pending_non_negative = pending_delta.unwrap_or(0) >= 0;
    let at_bottom = sticky || (grew && scroll_top >= previous_max_scroll);
    if at_bottom && pending_non_negative {
        scroll_top = max_scroll;
        pending_delta = None;
        sticky = true;
        followed_bottom = true;
    }

    if let Some(pending) = pending_delta {
        if pending != 0 {
            let result = if let Some(mode) = input.drain_mode {
                let past_mounted_clamp =
                    if let (Some(min), Some(max)) = (input.clamp_min, input.clamp_max) {
                        let lower = min.min(max);
                        let upper = min.max(max);
                        (pending < 0 && scroll_top < lower) || (pending > 0 && scroll_top > upper)
                    } else {
                        false
                    };
                let effective_height = if past_mounted_clamp {
                    // Mirrors CC Ink's past-clamp throttle:
                    // `Math.min(4, innerHeight >> 3)`. `drain_scroll_delta`
                    // itself still enforces a minimum hardware-scroll cap of 1.
                    (viewport_height >> 3).min(4)
                } else {
                    viewport_height
                };
                drain_scroll_delta(mode, pending, effective_height.min(u16::MAX as i32) as u16)
            } else {
                ScrollDrainResult {
                    applied: pending,
                    remaining: 0,
                }
            };
            drained_delta = result.applied;
            scroll_top = scroll_top.saturating_add(result.applied);
            pending_delta = (result.remaining != 0).then_some(result.remaining);
        }
    }

    let range_clamped = scroll_top.clamp(0, max_scroll);
    if range_clamped != scroll_top {
        scroll_top = range_clamped;
        pending_delta = None;
        clamped_to_scroll_range = true;
    }

    let committed_scroll_top = scroll_top;

    if let (Some(min), Some(max)) = (input.clamp_min, input.clamp_max) {
        let lower = min.clamp(0, max_scroll);
        let upper = max.clamp(lower, max_scroll);
        let mounted_clamped = scroll_top.clamp(lower, upper);
        if mounted_clamped != scroll_top {
            scroll_top = mounted_clamped;
            clamped_to_mounted_range = true;
        }
    }

    ScrollBoxRenderOffsetPlan {
        scroll_top,
        committed_scroll_top,
        pending_delta: pending_delta.filter(|delta| *delta != 0),
        sticky,
        anchor_consumed,
        followed_bottom,
        drained_delta,
        clamped_to_scroll_range,
        clamped_to_mounted_range,
    }
}

/// Computes one render-frame drain step for a pending scroll delta.
///
/// This mirrors CC Ink's `render-node-to-output.ts` `drainProportional(...)`
/// and `drainAdaptive(...)` helpers. It is a mode-neutral utility for custom
/// scroll containers: it does not mutate a component, wake a render loop, emit
/// DECSTBM, or change terminal mode. `viewport_height` is the scroll viewport's
/// inner height in rows; the applied step is capped to `viewport_height - 1` so
/// hardware scroll hints can still repaint edge rows.
pub fn drain_scroll_delta(
    mode: ScrollDrainMode,
    pending: i32,
    viewport_height: u16,
) -> ScrollDrainResult {
    if pending == 0 {
        return ScrollDrainResult::default();
    }

    match mode {
        ScrollDrainMode::Native => drain_scroll_delta_native(pending, viewport_height),
        ScrollDrainMode::XtermJs => drain_scroll_delta_xterm_js(pending, viewport_height),
    }
}

fn scroll_drain_cap(viewport_height: u16) -> i32 {
    i32::from(viewport_height).saturating_sub(1).max(1)
}

fn drain_scroll_delta_native(pending: i32, viewport_height: u16) -> ScrollDrainResult {
    let abs = pending.checked_abs().unwrap_or(i32::MAX);
    let cap = scroll_drain_cap(viewport_height);
    let proportional = ((i64::from(abs) * 3) >> 2).min(i64::from(i32::MAX)) as i32;
    let step = cap.min(SCROLL_DRAIN_MIN_PER_FRAME.max(proportional));
    if abs <= step {
        return ScrollDrainResult {
            applied: pending,
            remaining: 0,
        };
    }

    let applied = pending.signum() * step;
    ScrollDrainResult {
        applied,
        remaining: pending - applied,
    }
}

fn drain_scroll_delta_xterm_js(pending: i32, viewport_height: u16) -> ScrollDrainResult {
    let sign = pending.signum();
    let mut abs = pending.checked_abs().unwrap_or(i32::MAX);
    let mut applied = 0;

    if abs > SCROLL_DRAIN_XTERM_MAX_PENDING {
        applied += sign * (abs - SCROLL_DRAIN_XTERM_MAX_PENDING);
        abs = SCROLL_DRAIN_XTERM_MAX_PENDING;
    }

    let step = if abs <= SCROLL_DRAIN_XTERM_INSTANT_THRESHOLD {
        abs
    } else if abs < SCROLL_DRAIN_XTERM_HIGH_PENDING {
        SCROLL_DRAIN_XTERM_STEP_MED
    } else {
        SCROLL_DRAIN_XTERM_STEP_HIGH
    };
    applied += sign * step;

    let rem = abs - step;
    let cap = scroll_drain_cap(viewport_height);
    let total_abs = applied.abs();
    if total_abs > cap {
        let excess = total_abs - cap;
        return ScrollDrainResult {
            applied: sign * cap,
            remaining: sign * (rem + excess),
        };
    }

    ScrollDrainResult {
        applied,
        remaining: if rem > 0 { sign * rem } else { 0 },
    }
}

#[derive(Clone, Debug)]
struct WheelAccelState {
    start: Instant,
    time_ms: f64,
    mult: f64,
    dir: i8,
    xterm_js: Option<bool>,
    frac: f64,
    base: f64,
    pending_flip: bool,
    wheel_mode: bool,
    burst_count: u16,
}

impl WheelAccelState {
    fn from_env() -> Self {
        let base = std::env::var("CLAUDE_CODE_SCROLL_SPEED")
            .ok()
            .and_then(|raw| raw.parse::<f64>().ok())
            .filter(|value| *value > 0.0)
            .map(|value| value.min(20.0))
            .unwrap_or(1.0);
        Self::new_auto(base)
    }

    #[cfg(test)]
    fn new(xterm_js: bool, base: f64) -> Self {
        Self::new_inner(Some(xterm_js), base)
    }

    fn new_auto(base: f64) -> Self {
        Self::new_inner(None, base)
    }

    fn new_inner(xterm_js: Option<bool>, base: f64) -> Self {
        let base = if base.is_finite() && base > 0.0 {
            base.min(20.0)
        } else {
            1.0
        };
        Self {
            start: Instant::now(),
            time_ms: 0.0,
            mult: base,
            dir: 0,
            xterm_js,
            frac: 0.0,
            base,
            pending_flip: false,
            wheel_mode: false,
            burst_count: 0,
        }
    }

    fn compute_now(&mut self, dir: i8) -> i32 {
        self.compute(dir, self.start.elapsed().as_secs_f64() * 1000.0)
    }

    fn compute(&mut self, dir: i8, now_ms: f64) -> i32 {
        debug_assert!(dir == 1 || dir == -1);
        // Match CC Ink's lazy xterm.js wheel detection: XTVERSION probing is
        // asynchronous, so defer the env/probe read until the first real wheel
        // event instead of freezing a pre-probe value at component mount.
        let xterm_js = match self.xterm_js {
            Some(value) => value,
            None => {
                let value = crate::terminal::is_xterm_js();
                self.xterm_js = Some(value);
                value
            }
        };
        if !xterm_js {
            if self.wheel_mode && now_ms - self.time_ms > WHEEL_MODE_IDLE_DISENGAGE_MS {
                self.wheel_mode = false;
                self.burst_count = 0;
                self.mult = self.base;
            }

            if self.pending_flip {
                self.pending_flip = false;
                if dir != self.dir || now_ms - self.time_ms > WHEEL_BOUNCE_GAP_MAX_MS {
                    self.dir = dir;
                    self.time_ms = now_ms;
                    self.mult = self.base;
                    return self.mult.floor() as i32;
                }
                self.wheel_mode = true;
            }

            let gap = now_ms - self.time_ms;
            if dir != self.dir && self.dir != 0 {
                self.pending_flip = true;
                self.time_ms = now_ms;
                return 0;
            }
            self.dir = dir;
            self.time_ms = now_ms;

            if self.wheel_mode {
                if gap < WHEEL_BURST_MS {
                    self.burst_count += 1;
                    if self.burst_count >= 5 {
                        self.wheel_mode = false;
                        self.burst_count = 0;
                        self.mult = self.base;
                    } else {
                        return 1;
                    }
                } else {
                    self.burst_count = 0;
                }
            }

            if self.wheel_mode {
                let m = 0.5_f64.powf(gap / WHEEL_DECAY_HALFLIFE_MS);
                let cap = WHEEL_MODE_CAP.max(self.base * 2.0);
                let next = 1.0 + (self.mult - 1.0) * m + WHEEL_MODE_STEP * m;
                self.mult = cap.min(next).min(self.mult + WHEEL_MODE_RAMP);
                return self.mult.floor() as i32;
            }

            if gap > WHEEL_ACCEL_WINDOW_MS {
                self.mult = self.base;
            } else {
                let cap = WHEEL_ACCEL_MAX.max(self.base * 2.0);
                self.mult = cap.min(self.mult + WHEEL_ACCEL_STEP);
            }
            return self.mult.floor() as i32;
        }

        let gap = now_ms - self.time_ms;
        let same_dir = dir == self.dir;
        self.time_ms = now_ms;
        self.dir = dir;
        if same_dir && gap < WHEEL_BURST_MS {
            return 1;
        }
        if !same_dir || gap > WHEEL_DECAY_IDLE_MS {
            self.mult = 2.0;
            self.frac = 0.0;
        } else {
            let m = 0.5_f64.powf(gap / WHEEL_DECAY_HALFLIFE_MS);
            let cap = if gap >= WHEEL_DECAY_GAP_MS {
                WHEEL_DECAY_CAP_SLOW
            } else {
                WHEEL_DECAY_CAP_FAST
            };
            self.mult = cap.min(1.0 + (self.mult - 1.0) * m + WHEEL_DECAY_STEP * m);
        }
        let total = self.mult + self.frac;
        let rows = total.floor() as i32;
        self.frac = total - rows as f64;
        rows
    }
}

// -- Scrollbar component --

#[derive(Default, Props)]
struct ScrollViewScrollbarProps {
    viewport_height: u16,
    content_height: u16,
    scroll_offset: i32,
    thumb_color: Option<Color>,
    track_color: Option<Color>,
}

#[derive(Default)]
struct ScrollViewScrollbar {
    viewport_height: u16,
    content_height: u16,
    scroll_offset: i32,
    thumb_color: Option<Color>,
    track_color: Option<Color>,
}

impl Component for ScrollViewScrollbar {
    type Props<'a> = ScrollViewScrollbarProps;

    fn new(_props: &Self::Props<'_>) -> Self {
        Self::default()
    }

    fn update(
        &mut self,
        props: &mut Self::Props<'_>,
        _hooks: Hooks,
        updater: &mut ComponentUpdater,
    ) {
        self.viewport_height = props.viewport_height;
        self.content_height = props.content_height;
        self.scroll_offset = props.scroll_offset;
        self.thumb_color = props.thumb_color;
        self.track_color = props.track_color;

        updater.set_layout_style(taffy::style::Style {
            size: taffy::geometry::Size {
                width: taffy::style::Dimension::length(1.0),
                height: taffy::style::Dimension::percent(1.0),
            },
            ..Default::default()
        });
    }

    fn draw(&mut self, drawer: &mut ComponentDrawer<'_>) {
        if drawer.zero_height_sibling_shares_y() {
            return;
        }
        let vh = self.viewport_height as usize;
        let ch = self.content_height as usize;
        if vh == 0 || ch <= vh {
            return;
        }

        let thumb_size = (vh * vh / ch).max(1);
        let max_off = (ch - vh) as i32;
        let thumb_pos = if max_off > 0 {
            (self.scroll_offset as usize * (vh - thumb_size)) / max_off as usize
        } else {
            0
        };

        let thumb_color = self.thumb_color.unwrap_or(Color::White);
        let track_color = self.track_color.unwrap_or(Color::DarkGrey);
        let track_style = CanvasTextStyle {
            color: Some(track_color),
            ..Default::default()
        };
        let thumb_style = CanvasTextStyle {
            color: Some(thumb_color),
            ..Default::default()
        };

        let mut canvas = drawer.canvas();
        for y in 0..vh {
            if y >= thumb_pos && y < thumb_pos + thumb_size {
                canvas.set_text(0, y as isize, "\u{2503}", thumb_style); // ┃
            } else {
                canvas.set_text(0, y as isize, "\u{2502}", track_style); // │
            }
        }
    }
}

/// The props which can be passed to the [`ScrollView`] component.
#[non_exhaustive]
#[derive(Default, Props)]
pub struct ScrollViewProps<'a> {
    /// The children to render inside the scroll view.
    pub children: Vec<AnyElement<'a>>,
    /// When true, the scroll view stays pinned to the bottom as content grows.
    /// Scrolling up disengages auto scroll; reaching the bottom re-engages it.
    pub auto_scroll: bool,
    /// Number of lines to scroll per mouse wheel tick. Defaults to 3.
    /// When [`Self::wheel_acceleration`] is enabled, this is used only as the
    /// non-accelerated fallback; accelerated mode starts from the
    /// `CLAUDE_CODE_SCROLL_SPEED` baseline to match the CC Ink fork.
    pub scroll_step: Option<u16>,
    /// Enables CC Ink-style mouse wheel acceleration. Defaults to `false`; turn
    /// it on for transcript/copy-mode scroll boxes where wheel bursts should
    /// ramp while slow precision scrolls remain small.
    pub wheel_acceleration: Option<bool>,
    /// An optional handle which can be used for imperative control of the scroll view.
    pub handle: Option<Ref<ScrollViewHandle>>,
    /// Whether to show a scrollbar. Defaults to `true`.
    pub scrollbar: Option<bool>,
    /// Optional color for the scrollbar thumb. Defaults to `White`.
    pub scrollbar_thumb_color: Option<Color>,
    /// Optional color for the scrollbar track. Defaults to `DarkGrey`.
    pub scrollbar_track_color: Option<Color>,
    /// Whether keyboard events (arrow keys, Page Up/Down, Home/End) scroll
    /// the view. Defaults to `true`. The terminal events hook is always
    /// registered to maintain consistent hook ordering.
    pub keyboard_scroll: Option<bool>,
    /// Enables CC Ink transcript/modal pager keys in addition to the default
    /// scroll keys: `j`/`k`, Space/`b`, `g`/`G`, and Ctrl+U/D/B/F/N/P.
    /// Defaults to `false` because these printable keys are only safe when no
    /// text input is competing for them.
    pub modal_pager_keys: Option<bool>,
    /// Optional fullscreen selection context to keep text selections anchored
    /// while keyboard scroll jumps move the viewport. Wheel scrolling clears
    /// selection, matching CC Ink's ScrollKeybindingHandler behavior.
    pub selection: Option<SelectionContext>,
    /// Opt-in CC Ink-style wheel scroll draining mode.
    ///
    /// When set, mouse wheel deltas are accumulated and applied over subsequent
    /// animation frames using [`drain_scroll_delta`] instead of jumping all rows
    /// immediately. The default `None` preserves iocraft's eager, predictable
    /// `ScrollView` behavior.
    pub scroll_drain_mode: Option<ScrollDrainMode>,
}

// Hook that measures the component height in pre_component_draw and writes
// the result to a State<u16>.
struct MeasureHeightHook {
    out: State<u16>,
    out_top: State<u16>,
}

impl Hook for MeasureHeightHook {
    fn pre_component_draw(&mut self, drawer: &mut ComponentDrawer) {
        let h = drawer.size().height;
        if self.out.try_get() != Some(h) {
            self.out.set(h);
        }
        let top = drawer.canvas_position().y.max(0) as u16;
        if self.out_top.try_get() != Some(top) {
            self.out_top.set(top);
        }
    }
}

#[derive(Default)]
struct ScrollHintHook {
    scroll_offset: i32,
    enabled: bool,
    prev_scroll_offset: Option<i32>,
    prev_top: Option<i16>,
    prev_height: Option<u16>,
}

impl Hook for ScrollHintHook {
    fn pre_component_draw(&mut self, drawer: &mut ComponentDrawer) {
        let pos = drawer.canvas_position();
        let size = drawer.size();
        if self.enabled {
            if let (Some(prev_offset), Some(prev_top), Some(prev_height)) =
                (self.prev_scroll_offset, self.prev_top, self.prev_height)
            {
                let delta = self.scroll_offset - prev_offset;
                let same_region = prev_top == pos.y && prev_height == size.height;
                if same_region && delta != 0 && delta.unsigned_abs() < size.height as u32 {
                    drawer.set_scroll_hint(delta);
                }
            }
        }
        self.prev_scroll_offset = Some(self.scroll_offset);
        self.prev_top = Some(pos.y);
        self.prev_height = Some(size.height);
    }
}

#[derive(Default)]
struct SelectionScreenSnapshotHook {
    canvas: Arc<Mutex<Option<Canvas>>>,
    enabled: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct DragAutoscrollTickState {
    focus: Option<(usize, usize)>,
    ticks: usize,
    capped: bool,
    blocked_focus: Option<(usize, usize)>,
}

fn reset_drag_autoscroll_ticks_for_render(
    mut state: DragAutoscrollTickState,
    active: bool,
    focus: Option<(usize, usize)>,
) -> DragAutoscrollTickState {
    if !active {
        return DragAutoscrollTickState::default();
    }
    if state.focus != focus {
        state = DragAutoscrollTickState {
            focus,
            ticks: 0,
            capped: false,
            blocked_focus: None,
        };
    }
    state
}

fn allow_drag_autoscroll_tick(state: &mut DragAutoscrollTickState) -> bool {
    if state.capped {
        return false;
    }
    state.ticks = state.ticks.saturating_add(1);
    if state.ticks > crate::SELECTION_AUTOSCROLL_MAX_TICKS {
        state.capped = true;
        return false;
    }
    true
}

impl Hook for SelectionScreenSnapshotHook {
    fn post_component_draw(&mut self, drawer: &mut ComponentDrawer) {
        let mut canvas = self
            .canvas
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if self.enabled {
            *canvas = Some(drawer.root_canvas_mut().clone());
        } else {
            *canvas = None;
        }
    }
}

/// `ScrollView` is a component that provides scrollable content with keyboard and mouse support.
///
/// Place it inside a container with a fixed height. The scroll view will clip its children and
/// allow scrolling through them using arrow keys, Page Up/Down, Home/End, and mouse wheel.
///
/// # Example
///
/// ```
/// # use iocraft::prelude::*;
/// # #[component]
/// # fn MyComponent(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
/// element! {
///     View(width: 80, height: 20, border_style: BorderStyle::Round) {
///         ScrollView {
///             Text(content: "Line 1\nLine 2\nLine 3\n...")
///         }
///     }
/// }
/// # }
/// ```
#[component]
pub fn ScrollView<'a>(
    mut hooks: Hooks,
    props: &mut ScrollViewProps<'a>,
) -> impl Into<AnyElement<'a>> {
    let mut scroll_offset = hooks.use_state(|| 0i32);
    let mut user_scrolled_up = hooks.use_state(|| false);
    let mut pending_scroll_delta = hooks.use_state(|| 0i32);
    let mut content_height: State<u16> = hooks.use_state(|| 0u16);
    let viewport_height: State<u16> = hooks.use_state(|| 0u16);
    let viewport_top: State<u16> = hooks.use_state(|| 0u16);
    let clamp_bounds: State<ScrollClampBounds> = hooks.use_state(ScrollClampBounds::default);
    let subscribers = hooks.use_const_default::<SharedScrollViewSubscribers>();
    let content_height_ref: Ref<u16> = hooks.use_ref(|| 0u16);

    // Measure the viewport (this component's) height and absolute top.
    let h = hooks.use_hook(move || MeasureHeightHook {
        out: viewport_height,
        out_top: viewport_top,
    });
    h.out = viewport_height;
    h.out_top = viewport_top;

    let scroll_step = props.scroll_step.unwrap_or(DEFAULT_SCROLL_STEP) as i32;
    let wheel_acceleration = props.wheel_acceleration.unwrap_or(false);
    let wheel_accel = hooks.use_ref(WheelAccelState::from_env);
    let scroll_drain_mode = props.scroll_drain_mode;
    let drain_enabled = scroll_drain_mode.is_some();
    if !drain_enabled && pending_scroll_delta.get() != 0 {
        pending_scroll_delta.set(0);
    }
    let auto_scroll = props.auto_scroll;
    let keyboard_scroll = props.keyboard_scroll.unwrap_or(true);
    let modal_pager_keys = props.modal_pager_keys.unwrap_or(false);
    let selection = props.selection.unwrap_or_else(SelectionContext::disabled);
    let selection_screen_hook = hooks.use_hook(SelectionScreenSnapshotHook::default);
    selection_screen_hook.enabled = selection.is_enabled();
    let selection_screen = selection_screen_hook.canvas.clone();

    // Sync content height from the ref written by the measurer child.
    let ch = content_height_ref.get();
    if content_height.get() != ch {
        content_height.set(ch);
    }

    // Wire up the handle.
    if let Some(handle_ref) = props.handle.as_mut() {
        handle_ref.set(ScrollViewHandle {
            inner: Some(ScrollViewHandleInner {
                scroll_offset,
                content_height,
                viewport_height,
                viewport_top,
                user_scrolled_up,
                clamp_bounds,
                pending_scroll_delta,
                auto_scroll_enabled: auto_scroll,
                scroll_drain_enabled: drain_enabled,
                subscribers: subscribers.clone(),
            }),
        });
    }

    // Determine if we should use auto_scroll (pinned to bottom) mode.
    let pinned_to_bottom = auto_scroll && !user_scrolled_up.get();

    // When not pinned to bottom, keep the committed offset inside the real
    // scroll range. Virtual-scroll clamp bounds are visual-only and applied
    // later when computing the rendered offset.
    if !pinned_to_bottom {
        let clamped = clamp_offset_to_scroll_range(
            scroll_offset.get(),
            content_height.get(),
            viewport_height.get(),
        );
        if scroll_offset.get() != clamped {
            scroll_offset.set(clamped);
        }
    }

    hooks.use_local_propagated_terminal_events({
        let vh = viewport_height;
        let subscribers = subscribers.clone();
        let selection_screen_for_events = selection_screen.clone();
        let mut wheel_accel = wheel_accel;
        let mut pending_scroll_delta_for_events = pending_scroll_delta;
        move |event| {
            let action = match event.event() {
                TerminalEvent::Key(key_event) if key_event.kind != KeyEventKind::Release => {
                    if keyboard_scroll {
                        scroll_key_delta(key_event, vh.get(), modal_pager_keys)
                            .map(|delta| (delta, false))
                    } else {
                        None
                    }
                }
                TerminalEvent::FullscreenMouse(mouse) => match mouse.kind {
                    MouseEventKind::ScrollUp => {
                        let step = if wheel_acceleration {
                            wheel_accel.write().compute_now(-1)
                        } else {
                            scroll_step
                        };
                        (step > 0).then_some((-step, true))
                    }
                    MouseEventKind::ScrollDown => {
                        let step = if wheel_acceleration {
                            wheel_accel.write().compute_now(1)
                        } else {
                            scroll_step
                        };
                        (step > 0).then_some((step, true))
                    }
                    _ => None,
                },
                _ => None,
            };

            if let Some((delta, is_wheel)) = action {
                if is_wheel && scroll_drain_mode.is_some() {
                    selection.clear_selection();
                    pending_scroll_delta_for_events
                        .set(pending_scroll_delta_for_events.get().saturating_add(delta));
                    if auto_scroll && delta < 0 {
                        if !user_scrolled_up.get() {
                            scroll_offset.set(max_offset(content_height.get(), vh.get()));
                        }
                        user_scrolled_up.set(true);
                    }
                    subscribers.notify();
                    event.stop_propagation();
                    return;
                }

                let old_offset = if auto_scroll && !user_scrolled_up.get() {
                    max_offset(content_height.get(), vh.get())
                } else {
                    scroll_offset.get()
                };
                let old_visual_offset = clamp_offset_with_bounds(
                    old_offset,
                    content_height.get(),
                    vh.get(),
                    clamp_bounds.get(),
                );
                let old_user_scrolled_up = user_scrolled_up.get();
                let new_offset = clamp_offset_to_scroll_range(
                    old_offset + delta,
                    content_height.get(),
                    vh.get(),
                );
                let new_visual_offset = clamp_offset_with_bounds(
                    new_offset,
                    content_height.get(),
                    vh.get(),
                    clamp_bounds.get(),
                );
                let actual_delta = new_visual_offset - old_visual_offset;

                if is_wheel {
                    // CC Ink clears selections for wheel line-scroll because
                    // async wheel draining cannot synchronously capture the
                    // exact outgoing rows.
                    selection.clear_selection();
                } else if actual_delta != 0 && selection.is_enabled() && vh.get() > 0 {
                    let canvas = selection_screen_for_events
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .clone();
                    if let Some(canvas) = canvas {
                        let top = viewport_top.get() as usize;
                        let bottom = top + vh.get() as usize - 1;
                        selection.translate_for_scroll_jump(
                            &canvas,
                            actual_delta as isize,
                            top,
                            bottom,
                        );
                    }
                }

                scroll_offset.set(new_offset);

                if auto_scroll {
                    let max = max_offset(content_height.get(), vh.get());
                    if delta < 0 {
                        user_scrolled_up.set(true);
                    } else if new_offset >= max {
                        user_scrolled_up.set(false);
                    }
                }

                if new_offset != old_offset || user_scrolled_up.get() != old_user_scrolled_up {
                    subscribers.notify();
                    event.stop_propagation();
                }
            }
        }
    });

    hooks.use_interval(
        {
            let subscribers_for_drain = subscribers.clone();
            let mut scroll_offset_for_drain = scroll_offset;
            let mut user_scrolled_up_for_drain = user_scrolled_up;
            let mut pending_scroll_delta_for_drain = pending_scroll_delta;
            move || {
                let Some(mode) = scroll_drain_mode else {
                    return;
                };
                let pending = pending_scroll_delta_for_drain.get();
                if pending == 0 {
                    return;
                }
                let viewport_height = viewport_height.get();
                if viewport_height == 0 {
                    pending_scroll_delta_for_drain.set(0);
                    return;
                }

                let old_offset = if auto_scroll && !user_scrolled_up_for_drain.get() {
                    max_offset(content_height.get(), viewport_height)
                } else {
                    scroll_offset_for_drain.get()
                };
                let old_user_scrolled_up = user_scrolled_up_for_drain.get();
                let bounds = clamp_bounds.get();
                let max_valid = max_offset(content_height.get(), viewport_height);
                let lower = bounds.min.unwrap_or(0).clamp(0, max_valid);
                let upper = bounds.max.unwrap_or(max_valid).clamp(lower, max_valid);
                let past_mounted_clamp =
                    (pending < 0 && old_offset < lower) || (pending > 0 && old_offset > upper);
                let effective_viewport_height = if past_mounted_clamp {
                    (viewport_height >> 3).min(4)
                } else {
                    viewport_height
                };
                let result = drain_scroll_delta(mode, pending, effective_viewport_height);
                let unclamped_offset = old_offset.saturating_add(result.applied);
                let new_offset = clamp_offset_to_scroll_range(
                    unclamped_offset,
                    content_height.get(),
                    viewport_height,
                );
                let remaining = if new_offset == unclamped_offset {
                    result.remaining
                } else {
                    0
                };

                scroll_offset_for_drain.set(new_offset);
                pending_scroll_delta_for_drain.set(remaining);

                if auto_scroll {
                    let max = max_offset(content_height.get(), viewport_height);
                    if result.applied < 0 {
                        user_scrolled_up_for_drain.set(true);
                    } else if new_offset >= max && remaining == 0 {
                        user_scrolled_up_for_drain.set(false);
                    }
                }

                if new_offset != old_offset
                    || remaining != pending
                    || user_scrolled_up_for_drain.get() != old_user_scrolled_up
                {
                    subscribers_for_drain.notify();
                }
            }
        },
        (scroll_drain_mode.is_some() && pending_scroll_delta.get() != 0).then_some(FRAME_INTERVAL),
    );

    let drag_snapshot = selection.controller_snapshot();
    let drag_selection = drag_snapshot.selection();
    let drag_viewport_height = viewport_height.get() as usize;
    let drag_viewport_top = viewport_top.get() as usize;
    let drag_viewport_bottom =
        drag_viewport_top.saturating_add(drag_viewport_height.saturating_sub(1));
    let drag_focus = drag_selection.focus().map(|focus| (focus.col, focus.row));
    let drag_focus_outside = drag_selection
        .focus()
        .is_some_and(|focus| focus.row < drag_viewport_top || focus.row > drag_viewport_bottom);
    let drag_owned_by_viewport = drag_selection.anchor().is_some_and(|anchor| {
        anchor.row >= drag_viewport_top && anchor.row <= drag_viewport_bottom
    }) || !drag_selection.captured_rows_empty();
    let drag_autoscroll_raw_active = selection.is_enabled()
        && drag_viewport_height > 0
        && drag_selection.is_dragging()
        && drag_focus_outside
        && drag_owned_by_viewport;
    let mut drag_autoscroll_ticks = hooks.use_state(DragAutoscrollTickState::default);
    let next_drag_autoscroll_ticks = reset_drag_autoscroll_ticks_for_render(
        drag_autoscroll_ticks.get(),
        drag_autoscroll_raw_active,
        drag_focus,
    );
    if next_drag_autoscroll_ticks != drag_autoscroll_ticks.get() {
        drag_autoscroll_ticks.set(next_drag_autoscroll_ticks);
    }
    let blocked_same_focus =
        drag_focus.is_some() && next_drag_autoscroll_ticks.blocked_focus == drag_focus;
    let drag_autoscroll_active =
        drag_autoscroll_raw_active && !next_drag_autoscroll_ticks.capped && !blocked_same_focus;
    hooks.use_interval(
        {
            let selection_screen_for_drag = selection_screen.clone();
            let subscribers_for_drag = subscribers.clone();
            let mut scroll_offset_for_drag = scroll_offset;
            let mut user_scrolled_up_for_drag = user_scrolled_up;
            let pending_scroll_delta_for_drag = pending_scroll_delta;
            let mut drag_autoscroll_ticks_for_drag = drag_autoscroll_ticks;
            move || {
                let viewport_height = viewport_height.get() as usize;
                if viewport_height == 0 {
                    return;
                }
                let top = viewport_top.get() as usize;
                let bottom = top + viewport_height - 1;
                let Some(direction) = selection.drag_scroll_direction(top, bottom) else {
                    let blocked_focus = selection
                        .controller_snapshot()
                        .selection()
                        .focus()
                        .map(|focus| (focus.col, focus.row));
                    if blocked_focus.is_some_and(|(_, row)| row < top || row > bottom) {
                        let mut tick_state = drag_autoscroll_ticks_for_drag.get();
                        tick_state.focus = blocked_focus;
                        tick_state.blocked_focus = blocked_focus;
                        drag_autoscroll_ticks_for_drag.set(tick_state);
                    }
                    return;
                };
                let mut tick_state = drag_autoscroll_ticks_for_drag.get();
                if !allow_drag_autoscroll_tick(&mut tick_state) {
                    drag_autoscroll_ticks_for_drag.set(tick_state);
                    return;
                }
                drag_autoscroll_ticks_for_drag.set(tick_state);
                // CC Ink skips a drag-autoscroll tick while ScrollBox has a
                // pending wheel drain so selection capture observes stable
                // viewport rows. iocraft mirrors that when the opt-in drain
                // mode is enabled; eager scroll mode keeps this at zero.
                if pending_scroll_delta_for_drag.get() != 0 {
                    return;
                }
                let canvas = selection_screen_for_drag
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .clone();
                let Some(canvas) = canvas else {
                    return;
                };

                let old_offset = if auto_scroll && !user_scrolled_up_for_drag.get() {
                    max_offset(content_height.get(), viewport_height as u16)
                } else {
                    scroll_offset_for_drag.get()
                };
                let max = max_offset(content_height.get(), viewport_height as u16);
                let (new_offset, actual_lines) = match direction {
                    crate::SelectionDragScrollDirection::Up => {
                        if old_offset <= 0 {
                            return;
                        }
                        let actual = SELECTION_AUTOSCROLL_LINES.min(old_offset) as usize;
                        (old_offset - actual as i32, actual)
                    }
                    crate::SelectionDragScrollDirection::Down => {
                        if old_offset >= max {
                            return;
                        }
                        let actual = SELECTION_AUTOSCROLL_LINES.min(max - old_offset) as usize;
                        (old_offset + actual as i32, actual)
                    }
                };
                if actual_lines == 0 {
                    return;
                }

                selection.translate_for_drag_autoscroll(
                    &canvas,
                    direction,
                    actual_lines,
                    top,
                    bottom,
                );
                scroll_offset_for_drag.set(new_offset);
                if auto_scroll {
                    user_scrolled_up_for_drag.set(true);
                }
                subscribers_for_drag.notify();
            }
        },
        drag_autoscroll_active.then_some(SELECTION_AUTOSCROLL_INTERVAL),
    );

    let show_scrollbar =
        props.scrollbar.unwrap_or(true) && content_height.get() > viewport_height.get();
    let scrollbar_thumb_color = props.scrollbar_thumb_color;
    let scrollbar_track_color = props.scrollbar_track_color;

    let committed_offset = if pinned_to_bottom {
        max_offset(content_height.get(), viewport_height.get())
    } else {
        scroll_offset.get()
    };
    let effective_offset = clamp_offset_with_bounds(
        committed_offset,
        content_height.get(),
        viewport_height.get(),
        clamp_bounds.get(),
    );

    let scroll_hint = hooks.use_hook(ScrollHintHook::default);
    scroll_hint.scroll_offset = effective_offset;
    scroll_hint.enabled = content_height.get() > viewport_height.get() && viewport_height.get() > 1;

    if pinned_to_bottom {
        if show_scrollbar {
            element! {
                View(width: 100pct, height: 100pct, flex_direction: FlexDirection::Row) {
                    View(
                        overflow: Overflow::Hidden,
                        flex_grow: 1.0,
                        height: 100pct,
                        flex_direction: FlexDirection::Column,
                        justify_content: JustifyContent::FLEX_END,
                    ) {
                        ScrollViewContentMeasurer(
                            content_height_ref: Some(content_height_ref),
                            selection,
                            selection_screen: selection_screen.clone(),
                            viewport_top: Some(viewport_top),
                            viewport_height: Some(viewport_height),
                            pinned_to_bottom,
                        ) {
                            #(props.children.iter_mut())
                        }
                    }
                    ScrollViewScrollbar(
                        viewport_height: viewport_height.get(),
                        content_height: content_height.get(),
                        scroll_offset: effective_offset,
                        thumb_color: scrollbar_thumb_color,
                        track_color: scrollbar_track_color,
                    )
                }
            }
        } else {
            element! {
                View(
                    overflow: Overflow::Hidden,
                    width: 100pct,
                    height: 100pct,
                    flex_direction: FlexDirection::Column,
                    justify_content: JustifyContent::FLEX_END,
                ) {
                    ScrollViewContentMeasurer(
                        content_height_ref: Some(content_height_ref),
                        selection,
                        selection_screen: selection_screen.clone(),
                        viewport_top: Some(viewport_top),
                        viewport_height: Some(viewport_height),
                        pinned_to_bottom,
                    ) {
                        #(props.children.iter_mut())
                    }
                }
            }
        }
    } else if show_scrollbar {
        element! {
            View(width: 100pct, height: 100pct, flex_direction: FlexDirection::Row) {
                View(overflow: Overflow::Hidden, flex_grow: 1.0, height: 100pct) {
                    View(position: Position::Absolute, top: -effective_offset, width: 100pct) {
                        ScrollViewContentMeasurer(
                            content_height_ref: Some(content_height_ref),
                            selection,
                            selection_screen: selection_screen.clone(),
                            viewport_top: Some(viewport_top),
                            viewport_height: Some(viewport_height),
                            pinned_to_bottom,
                        ) {
                            #(props.children.iter_mut())
                        }
                    }
                }
                ScrollViewScrollbar(
                    viewport_height: viewport_height.get(),
                    content_height: content_height.get(),
                    scroll_offset: effective_offset,
                    thumb_color: scrollbar_thumb_color,
                    track_color: scrollbar_track_color,
                )
            }
        }
    } else {
        element! {
            View(overflow: Overflow::Hidden, width: 100pct, height: 100pct) {
                View(position: Position::Absolute, top: -effective_offset, width: 100pct) {
                    ScrollViewContentMeasurer(
                        content_height_ref: Some(content_height_ref),
                        selection,
                        selection_screen: selection_screen.clone(),
                        viewport_top: Some(viewport_top),
                        viewport_height: Some(viewport_height),
                        pinned_to_bottom,
                    ) {
                        #(props.children.iter_mut())
                    }
                }
            }
        }
    }
}

#[derive(Default, Props)]
struct ScrollViewContentMeasurerProps<'a> {
    children: Vec<AnyElement<'a>>,
    content_height_ref: Option<Ref<u16>>,
    selection: SelectionContext,
    selection_screen: Arc<Mutex<Option<Canvas>>>,
    viewport_top: Option<State<u16>>,
    viewport_height: Option<State<u16>>,
    pinned_to_bottom: bool,
}

// Hook that measures this component's height and writes it to a Ref<u16>
// shared with the parent ScrollView.
struct ContentHeightHook {
    out: Option<Ref<u16>>,
    selection: SelectionContext,
    selection_screen: Arc<Mutex<Option<Canvas>>>,
    viewport_top: Option<State<u16>>,
    viewport_height: Option<State<u16>>,
    pinned_to_bottom: bool,
}

impl Hook for ContentHeightHook {
    fn pre_component_draw(&mut self, drawer: &mut ComponentDrawer) {
        if let Some(mut out) = self.out {
            let h = drawer.size().height;
            let old_h = out.try_get().unwrap_or(0);
            if self.pinned_to_bottom && self.selection.is_enabled() && old_h > 0 && h > old_h {
                if let (Some(viewport_top), Some(viewport_height)) =
                    (self.viewport_top, self.viewport_height)
                {
                    let viewport_height = viewport_height.get() as usize;
                    if viewport_height > 0 {
                        let canvas = self
                            .selection_screen
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner())
                            .clone();
                        if let Some(canvas) = canvas {
                            let top = viewport_top.get() as usize;
                            let bottom = top + viewport_height - 1;
                            self.selection.translate_for_follow_scroll(
                                &canvas,
                                (h - old_h) as usize,
                                top,
                                bottom,
                            );
                        }
                    }
                }
            }
            if old_h != h {
                out.set(h);
            }
        }
    }
}

/// Private component that lives inside the scroll pane so that
/// its `pre_component_draw` size equals the natural content height.
#[component]
fn ScrollViewContentMeasurer<'a>(
    mut hooks: Hooks,
    props: &mut ScrollViewContentMeasurerProps<'a>,
) -> impl Into<AnyElement<'a>> {
    let content_height_ref = props.content_height_ref;
    let selection = props.selection;
    let selection_screen = props.selection_screen.clone();
    let viewport_top = props.viewport_top;
    let viewport_height = props.viewport_height;
    let pinned_to_bottom = props.pinned_to_bottom;
    let selection_screen_for_init = selection_screen.clone();
    let h = hooks.use_hook(move || ContentHeightHook {
        out: content_height_ref,
        selection,
        selection_screen: selection_screen_for_init.clone(),
        viewport_top,
        viewport_height,
        pinned_to_bottom,
    });
    h.out = content_height_ref;
    h.selection = selection;
    h.selection_screen = selection_screen;
    h.viewport_top = viewport_top;
    h.viewport_height = viewport_height;
    h.pinned_to_bottom = pinned_to_bottom;

    element! {
        View(width: 100pct) {
            #(props.children.iter_mut())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        allow_drag_autoscroll_tick, reset_drag_autoscroll_ticks_for_render,
        scroll_drain_mode_for_xterm_js_host, DragAutoscrollTickState, WheelAccelState,
    };
    use crate::prelude::*;
    use futures::stream::{self, StreamExt};
    use macro_rules_attribute::apply;
    use smol_macros::test;
    use std::sync::{Arc, Mutex};

    #[test]
    fn test_drag_autoscroll_tick_cap_allows_200_ticks_then_caps() {
        let mut state = DragAutoscrollTickState {
            focus: Some((3, 4)),
            ticks: 0,
            capped: false,
            blocked_focus: None,
        };
        for _ in 0..crate::SELECTION_AUTOSCROLL_MAX_TICKS {
            assert!(allow_drag_autoscroll_tick(&mut state));
        }
        assert_eq!(state.ticks, crate::SELECTION_AUTOSCROLL_MAX_TICKS);
        assert!(!state.capped);

        assert!(!allow_drag_autoscroll_tick(&mut state));
        assert!(state.capped);
    }

    #[test]
    fn test_drag_autoscroll_tick_cap_resets_on_focus_change_or_inactive() {
        let capped = DragAutoscrollTickState {
            focus: Some((3, 4)),
            ticks: crate::SELECTION_AUTOSCROLL_MAX_TICKS + 1,
            capped: true,
            blocked_focus: Some((3, 4)),
        };

        assert_eq!(
            reset_drag_autoscroll_ticks_for_render(capped, true, Some((3, 4))),
            capped,
            "same stationary focus keeps the lost-release cap/block in force"
        );
        assert_eq!(
            reset_drag_autoscroll_ticks_for_render(capped, true, Some((3, 5))),
            DragAutoscrollTickState {
                focus: Some((3, 5)),
                ticks: 0,
                capped: false,
                blocked_focus: None,
            }
        );
        assert_eq!(
            reset_drag_autoscroll_ticks_for_render(capped, false, Some((3, 4))),
            DragAutoscrollTickState::default()
        );
    }

    #[test]
    fn test_scroll_box_render_offset_plan_matches_cc_anchor_sticky_drain_and_clamp_order() {
        let anchor = plan_scroll_box_render_offset(ScrollBoxRenderOffsetInput {
            current_scroll_top: 5,
            previous_scroll_height: 50,
            scroll_height: 50,
            viewport_height: 10,
            sticky: true,
            pending_delta: Some(12),
            anchor_top: Some(20),
            anchor_offset: -2,
            clamp_min: None,
            clamp_max: None,
            drain_mode: Some(ScrollDrainMode::Native),
        });
        assert_eq!(
            anchor,
            ScrollBoxRenderOffsetPlan {
                scroll_top: 18,
                committed_scroll_top: 18,
                pending_delta: None,
                sticky: false,
                anchor_consumed: true,
                followed_bottom: false,
                drained_delta: 0,
                clamped_to_scroll_range: false,
                clamped_to_mounted_range: false,
            },
            "scrollToElement-style anchors should consume pending drain and break stickiness"
        );

        let follow = plan_scroll_box_render_offset(ScrollBoxRenderOffsetInput {
            current_scroll_top: 40,
            previous_scroll_height: 50,
            scroll_height: 55,
            viewport_height: 10,
            sticky: false,
            pending_delta: Some(0),
            anchor_top: None,
            anchor_offset: 0,
            clamp_min: None,
            clamp_max: None,
            drain_mode: Some(ScrollDrainMode::Native),
        });
        assert_eq!(follow.scroll_top, 45);
        assert!(follow.sticky);
        assert!(follow.followed_bottom);
        assert_eq!(follow.pending_delta, None);

        let drained = plan_scroll_box_render_offset(ScrollBoxRenderOffsetInput {
            current_scroll_top: 13,
            previous_scroll_height: 100,
            scroll_height: 100,
            viewport_height: 10,
            sticky: false,
            pending_delta: Some(20),
            anchor_top: None,
            anchor_offset: 0,
            clamp_min: Some(12),
            clamp_max: Some(14),
            drain_mode: Some(ScrollDrainMode::Native),
        });
        assert_eq!(
            drained,
            ScrollBoxRenderOffsetPlan {
                scroll_top: 14,
                committed_scroll_top: 22,
                pending_delta: Some(11),
                sticky: false,
                anchor_consumed: false,
                followed_bottom: false,
                drained_delta: 9,
                clamped_to_scroll_range: false,
                clamped_to_mounted_range: true,
            },
            "mounted-range clamp should be visual-only and leave committed scroll/pending drain intact"
        );

        let past_clamp = plan_scroll_box_render_offset(ScrollBoxRenderOffsetInput {
            current_scroll_top: 22,
            previous_scroll_height: 100,
            scroll_height: 100,
            viewport_height: 32,
            sticky: false,
            pending_delta: Some(20),
            anchor_top: None,
            anchor_offset: 0,
            clamp_min: Some(12),
            clamp_max: Some(14),
            drain_mode: Some(ScrollDrainMode::Native),
        });
        assert_eq!(past_clamp.drained_delta, 3);
        assert_eq!(past_clamp.committed_scroll_top, 25);
        assert_eq!(past_clamp.scroll_top, 14);
        assert_eq!(past_clamp.pending_delta, Some(17));
    }

    #[component]
    fn TestScrollView(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let mut done = hooks.use_state(|| false);

        hooks.use_terminal_events(move |event| {
            if let TerminalEvent::Key(KeyEvent {
                code: KeyCode::Char('q'),
                kind: KeyEventKind::Press,
                ..
            }) = event
            {
                done.set(true);
            }
        });

        if done.get() {
            system.exit();
        }

        let mut lines = String::new();
        for i in 0..20 {
            if i > 0 {
                lines.push('\n');
            }
            lines.push_str(&format!("Line {i}"));
        }

        element! {
            View(width: 20, height: 5) {
                ScrollView {
                    Text(content: lines)
                }
            }
        }
    }

    #[apply(test!)]
    async fn test_scroll_view_basic_render() {
        let canvases: Vec<_> = element!(TestScrollView)
            .mock_terminal_render_loop(MockTerminalConfig::with_events(stream::iter(vec![
                TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Char('q'))),
            ])))
            .collect()
            .await;

        let output = canvases.last().unwrap().to_string();
        assert!(output.contains("Line 0"));
        assert!(output.contains("Line 4"));
        assert!(!output.contains("Line 5"));
    }

    #[apply(test!)]
    async fn test_scroll_view_keyboard_scroll() {
        let canvases: Vec<_> = element!(TestScrollView)
            .mock_terminal_render_loop(MockTerminalConfig::with_events(stream::iter(vec![
                TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Down)),
                TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Down)),
                TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Char('q'))),
            ])))
            .collect()
            .await;

        let output = canvases.last().unwrap().to_string();
        assert!(output.contains("Line 2"));
        assert!(!output.contains("Line 0"));
    }

    #[component]
    fn TestScrollViewModalPager(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let mut done = hooks.use_state(|| false);

        hooks.use_terminal_events(move |event| {
            if matches!(
                event,
                TerminalEvent::Key(KeyEvent {
                    code: KeyCode::Char('q'),
                    kind: KeyEventKind::Press,
                    ..
                })
            ) {
                done.set(true);
            }
        });

        if done.get() {
            system.exit();
        }

        let lines = (0..20)
            .map(|i| format!("Line {i}"))
            .collect::<Vec<_>>()
            .join("\n");

        element! {
            View(width: 20, height: 5) {
                ScrollView(modal_pager_keys: Some(true), scrollbar: Some(false)) {
                    Text(content: lines)
                }
            }
        }
    }

    #[apply(test!)]
    async fn test_scroll_view_modal_pager_ctrl_d_scrolls_half_page() {
        let canvases: Vec<_> = element!(TestScrollViewModalPager)
            .mock_terminal_render_loop(MockTerminalConfig::with_events(stream::iter(vec![
                TerminalEvent::Key(KeyEvent {
                    code: KeyCode::Char('d'),
                    modifiers: KeyModifiers::CONTROL,
                    kind: KeyEventKind::Press,
                }),
                TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Char('q'))),
            ])))
            .collect()
            .await;

        let output = canvases.last().unwrap().to_string();
        assert!(
            output.contains("Line 2"),
            "ctrl+d should half-page down: {output:?}"
        );
        assert!(
            !output.contains("Line 0"),
            "top line should have scrolled away: {output:?}"
        );
    }

    #[apply(test!)]
    async fn test_scroll_view_modal_pager_g_and_shift_g_jump_bounds() {
        let canvases: Vec<_> = element!(TestScrollViewModalPager)
            .mock_terminal_render_loop(MockTerminalConfig::with_events(stream::iter(vec![
                TerminalEvent::Key(KeyEvent {
                    code: KeyCode::Char('G'),
                    modifiers: KeyModifiers::SHIFT,
                    kind: KeyEventKind::Press,
                }),
                TerminalEvent::Key(KeyEvent {
                    code: KeyCode::Char('g'),
                    modifiers: KeyModifiers::empty(),
                    kind: KeyEventKind::Press,
                }),
                TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Char('q'))),
            ])))
            .collect()
            .await;

        let output = canvases.last().unwrap().to_string();
        assert!(
            output.contains("Line 0"),
            "g should return to top after G bottom jump: {output:?}"
        );
        assert!(
            !output.contains("Line 15"),
            "bottom jump should have been replaced by g top jump: {output:?}"
        );
    }

    #[test]
    fn test_wheel_accel_native_rapid_events_ramp_and_idle_reset() {
        let mut accel = WheelAccelState::new(false, 1.0);
        assert_eq!(accel.compute(1, 0.0), 1);
        assert_eq!(accel.compute(1, 20.0), 1);
        assert_eq!(accel.compute(1, 40.0), 1);
        assert_eq!(accel.compute(1, 60.0), 2);
        assert_eq!(accel.compute(1, 500.0), 1);
    }

    #[test]
    fn test_wheel_accel_xterm_decay_and_fractional_carry() {
        let mut accel = WheelAccelState::new(true, 1.0);
        assert_eq!(accel.compute(1, 0.0), 2);
        let rows = accel.compute(1, 100.0);
        assert!(
            rows >= 3,
            "xterm decay should accelerate sparse same-direction events: {rows}"
        );
        assert_eq!(
            accel.compute(1, 101.0),
            1,
            "same-batch bursts stay one row per event"
        );
    }

    #[test]
    fn test_scroll_drain_mode_host_strategy_matches_cc_ink_xterm_branch() {
        assert_eq!(
            scroll_drain_mode_for_xterm_js_host(false),
            ScrollDrainMode::Native
        );
        assert_eq!(
            scroll_drain_mode_for_xterm_js_host(true),
            ScrollDrainMode::XtermJs
        );
        assert_eq!(
            ScrollDrainMode::for_current_terminal(),
            scroll_drain_mode_for_xterm_js_host(crate::terminal::is_xterm_js())
        );
    }

    #[test]
    fn test_scroll_drain_delta_matches_cc_ink_native_and_xterm_rules() {
        assert_eq!(
            drain_scroll_delta(ScrollDrainMode::Native, 3, 10),
            ScrollDrainResult {
                applied: 3,
                remaining: 0,
            },
            "native small deltas drain fully"
        );
        assert_eq!(
            drain_scroll_delta(ScrollDrainMode::Native, 20, 10),
            ScrollDrainResult {
                applied: 9,
                remaining: 11,
            },
            "native drain is proportional and capped at viewport-1"
        );
        assert_eq!(
            drain_scroll_delta(ScrollDrainMode::Native, -20, 10),
            ScrollDrainResult {
                applied: -9,
                remaining: -11,
            }
        );

        assert_eq!(
            drain_scroll_delta(ScrollDrainMode::XtermJs, 4, 10),
            ScrollDrainResult {
                applied: 4,
                remaining: 0,
            },
            "xterm.js slow clicks drain immediately"
        );
        assert_eq!(
            drain_scroll_delta(ScrollDrainMode::XtermJs, 10, 10),
            ScrollDrainResult {
                applied: 2,
                remaining: 8,
            },
            "xterm.js medium pending uses the fixed medium step"
        );
        assert_eq!(
            drain_scroll_delta(ScrollDrainMode::XtermJs, 40, 10),
            ScrollDrainResult {
                applied: 9,
                remaining: 31,
            },
            "xterm.js snaps excess but still caps the visible frame delta"
        );
        assert_eq!(
            drain_scroll_delta(ScrollDrainMode::XtermJs, -40, 4),
            ScrollDrainResult {
                applied: -3,
                remaining: -37,
            },
            "negative xterm.js drains preserve sign and viewport cap"
        );
    }

    #[test]
    fn test_scroll_drain_state_keeps_cc_ink_pending_delta_as_explicit_rust_state() {
        let mut state = ScrollDrainState::with_pending(ScrollDrainMode::XtermJs, 40);
        assert_eq!(state.mode(), ScrollDrainMode::XtermJs);
        assert!(state.has_pending_delta());

        assert_eq!(
            state.drain_frame(10),
            ScrollDrainResult {
                applied: 9,
                remaining: 31,
            }
        );
        assert_eq!(state.pending_delta(), 31);

        state.add_delta(-6);
        assert_eq!(state.pending_delta(), 25);
        assert_eq!(
            state.drain_frame(10),
            ScrollDrainResult {
                applied: 3,
                remaining: 22,
            }
        );

        state.set_mode(ScrollDrainMode::Native);
        state.set_pending_delta(3);
        assert_eq!(
            state.drain_frame(10),
            ScrollDrainResult {
                applied: 3,
                remaining: 0,
            }
        );
        assert!(!state.has_pending_delta());

        state.add_delta(i32::MAX);
        state.add_delta(1);
        assert_eq!(state.pending_delta(), i32::MAX);
        state.clear();
        assert_eq!(state.pending_delta(), 0);
    }

    #[test]
    fn test_virtual_scroll_plan_cold_start_mounts_tail_with_coverage() {
        let heights = vec![Some(2); 100];
        let config = VirtualScrollConfig {
            cold_start_count: 10,
            ..Default::default()
        };
        let plan = plan_virtual_scroll_range(&heights, VirtualScrollInput::default(), config);

        assert_eq!(plan.range, VirtualScrollRange::new(20, 100));
        assert_eq!(plan.top_spacer, 40);
        assert_eq!(plan.bottom_spacer, 0);
        assert_eq!(plan.total_height, 200);
        assert_eq!(plan.clamp_min, None);
        assert_eq!(plan.clamp_max, None);
    }

    #[test]
    fn test_virtual_scroll_plan_sticky_tail_keeps_coverage() {
        let heights = vec![Some(2); 100];
        let config = VirtualScrollConfig {
            overscan_rows: 6,
            ..Default::default()
        };
        let plan = plan_virtual_scroll_range(
            &heights,
            VirtualScrollInput {
                scroll_top: Some(170),
                viewport_height: 10,
                is_sticky: true,
                ..Default::default()
            },
            config,
        );

        assert_eq!(plan.range, VirtualScrollRange::new(89, 100));
        assert_eq!(plan.top_spacer, 178);
        assert_eq!(plan.bottom_spacer, 0);
        assert_eq!(plan.clamp_min, None);
        assert_eq!(plan.clamp_max, None);
    }

    #[test]
    fn test_virtual_scroll_plan_spans_committed_and_pending_delta() {
        let heights = vec![None; 1000];
        let config = VirtualScrollConfig {
            overscan_rows: 6,
            scroll_quantum_rows: 10,
            max_mounted_items: 50,
            ..Default::default()
        };
        let plan = plan_virtual_scroll_range(
            &heights,
            VirtualScrollInput {
                scroll_top: Some(300),
                pending_delta: 120,
                viewport_height: 20,
                is_sticky: false,
                ..Default::default()
            },
            config,
        );

        assert_eq!(plan.range, VirtualScrollRange::new(98, 130));
        assert_eq!(plan.top_spacer, 294);
        assert_eq!(plan.clamp_min, Some(294));
        assert_eq!(plan.clamp_max, Some(370));
        assert_eq!(plan.target_scroll_top, 420);
        assert_eq!(plan.snapshot_bin, 42);
    }

    #[test]
    fn test_virtual_scroll_plan_slide_caps_large_forward_jump() {
        let heights = vec![Some(1); 1000];
        let config = VirtualScrollConfig {
            overscan_rows: 5,
            slide_step_items: 10,
            max_mounted_items: 100,
            ..Default::default()
        };
        let plan = plan_virtual_scroll_range(
            &heights,
            VirtualScrollInput {
                scroll_top: Some(500),
                pending_delta: 1000,
                viewport_height: 20,
                is_sticky: false,
                previous_range: Some(VirtualScrollRange::new(100, 130)),
                previous_scroll_top: Some(100),
                ..Default::default()
            },
            config,
        );

        assert_eq!(plan.range, VirtualScrollRange::new(495, 505));
        assert_eq!(plan.range.len(), config.slide_step_items);
    }

    #[test]
    fn test_virtual_scroll_snapshot_bin_marks_sticky_transitions() {
        let config = VirtualScrollConfig {
            scroll_quantum_rows: 40,
            ..Default::default()
        };
        assert_eq!(virtual_scroll_snapshot_bin(80, 30, false, config), 2);
        assert_eq!(virtual_scroll_snapshot_bin(80, 30, true, config), !2);
    }

    #[test]
    fn test_virtual_scroll_state_caches_heights_and_retain_keys() {
        let mut state = VirtualScrollState::new();
        state.set_height("a", 2);
        state.set_height("b", 5);
        state.set_height("stale", 9);
        assert_eq!(state.len(), 3);
        assert!(state.retain_keys(["a", "b"].iter()));
        assert_eq!(state.height(&"stale"), None);

        let keys = ["a", "b", "c"];
        let plan = state.plan(
            &keys,
            VirtualScrollInput {
                scroll_top: Some(0),
                viewport_height: 4,
                is_sticky: false,
                ..Default::default()
            },
            VirtualScrollConfig {
                overscan_rows: 1,
                max_mounted_items: 10,
                ..Default::default()
            },
        );
        assert_eq!(
            plan.total_height, 10,
            "cached heights plus default estimate"
        );
        assert_eq!(state.previous_range(), Some(plan.range));
    }

    #[test]
    fn test_virtual_scroll_state_scales_heights_and_freezes_range_on_columns_change() {
        let keys = (0..100).collect::<Vec<_>>();
        let mut state = VirtualScrollState::new();
        for key in &keys {
            state.set_height(*key, 2);
        }
        state.set_columns(100);
        let first = state.plan(
            &keys,
            VirtualScrollInput {
                scroll_top: Some(100),
                viewport_height: 10,
                is_sticky: false,
                ..Default::default()
            },
            VirtualScrollConfig {
                overscan_rows: 4,
                max_mounted_items: 50,
                ..Default::default()
            },
        );

        assert!(state.set_columns(50));
        assert!(state.take_skip_measurement());
        assert!(!state.take_skip_measurement());
        assert_eq!(
            state.height(&0),
            Some(4),
            "narrower terminal scales cached heights up"
        );
        assert_eq!(state.freeze_remaining(), 2);

        let frozen = state.plan(
            &keys,
            VirtualScrollInput {
                scroll_top: Some(100),
                viewport_height: 10,
                is_sticky: false,
                ..Default::default()
            },
            VirtualScrollConfig {
                overscan_rows: 4,
                max_mounted_items: 50,
                ..Default::default()
            },
        );
        assert_eq!(
            frozen.range, first.range,
            "first post-resize pass reuses old range"
        );
        assert_eq!(state.freeze_remaining(), 1);
    }

    #[apply(test!)]
    async fn test_scroll_view_emits_scroll_hint_for_small_scroll_delta() {
        let canvases: Vec<_> = element!(TestScrollView)
            .mock_terminal_render_loop(MockTerminalConfig::with_events(stream::iter(vec![
                TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Down)),
                TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Char('q'))),
            ])))
            .collect()
            .await;

        assert!(
            canvases.iter().any(|canvas| canvas.scroll_hint()
                == Some(ScrollHint {
                    top: 0,
                    bottom: 4,
                    delta: 1,
                })),
            "ScrollView should annotate small stable-region scrolls for DECSTBM optimization"
        );
    }

    #[component]
    fn ScrollViewSelectionTrackingApp(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let selection = create_selection_context(&mut hooks);
        let mut page_seen = hooks.use_state(|| false);
        let last_copy = hooks.use_state(String::new);

        hooks.use_terminal_events(move |event| {
            if matches!(
                event,
                TerminalEvent::Key(KeyEvent {
                    code: KeyCode::PageDown,
                    kind: KeyEventKind::Press,
                    ..
                })
            ) {
                page_seen.set(true);
            }
        });

        if !selection.has_selection() && last_copy.read().is_empty() {
            let mut controller = SelectionController::new();
            controller.selection_mut().start(0, 0);
            controller.selection_mut().update(3, 2);
            controller.selection_mut().finish();
            selection.set_controller(controller);
        }

        let mut last_copy_for_callback = last_copy;
        hooks.use_copy_on_select_text(selection, page_seen.get(), move |text| {
            last_copy_for_callback.set(text);
        });

        if !last_copy.read().is_empty() && !selection.copy_on_select_would_mutate() {
            system.exit();
        }

        let lines = (0..5)
            .map(|i| format!("row{i}"))
            .collect::<Vec<_>>()
            .join("\n");

        element! {
            View(flex_direction: FlexDirection::Column) {
                View(width: 6, height: 3) {
                    ScrollView(selection: Some(selection), scrollbar: Some(false)) {
                        Text(content: lines)
                    }
                }
                Text(content: format!("copy={}", &*last_copy.read()))
            }
        }
    }

    #[apply(test!)]
    async fn test_scroll_view_selection_tracks_keyboard_scroll_jump() {
        let canvases: Vec<_> = element!(ScrollViewSelectionTrackingApp)
            .mock_terminal_render_loop(MockTerminalConfig::with_events(stream::iter(vec![
                TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::PageDown)),
            ])))
            .collect()
            .await;

        assert!(
            canvases
                .last()
                .unwrap()
                .to_string()
                .contains("copy=row0\nrow1\nrow2"),
            "selection should remain anchored to pre-scroll text: {:?}",
            canvases.last().unwrap().to_string()
        );
    }

    #[component]
    fn ScrollViewWheelClearsSelectionApp(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let selection = create_selection_context(&mut hooks);
        let mut wheel_seen = hooks.use_state(|| false);

        hooks.use_terminal_events(move |event| {
            if matches!(
                event,
                TerminalEvent::FullscreenMouse(FullscreenMouseEvent {
                    kind: MouseEventKind::ScrollDown,
                    ..
                })
            ) {
                wheel_seen.set(true);
            }
        });

        if !selection.has_selection() && !wheel_seen.get() {
            let mut controller = SelectionController::new();
            controller.selection_mut().start(0, 0);
            controller.selection_mut().update(3, 0);
            controller.selection_mut().finish();
            selection.set_controller(controller);
        }

        if wheel_seen.get() {
            system.exit();
        }

        let lines = (0..5)
            .map(|i| format!("row{i}"))
            .collect::<Vec<_>>()
            .join("\n");

        element! {
            View(flex_direction: FlexDirection::Column) {
                View(width: 6, height: 3) {
                    ScrollView(selection: Some(selection), scrollbar: Some(false)) {
                        Text(content: lines)
                    }
                }
                Text(content: format!("has={}", selection.has_selection()))
            }
        }
    }

    #[apply(test!)]
    async fn test_scroll_view_selection_clears_on_wheel_scroll() {
        let canvases: Vec<_> = element!(ScrollViewWheelClearsSelectionApp)
            .mock_terminal_render_loop(MockTerminalConfig::with_events(stream::iter(vec![
                TerminalEvent::FullscreenMouse(FullscreenMouseEvent::new(
                    MouseEventKind::ScrollDown,
                    0,
                    0,
                )),
            ])))
            .collect()
            .await;

        assert!(
            canvases.last().unwrap().to_string().contains("has=false"),
            "wheel scrolling should clear selection: {:?}",
            canvases.last().unwrap().to_string()
        );
    }

    #[component]
    fn ScrollViewSelectionFollowApp(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let selection = create_selection_context(&mut hooks);
        let mut rows = hooks.use_state(|| 3usize);
        let copied = hooks.use_state(String::new);

        if !selection.has_selection() && copied.read().is_empty() {
            let mut controller = SelectionController::new();
            controller.selection_mut().start(0, 0);
            controller.selection_mut().update(3, 2);
            controller.selection_mut().finish();
            selection.set_controller(controller);
        }

        let row_count = rows.get();
        if row_count == 3 {
            rows.set(4);
        }

        let mut copied_for_callback = copied;
        hooks.use_copy_on_select_text(selection, row_count > 3, move |text| {
            copied_for_callback.set(text);
        });

        if !copied.read().is_empty() && !selection.copy_on_select_would_mutate() {
            system.exit();
        }

        let lines = (0..row_count)
            .map(|i| format!("row{i}"))
            .collect::<Vec<_>>()
            .join("\n");

        element! {
            View(flex_direction: FlexDirection::Column) {
                View(width: 6, height: 3) {
                    ScrollView(auto_scroll: true, selection: Some(selection), scrollbar: Some(false)) {
                        Text(content: lines)
                    }
                }
                Text(content: format!("copy={}", &*copied.read()))
            }
        }
    }

    #[apply(test!)]
    async fn test_scroll_view_selection_tracks_sticky_follow_scroll() {
        let canvases: Vec<_> = element!(ScrollViewSelectionFollowApp)
            .mock_terminal_render_loop(MockTerminalConfig::default())
            .collect()
            .await;

        assert!(
            canvases
                .last()
                .unwrap()
                .to_string()
                .contains("copy=row0\nrow1\nrow2"),
            "selection should follow sticky scroll growth: {:?}",
            canvases.last().unwrap().to_string()
        );
    }

    #[component]
    fn ScrollViewSelectionDragAutoscrollApp(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let selection = create_selection_context(&mut hooks);
        let handle = hooks.use_ref_default::<ScrollViewHandle>();

        if !selection.controller_snapshot().selection().is_dragging() {
            let mut controller = SelectionController::new();
            controller.selection_mut().start(0, 1);
            controller.selection_mut().update(3, 3);
            selection.set_controller(controller);
        }

        if handle.read().get_scroll_top() > 0 {
            system.exit();
        }

        let lines = (0..5)
            .map(|i| format!("row{i}"))
            .collect::<Vec<_>>()
            .join("\n");

        element! {
            View(width: 6, height: 3) {
                ScrollView(handle, selection: Some(selection), scrollbar: Some(false)) {
                    Text(content: lines)
                }
            }
        }
    }

    #[apply(test!)]
    async fn test_scroll_view_selection_drag_autoscrolls_when_focus_leaves_viewport() {
        let canvases: Vec<_> = element!(ScrollViewSelectionDragAutoscrollApp)
            .mock_terminal_render_loop(MockTerminalConfig::default())
            .collect()
            .await;
        let output = canvases.last().unwrap().to_string();
        assert!(
            output.contains("row2"),
            "drag autoscroll should move down: {output:?}"
        );
        assert!(
            !output.contains("row0"),
            "row0 should have scrolled away: {output:?}"
        );
    }

    #[apply(test!)]
    async fn test_scroll_view_content_shorter_than_viewport() {
        #[component]
        fn ShortContent(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
            let mut system = hooks.use_context_mut::<SystemContext>();
            let mut done = hooks.use_state(|| false);

            hooks.use_terminal_events(move |event| {
                if let TerminalEvent::Key(KeyEvent {
                    code: KeyCode::Char('q'),
                    kind: KeyEventKind::Press,
                    ..
                }) = event
                {
                    done.set(true);
                }
            });

            if done.get() {
                system.exit();
            }

            element! {
                View(width: 20, height: 10) {
                    ScrollView {
                        Text(content: "Short")
                    }
                }
            }
        }

        let canvases: Vec<_> = element!(ShortContent)
            .mock_terminal_render_loop(MockTerminalConfig::with_events(stream::iter(vec![
                TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Down)),
                TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Char('q'))),
            ])))
            .collect()
            .await;

        let output = canvases.last().unwrap().to_string();
        assert!(output.contains("Short"));
    }

    #[apply(test!)]
    async fn test_scroll_view_auto_scroll() {
        #[component]
        fn AutoScrollContent(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
            let mut system = hooks.use_context_mut::<SystemContext>();
            let mut done = hooks.use_state(|| false);

            hooks.use_terminal_events(move |event| {
                if let TerminalEvent::Key(KeyEvent {
                    code: KeyCode::Char('q'),
                    kind: KeyEventKind::Press,
                    ..
                }) = event
                {
                    done.set(true);
                }
            });

            if done.get() {
                system.exit();
            }

            let mut lines = String::new();
            for i in 0..20 {
                if i > 0 {
                    lines.push('\n');
                }
                lines.push_str(&format!("Line {i}"));
            }

            element! {
                View(width: 20, height: 5) {
                    ScrollView(auto_scroll: true) {
                        Text(content: lines)
                    }
                }
            }
        }

        let canvases: Vec<_> = element!(AutoScrollContent)
            .mock_terminal_render_loop(MockTerminalConfig::with_events(stream::iter(vec![
                TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Char('q'))),
            ])))
            .collect()
            .await;

        let output = canvases.last().unwrap().to_string();
        // With auto_scroll and content exceeding viewport, should see the last lines.
        assert!(output.contains("Line 19"));
        assert!(!output.contains("Line 0"));
    }

    #[apply(test!)]
    async fn test_scroll_view_drain_scroll_up_from_auto_bottom_starts_at_bottom() {
        #[component]
        fn AutoDrainContent(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
            let mut system = hooks.use_context_mut::<SystemContext>();
            let handle = hooks.use_ref_default::<ScrollViewHandle>();
            let mut ticks = hooks.use_state(|| 0usize);
            let mut did_queue = hooks.use_state(|| false);
            hooks.use_interval(
                {
                    let mut handle = handle;
                    move || {
                        let next_tick = ticks.get().saturating_add(1);
                        ticks.set(next_tick);
                        if next_tick == 1 && !did_queue.get() {
                            handle.write().scroll_by(-3);
                            did_queue.set(true);
                        }
                    }
                },
                Some(FRAME_INTERVAL),
            );
            if ticks.get() >= 5 {
                system.exit();
            }

            let lines = (0..20)
                .map(|i| format!("Line {i}"))
                .collect::<Vec<_>>()
                .join("\n");

            element! {
                View(width: 20, height: 5) {
                    ScrollView(
                        handle,
                        auto_scroll: true,
                        scroll_step: Some(3),
                        scroll_drain_mode: Some(ScrollDrainMode::Native),
                        scrollbar: Some(false),
                    ) {
                        Text(content: lines)
                    }
                }
            }
        }

        let canvases: Vec<_> = element!(AutoDrainContent)
            .mock_terminal_render_loop(MockTerminalConfig::with_events(stream::iter(vec![])))
            .collect()
            .await;

        let outputs = canvases
            .iter()
            .map(|canvas| canvas.to_string())
            .collect::<Vec<_>>();
        assert!(
            outputs.iter().any(|output| output.contains("Line 12")),
            "scrolling up from sticky bottom with drain should begin at the previous max offset, not top: {outputs:#?}"
        );
    }

    #[apply(test!)]
    async fn test_scroll_view_shows_scrollbar() {
        let canvases: Vec<_> = element!(TestScrollView)
            .mock_terminal_render_loop(MockTerminalConfig::with_events(stream::iter(vec![
                TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Char('q'))),
            ])))
            .collect()
            .await;

        let output = canvases.last().unwrap().to_string();
        // Scrollbar track character should be present when content exceeds viewport.
        assert!(output.contains('\u{2502}')); // │
    }

    #[apply(test!)]
    async fn test_scroll_view_handle_subscribe_and_clamp_bounds() {
        #[component]
        fn HandleApp(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
            let mut system = hooks.use_context_mut::<SystemContext>();
            let handle = hooks.use_ref_default::<ScrollViewHandle>();
            let notifications = hooks.use_state(|| 0usize);
            let subscription: Arc<Mutex<Option<ScrollViewSubscription>>> =
                hooks.use_const_default();

            hooks.use_terminal_events({
                let mut handle = handle;
                move |event| {
                    if matches!(
                        event,
                        TerminalEvent::Key(KeyEvent {
                            code: KeyCode::Char('j'),
                            kind: KeyEventKind::Press,
                            ..
                        })
                    ) {
                        if subscription.lock().unwrap().is_none() {
                            let mut notifications_for_listener = notifications;
                            *subscription.lock().unwrap() =
                                Some(handle.read().subscribe(move || {
                                    notifications_for_listener += 1;
                                }));
                        }
                        let mut handle = handle.write();
                        handle.set_clamp_bounds(Some(2), Some(4));
                        handle.scroll_by(99);
                    }
                }
            });

            if notifications.get() >= 2 && handle.read().get_scroll_top() > 4 {
                system.exit();
            }

            let lines = (0..20)
                .map(|i| format!("Line {i}"))
                .collect::<Vec<_>>()
                .join("\n");

            element! {
                View(width: 20, height: 5) {
                    ScrollView(handle, scrollbar: Some(false)) {
                        Text(content: lines)
                    }
                }
            }
        }

        let canvases: Vec<_> = element!(HandleApp)
            .mock_terminal_render_loop(MockTerminalConfig::with_events(stream::iter(vec![
                TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Char('j'))),
            ])))
            .collect()
            .await;

        let output = canvases.last().unwrap().to_string();
        assert!(
            output.contains("Line 4"),
            "expected visual clamp to render scroll top 4 while logical target runs ahead in {output:?}"
        );
        assert!(!output.contains("Line 0"));
    }

    #[test]
    fn test_scroll_content_top_for_absolute_rect_matches_cc_scroll_to_element_math() {
        let rect = taffy::Rect {
            left: 0,
            right: 10,
            top: 22,
            bottom: 23,
        };
        assert_eq!(
            scroll_content_top_for_absolute_rect(5, 10, rect, -1),
            16,
            "content row = current scroll + absolute target top - viewport top + offset"
        );

        let saturated = taffy::Rect {
            left: 0,
            right: 1,
            top: i32::MAX,
            bottom: i32::MAX,
        };
        assert_eq!(
            scroll_content_top_for_absolute_rect(i32::MAX, -1, saturated, 10),
            i32::MAX,
            "large stale rects should saturate instead of overflowing"
        );
    }

    #[apply(test!)]
    async fn test_scroll_view_no_scrollbar_when_disabled() {
        #[component]
        fn NoScrollbar(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
            let mut system = hooks.use_context_mut::<SystemContext>();
            let mut done = hooks.use_state(|| false);

            hooks.use_terminal_events(move |event| {
                if let TerminalEvent::Key(KeyEvent {
                    code: KeyCode::Char('q'),
                    kind: KeyEventKind::Press,
                    ..
                }) = event
                {
                    done.set(true);
                }
            });

            if done.get() {
                system.exit();
            }

            let mut lines = String::new();
            for i in 0..20 {
                if i > 0 {
                    lines.push('\n');
                }
                lines.push_str(&format!("Line {i}"));
            }

            element! {
                View(width: 20, height: 5) {
                    ScrollView(scrollbar: Some(false)) {
                        Text(content: lines)
                    }
                }
            }
        }

        let canvases: Vec<_> = element!(NoScrollbar)
            .mock_terminal_render_loop(MockTerminalConfig::with_events(stream::iter(vec![
                TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Char('q'))),
            ])))
            .collect()
            .await;

        let output = canvases.last().unwrap().to_string();
        // Scrollbar characters should not be present.
        assert!(!output.contains('\u{2502}')); // │
        assert!(!output.contains('\u{2503}')); // ┃
    }
}
