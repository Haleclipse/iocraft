use crate::{
    canvas::{Canvas, CanvasSubviewMut, DamageRegion, ScrollHint},
    component::{ComponentHelperExt, Components, InstantiatedComponent},
    context::{Context, ContextStack, ExitOnCtrlCContext, SystemContext},
    element::ElementExt,
    multimap::AppendOnlyMultimap,
    props::AnyProps,
    terminal::{
        plan_terminal_scroll_hint_patch, terminal_scroll_hint_to_ansi, MockTerminalConfig,
        MockTerminalOutputStream, PendingTerminalFlush, PendingTerminalQuery, Terminal,
        TerminalEvents, TerminalQuery, TerminalScrollHintBounds, TerminalScrollHintPatchOptions,
        TerminalScrollHintPatchPlan, TerminalScrollHintPatchSkipReason,
        TerminalScrollHintRejection,
    },
};
use core::{
    any::Any,
    cell::{Ref, RefMut},
    pin::Pin,
    task::{self, Poll},
};
use futures::{
    future::{select, FutureExt, LocalBoxFuture},
    stream::{Stream, StreamExt},
};
use std::{
    collections::{HashMap, HashSet},
    hash::Hash,
    io,
    time::Duration,
};
use taffy::{
    AvailableSpace, Display, Layout, NodeId, Overflow, Point, Position, Rect, Size, Style,
    TaffyTree,
};

pub(crate) struct UpdateContext<'a, 'w> {
    terminal: Option<&'a mut Terminal<'w>>,
    layout_engine: &'a mut LayoutEngine,
    did_clear_terminal_output: bool,
    force_full_repaint: bool,
    invalidate_prev_frame: bool,
}

/// Callback invoked after a terminal render-loop frame when frame profiling is enabled.
pub type FrameProfileCallback<'a> = Box<dyn FnMut(RenderFrameProfile) + Send + 'a>;

/// Per-frame render-loop profiling event.
///
/// This is the Rust-native counterpart to CC Ink's `FrameEvent`: it reports
/// phase timings, repaint cause, canvas size, and retained-canvas change counts
/// without forcing any particular logging or analytics policy into the framework.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenderFrameProfile {
    /// Total wall-clock duration of the frame.
    pub duration: Duration,
    /// Phase timings and counters collected for the frame.
    pub phases: RenderFramePhases,
    /// Repaint metadata for the frame, if terminal output was written.
    pub repaint: Option<DebugRepaintInfo>,
}

/// Phase timings and counters for one render-loop frame.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RenderFramePhases {
    /// Component update/reconciliation time.
    pub update: Duration,
    /// Layout calculation time.
    pub layout: Duration,
    /// Canvas drawing time.
    pub draw: Duration,
    /// Terminal diff/write/cursor-positioning time.
    pub terminal_write: Duration,
    /// Number of retained-canvas cells that differed from the previous frame.
    pub changed_cells: usize,
    /// Width of the rendered canvas in terminal cells.
    pub canvas_width: usize,
    /// Height of the rendered canvas in terminal rows.
    pub canvas_height: usize,
}

/// Accumulator for [`RenderFrameProfile`] events.
///
/// Benchmark harnesses can pass [`RenderLoopFuture::on_frame_profile`](crate::RenderLoopFuture::on_frame_profile)
/// a closure that records into this struct and then compare totals, maxima, or
/// averages across renderer implementations. The helper is deliberately small
/// and deterministic: it does not sample clocks itself, spawn threads, write
/// logs, or depend on terminal mode.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RenderFrameProfileStats {
    /// Number of frames recorded.
    pub frames: usize,
    /// Number of frames that wrote terminal output.
    pub repaint_frames: usize,
    /// Total wall-clock frame duration.
    pub total_duration: Duration,
    /// Maximum single-frame duration.
    pub max_duration: Duration,
    /// Total update/reconciliation time.
    pub total_update: Duration,
    /// Total layout time.
    pub total_layout: Duration,
    /// Total canvas draw time.
    pub total_draw: Duration,
    /// Total terminal diff/write/cursor-positioning time.
    pub total_terminal_write: Duration,
    /// Sum of retained-canvas changed cells across frames.
    pub total_changed_cells: usize,
    /// Maximum retained-canvas changed cells in any frame.
    pub max_changed_cells: usize,
}

impl RenderFrameProfileStats {
    /// Records one frame profile.
    pub fn record(&mut self, event: &RenderFrameProfile) {
        self.frames = self.frames.saturating_add(1);
        if event.repaint.is_some() {
            self.repaint_frames = self.repaint_frames.saturating_add(1);
        }
        saturating_duration_add(&mut self.total_duration, event.duration);
        self.max_duration = self.max_duration.max(event.duration);
        saturating_duration_add(&mut self.total_update, event.phases.update);
        saturating_duration_add(&mut self.total_layout, event.phases.layout);
        saturating_duration_add(&mut self.total_draw, event.phases.draw);
        saturating_duration_add(&mut self.total_terminal_write, event.phases.terminal_write);
        self.total_changed_cells = self
            .total_changed_cells
            .saturating_add(event.phases.changed_cells);
        self.max_changed_cells = self.max_changed_cells.max(event.phases.changed_cells);
    }

    /// Average frame duration, or zero when no frames were recorded.
    pub fn average_duration(&self) -> Duration {
        average_duration(self.total_duration, self.frames)
    }

    /// Average update/reconciliation time.
    pub fn average_update(&self) -> Duration {
        average_duration(self.total_update, self.frames)
    }

    /// Average layout time.
    pub fn average_layout(&self) -> Duration {
        average_duration(self.total_layout, self.frames)
    }

    /// Average canvas draw time.
    pub fn average_draw(&self) -> Duration {
        average_duration(self.total_draw, self.frames)
    }

    /// Average terminal write time.
    pub fn average_terminal_write(&self) -> Duration {
        average_duration(self.total_terminal_write, self.frames)
    }

    /// Average changed cell count per frame.
    pub fn average_changed_cells(&self) -> f64 {
        if self.frames == 0 {
            0.0
        } else {
            self.total_changed_cells as f64 / self.frames as f64
        }
    }

    /// Fraction of recorded frames that repainted terminal output.
    pub fn repaint_ratio(&self) -> f64 {
        if self.frames == 0 {
            0.0
        } else {
            self.repaint_frames as f64 / self.frames as f64
        }
    }
}

fn saturating_duration_add(total: &mut Duration, delta: Duration) {
    *total = total.checked_add(delta).unwrap_or(Duration::MAX);
}

fn average_duration(total: Duration, count: usize) -> Duration {
    if count == 0 {
        Duration::ZERO
    } else {
        Duration::from_secs_f64(total.as_secs_f64() / count as f64)
    }
}

/// Reason a frame wrote terminal output.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DebugRepaintReason {
    /// No previous frame existed, so the terminal needed an initial paint.
    FirstFrame,
    /// Application code cleared terminal output during update.
    TerminalCleared,
    /// Dynamic alternate-screen state changed and invalidated the previous buffer.
    AlternateScreenChanged,
    /// The terminal size changed.
    TerminalResized,
    /// The current canvas requested a full repaint.
    ForceFullRepaint,
    /// The current canvas carries explicit damage metadata.
    CurrentDamage,
    /// The previous retained canvas carried damage metadata that must be cleaned up.
    PreviousDamage,
    /// The logical retained canvas changed.
    CanvasChanged,
}

/// Debug repaint metadata for a frame that wrote terminal output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DebugRepaintInfo {
    /// Why the frame wrote terminal output.
    pub reason: DebugRepaintReason,
    /// Current canvas damage region, if any.
    pub damage: Option<DamageRegion>,
    /// Previous canvas damage region, if any.
    pub previous_damage: Option<DamageRegion>,
    /// Number of changed retained-canvas cells observed for this frame.
    pub changed_cells: usize,
    /// Width of the rendered canvas in terminal cells.
    pub canvas_width: usize,
    /// Height of the rendered canvas in terminal rows.
    pub canvas_height: usize,
}

#[derive(Clone, Copy, Debug, Default)]
struct RenderPhaseProfile {
    update: Duration,
    layout: Duration,
    draw: Duration,
}

/// Cached layout bounds for a rendered node.
///
/// This is the Rust counterpart to CC Ink's `node-cache.ts` `CachedLayout`:
/// custom retained renderers can store a node's previous absolute bounds and
/// decide whether a clean subtree may be restored via a screen-buffer blit.
/// `top` is the parent-local vertical layout position used by scroll viewport
/// culling optimizations; it may be omitted by renderers that do not need it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CachedLayoutBounds {
    /// Absolute X position in terminal cells.
    pub x: i32,
    /// Absolute Y position in terminal rows.
    pub y: i32,
    /// Width in terminal cells.
    pub width: i32,
    /// Height in terminal rows.
    pub height: i32,
    /// Optional parent-local top position.
    pub top: Option<i32>,
}

/// Cached rectangle used by [`RendererNodeCache`] pending clears.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CachedClearRegion {
    /// Absolute X position in terminal cells.
    pub x: i32,
    /// Absolute Y position in terminal rows.
    pub y: i32,
    /// Width in terminal cells.
    pub width: i32,
    /// Height in terminal rows.
    pub height: i32,
}

impl From<CachedLayoutBounds> for CachedClearRegion {
    fn from(value: CachedLayoutBounds) -> Self {
        Self {
            x: value.x,
            y: value.y,
            width: value.width,
            height: value.height,
        }
    }
}

impl CachedClearRegion {
    /// Clips this possibly-negative cached rectangle to a canvas and returns a
    /// [`DamageRegion`] suitable for [`Canvas::clear_region`] or
    /// [`Canvas::blit_region_from_excluding_clears`].
    ///
    /// This mirrors CC Ink `output.ts` clear-region clamping: absolute overlays
    /// can have negative coordinates, but retained buffers only track the
    /// visible intersection.
    pub fn clipped_to_canvas(self, width: usize, height: usize) -> Option<DamageRegion> {
        if self.width <= 0 || self.height <= 0 || width == 0 || height == 0 {
            return None;
        }

        let left = self.x.max(0) as usize;
        let top = self.y.max(0) as usize;
        let right = self.x.saturating_add(self.width).max(0) as usize;
        let bottom = self.y.saturating_add(self.height).max(0) as usize;
        let right = right.min(width);
        let bottom = bottom.min(height);
        if right <= left || bottom <= top {
            return None;
        }

        Some(DamageRegion {
            x: left,
            y: top,
            width: right - left,
            height: bottom - top,
        })
    }
}

/// Stable retained-renderer node identity with an explicit generation.
///
/// CC Ink can key caches by mutable DOM object identity (`WeakMap<DOMElement,
/// ...>`), so a removed component cannot accidentally reuse an old layout cache
/// unless it is the same DOM object. Rust retained renderers often start from a
/// caller-owned logical key (`row-42`, component id, etc.); adding a generation
/// makes key reuse/remounts explicit and prevents stale blits after removal.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct RendererStableNodeId<K> {
    /// Caller-owned logical key.
    pub key: K,
    /// Monotonic generation for this key.
    pub generation: u64,
}

/// Tracks generations for retained renderer node identifiers.
///
/// This helper is opt-in and mode-neutral. It does not inspect iocraft's
/// component tree or mutate caches by itself; callers can use the returned
/// [`RendererStableNodeId`] values as keys for [`RendererNodeCache`],
/// [`RendererDirtyTree`], or [`RendererRetainedTreeState`].
#[derive(Clone, Debug)]
pub struct RendererNodeGenerationState<K> {
    generations: HashMap<K, u64>,
}

impl<K> Default for RendererNodeGenerationState<K> {
    fn default() -> Self {
        Self {
            generations: HashMap::new(),
        }
    }
}

impl<K> RendererNodeGenerationState<K>
where
    K: Eq + Hash + Clone,
{
    /// Creates an empty generation tracker.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the current stable id for `key`, inserting generation `0` if needed.
    pub fn current_id(&mut self, key: K) -> RendererStableNodeId<K> {
        let generation = *self.generations.entry(key.clone()).or_insert(0);
        RendererStableNodeId { key, generation }
    }

    /// Returns the current stable id for `key` without inserting it.
    pub fn id(&self, key: &K) -> Option<RendererStableNodeId<K>> {
        self.generations
            .get(key)
            .copied()
            .map(|generation| RendererStableNodeId {
                key: key.clone(),
                generation,
            })
    }

    /// Bumps `key` to a new generation and returns the new stable id.
    pub fn remount(&mut self, key: K) -> RendererStableNodeId<K> {
        let generation = self
            .generations
            .entry(key.clone())
            .and_modify(|generation| *generation = generation.saturating_add(1))
            .or_insert(0);
        RendererStableNodeId {
            key,
            generation: *generation,
        }
    }

    /// Marks `key` removed by bumping its next generation.
    ///
    /// The returned id is the generation that was live before removal, suitable
    /// for dropping retained cache entries or queuing clears. Calling
    /// [`Self::current_id`] for the same logical key afterwards returns a fresh
    /// generation that will not collide with old cache keys.
    pub fn remove(&mut self, key: &K) -> Option<RendererStableNodeId<K>> {
        let generation = self.generations.get_mut(key)?;
        let removed = RendererStableNodeId {
            key: key.clone(),
            generation: *generation,
        };
        *generation = generation.saturating_add(1);
        Some(removed)
    }

    /// Bumps every tracked logical key that is not in `keys`.
    ///
    /// Returns the stable ids that were live before removal. This is useful for
    /// list diffing: remove those ids from retained caches, then ask
    /// [`Self::current_id`] for surviving/new keys. Generation tombstones remain
    /// tracked so a later reinserted logical key cannot collide with old caches.
    pub fn bump_unretained_keys<I>(&mut self, keys: I) -> Vec<RendererStableNodeId<K>>
    where
        I: IntoIterator<Item = K>,
    {
        let keep = keys.into_iter().collect::<HashSet<_>>();
        let removed_keys = self
            .generations
            .keys()
            .filter(|key| !keep.contains(*key))
            .cloned()
            .collect::<Vec<_>>();
        removed_keys
            .into_iter()
            .filter_map(|key| self.remove(&key))
            .collect()
    }

    /// Number of logical keys tracked.
    pub fn len(&self) -> usize {
        self.generations.len()
    }

    /// Whether no logical keys are tracked.
    pub fn is_empty(&self) -> bool {
        self.generations.is_empty()
    }

    /// Clears all generations.
    pub fn clear(&mut self) {
        self.generations.clear();
    }
}

/// Mode-neutral plan for a retained scroll blit/shift fast path.
///
/// This is the Rust-native, opt-in counterpart to CC Ink's ScrollBox fast path
/// in `render-node-to-output.ts`: custom renderers can blit the previous
/// viewport, shift it by [`Self::delta`], repaint [`Self::edge_region`], and
/// repaint [`Self::absolute_repair_regions`] that may contain shifted copies of
/// previously-rendered absolute overlays. The plan does not mutate a canvas,
/// write terminal output, emit DECSTBM, or change screen mode.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScrollFastPathPlan {
    /// Previous viewport region to copy before shifting.
    pub blit_region: CachedClearRegion,
    /// Scroll delta in rows; positive means content moved up / scrollTop increased.
    pub delta: i32,
    /// Newly exposed edge rows that must be cleared and repainted after the shift.
    pub edge_region: CachedClearRegion,
    /// Full-width row regions to repaint because previous absolute-overlay
    /// pixels were blitted and shifted into stale positions.
    pub absolute_repair_regions: Vec<CachedClearRegion>,
}

/// Child layout metadata used by [`plan_scroll_fast_path_child_repairs`].
///
/// This is a Rust-native substitute for CC Ink's DOM node + `nodeCache` reads in
/// the ScrollBox fast path. `top` and `height` are content-local post-layout
/// coordinates. `cached_y`/`cached_height` describe the previous-frame absolute
/// screen position and height, if the child was present in the retained cache.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScrollFastPathChild<K> {
    /// Caller-owned child identifier returned with any repair region.
    pub key: K,
    /// Current content-local top row of the child.
    pub top: i32,
    /// Current child height in rows.
    pub height: i32,
    /// Previous-frame absolute top row, if cached.
    pub cached_y: Option<i32>,
    /// Previous-frame height, if cached.
    pub cached_height: Option<i32>,
    /// Whether this child was dirty before the edge-row render pass.
    pub dirty: bool,
}

/// Repair region for a child affected by a retained scroll fast path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScrollFastPathChildRepair<K> {
    /// The child identifier supplied in [`ScrollFastPathChild::key`].
    pub key: K,
    /// Full-width visible region that should be cleared and re-rendered.
    pub region: CachedClearRegion,
}

/// Input for [`ScrollFastPathFrameState::plan_frame`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScrollFastPathFrameInput<K> {
    /// Absolute retained-buffer rectangle of the scroll viewport.
    pub viewport: CachedClearRegion,
    /// Current absolute Y position of the scroll content wrapper after `scroll_top` translation.
    pub content_y: i32,
    /// Current committed scroll offset in rows.
    pub scroll_top: i32,
    /// Current scroll content height in rows.
    pub content_height: i32,
    /// Visible child layout metadata used for stable-row repair planning.
    pub children: Vec<ScrollFastPathChild<K>>,
}

/// State-backed ScrollBox fast-path plan for one frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScrollFastPathFramePlan<K> {
    /// Scroll delta in rows; positive means content moved up / scrollTop increased.
    pub delta: i32,
    /// Change in scroll content height since the previous committed frame.
    pub content_height_delta: i32,
    /// Whether the viewport rectangle matched the previous frame.
    pub viewport_stable: bool,
    /// Whether the content-height delta matches CC Ink's safe fast-path guard.
    pub content_delta_safe: bool,
    /// Main blit/shift/edge/absolute repair plan, when the fast path is safe.
    pub fast_path: Option<ScrollFastPathPlan>,
    /// Stable-row child repairs to apply after edge rows are repainted.
    pub child_repairs: Vec<ScrollFastPathChildRepair<K>>,
}

/// Stateful opt-in owner for retained ScrollBox fast-path planning.
///
/// CC Ink keeps previous content-wrapper layout and previous absolute overlay
/// rects in renderer module state so `render-node-to-output.ts` can decide when
/// to blit+shift a scroll viewport, repaint edge rows, then repair dirty or
/// displaced stable rows. This Rust helper keeps the same bookkeeping explicit:
/// callers choose when a frame begins, record absolute rects they rendered, ask
/// for a fast-path plan, and commit the content snapshot after applying it.
/// It never mutates a canvas, emits DECSTBM, writes terminal output, or changes
/// screen mode.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScrollFastPathFrameState {
    previous_viewport: Option<CachedClearRegion>,
    previous_content_y: Option<i32>,
    previous_content_height: Option<i32>,
    previous_absolute_rects: Vec<CachedClearRegion>,
    current_absolute_rects: Vec<CachedClearRegion>,
}

impl ScrollFastPathFrameState {
    /// Creates an empty retained scroll fast-path state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Starts a new frame by promoting current absolute rects to previous rects.
    ///
    /// This mirrors CC Ink's `resetScrollHint()` bookkeeping where absolute
    /// rects from the just-finished frame are used to repair shifted overlay
    /// pixels in the next frame.
    pub fn begin_frame(&mut self) {
        self.previous_absolute_rects = std::mem::take(&mut self.current_absolute_rects);
    }

    /// Records an absolute-positioned rect rendered in the current frame.
    pub fn record_absolute_rect(&mut self, rect: CachedClearRegion) {
        self.current_absolute_rects.push(rect);
    }

    /// Returns previous-frame absolute rects used for shifted-overlay repair.
    pub fn previous_absolute_rects(&self) -> &[CachedClearRegion] {
        &self.previous_absolute_rects
    }

    /// Plans the retained scroll fast path for the current frame.
    pub fn plan_frame<K>(&self, input: ScrollFastPathFrameInput<K>) -> ScrollFastPathFramePlan<K> {
        let viewport_stable = self.previous_viewport == Some(input.viewport);
        let delta = if viewport_stable {
            self.previous_content_y
                .map(|previous| previous.saturating_sub(input.content_y))
                .unwrap_or(0)
        } else {
            0
        };
        let content_height_delta = self
            .previous_content_height
            .map(|previous| input.content_height.saturating_sub(previous))
            .unwrap_or(0);
        let content_delta_safe =
            is_scroll_fast_path_content_delta_safe(delta, content_height_delta);
        let fast_path = if viewport_stable && content_delta_safe {
            plan_scroll_fast_path(
                input.viewport,
                delta,
                self.previous_absolute_rects.iter().copied(),
            )
        } else {
            None
        };
        let child_repairs = fast_path
            .as_ref()
            .map(|plan| {
                plan_scroll_fast_path_child_repairs(
                    input.viewport,
                    input.content_y,
                    input.scroll_top,
                    delta,
                    plan.edge_region,
                    input.children,
                )
            })
            .unwrap_or_default();

        ScrollFastPathFramePlan {
            delta,
            content_height_delta,
            viewport_stable,
            content_delta_safe,
            fast_path,
            child_repairs,
        }
    }

    /// Commits the scroll content snapshot after a frame has been applied.
    pub fn commit_frame(
        &mut self,
        viewport: CachedClearRegion,
        content_y: i32,
        content_height: i32,
    ) {
        self.previous_viewport = Some(viewport);
        self.previous_content_y = Some(content_y);
        self.previous_content_height = Some(content_height);
    }

    /// Clears all retained scroll fast-path state.
    pub fn clear(&mut self) {
        self.previous_viewport = None;
        self.previous_content_y = None;
        self.previous_content_height = None;
        self.previous_absolute_rects.clear();
        self.current_absolute_rects.clear();
    }
}

/// Returns whether a scroll-content height delta is safe for the retained
/// scroll fast path.
///
/// This mirrors CC Ink's `safeForFastPath` guard: pure scroll (`0`) is safe,
/// and bottom-append while scrolling down is safe when the content grew by the
/// same amount as the scroll delta. Other insertion/removal patterns can leave
/// stale shifted rows and should fall back to a full viewport render.
pub fn is_scroll_fast_path_content_delta_safe(delta: i32, content_height_delta: i32) -> bool {
    content_height_delta == 0 || (delta > 0 && content_height_delta == delta)
}

/// Plans CC Ink-style retained ScrollBox blit/shift/repair regions.
///
/// `viewport` is the absolute retained-buffer rectangle of the scroll viewport.
/// `delta > 0` means content moved up (scrollTop increased); `delta < 0` means
/// content moved down. `previous_absolute_rects` are absolute-positioned
/// rectangles from the previous frame. The returned regions are signed
/// [`CachedClearRegion`] values so callers can clip them to their canvas or
/// terminal viewport as appropriate.
///
/// Returns `None` when the shift is not useful/safe for this fast path: empty
/// viewport, zero delta, or `abs(delta) >= viewport.height`. Those cases should
/// use a full viewport render, matching CC Ink's guard before DECSTBM hints are
/// emitted.
pub fn plan_scroll_fast_path(
    viewport: CachedClearRegion,
    delta: i32,
    previous_absolute_rects: impl IntoIterator<Item = CachedClearRegion>,
) -> Option<ScrollFastPathPlan> {
    if viewport.width <= 0 || viewport.height <= 0 || delta == 0 {
        return None;
    }

    let abs_delta = delta.checked_abs().unwrap_or(i32::MAX);
    if abs_delta >= viewport.height {
        return None;
    }

    let viewport_top = i64::from(viewport.y);
    let viewport_bottom_exclusive = viewport_top + i64::from(viewport.height);
    let edge_top = if delta > 0 {
        viewport_bottom_exclusive - i64::from(abs_delta)
    } else {
        viewport_top
    };
    let edge_bottom_exclusive = edge_top + i64::from(abs_delta);
    let edge_region = CachedClearRegion {
        x: viewport.x,
        y: clamp_i64_to_i32(edge_top),
        width: viewport.width,
        height: abs_delta,
    };

    let mut absolute_repair_regions = Vec::new();
    for rect in previous_absolute_rects {
        if rect.height <= 0 || rect.width <= 0 {
            continue;
        }

        let rect_top = i64::from(rect.y);
        let rect_bottom = rect_top + i64::from(rect.height);
        if rect_top >= viewport_bottom_exclusive || rect_bottom <= viewport_top {
            continue;
        }

        let shifted_top = viewport_top.max(rect_top - i64::from(delta));
        let shifted_bottom = viewport_bottom_exclusive.min(rect_bottom - i64::from(delta));
        if shifted_top >= shifted_bottom {
            continue;
        }

        // Edge rows are already cleared and repainted by the first pass.
        if shifted_top >= edge_top && shifted_bottom <= edge_bottom_exclusive {
            continue;
        }

        absolute_repair_regions.push(CachedClearRegion {
            x: viewport.x,
            y: clamp_i64_to_i32(shifted_top),
            width: viewport.width,
            height: clamp_i64_to_i32(shifted_bottom - shifted_top),
        });
    }

    Some(ScrollFastPathPlan {
        blit_region: viewport,
        delta,
        edge_region,
        absolute_repair_regions,
    })
}

/// Converts a retained scroll fast-path plan into a fullscreen scroll hint.
///
/// DECSTBM scroll regions operate on whole terminal rows, so the plan must cover
/// the full retained canvas width (`x == 0 && width == canvas_width`). Partial
/// scroll containers can still use [`apply_scroll_fast_path_to_canvas`] as a
/// retained-canvas optimization, but should not emit terminal scroll-region
/// patches by default.
pub fn scroll_fast_path_plan_to_scroll_hint(
    plan: &ScrollFastPathPlan,
    canvas_width: usize,
) -> Option<ScrollHint> {
    let viewport = plan.blit_region;
    if canvas_width == 0
        || viewport.x != 0
        || viewport.width <= 0
        || viewport.width as usize != canvas_width
        || viewport.y < 0
        || viewport.height <= 0
        || plan.delta == 0
        || plan.delta.unsigned_abs() >= viewport.height as u32
    {
        return None;
    }

    Some(ScrollHint {
        top: viewport.y as usize,
        bottom: viewport.y as usize + viewport.height as usize - 1,
        delta: plan.delta,
    })
}

/// Applies a retained scroll fast-path plan to a caller-owned canvas.
///
/// This is the retained-canvas counterpart to CC Ink's ScrollBox fast path: it
/// blits the previous viewport from `previous`, shifts the copied rows by
/// `plan.delta`, then clears the edge and absolute-overlay repair regions so a
/// caller can repaint them. For full-width viewports it also records a
/// [`ScrollHint`] on `next`; the hint is metadata only and terminal backends
/// still decide whether fullscreen DECSTBM is safe. The helper performs no
/// terminal I/O and does not render children; pair it with
/// [`plan_scroll_fast_path_child_repairs`] for stable-row child repairs.
pub fn apply_scroll_fast_path_to_canvas(
    next: &mut Canvas,
    previous: &Canvas,
    plan: &ScrollFastPathPlan,
) -> bool {
    let Some(viewport) = plan
        .blit_region
        .clipped_to_canvas(next.width(), next.height())
    else {
        return false;
    };
    if viewport.width == 0 || viewport.height == 0 {
        return false;
    }

    next.blit_region_from(
        previous,
        viewport.x,
        viewport.y,
        viewport.width,
        viewport.height,
    );
    next.shift_rows(viewport.y, viewport.y + viewport.height - 1, plan.delta);

    for region in
        std::iter::once(plan.edge_region).chain(plan.absolute_repair_regions.iter().copied())
    {
        if let Some(region) = region.clipped_to_canvas(next.width(), next.height()) {
            next.clear_region(region.x, region.y, region.width, region.height);
        }
    }

    if let Some(hint) = scroll_fast_path_plan_to_scroll_hint(plan, next.width()) {
        next.set_scroll_hint(hint);
    }

    true
}

/// Applies a stateful retained scroll fast-path frame plan to a caller-owned canvas.
///
/// This is the canvas-side skeleton of CC Ink's three-pass ScrollBox fast path:
/// it blits/shifts the previous viewport, clears newly exposed edge rows,
/// clears shifted absolute-overlay repairs, and also clears stable-row child
/// repair regions from [`ScrollFastPathFramePlan::child_repairs`]. Callers are
/// still responsible for re-rendering the edge rows and each repaired child into
/// the cleared regions. The helper performs no terminal I/O, does not schedule
/// another frame, and only records a [`ScrollHint`] when the underlying fast
/// path is full-width and fullscreen-safe.
pub fn apply_scroll_fast_path_frame_plan_to_canvas<K>(
    next: &mut Canvas,
    previous: &Canvas,
    plan: &ScrollFastPathFramePlan<K>,
) -> bool {
    let Some(fast_path) = plan.fast_path.as_ref() else {
        return false;
    };
    if !apply_scroll_fast_path_to_canvas(next, previous, fast_path) {
        return false;
    }

    for repair in &plan.child_repairs {
        if let Some(region) = repair.region.clipped_to_canvas(next.width(), next.height()) {
            next.clear_region(region.x, region.y, region.width, region.height);
        }
    }

    true
}

/// Fullscreen terminal-side bridge for a retained scroll fast-path frame.
///
/// This packages the DECSTBM scroll patch that mirrors CC Ink's fullscreen
/// ScrollBox fast path together with the row regions that a custom renderer must
/// repaint after the terminal performs the hardware scroll. It is explicitly
/// fullscreen-only metadata: it does not write to the terminal, does not wrap the
/// patch in synchronized-output markers, and should not be used by main-screen
/// renderers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScrollFastPathTerminalFramePatch<'a, K> {
    /// Full-width scroll hint derived from the retained fast-path frame.
    pub scroll_hint: ScrollHint,
    /// Serialized DECSTBM/SU/SD/reset/cursor-home patch.
    pub scroll_patch_ansi: String,
    /// Newly exposed edge rows that must be rendered after the hardware scroll.
    pub edge_region: CachedClearRegion,
    /// Regions where previously-rendered absolute overlays were shifted into
    /// stale positions and must be repainted.
    pub absolute_repair_regions: &'a [CachedClearRegion],
    /// Stable-row child repairs that must be cleared/re-rendered after edge rows.
    pub child_repairs: &'a [ScrollFastPathChildRepair<K>],
}

/// Converts a retained scroll fast-path frame into a fullscreen DECSTBM patch plan.
///
/// Returns `Ok(None)` when the frame has no safe fast path or when the viewport
/// is not full-width for `canvas_width`; partial-width scroll containers can
/// still use [`apply_scroll_fast_path_frame_plan_to_canvas`] but must not emit a
/// terminal scroll-region patch. Returns `Err` when the full-width hint fails
/// terminal bounds validation. This low-level serializer assumes the caller has
/// already performed fullscreen/atomic safety gating; use
/// [`plan_scroll_fast_path_frame_terminal_patch`] when you want CC Ink-style
/// `altScreen && decstbmSafe` gating included.
pub fn scroll_fast_path_frame_plan_to_terminal_patch<'a, K>(
    plan: &'a ScrollFastPathFramePlan<K>,
    canvas_width: usize,
    bounds: TerminalScrollHintBounds,
) -> Result<Option<ScrollFastPathTerminalFramePatch<'a, K>>, TerminalScrollHintRejection> {
    let Some(fast_path) = plan.fast_path.as_ref() else {
        return Ok(None);
    };
    let Some(scroll_hint) = scroll_fast_path_plan_to_scroll_hint(fast_path, canvas_width) else {
        return Ok(None);
    };
    let scroll_patch_ansi = terminal_scroll_hint_to_ansi(scroll_hint, bounds)?;

    Ok(Some(ScrollFastPathTerminalFramePatch {
        scroll_hint,
        scroll_patch_ansi,
        edge_region: fast_path.edge_region,
        absolute_repair_regions: &fast_path.absolute_repair_regions,
        child_repairs: &plan.child_repairs,
    }))
}

/// Terminal patch request for [`apply_scroll_fast_path_frame_plan`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScrollFastPathTerminalFramePatchRequest {
    /// Previous/next retained screen bounds used to validate the scroll hint.
    pub bounds: TerminalScrollHintBounds,
    /// Fullscreen and synchronized-output safety gate.
    pub options: TerminalScrollHintPatchOptions,
}

/// Outcome of planning a guarded fullscreen terminal patch for a retained scroll frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScrollFastPathTerminalFramePatchPlan<'a, K> {
    /// Emit this DECSTBM patch before repainting the returned repair regions.
    Emit(ScrollFastPathTerminalFramePatch<'a, K>),
    /// Skip DECSTBM and fall back to the normal diff path for this frame.
    Skip(TerminalScrollHintPatchSkipReason),
}

/// Plans a retained scroll fast-path frame terminal patch with fullscreen/atomic gates.
///
/// `Ok(None)` still means there is no full-width retained fast path for this
/// frame. `Ok(Some(Skip(...)))` means the frame had a full-width scroll hint, but
/// CC Ink's fullscreen/atomic safety gate rejected terminal-side DECSTBM.
pub fn plan_scroll_fast_path_frame_terminal_patch<'a, K>(
    plan: &'a ScrollFastPathFramePlan<K>,
    canvas_width: usize,
    request: ScrollFastPathTerminalFramePatchRequest,
) -> Result<Option<ScrollFastPathTerminalFramePatchPlan<'a, K>>, TerminalScrollHintRejection> {
    let Some(fast_path) = plan.fast_path.as_ref() else {
        return Ok(None);
    };
    let Some(scroll_hint) = scroll_fast_path_plan_to_scroll_hint(fast_path, canvas_width) else {
        return Ok(None);
    };

    match plan_terminal_scroll_hint_patch(scroll_hint, request.bounds, request.options)? {
        TerminalScrollHintPatchPlan::Emit(scroll_patch_ansi) => Ok(Some(
            ScrollFastPathTerminalFramePatchPlan::Emit(ScrollFastPathTerminalFramePatch {
                scroll_hint,
                scroll_patch_ansi,
                edge_region: fast_path.edge_region,
                absolute_repair_regions: &fast_path.absolute_repair_regions,
                child_repairs: &plan.child_repairs,
            }),
        )),
        TerminalScrollHintPatchPlan::Skip(reason) => {
            Ok(Some(ScrollFastPathTerminalFramePatchPlan::Skip(reason)))
        }
    }
}

/// Result of applying a retained scroll fast-path frame skeleton.
///
/// `canvas_applied` reports whether the retained-canvas blit/shift/clear
/// skeleton was applied. `terminal_patch` is only populated when the caller also
/// supplied a fullscreen/atomic-safe terminal patch request and the frame's
/// viewport was full-width.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScrollFastPathFrameApplication<'a, K> {
    /// Whether [`apply_scroll_fast_path_frame_plan_to_canvas`] applied the skeleton.
    pub canvas_applied: bool,
    /// Optional fullscreen DECSTBM patch metadata for custom terminal renderers.
    pub terminal_patch: Option<ScrollFastPathTerminalFramePatch<'a, K>>,
    /// Reason a full-width terminal patch was skipped by the safety gate.
    pub terminal_patch_skip_reason: Option<TerminalScrollHintPatchSkipReason>,
}

impl<K> ScrollFastPathFrameApplication<'_, K> {
    /// Applies this application's terminal scroll shift to a previous-frame canvas clone.
    ///
    /// Custom fullscreen renderers should call this after writing the DECSTBM
    /// patch and before computing their sparse diff. It mirrors CC Ink's
    /// `shiftRows(prev.screen, top, bottom, delta)` step so the diff sees only
    /// newly exposed edge rows and explicit repair regions instead of the entire
    /// shifted viewport. Returns `false` when no terminal patch was emitted or
    /// the patch no longer fits the supplied canvas.
    pub fn shift_previous_canvas_for_terminal_diff(&self, previous: &mut Canvas) -> bool {
        self.terminal_patch.as_ref().is_some_and(|patch| {
            apply_scroll_fast_path_terminal_patch_to_previous_canvas(previous, patch)
        })
    }
}

/// Applies a fullscreen scroll-fast-path terminal patch to a previous-frame canvas clone.
///
/// This is the retained-canvas mirror of the terminal's hardware scroll and CC
/// Ink's `log-update.ts` `shiftRows(prev.screen, ...)` step. It does not write
/// to the terminal and does not repaint repair regions; callers use the shifted
/// previous canvas as the baseline for their normal sparse diff after repainting
/// edge/repair rows into the next canvas.
pub fn apply_scroll_fast_path_terminal_patch_to_previous_canvas<K>(
    previous: &mut Canvas,
    patch: &ScrollFastPathTerminalFramePatch<'_, K>,
) -> bool {
    let hint = patch.scroll_hint;
    if hint.top > hint.bottom || hint.bottom >= previous.height() || hint.delta == 0 {
        return false;
    }

    let region_height = hint.bottom - hint.top + 1;
    if hint.delta.unsigned_abs() as usize >= region_height {
        return false;
    }

    previous.shift_rows(hint.top, hint.bottom, hint.delta);
    true
}

/// Applies a retained scroll fast-path frame skeleton and optionally prepares a terminal patch.
///
/// This is the opt-in componentized path for custom fullscreen renderers: it
/// mutates only the caller-owned `next` canvas (blit/shift/clear skeleton) and,
/// when `terminal_patch_request` is supplied, returns the fullscreen DECSTBM
/// patch plus repair metadata only if the request says fullscreen + atomic output
/// is safe. Passing `None` keeps the helper retained-canvas-only. The function
/// never writes to the terminal and never changes the default renderer.
pub fn apply_scroll_fast_path_frame_plan<'a, K>(
    next: &mut Canvas,
    previous: &Canvas,
    plan: &'a ScrollFastPathFramePlan<K>,
    terminal_patch_request: Option<ScrollFastPathTerminalFramePatchRequest>,
) -> Result<ScrollFastPathFrameApplication<'a, K>, TerminalScrollHintRejection> {
    let canvas_applied = apply_scroll_fast_path_frame_plan_to_canvas(next, previous, plan);
    let (terminal_patch, terminal_patch_skip_reason) = if canvas_applied {
        match terminal_patch_request {
            Some(request) => {
                match plan_scroll_fast_path_frame_terminal_patch(plan, next.width(), request)? {
                    Some(ScrollFastPathTerminalFramePatchPlan::Emit(patch)) => (Some(patch), None),
                    Some(ScrollFastPathTerminalFramePatchPlan::Skip(reason)) => {
                        (None, Some(reason))
                    }
                    None => (None, None),
                }
            }
            None => (None, None),
        }
    } else {
        (None, None)
    };

    Ok(ScrollFastPathFrameApplication {
        canvas_applied,
        terminal_patch,
        terminal_patch_skip_reason,
    })
}

/// Plans stable-row child repairs after a retained scroll blit/shift pass.
///
/// This mirrors CC Ink's second ScrollBox pass in a mode-neutral form. Edge rows
/// are assumed to have already been repainted. The returned regions cover dirty
/// children in stable rows, uncached children that were not painted by the blit,
/// and clean children after a middle-growth point whose old pixels were shifted
/// to the wrong screen row. The regions are clipped to `viewport` and full-width
/// so callers can clear stale shifted content before re-rendering the child.
///
/// The helper only computes repair metadata. It does not inspect iocraft's
/// component tree, mutate a cache, write terminal output, or schedule another
/// render frame.
pub fn plan_scroll_fast_path_child_repairs<K>(
    viewport: CachedClearRegion,
    content_y: i32,
    scroll_top: i32,
    delta: i32,
    edge_region: CachedClearRegion,
    children: impl IntoIterator<Item = ScrollFastPathChild<K>>,
) -> Vec<ScrollFastPathChildRepair<K>> {
    if viewport.width <= 0 || viewport.height <= 0 {
        return Vec::new();
    }

    let viewport_top = i64::from(viewport.y);
    let viewport_bottom = viewport_top + i64::from(viewport.height);
    let visible_top = i64::from(scroll_top);
    let visible_bottom = visible_top + i64::from(viewport.height);
    let edge_top_local = i64::from(edge_region.y) - i64::from(content_y);
    let edge_bottom_local = edge_top_local + i64::from(edge_region.height.max(0));
    let mut cumulative_height_shift = 0i64;
    let mut repairs = Vec::new();

    for child in children {
        let child_top = i64::from(child.top);
        let child_height = i64::from(child.height);
        if child_height <= 0 {
            continue;
        }

        if !child.dirty && cumulative_height_shift == 0 && child.cached_y.is_some() {
            continue;
        }

        let child_bottom = child_top + child_height;
        if child.dirty {
            let previous_height = i64::from(child.cached_height.unwrap_or(0));
            cumulative_height_shift += child_height - previous_height;
        }

        if child_bottom <= visible_top || child_top >= visible_bottom {
            continue;
        }

        if child_top >= edge_top_local && child_bottom <= edge_bottom_local {
            continue;
        }

        let screen_y = i64::from(content_y) + child_top;
        if !child.dirty {
            if let Some(cached_y) = child.cached_y {
                if i64::from(cached_y) - i64::from(delta) == screen_y {
                    continue;
                }
            }
        }

        let repair_top = viewport_top.max(screen_y);
        let repair_bottom = viewport_bottom.min(i64::from(content_y) + child_bottom);
        if repair_top >= repair_bottom {
            continue;
        }

        repairs.push(ScrollFastPathChildRepair {
            key: child.key,
            region: CachedClearRegion {
                x: viewport.x,
                y: clamp_i64_to_i32(repair_top),
                width: viewport.width,
                height: clamp_i64_to_i32(repair_bottom - repair_top),
            },
        });
    }

    repairs
}

fn clamp_i64_to_i32(value: i64) -> i32 {
    value.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

/// Child metadata for [`plan_retained_child_blits`].
///
/// This is the mode-neutral shape of the per-child state used by CC Ink's
/// `renderChildren(...)` contamination guard in `render-node-to-output.ts`.
/// It is intentionally independent of iocraft's component tree and canvas.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetainedChildBlitInput<K> {
    /// Caller-owned child identifier returned in the corresponding decision.
    pub key: K,
    /// Whether the child was dirty before rendering this frame.
    pub dirty: bool,
    /// Whether the child's own overflow clips on both axes.
    pub clips_both_axes: bool,
    /// Whether the child is absolutely positioned.
    pub absolute: bool,
    /// Whether the child fills its layout rectangle opaquely.
    pub opaque: bool,
    /// Whether the child has an explicit background fill.
    pub has_background: bool,
}

/// Per-child retained-blit decision returned by [`plan_retained_child_blits`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetainedChildBlitDecision<K> {
    /// Caller-owned child identifier from [`RetainedChildBlitInput::key`].
    pub key: K,
    /// Whether the caller may pass the previous retained screen/canvas to this
    /// child subtree for descendant blits.
    pub allow_previous_screen: bool,
    /// Whether this child must skip its own direct node-level blit even if its
    /// cached layout matches.
    pub skip_self_blit: bool,
}

/// Plans sibling retained-blit contamination guards.
///
/// This mirrors CC Ink's `renderChildren(...)` sibling-overflow guard in a
/// Rust-native, optimization-only helper. A dirty unclipped child can paint
/// outside its layout bounds, so later siblings must not blit from the previous
/// screen. A dirty clipped child is safe for ordinary later siblings, but a
/// later non-opaque absolute sibling overlapping the clipped region must skip
/// its own direct blit to avoid restoring stale cells. The dirty child itself
/// still receives the previous screen unless a child was removed before the
/// frame; its own direct blit should fail because it is dirty, but clean
/// descendants may still benefit.
///
/// The helper computes metadata only: it does not mutate a cache, inspect
/// layout nodes, draw, write terminal output, or change screen mode.
pub fn plan_retained_child_blits<K>(
    has_removed_child: bool,
    children: impl IntoIterator<Item = RetainedChildBlitInput<K>>,
) -> Vec<RetainedChildBlitDecision<K>> {
    let mut seen_dirty_unclipped = false;
    let mut seen_dirty_clipped = false;
    let mut decisions = Vec::new();

    for child in children {
        let allow_previous_screen = !has_removed_child && !seen_dirty_unclipped;
        let skip_self_blit =
            seen_dirty_clipped && child.absolute && !child.opaque && !child.has_background;

        let dirty = child.dirty;
        let clips_both_axes = child.clips_both_axes;
        let absolute = child.absolute;
        decisions.push(RetainedChildBlitDecision {
            key: child.key,
            allow_previous_screen,
            skip_self_blit,
        });

        if dirty && !seen_dirty_unclipped {
            if !clips_both_axes || absolute {
                seen_dirty_unclipped = true;
            } else {
                seen_dirty_clipped = true;
            }
        }
    }

    decisions
}

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

/// Cached absolute descendant input for [`plan_escaping_absolute_descendant_blits`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AbsoluteDescendantRect<K> {
    /// Caller-owned descendant identifier returned with any required blit.
    pub key: K,
    /// Previous-frame cached absolute rectangle for the absolute descendant.
    pub rect: CachedClearRegion,
}

/// Blit required for an absolute descendant that escapes its parent layout box.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EscapingAbsoluteDescendantBlit<K> {
    /// Caller-owned descendant identifier from [`AbsoluteDescendantRect::key`].
    pub key: K,
    /// Previous-frame absolute rectangle that should be restored from the retained screen.
    pub rect: CachedClearRegion,
}

/// Plans blits for absolute descendants that paint outside a blitted parent.
///
/// CC Ink's `blitEscapingAbsoluteDescendants(...)` repairs a retained parent
/// blit by re-blitting cached absolute descendants whose rectangles extend
/// outside the parent's layout bounds. Otherwise a dirty sibling that repainted
/// those outside cells can make an overlay disappear when the clean parent is
/// restored from the previous screen.
///
/// This helper is mode-neutral and optimization-only: callers provide the
/// already-discovered cached absolute descendant rectangles, receive only the
/// escaping rects that need a retained-buffer blit, and remain responsible for
/// cache traversal, clipping, drawing, and terminal output.
pub fn plan_escaping_absolute_descendant_blits<K>(
    parent: CachedClearRegion,
    descendants: impl IntoIterator<Item = AbsoluteDescendantRect<K>>,
) -> Vec<EscapingAbsoluteDescendantBlit<K>> {
    let parent_left = i64::from(parent.x);
    let parent_top = i64::from(parent.y);
    let parent_right = parent_left + i64::from(parent.width);
    let parent_bottom = parent_top + i64::from(parent.height);
    let mut blits = Vec::new();

    for descendant in descendants {
        let rect = descendant.rect;
        if rect.width <= 0 || rect.height <= 0 {
            continue;
        }

        let left = i64::from(rect.x);
        let top = i64::from(rect.y);
        let right = left + i64::from(rect.width);
        let bottom = top + i64::from(rect.height);
        if left < parent_left || top < parent_top || right > parent_right || bottom > parent_bottom
        {
            blits.push(EscapingAbsoluteDescendantBlit {
                key: descendant.key,
                rect,
            });
        }
    }

    blits
}

/// Canvas repair applied for an escaping absolute descendant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EscapingAbsoluteDescendantCanvasBlit<K> {
    /// Caller-owned descendant identifier from [`EscapingAbsoluteDescendantBlit::key`].
    pub key: K,
    /// Canvas-clipped rectangle restored from the previous retained canvas.
    pub region: DamageRegion,
}

/// Applies planned escaping absolute-descendant repairs to a canvas.
///
/// This is the canvas-side companion to
/// [`plan_escaping_absolute_descendant_blits`]. CC Ink re-blits cached absolute
/// descendants after a clean parent subtree was restored, because those
/// descendants may have painted outside the parent's layout rectangle and can be
/// overwritten by dirty siblings. This helper keeps that behavior opt-in and
/// renderer-owned: it only copies clipped rectangles from `previous` to `next`
/// and reports what was copied.
pub fn apply_escaping_absolute_descendant_blits_to_canvas<K>(
    next: &mut Canvas,
    previous: &Canvas,
    blits: impl IntoIterator<Item = EscapingAbsoluteDescendantBlit<K>>,
) -> Vec<EscapingAbsoluteDescendantCanvasBlit<K>> {
    let mut applied = Vec::new();
    for blit in blits {
        let Some(region) = blit.rect.clipped_to_canvas(next.width(), next.height()) else {
            continue;
        };
        next.blit_region_from(previous, region.x, region.y, region.width, region.height);
        applied.push(EscapingAbsoluteDescendantCanvasBlit {
            key: blit.key,
            region,
        });
    }
    applied
}

/// Layout snapshot used by [`RendererLayoutShiftTracker`].
///
/// This is a stable, renderer-agnostic counterpart to CC Ink's per-node cached
/// layout fields used by `resetLayoutShifted()` / `didLayoutShift()`. It avoids
/// binding public APIs to iocraft's internal `taffy::NodeId` or a DOM-like tree.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct RendererLayoutSnapshot {
    /// Absolute or renderer-defined X position in terminal cells.
    pub x: i32,
    /// Absolute or renderer-defined Y position in terminal rows.
    pub y: i32,
    /// Width in terminal cells.
    pub width: i32,
    /// Height in terminal rows.
    pub height: i32,
}

impl From<CachedLayoutBounds> for RendererLayoutSnapshot {
    fn from(value: CachedLayoutBounds) -> Self {
        Self {
            x: value.x,
            y: value.y,
            width: value.width,
            height: value.height,
        }
    }
}

/// Explicit layout-shift tracker for retained custom renderers.
///
/// CC Ink keeps a per-frame module-global `layoutShifted` flag: any node layout
/// change or child removal forces a broad damage backstop so stale retained
/// blits cannot survive. iocraft's built-in renderer has its own internal
/// tracker; this public helper exposes the same idea in a Rust-native form for
/// custom renderers and benchmark harnesses. It records caller-owned node keys
/// and returns `true` only after a previous snapshot exists and the current set
/// differs by key, position, or size.
#[derive(Clone, Debug)]
pub struct RendererLayoutShiftTracker<K> {
    previous: HashMap<K, RendererLayoutSnapshot>,
}

impl<K> Default for RendererLayoutShiftTracker<K> {
    fn default() -> Self {
        Self {
            previous: HashMap::new(),
        }
    }
}

impl<K> RendererLayoutShiftTracker<K>
where
    K: Eq + Hash,
{
    /// Creates an empty layout-shift tracker.
    pub fn new() -> Self {
        Self::default()
    }

    /// Replaces the previous snapshot set and returns whether layout shifted.
    ///
    /// The first call returns `false`, matching iocraft's built-in renderer and
    /// CC Ink's behavior: there is no retained previous layout to protect yet.
    pub fn update(
        &mut self,
        current: impl IntoIterator<Item = (K, RendererLayoutSnapshot)>,
    ) -> bool {
        let current = current.into_iter().collect::<HashMap<_, _>>();
        let shifted = !self.previous.is_empty()
            && (self.previous.len() != current.len()
                || current
                    .iter()
                    .any(|(key, snapshot)| self.previous.get(key) != Some(snapshot)));
        self.previous = current;
        shifted
    }

    /// Replaces the previous snapshot set from [`CachedLayoutBounds`] values.
    pub fn update_from_layouts(
        &mut self,
        current: impl IntoIterator<Item = (K, CachedLayoutBounds)>,
    ) -> bool {
        self.update(
            current
                .into_iter()
                .map(|(key, bounds)| (key, RendererLayoutSnapshot::from(bounds))),
        )
    }

    /// Returns a previously recorded snapshot for `key`, if present.
    pub fn snapshot(&self, key: &K) -> Option<RendererLayoutSnapshot> {
        self.previous.get(key).copied()
    }

    /// Number of snapshots currently retained.
    pub fn len(&self) -> usize {
        self.previous.len()
    }

    /// Whether no snapshots are currently retained.
    pub fn is_empty(&self) -> bool {
        self.previous.is_empty()
    }

    /// Clears all retained snapshots.
    pub fn clear(&mut self) {
        self.previous.clear();
    }
}

/// Input for [`plan_retained_node_render`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetainedNodeRenderInput {
    /// Current absolute layout bounds for the node.
    pub current_layout: CachedLayoutBounds,
    /// Previous cached layout bounds for the node, if present.
    pub cached_layout: Option<CachedLayoutBounds>,
    /// Whether the node's content/subtree is dirty this frame.
    pub dirty: bool,
    /// Whether the caller must force descent instead of direct self blit.
    pub skip_self_blit: bool,
    /// Whether render-time scroll draining is pending for this node.
    pub pending_scroll_delta: bool,
    /// Whether a trustworthy previous retained screen/canvas is available.
    pub previous_screen_available: bool,
    /// Whether the node is currently hidden/display:none.
    pub hidden: bool,
    /// Whether the node is absolutely positioned.
    pub absolute: bool,
    /// Pending clear regions for removed children under this node.
    pub pending_clears: Vec<CachedClearRegion>,
}

/// High-level retained node render decision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RetainedNodeRenderAction {
    /// Restore this node's cached rectangle from the previous retained buffer.
    Blit,
    /// Descend and render the node/subtree normally.
    Render,
    /// Skip rendering because the node is hidden.
    Hidden,
}

/// Plan returned by [`plan_retained_node_render`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetainedNodeRenderPlan {
    /// High-level action for this node.
    pub action: RetainedNodeRenderAction,
    /// Region to blit from the previous retained buffer when [`Self::action`] is
    /// [`RetainedNodeRenderAction::Blit`].
    pub blit_region: Option<CachedClearRegion>,
    /// Cached old region to clear before re-rendering or after becoming hidden.
    pub clear_old_region: Option<CachedClearRegion>,
    /// Whether [`Self::clear_old_region`] belongs to an absolute node and should
    /// poison unsafe sibling blits.
    pub clear_old_from_absolute: bool,
    /// Pending clear regions for removed children.
    pub pending_clear_regions: Vec<CachedClearRegion>,
    /// Whether pending child clears were present.
    pub has_removed_child: bool,
    /// Whether this node's layout changed relative to the cache.
    pub position_changed: bool,
    /// Whether the frame should use a broad layout-shift damage backstop.
    pub layout_shifted: bool,
    /// Whether a hidden node's subtree cache should be dropped.
    pub drop_subtree_cache: bool,
    /// Whether an absolute node's current/cached rect should be recorded for
    /// later scroll/escaping-overlay repairs.
    pub record_absolute_rect: bool,
}

/// Plans CC Ink-style retained node blit/clear behavior for a custom renderer.
///
/// This helper captures the first-stage decision in CC Ink
/// `renderNodeToOutput(...)`: clean unchanged nodes can direct-blit from the
/// previous screen, dirty or moved nodes clear their old cached rectangle before
/// rendering, removed children contribute pending clears and a layout-shift
/// backstop, and hidden dirty nodes clear/drop their subtree cache. It is an
/// explicit Rust helper, not a hidden global renderer policy: callers own node
/// keys, cache mutation, subtree traversal, canvas writes, and terminal output.
pub fn plan_retained_node_render(input: RetainedNodeRenderInput) -> RetainedNodeRenderPlan {
    let pending_clear_regions = input.pending_clears;
    let has_removed_child = !pending_clear_regions.is_empty();

    if input.hidden {
        let clear_old_region = input
            .dirty
            .then_some(input.cached_layout)
            .flatten()
            .map(Into::into);
        let drop_subtree_cache = clear_old_region.is_some();
        return RetainedNodeRenderPlan {
            action: RetainedNodeRenderAction::Hidden,
            blit_region: None,
            clear_old_region,
            clear_old_from_absolute: input.absolute && clear_old_region.is_some(),
            pending_clear_regions: Vec::new(),
            has_removed_child: false,
            position_changed: false,
            layout_shifted: drop_subtree_cache,
            drop_subtree_cache,
            record_absolute_rect: false,
        };
    }

    let can_blit = !input.dirty
        && !input.skip_self_blit
        && !input.pending_scroll_delta
        && input.previous_screen_available
        && input.cached_layout == Some(input.current_layout);
    if can_blit {
        return RetainedNodeRenderPlan {
            action: RetainedNodeRenderAction::Blit,
            blit_region: Some(input.current_layout.into()),
            clear_old_region: None,
            clear_old_from_absolute: false,
            pending_clear_regions,
            has_removed_child,
            position_changed: false,
            layout_shifted: has_removed_child,
            drop_subtree_cache: false,
            record_absolute_rect: input.absolute,
        };
    }

    let position_changed = input
        .cached_layout
        .is_some_and(|cached| cached != input.current_layout);
    let clear_old_region = input
        .cached_layout
        .filter(|_| input.dirty || position_changed)
        .map(Into::into);
    let layout_shifted = position_changed || has_removed_child;

    RetainedNodeRenderPlan {
        action: RetainedNodeRenderAction::Render,
        blit_region: None,
        clear_old_region,
        clear_old_from_absolute: input.absolute && clear_old_region.is_some(),
        pending_clear_regions,
        has_removed_child,
        position_changed,
        layout_shifted,
        drop_subtree_cache: false,
        record_absolute_rect: false,
    }
}

/// Canvas mutations applied from a retained node render plan.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RetainedNodeCanvasApplication {
    /// Region restored from the previous retained canvas for clean blit plans.
    pub blitted_region: Option<DamageRegion>,
    /// Old cached region cleared before a dirty/moved/hidden node is rendered.
    pub cleared_old_region: Option<DamageRegion>,
    /// Pending child clear regions that were visible and cleared.
    pub pending_clear_regions: Vec<DamageRegion>,
}

/// Applies the canvas-side skeleton of a retained node render plan.
///
/// This is an opt-in bridge between [`plan_retained_node_render`] and a custom
/// retained renderer. It performs only the mechanical canvas operations that CC
/// Ink's renderer does around a node decision: clean nodes blit their cached
/// rectangle from the previous retained canvas, dirty/moved/hidden nodes clear
/// their old cached rectangle, and removed children clear their pending regions.
/// The caller is still responsible for traversing children, drawing dirty
/// content after clears, committing cache state, and writing terminal patches.
pub fn apply_retained_node_render_plan_to_canvas(
    next: &mut Canvas,
    previous: &Canvas,
    plan: &RetainedNodeRenderPlan,
) -> RetainedNodeCanvasApplication {
    let mut application = RetainedNodeCanvasApplication::default();

    if let Some(region) = plan
        .clear_old_region
        .and_then(|region| region.clipped_to_canvas(next.width(), next.height()))
    {
        next.clear_region(region.x, region.y, region.width, region.height);
        application.cleared_old_region = Some(region);
    }

    if plan.action == RetainedNodeRenderAction::Blit {
        if let Some(region) = plan
            .blit_region
            .and_then(|region| region.clipped_to_canvas(next.width(), next.height()))
        {
            next.blit_region_from(previous, region.x, region.y, region.width, region.height);
            application.blitted_region = Some(region);
        }
    }

    for region in &plan.pending_clear_regions {
        if let Some(region) = region.clipped_to_canvas(next.width(), next.height()) {
            next.clear_region(region.x, region.y, region.width, region.height);
            application.pending_clear_regions.push(region);
        }
    }

    application
}

/// Mode-neutral node layout/pending-clear cache for custom retained renderers.
///
/// This mirrors CC Ink's `node-cache.ts` at the framework-utility level: layout
/// bounds are keyed by the caller's node identifier, removed children can queue
/// clear regions for their parent, and clearing an absolute-positioned node sets
/// a one-shot contamination flag so the next frame can disable unsafe blits from
/// the previous screen. It does not draw, write terminal output, or change screen
/// mode; callers decide how to apply blits/clears to their own retained buffer.
#[derive(Clone, Debug)]
pub struct RendererNodeCache<K> {
    layouts: HashMap<K, CachedLayoutBounds>,
    pending_clears: HashMap<K, Vec<CachedClearRegion>>,
    absolute_node_removed: bool,
}

impl<K> Default for RendererNodeCache<K> {
    fn default() -> Self {
        Self {
            layouts: HashMap::new(),
            pending_clears: HashMap::new(),
            absolute_node_removed: false,
        }
    }
}

impl<K> RendererNodeCache<K>
where
    K: Eq + Hash,
{
    /// Creates an empty renderer node cache.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns cached layout bounds for `node`, if present.
    pub fn layout(&self, node: &K) -> Option<CachedLayoutBounds> {
        self.layouts.get(node).copied()
    }

    /// Stores layout bounds for `node`.
    pub fn set_layout(&mut self, node: K, layout: CachedLayoutBounds) {
        self.layouts.insert(node, layout);
    }

    /// Removes and returns cached layout bounds for `node`.
    pub fn remove_layout(&mut self, node: &K) -> Option<CachedLayoutBounds> {
        self.layouts.remove(node)
    }

    /// Drops cached layout and pending-clear metadata for a subtree.
    ///
    /// This mirrors CC Ink `render-node-to-output.ts` `dropSubtreeCache(...)`,
    /// used when hidden/culled descendants should not later blit or clear stale
    /// coordinates after re-entering the retained render tree. The caller owns
    /// the tree structure and supplies `children`, keeping this helper
    /// mode-neutral and independent of iocraft's internal component tree.
    pub fn remove_subtree<I, F>(&mut self, root: &K, mut children: F)
    where
        K: Clone,
        F: FnMut(&K) -> I,
        I: IntoIterator<Item = K>,
    {
        fn walk<K, I, F>(cache: &mut RendererNodeCache<K>, node: &K, children: &mut F)
        where
            K: Eq + Hash + Clone,
            F: FnMut(&K) -> I,
            I: IntoIterator<Item = K>,
        {
            cache.layouts.remove(node);
            cache.pending_clears.remove(node);
            let kids = children(node).into_iter().collect::<Vec<_>>();
            for child in kids {
                walk(cache, &child, children);
            }
        }

        walk(self, root, &mut children);
    }

    /// Returns whether a clean node with `layout` can be blitted from the
    /// previous retained buffer according to the cached bounds.
    pub fn can_blit(&self, node: &K, layout: CachedLayoutBounds) -> bool {
        self.layout(node) == Some(layout)
    }

    /// Queues a clear region on `parent` for a removed or hidden child.
    ///
    /// When `is_absolute` is true, [`Self::consume_absolute_removed_flag`] will
    /// return true once. This matches CC Ink's absolute-overlay safeguard: a
    /// removed absolute node may have painted over unrelated siblings, so the
    /// next frame should avoid prev-screen blits that could restore stale pixels.
    pub fn add_pending_clear(&mut self, parent: K, region: CachedClearRegion, is_absolute: bool) {
        self.pending_clears.entry(parent).or_default().push(region);
        if is_absolute {
            self.absolute_node_removed = true;
        }
    }

    /// Takes all pending clear regions queued for `parent`.
    pub fn take_pending_clears(&mut self, parent: &K) -> Vec<CachedClearRegion> {
        self.pending_clears.remove(parent).unwrap_or_default()
    }

    /// Consumes the one-shot "absolute node removed" contamination flag.
    pub fn consume_absolute_removed_flag(&mut self) -> bool {
        let had = self.absolute_node_removed;
        self.absolute_node_removed = false;
        had
    }

    /// Clears all cached layouts, pending clears, and contamination flags.
    pub fn clear(&mut self) {
        self.layouts.clear();
        self.pending_clears.clear();
        self.absolute_node_removed = false;
    }
}

/// Input for [`RendererRetainedFrameState::plan_node`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetainedFrameNodeInput<K> {
    /// Caller-owned stable node identifier.
    pub key: K,
    /// Current absolute layout bounds for the node.
    pub current_layout: CachedLayoutBounds,
    /// Whether the node's content/subtree is dirty this frame.
    pub dirty: bool,
    /// Whether the caller must force descent instead of direct self blit.
    pub skip_self_blit: bool,
    /// Whether render-time scroll draining is pending for this node.
    pub pending_scroll_delta: bool,
    /// Whether a trustworthy previous retained screen/canvas is available.
    pub previous_screen_available: bool,
    /// Whether the node is currently hidden/display:none.
    pub hidden: bool,
    /// Whether the node is absolutely positioned.
    pub absolute: bool,
}

/// Node plan produced by [`RendererRetainedFrameState::plan_node`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetainedFrameNodePlan<K> {
    /// Caller-owned stable node identifier.
    pub key: K,
    /// Current absolute layout bounds used for the decision.
    pub current_layout: CachedLayoutBounds,
    /// Retained-render decision and clear/blit metadata.
    pub plan: RetainedNodeRenderPlan,
}

/// Stateful opt-in owner for retained renderer node-cache planning.
///
/// This is a Rust-native integration layer around [`RendererNodeCache`] and
/// [`plan_retained_node_render`]. It mirrors the CC Ink renderer's automatic
/// per-frame bookkeeping without becoming iocraft's default render path:
/// callers still own stable node IDs, dirty invalidation, traversal order,
/// actual canvas blits/clears, and terminal writes. The state consumes pending
/// child clears, tracks layout-shift damage backstops, exposes absolute-clear
/// contamination, and commits cached bounds when a node decision has been
/// applied.
#[derive(Clone, Debug)]
pub struct RendererRetainedFrameState<K> {
    cache: RendererNodeCache<K>,
    layout_shifted: bool,
    absolute_clear_this_frame: bool,
    absolute_removed_at_frame_start: bool,
}

impl<K> Default for RendererRetainedFrameState<K> {
    fn default() -> Self {
        Self {
            cache: RendererNodeCache::default(),
            layout_shifted: false,
            absolute_clear_this_frame: false,
            absolute_removed_at_frame_start: false,
        }
    }
}

impl<K> RendererRetainedFrameState<K>
where
    K: Eq + Hash,
{
    /// Creates an empty retained-frame state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the underlying node cache.
    pub fn cache(&self) -> &RendererNodeCache<K> {
        &self.cache
    }

    /// Returns the underlying node cache mutably.
    pub fn cache_mut(&mut self) -> &mut RendererNodeCache<K> {
        &mut self.cache
    }

    /// Starts a new retained planning frame.
    ///
    /// This resets one-frame layout/absolute-clear flags and consumes the
    /// previous frame's queued absolute-removal contamination flag. The return
    /// value tells callers whether they should disable unsafe prev-screen blits
    /// because an absolute node was removed before this frame.
    pub fn begin_frame(&mut self) -> bool {
        self.layout_shifted = false;
        self.absolute_clear_this_frame = false;
        self.absolute_removed_at_frame_start = self.cache.consume_absolute_removed_flag();
        self.absolute_removed_at_frame_start
    }

    /// Returns whether any planned node marked the frame as layout-shifted.
    pub fn layout_shifted(&self) -> bool {
        self.layout_shifted
    }

    /// Returns whether an absolute clear happened while planning this frame.
    pub fn absolute_clear_this_frame(&self) -> bool {
        self.absolute_clear_this_frame
    }

    /// Returns the absolute-removal contamination consumed at frame start.
    pub fn absolute_removed_at_frame_start(&self) -> bool {
        self.absolute_removed_at_frame_start
    }

    /// Queues a pending clear for a removed or hidden child.
    pub fn queue_child_clear(&mut self, parent: K, region: CachedClearRegion, is_absolute: bool) {
        self.cache.add_pending_clear(parent, region, is_absolute);
    }

    /// Plans retained rendering for one node and consumes its pending child clears.
    pub fn plan_node(&mut self, input: RetainedFrameNodeInput<K>) -> RetainedFrameNodePlan<K>
    where
        K: Clone,
    {
        let cached_layout = self.cache.layout(&input.key);
        let pending_clears = self.cache.take_pending_clears(&input.key);
        let plan = plan_retained_node_render(RetainedNodeRenderInput {
            current_layout: input.current_layout,
            cached_layout,
            dirty: input.dirty,
            skip_self_blit: input.skip_self_blit,
            pending_scroll_delta: input.pending_scroll_delta,
            previous_screen_available: input.previous_screen_available,
            hidden: input.hidden,
            absolute: input.absolute,
            pending_clears,
        });
        self.layout_shifted |= plan.layout_shifted;
        self.absolute_clear_this_frame |= plan.clear_old_from_absolute;
        RetainedFrameNodePlan {
            key: input.key,
            current_layout: input.current_layout,
            plan,
        }
    }

    /// Commits a node plan after the caller has applied its blit/clear/render work.
    ///
    /// Hidden nodes remove their own cached layout; visible nodes store their
    /// latest bounds. If [`RetainedNodeRenderPlan::drop_subtree_cache`] is true,
    /// callers that track descendants should prefer
    /// [`Self::commit_node_plan_with_children`] so descendant layouts are dropped
    /// at the same time.
    pub fn commit_node_plan(&mut self, plan: &RetainedFrameNodePlan<K>)
    where
        K: Clone,
    {
        if plan.plan.action == RetainedNodeRenderAction::Hidden {
            self.cache.remove_layout(&plan.key);
        } else {
            self.cache.set_layout(plan.key.clone(), plan.current_layout);
        }
    }

    /// Commits a node plan and drops descendant cache when requested by the plan.
    pub fn commit_node_plan_with_children<I, F>(
        &mut self,
        plan: &RetainedFrameNodePlan<K>,
        mut children: F,
    ) where
        K: Clone,
        F: FnMut(&K) -> I,
        I: IntoIterator<Item = K>,
    {
        if plan.plan.drop_subtree_cache {
            self.cache.remove_subtree(&plan.key, &mut children);
        } else {
            self.commit_node_plan(plan);
        }
    }

    /// Clears all retained cache and frame flags.
    pub fn clear(&mut self) {
        self.cache.clear();
        self.layout_shifted = false;
        self.absolute_clear_this_frame = false;
        self.absolute_removed_at_frame_start = false;
    }
}

/// Opt-in dirty tree for retained renderer experiments.
///
/// CC Ink stores `dirty` on mutable DOM nodes and `markDirty(...)` walks to the
/// root so clean subtree blits know where they are safe. iocraft's default
/// renderer does not use this helper, but custom retained renderers can use it
/// with stable Rust node identifiers to model the same invalidation flow without
/// JS object identity or hidden globals. Children are kept in attachment order
/// because CC Ink traverses `childNodes` order for sibling contamination and
/// retained subtree repair decisions. A separate `measure_dirty` set mirrors
/// CC Ink's yoga-measure dirtying for text/raw-ANSI leaf nodes.
#[derive(Clone, Debug)]
pub struct RendererDirtyTree<K> {
    parents: HashMap<K, K>,
    children: HashMap<K, Vec<K>>,
    dirty: HashSet<K>,
    measure_dirty: HashSet<K>,
}

impl<K> Default for RendererDirtyTree<K> {
    fn default() -> Self {
        Self {
            parents: HashMap::new(),
            children: HashMap::new(),
            dirty: HashSet::new(),
            measure_dirty: HashSet::new(),
        }
    }
}

impl<K> RendererDirtyTree<K>
where
    K: Eq + Hash + Clone,
{
    /// Creates an empty dirty tree.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a root node with no parent.
    pub fn register_root(&mut self, node: K) {
        if let Some(old_parent) = self.parents.remove(&node) {
            if let Some(siblings) = self.children.get_mut(&old_parent) {
                siblings.retain(|sibling| sibling != &node);
            }
        }
        self.children.entry(node).or_default();
    }

    /// Attaches `node` under `parent`, updating any old parent edge.
    pub fn attach(&mut self, node: K, parent: K) {
        if let Some(old_parent) = self.parents.insert(node.clone(), parent.clone()) {
            if let Some(siblings) = self.children.get_mut(&old_parent) {
                siblings.retain(|sibling| sibling != &node);
            }
        }
        let siblings = self.children.entry(parent).or_default();
        if !siblings.contains(&node) {
            siblings.push(node.clone());
        }
        self.children.entry(node).or_default();
    }

    /// Returns the parent for `node`, if one is registered.
    pub fn parent(&self, node: &K) -> Option<&K> {
        self.parents.get(node)
    }

    /// Returns the currently registered children for `node` in attachment order.
    pub fn child_keys(&self, node: &K) -> Vec<K> {
        self.children.get(node).cloned().unwrap_or_default()
    }

    /// Returns `root` and all descendants currently registered under it.
    pub fn subtree_nodes(&self, root: &K) -> Vec<K> {
        let mut nodes = Vec::new();
        self.collect_subtree_nodes(root, &mut nodes);
        nodes
    }

    fn collect_subtree_nodes(&self, node: &K, nodes: &mut Vec<K>) {
        nodes.push(node.clone());
        if let Some(children) = self.children.get(node) {
            for child in children {
                self.collect_subtree_nodes(child, nodes);
            }
        }
    }

    /// Returns whether `node` is marked dirty.
    pub fn is_dirty(&self, node: &K) -> bool {
        self.dirty.contains(node)
    }

    /// Returns whether `node` needs text/raw measurement refresh.
    pub fn is_measure_dirty(&self, node: &K) -> bool {
        self.measure_dirty.contains(node)
    }

    /// Marks `node` and all ancestors dirty.
    ///
    /// Set `measure_dirty` for text-like leaf mutations that should refresh
    /// layout measurement. The measurement flag is applied only to `node`; the
    /// ancestor walk marks render dirtiness, matching CC Ink's `markDirty(...)`.
    pub fn mark_dirty(&mut self, node: &K, measure_dirty: bool) {
        if measure_dirty {
            self.measure_dirty.insert(node.clone());
        }
        let mut current = Some(node.clone());
        while let Some(key) = current {
            self.dirty.insert(key.clone());
            current = self.parents.get(&key).cloned();
        }
    }

    /// Clears render and measurement dirtiness for one node.
    pub fn clear_node(&mut self, node: &K) {
        self.dirty.remove(node);
        self.measure_dirty.remove(node);
    }

    /// Clears all frame dirtiness while preserving tree edges.
    pub fn clear_dirty(&mut self) {
        self.dirty.clear();
        self.measure_dirty.clear();
    }

    /// Returns the currently dirty nodes.
    pub fn dirty_nodes(&self) -> impl Iterator<Item = &K> {
        self.dirty.iter()
    }

    /// Returns the currently measurement-dirty nodes.
    pub fn measure_dirty_nodes(&self) -> impl Iterator<Item = &K> {
        self.measure_dirty.iter()
    }

    /// Removes a subtree and returns the removed node identifiers.
    ///
    /// The parent is marked dirty because its child list/layout changed. This
    /// helper only tracks invalidation; pair it with [`RendererNodeCache`] or
    /// [`RendererRetainedFrameState`] to queue clear rectangles for removed
    /// cached layouts.
    pub fn remove_subtree(&mut self, root: &K) -> Vec<K> {
        let parent = self.parents.get(root).cloned();
        if let Some(parent) = &parent {
            if let Some(siblings) = self.children.get_mut(parent) {
                siblings.retain(|sibling| sibling != root);
            }
            self.mark_dirty(parent, false);
        }

        let mut removed = Vec::new();
        self.remove_subtree_inner(root, &mut removed);
        removed
    }

    fn remove_subtree_inner(&mut self, node: &K, removed: &mut Vec<K>) {
        let children = self
            .children
            .remove(node)
            .unwrap_or_default()
            .into_iter()
            .collect::<Vec<_>>();
        for child in children {
            self.remove_subtree_inner(&child, removed);
        }
        self.parents.remove(node);
        self.dirty.remove(node);
        self.measure_dirty.remove(node);
        removed.push(node.clone());
    }

    /// Clears all tree edges and dirty state.
    pub fn clear(&mut self) {
        self.parents.clear();
        self.children.clear();
        self.dirty.clear();
        self.measure_dirty.clear();
    }
}

/// Input for [`RendererRetainedTreeState::plan_node`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetainedTreeNodeInput<K> {
    /// Caller-owned stable node identifier.
    pub key: K,
    /// Current absolute layout bounds for the node.
    pub current_layout: CachedLayoutBounds,
    /// Whether the caller must force descent instead of direct self blit.
    pub skip_self_blit: bool,
    /// Whether render-time scroll draining is pending for this node.
    pub pending_scroll_delta: bool,
    /// Whether a trustworthy previous retained screen/canvas is available.
    pub previous_screen_available: bool,
    /// Whether the node is currently hidden/display:none.
    pub hidden: bool,
    /// Whether the node is absolutely positioned.
    pub absolute: bool,
}

/// Opt-in retained renderer state that combines dirty invalidation and node cache.
///
/// This is the closest Rust-first building block to CC Ink's automatic
/// DOM-element `dirty` + `nodeCache` pipeline. It keeps the default iocraft
/// renderer unchanged, but custom retained renderers can use one explicit state
/// owner for tree edges, ancestor dirty propagation, cached layout bounds,
/// removed-child pending clears, and per-frame retained blit planning.
#[derive(Clone, Debug)]
pub struct RendererRetainedTreeState<K> {
    dirty_tree: RendererDirtyTree<K>,
    frame_state: RendererRetainedFrameState<K>,
}

impl<K> Default for RendererRetainedTreeState<K> {
    fn default() -> Self {
        Self {
            dirty_tree: RendererDirtyTree::default(),
            frame_state: RendererRetainedFrameState::default(),
        }
    }
}

impl<K> RendererRetainedTreeState<K>
where
    K: Eq + Hash + Clone,
{
    /// Creates an empty retained tree state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the dirty tree.
    pub fn dirty_tree(&self) -> &RendererDirtyTree<K> {
        &self.dirty_tree
    }

    /// Returns the dirty tree mutably.
    pub fn dirty_tree_mut(&mut self) -> &mut RendererDirtyTree<K> {
        &mut self.dirty_tree
    }

    /// Returns the retained frame/cache state.
    pub fn frame_state(&self) -> &RendererRetainedFrameState<K> {
        &self.frame_state
    }

    /// Returns the retained frame/cache state mutably.
    pub fn frame_state_mut(&mut self) -> &mut RendererRetainedFrameState<K> {
        &mut self.frame_state
    }

    /// Registers a root node with no parent.
    pub fn register_root(&mut self, node: K) {
        self.dirty_tree.register_root(node);
    }

    /// Attaches `node` under `parent` and dirties the parent, because the child
    /// list/layout may have changed.
    pub fn attach(&mut self, node: K, parent: K) {
        self.dirty_tree.attach(node, parent.clone());
        self.dirty_tree.mark_dirty(&parent, false);
    }

    /// Marks `node` and all ancestors dirty.
    pub fn mark_dirty(&mut self, node: &K, measure_dirty: bool) {
        self.dirty_tree.mark_dirty(node, measure_dirty);
    }

    /// Returns whether `node` is dirty.
    pub fn is_dirty(&self, node: &K) -> bool {
        self.dirty_tree.is_dirty(node)
    }

    /// Starts a retained planning frame.
    pub fn begin_frame(&mut self) -> bool {
        self.frame_state.begin_frame()
    }

    /// Plans one node using dirtiness from [`RendererDirtyTree`].
    pub fn plan_node(&mut self, input: RetainedTreeNodeInput<K>) -> RetainedFrameNodePlan<K> {
        let dirty = self.dirty_tree.is_dirty(&input.key);
        self.frame_state.plan_node(RetainedFrameNodeInput {
            key: input.key,
            current_layout: input.current_layout,
            dirty,
            skip_self_blit: input.skip_self_blit,
            pending_scroll_delta: input.pending_scroll_delta,
            previous_screen_available: input.previous_screen_available,
            hidden: input.hidden,
            absolute: input.absolute,
        })
    }

    /// Commits a node plan and clears that node's frame dirtiness.
    pub fn commit_node_plan(&mut self, plan: &RetainedFrameNodePlan<K>) {
        self.frame_state.commit_node_plan(plan);
        self.dirty_tree.clear_node(&plan.key);
    }

    /// Commits a node plan, dropping descendant cached layouts when requested.
    pub fn commit_node_plan_with_children(&mut self, plan: &RetainedFrameNodePlan<K>) {
        let children = self.dirty_tree.children.clone();
        let dirty_nodes_to_clear = plan
            .plan
            .drop_subtree_cache
            .then(|| self.dirty_tree.subtree_nodes(&plan.key));
        self.frame_state
            .commit_node_plan_with_children(plan, |key| {
                children
                    .get(key)
                    .map(|children| children.iter().cloned().collect::<Vec<_>>())
                    .unwrap_or_default()
            });
        if let Some(nodes) = dirty_nodes_to_clear {
            for node in nodes {
                self.dirty_tree.clear_node(&node);
            }
        } else {
            self.dirty_tree.clear_node(&plan.key);
        }
    }

    /// Removes a subtree from the dirty tree and retained cache.
    ///
    /// If the removed root had cached layout and a parent, a pending clear is
    /// queued on the parent. `is_absolute` should reflect the removed root's
    /// positioning; absolute removals poison unsafe prev-screen blits at the
    /// start of the next frame, matching CC Ink's `absoluteNodeRemoved` guard.
    pub fn remove_subtree(&mut self, root: &K, is_absolute: bool) -> Vec<K> {
        let parent = self.dirty_tree.parent(root).cloned();
        let cached_layout = self.frame_state.cache().layout(root);
        let children = self.dirty_tree.children.clone();
        self.frame_state.cache_mut().remove_subtree(root, |key| {
            children
                .get(key)
                .map(|children| children.iter().cloned().collect::<Vec<_>>())
                .unwrap_or_default()
        });
        let removed = self.dirty_tree.remove_subtree(root);
        if let (Some(parent), Some(layout)) = (parent, cached_layout) {
            self.frame_state
                .queue_child_clear(parent, layout.into(), is_absolute);
        }
        removed
    }

    /// Clears dirtiness for all nodes while preserving tree/cache state.
    pub fn clear_dirty(&mut self) {
        self.dirty_tree.clear_dirty();
    }

    /// Clears all retained tree/cache/frame state.
    pub fn clear(&mut self) {
        self.dirty_tree.clear();
        self.frame_state.clear();
    }
}

/// Input for [`RendererRetainedTreeReconciler::plan_node`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetainedLogicalTreeNodeInput<K> {
    /// Caller-owned logical node key. The reconciler maps it to a generation-stamped id.
    pub key: K,
    /// Current absolute layout bounds for the node.
    pub current_layout: CachedLayoutBounds,
    /// Whether the caller must force descent instead of direct self blit.
    pub skip_self_blit: bool,
    /// Whether render-time scroll draining is pending for this node.
    pub pending_scroll_delta: bool,
    /// Whether a trustworthy previous retained screen/canvas is available.
    pub previous_screen_available: bool,
    /// Whether the node is currently hidden/display:none.
    pub hidden: bool,
    /// Whether the node is absolutely positioned.
    pub absolute: bool,
}

/// Opt-in reconciler for logical retained trees.
///
/// This combines [`RendererNodeGenerationState`] with
/// [`RendererRetainedTreeState`]. It is a design building block for an automatic
/// Rust renderer path: logical keys are converted into generation-stamped ids,
/// tree attachment/removal updates dirty propagation in attachment order, and
/// removed subtrees bump generations so a future remount cannot reuse stale
/// cached layouts. It still performs no drawing, terminal I/O, or default
/// renderer integration.
#[derive(Clone, Debug)]
pub struct RendererRetainedTreeReconciler<K> {
    generations: RendererNodeGenerationState<K>,
    retained: RendererRetainedTreeState<RendererStableNodeId<K>>,
    parents: HashMap<K, K>,
    children: HashMap<K, Vec<K>>,
}

impl<K> Default for RendererRetainedTreeReconciler<K> {
    fn default() -> Self {
        Self {
            generations: RendererNodeGenerationState::default(),
            retained: RendererRetainedTreeState::default(),
            parents: HashMap::new(),
            children: HashMap::new(),
        }
    }
}

impl<K> RendererRetainedTreeReconciler<K>
where
    K: Eq + Hash + Clone,
{
    /// Creates an empty retained tree reconciler.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the generation tracker.
    pub fn generations(&self) -> &RendererNodeGenerationState<K> {
        &self.generations
    }

    /// Returns the retained tree state keyed by stable ids.
    pub fn retained_state(&self) -> &RendererRetainedTreeState<RendererStableNodeId<K>> {
        &self.retained
    }

    /// Returns the retained tree state mutably.
    pub fn retained_state_mut(
        &mut self,
    ) -> &mut RendererRetainedTreeState<RendererStableNodeId<K>> {
        &mut self.retained
    }

    /// Returns the current stable id for a logical key, if tracked.
    pub fn id(&self, key: &K) -> Option<RendererStableNodeId<K>> {
        self.generations.id(key)
    }

    /// Returns or creates the current stable id for a logical key.
    pub fn current_id(&mut self, key: K) -> RendererStableNodeId<K> {
        self.generations.current_id(key)
    }

    /// Registers a logical root node and returns its stable id.
    pub fn register_root(&mut self, key: K) -> RendererStableNodeId<K> {
        if let Some(old_parent) = self.parents.remove(&key) {
            if let Some(siblings) = self.children.get_mut(&old_parent) {
                siblings.retain(|sibling| sibling != &key);
            }
        }
        let id = self.generations.current_id(key.clone());
        self.children.entry(key).or_default();
        self.retained.register_root(id.clone());
        id
    }

    /// Attaches a logical node under a logical parent and returns their stable ids.
    pub fn attach(
        &mut self,
        key: K,
        parent: K,
    ) -> (RendererStableNodeId<K>, RendererStableNodeId<K>) {
        if let Some(old_parent) = self.parents.insert(key.clone(), parent.clone()) {
            if let Some(siblings) = self.children.get_mut(&old_parent) {
                siblings.retain(|sibling| sibling != &key);
            }
        }
        let siblings = self.children.entry(parent.clone()).or_default();
        if !siblings.contains(&key) {
            siblings.push(key.clone());
        }
        self.children.entry(key.clone()).or_default();

        let parent_id = self.generations.current_id(parent);
        let id = self.generations.current_id(key);
        self.retained.attach(id.clone(), parent_id.clone());
        (id, parent_id)
    }

    /// Marks a logical node and its stable-id ancestors dirty.
    pub fn mark_dirty(&mut self, key: &K, measure_dirty: bool) -> bool {
        let Some(id) = self.generations.id(key) else {
            return false;
        };
        self.retained.mark_dirty(&id, measure_dirty);
        true
    }

    /// Returns whether a logical node is dirty.
    pub fn is_dirty(&self, key: &K) -> bool {
        self.generations
            .id(key)
            .is_some_and(|id| self.retained.is_dirty(&id))
    }

    /// Starts a retained planning frame.
    pub fn begin_frame(&mut self) -> bool {
        self.retained.begin_frame()
    }

    /// Plans one logical node by mapping it to its generation-stamped id.
    pub fn plan_node(
        &mut self,
        input: RetainedLogicalTreeNodeInput<K>,
    ) -> RetainedFrameNodePlan<RendererStableNodeId<K>> {
        let key = self.generations.current_id(input.key);
        self.retained.plan_node(RetainedTreeNodeInput {
            key,
            current_layout: input.current_layout,
            skip_self_blit: input.skip_self_blit,
            pending_scroll_delta: input.pending_scroll_delta,
            previous_screen_available: input.previous_screen_available,
            hidden: input.hidden,
            absolute: input.absolute,
        })
    }

    /// Commits a retained node plan.
    pub fn commit_node_plan(&mut self, plan: &RetainedFrameNodePlan<RendererStableNodeId<K>>) {
        self.retained.commit_node_plan(plan);
    }

    /// Removes a logical subtree, drops stable-id cache state, and bumps generations.
    pub fn remove_subtree(&mut self, root: &K, is_absolute: bool) -> Vec<RendererStableNodeId<K>> {
        let Some(root_id) = self.generations.id(root) else {
            return Vec::new();
        };
        let logical_nodes = self.logical_subtree(root);
        let removed = self.retained.remove_subtree(&root_id, is_absolute);

        if let Some(parent) = self.parents.get(root).cloned() {
            if let Some(siblings) = self.children.get_mut(&parent) {
                siblings.retain(|sibling| sibling != root);
            }
        }
        for key in logical_nodes {
            self.parents.remove(&key);
            self.children.remove(&key);
            self.generations.remove(&key);
        }
        removed
    }

    fn logical_subtree(&self, root: &K) -> Vec<K> {
        let mut nodes = Vec::new();
        self.collect_logical_subtree(root, &mut nodes);
        nodes
    }

    fn collect_logical_subtree(&self, key: &K, nodes: &mut Vec<K>) {
        nodes.push(key.clone());
        if let Some(children) = self.children.get(key) {
            for child in children {
                self.collect_logical_subtree(child, nodes);
            }
        }
    }

    /// Clears all dirty flags while preserving generations, tree edges, and cache.
    pub fn clear_dirty(&mut self) {
        self.retained.clear_dirty();
    }

    /// Clears generations, logical tree edges, and retained state.
    pub fn clear(&mut self) {
        self.generations.clear();
        self.retained.clear();
        self.parents.clear();
        self.children.clear();
    }
}

/// Provides information and operations that low level component implementations may need to
/// utilize during the update phase.
pub struct ComponentUpdater<'a, 'b: 'a, 'c: 'a, 'w> {
    node_id: NodeId,
    transparent_layout: bool,
    children: &'a mut Components,
    unattached_child_node_ids: &'a mut Vec<NodeId>,
    context: &'a mut UpdateContext<'b, 'w>,
    component_context_stack: &'a mut ContextStack<'c>,
}

impl<'a, 'b, 'c, 'w> ComponentUpdater<'a, 'b, 'c, 'w> {
    pub(crate) fn new(
        node_id: NodeId,
        children: &'a mut Components,
        unattached_child_node_ids: &'a mut Vec<NodeId>,
        context: &'a mut UpdateContext<'b, 'w>,
        component_context_stack: &'a mut ContextStack<'c>,
    ) -> Self {
        Self {
            node_id,
            transparent_layout: false,
            children,
            unattached_child_node_ids,
            context,
            component_context_stack,
        }
    }

    /// Puts the terminal into raw mode if it isn't already, and returns a stream of terminal
    /// events.
    pub fn terminal_events(&mut self) -> Option<TerminalEvents> {
        self.context.terminal.as_mut().and_then(|t| t.events().ok())
    }

    /// Sends a CC Ink-style terminal query on the render output side band.
    ///
    /// Pair this with [`Self::flush_terminal_queries`] to resolve unsupported
    /// queries without timeouts. Sending a query starts the backend event stream
    /// so any parsed [`crate::TerminalEvent::Response`] values can be routed by
    /// the render loop into the query queue even when no component separately
    /// subscribes to input.
    pub fn send_terminal_query(
        &mut self,
        query: TerminalQuery,
    ) -> io::Result<Option<PendingTerminalQuery>> {
        if let Some(terminal) = self.context.terminal.as_mut() {
            terminal.send_terminal_query(query).map(Some)
        } else {
            Ok(None)
        }
    }

    /// Sends the DA1 sentinel used to flush pending terminal queries.
    pub fn flush_terminal_queries(&mut self) -> io::Result<Option<PendingTerminalFlush>> {
        if let Some(terminal) = self.context.terminal.as_mut() {
            terminal.flush_terminal_queries().map(Some)
        } else {
            Ok(None)
        }
    }

    /// Returns whether the terminal input supports raw mode.
    pub fn is_terminal_raw_mode_supported(&self) -> bool {
        self.context
            .terminal
            .as_ref()
            .map(|t| t.is_raw_mode_supported())
            .unwrap_or(false)
    }

    /// Returns whether the terminal is in raw mode.
    pub fn is_terminal_raw_mode_enabled(&self) -> bool {
        self.context
            .terminal
            .as_ref()
            .map(|t| t.is_raw_mode_enabled())
            .unwrap_or(false)
    }

    /// Removes the currently rendered output from the terminal, e.g. to allow for the printing of
    /// output above the component.
    pub fn clear_terminal_output(&mut self) {
        if !self.context.did_clear_terminal_output {
            if let Some(terminal) = self.context.terminal.as_mut() {
                terminal.clear_canvas().unwrap();
            }
            self.context.did_clear_terminal_output = true;
        }
    }

    /// Clears the visible terminal screen while preserving scrollback, then treats
    /// the next canvas write as a fresh frame. This mirrors Ink's main-screen
    /// repaint path: no alternate screen, native terminal scrollback remains usable.
    pub fn clear_screen(&mut self) {
        if !self.context.did_clear_terminal_output {
            if let Some(terminal) = self.context.terminal.as_mut() {
                terminal.clear_screen().unwrap();
            }
            self.context.did_clear_terminal_output = true;
        }
    }

    /// Clears the full terminal screen and scrollback, then treats the next canvas
    /// write as a fresh frame.
    pub fn clear_terminal(&mut self) {
        if !self.context.did_clear_terminal_output {
            if let Some(terminal) = self.context.terminal.as_mut() {
                terminal.clear_terminal().unwrap();
            }
            self.context.did_clear_terminal_output = true;
        }
    }

    /// Requests a full repaint for the current frame. Unlike clearing helpers,
    /// this does not write to the terminal immediately; it marks the canvas so
    /// the backend refreshes every row during the normal frame commit. This is
    /// useful for one-shot recovery from contaminated retained output.
    pub fn force_full_repaint(&mut self) {
        self.context.force_full_repaint = true;
    }

    /// Marks the previously retained frame as untrustworthy without clearing the
    /// terminal. The next rendered canvas is tagged with full-screen damage,
    /// mirroring CC Ink's `invalidatePrevFrame()` / `prevFrameContaminated`
    /// path: all rows are considered during diffing, but scroll optimizations
    /// such as DECSTBM remain available.
    pub fn invalidate_previous_frame(&mut self) {
        self.context.invalidate_prev_frame = true;
    }

    /// Returns a mutable reference to the terminal, if we're in a terminal render loop.
    pub(crate) fn terminal_mut(&mut self) -> Option<&mut Terminal<'w>> {
        self.context.terminal.as_deref_mut()
    }

    #[doc(hidden)]
    pub fn component_context_stack(&self) -> &ContextStack<'c> {
        self.component_context_stack
    }

    /// Gets an immutable reference to context of the given type.
    pub fn get_context<T: Any>(&self) -> Option<Ref<'_, T>> {
        self.component_context_stack.get_context()
    }

    /// Gets a mutable reference to context of the given type.
    pub fn get_context_mut<T: Any>(&self) -> Option<RefMut<'_, T>> {
        self.component_context_stack.get_context_mut()
    }

    /// Sets the layout style of the current component.
    pub fn set_layout_style(&mut self, layout_style: taffy::style::Style) {
        self.context
            .layout_engine
            .set_style(self.node_id, layout_style)
            .expect("we should be able to set the style");
    }

    /// Sets the layout style only when it differs from the current style.
    ///
    /// This mirrors CC Ink's style equality guard: rebuilding an equivalent
    /// style value should not dirty layout or wake Taffy recomputation.
    /// Returns `true` when the style was changed.
    pub fn set_layout_style_if_changed(&mut self, layout_style: taffy::style::Style) -> bool {
        if self
            .context
            .layout_engine
            .style(self.node_id)
            .is_ok_and(|current| current == &layout_style)
        {
            return false;
        }

        self.set_layout_style(layout_style);
        true
    }

    /// Sets the measure function of the current component, which is invoked to calculate the area
    /// that the component's content should occupy.
    pub fn set_measure_func(&mut self, measure_func: MeasureFunc) {
        self.context
            .layout_engine
            .get_node_context_mut(self.node_id)
            .expect("we should be able to get the node")
            .measure_func = Some(measure_func);
        self.context
            .layout_engine
            .mark_dirty(self.node_id)
            .expect("we should be able to mark the node as dirty");
    }

    /// If set to `true`, the layout of the current component will be transparent, meaning that
    /// children will effectively be direct descendants of the parent of the current component for
    /// layout purposes.
    pub fn set_transparent_layout(&mut self, transparent_layout: bool) {
        if transparent_layout && !self.transparent_layout {
            self.context
                .layout_engine
                .set_style(
                    self.node_id,
                    Style {
                        display: Display::None,
                        ..Default::default()
                    },
                )
                .expect("we should be able to set the style");
        }
        self.transparent_layout = transparent_layout;
    }

    pub(crate) fn has_transparent_layout(&self) -> bool {
        self.transparent_layout
    }

    /// Updates the children of the current component.
    pub fn update_children<I, T>(&mut self, children: I, context: Option<Context>)
    where
        I: IntoIterator<Item = T>,
        T: ElementExt,
    {
        self.component_context_stack
            .with_context(context, |component_context_stack| {
                let mut used_components = AppendOnlyMultimap::default();

                let mut direct_child_node_ids = Vec::new();
                let child_node_ids = if self.transparent_layout {
                    &mut self.unattached_child_node_ids
                } else {
                    &mut direct_child_node_ids
                };

                for mut child in children {
                    let mut component: InstantiatedComponent =
                        match self.children.components.pop_front(child.key()) {
                            Some(component)
                                if component.component().type_id()
                                    == child.helper().component_type_id() =>
                            {
                                child_node_ids.push(component.node_id());
                                component
                            }
                            _ => {
                                let new_node_id = self
                                    .context
                                    .layout_engine
                                    .new_leaf_with_context(
                                        Style::default(),
                                        LayoutEngineNodeContext::default(),
                                    )
                                    .expect("we should be able to add the node");
                                child_node_ids.push(new_node_id);
                                let h = child.helper();
                                InstantiatedComponent::new(new_node_id, child.props_mut(), h)
                            }
                        };
                    component.update(
                        self.context,
                        child_node_ids,
                        component_context_stack,
                        child.props_mut(),
                    );

                    used_components.push_back(child.key().clone(), component);
                }

                self.context
                    .layout_engine
                    .set_children(self.node_id, &direct_child_node_ids)
                    .expect("we should be able to set the children");

                for component in self.children.components.iter() {
                    self.context
                        .layout_engine
                        .remove(component.node_id())
                        .expect("we should be able to remove the node");
                }
                self.children.components = used_components.into();
            });
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DeferredNoSelectRegion {
    x: usize,
    y: usize,
    width: usize,
    height: usize,
}

struct DrawContext<'a> {
    layout_engine: &'a LayoutEngine,
    canvas: &'a mut Canvas,
    deferred_no_select: &'a mut Vec<DeferredNoSelectRegion>,
}

/// Provides information and operations that low level component implementations may need to
/// utilize during the draw phase.
pub struct ComponentDrawer<'a> {
    node_id: NodeId,
    node_position: Point<i16>,
    node_size: Size<u16>,
    clip_rect: Rect<u16>,
    vertical_clip_active: bool,
    skip_children: bool,
    context: DrawContext<'a>,
}

impl ComponentDrawer<'_> {
    /// Gets the calculated layout of the current node.
    pub fn layout(&self) -> Layout {
        *self
            .context
            .layout_engine
            .layout(self.node_id)
            .expect("we should be able to get the layout")
    }

    /// Gets the style of the current node.
    pub fn style(&self) -> &Style {
        self.context
            .layout_engine
            .style(self.node_id)
            .expect("we should be able to get the style")
    }

    /// Gets the size of the component.
    pub fn size(&self) -> Size<u16> {
        self.node_size
    }

    /// Gets the drawable content size of the component, excluding padding and border.
    ///
    /// This mirrors CC Ink's `get-max-width.ts` helper, generalized to both
    /// axes: content width/height are the computed node size minus computed
    /// padding and border. Values saturate at zero.
    pub fn content_size(&self) -> Size<u16> {
        let layout = self.layout();
        Size {
            width: (layout.size.width
                - layout.padding.left
                - layout.padding.right
                - layout.border.left
                - layout.border.right)
                .max(0.0) as u16,
            height: (layout.size.height
                - layout.padding.top
                - layout.padding.bottom
                - layout.border.top
                - layout.border.bottom)
                .max(0.0) as u16,
        }
    }

    /// Gets the visible component size inside the current canvas clip.
    pub fn visible_size(&self) -> Size<u16> {
        let x = self.node_position.x as isize;
        let y = self.node_position.y as isize;
        let node_right = x + self.node_size.width as isize;
        let node_bottom = y + self.node_size.height as isize;
        let left = x.max(self.clip_rect.left as isize).max(0);
        let top = y.max(self.clip_rect.top as isize).max(0);
        let right = node_right.min(self.clip_rect.right as isize).max(0);
        let bottom = node_bottom.min(self.clip_rect.bottom as isize).max(0);
        Size {
            width: (right - left).max(0) as u16,
            height: (bottom - top).max(0) as u16,
        }
    }

    /// Gets the component size remaining before the root canvas edge.
    ///
    /// Text renderers use this to mirror CC Ink's `render-node-to-output.ts`
    /// clamp of `getMaxWidth(yogaNode)` to `output.width - x`. Yoga can compute
    /// an overflowing cross-axis width that is wider than the terminal canvas;
    /// wrapping against this clamped width keeps rendered line count consistent
    /// with the cells that can actually exist in the screen buffer.
    pub fn remaining_canvas_size(&self) -> Size<u16> {
        let x = self.node_position.x as isize;
        let y = self.node_position.y as isize;
        Size {
            width: (self.context.canvas.width() as isize - x)
                .max(0)
                .min(self.node_size.width as isize) as u16,
            height: (self.context.canvas.height() as isize - y)
                .max(0)
                .min(self.node_size.height as isize) as u16,
        }
    }

    /// Gets the position of the component relative to the top left of the canvas.
    pub fn canvas_position(&self) -> Point<i16> {
        self.node_position
    }

    /// Requests that this frame be fully repainted even if the produced canvas
    /// is otherwise identical to the previous frame. Use this as a one-shot
    /// recovery path when previously rendered terminal cells may be contaminated
    /// by external output or post-render overlays.
    pub fn force_full_repaint(&mut self) {
        self.context.canvas.force_full_repaint();
    }

    /// Marks this component's current canvas bounds as damaged. Damaged rows
    /// are repainted even if their cells are otherwise equal to the previous
    /// frame, mirroring CC Ink's screen damage escape hatch for retained-output
    /// contamination.
    pub fn mark_damage(&mut self) {
        self.mark_damage_region(
            0,
            0,
            self.node_size.width as usize,
            self.node_size.height as usize,
        );
    }

    /// Marks a component-local rectangle as damaged. Coordinates are relative
    /// to this component's top-left canvas position.
    pub fn mark_damage_region(&mut self, x: usize, y: usize, width: usize, height: usize) {
        if width == 0 || height == 0 {
            return;
        }

        let node_width = self.node_size.width as usize;
        let node_height = self.node_size.height as usize;
        if x >= node_width || y >= node_height {
            return;
        }

        let local_left = x;
        let local_top = y;
        let local_right = x.saturating_add(width).min(node_width);
        let local_bottom = y.saturating_add(height).min(node_height);

        let abs_left = self.node_position.x as isize + local_left as isize;
        let abs_top = self.node_position.y as isize + local_top as isize;
        let abs_right = self.node_position.x as isize + local_right as isize;
        let abs_bottom = self.node_position.y as isize + local_bottom as isize;

        let clipped_left = abs_left.max(self.clip_rect.left as isize).max(0);
        let clipped_top = abs_top.max(self.clip_rect.top as isize).max(0);
        let clipped_right = abs_right.min(self.clip_rect.right as isize).max(0);
        let clipped_bottom = abs_bottom.min(self.clip_rect.bottom as isize).max(0);

        if clipped_right <= clipped_left || clipped_bottom <= clipped_top {
            return;
        }

        self.context.canvas.mark_damage(DamageRegion {
            x: clipped_left as usize,
            y: clipped_top as usize,
            width: (clipped_right - clipped_left) as usize,
            height: (clipped_bottom - clipped_top) as usize,
        });
    }

    /// Marks this component's current canvas bounds as excluded from fullscreen
    /// text selection. This mirrors CC Ink's `noSelect` metadata: it has no
    /// terminal output effect and is consumed by selection/copy overlays.
    pub fn mark_no_select(&mut self) {
        self.mark_no_select_region(
            0,
            0,
            self.node_size.width as usize,
            self.node_size.height as usize,
        );
    }

    /// Marks a component-local rectangle as excluded from fullscreen text selection.
    pub fn mark_no_select_region(&mut self, x: usize, y: usize, width: usize, height: usize) {
        self.mark_no_select_region_signed(x as isize, y as isize, width, height);
    }

    pub(crate) fn mark_no_select_region_signed(
        &mut self,
        x: isize,
        y: isize,
        width: usize,
        height: usize,
    ) {
        if width == 0 || height == 0 {
            return;
        }

        let clipped = self.clipped_canvas_region(x, y, width, height);
        self.canvas().mark_no_select_region(x, y, width, height);
        if let Some((x, y, width, height)) = clipped {
            // CC Ink's Output applies noSelect ops after writes and blits so
            // a parent <NoSelect> still wins when a clean child subtree is
            // restored from a cached/previous screen buffer. Keep the immediate
            // mark above so retained snapshots also preserve noSelect metadata,
            // then replay it before post-render overlays and at frame end.
            self.context
                .deferred_no_select
                .push(DeferredNoSelectRegion {
                    x,
                    y,
                    width,
                    height,
                });
        }
    }

    fn clipped_canvas_region(
        &self,
        x: isize,
        y: isize,
        width: usize,
        height: usize,
    ) -> Option<(usize, usize, usize, usize)> {
        let mut left = self.node_position.x as isize + x;
        let mut top = self.node_position.y as isize + y;
        let mut right = left + width as isize;
        let mut bottom = top + height as isize;

        left = left.max(self.clip_rect.left as isize).max(0);
        top = top.max(self.clip_rect.top as isize).max(0);
        right = right.min(self.clip_rect.right as isize).max(0);
        bottom = bottom.min(self.clip_rect.bottom as isize).max(0);

        if right <= left || bottom <= top {
            return None;
        }

        Some((
            left as usize,
            top as usize,
            (right - left) as usize,
            (bottom - top) as usize,
        ))
    }

    /// Declares that this component's fullscreen rows shifted vertically since
    /// the previous frame. The terminal renderer may turn this into a DECSTBM
    /// hardware scroll and then repaint the rows that changed.
    pub fn set_scroll_hint(&mut self, delta: i32) {
        if delta == 0 || self.node_size.height == 0 {
            return;
        }

        let canvas_width = self.context.canvas.width() as isize;
        if canvas_width <= 0 {
            return;
        }

        let abs_left = self.node_position.x as isize;
        let abs_right = self.node_position.x as isize + self.node_size.width as isize;
        let clipped_left = abs_left.max(self.clip_rect.left as isize).max(0);
        let clipped_right = abs_right
            .min(self.clip_rect.right as isize)
            .min(canvas_width)
            .max(0);
        if clipped_left != 0 || clipped_right < canvas_width {
            return;
        }

        let abs_top = self.node_position.y as isize;
        let abs_bottom = self.node_position.y as isize + self.node_size.height as isize;
        let clipped_top = abs_top.max(self.clip_rect.top as isize).max(0);
        let clipped_bottom = abs_bottom.min(self.clip_rect.bottom as isize).max(0);
        if clipped_bottom <= clipped_top {
            return;
        }

        self.context.canvas.set_scroll_hint(ScrollHint {
            top: clipped_top as usize,
            bottom: (clipped_bottom - 1) as usize,
            delta,
        });
    }

    /// Skips drawing this component's children for the current frame.
    ///
    /// This is intended for retained/cached subtree components that have
    /// already restored their child output into the canvas during
    /// `pre_component_draw`, mirroring CC Ink's clean-subtree blit fast path.
    pub fn skip_children(&mut self) {
        self.skip_children = true;
    }

    pub(crate) fn take_skip_children(&mut self) -> bool {
        let skip = self.skip_children;
        self.skip_children = false;
        skip
    }

    pub(crate) fn replay_deferred_no_select(&mut self) {
        // Re-apply queued noSelect regions without draining them. A later sibling
        // blit can still overwrite the bitmap, so ancestors/root replay the same
        // regions again before their post-render overlay hooks run.
        for region in self.context.deferred_no_select.iter().copied() {
            self.context.canvas.mark_no_select_region(
                region.x,
                region.y,
                region.width,
                region.height,
            );
        }
    }

    pub(crate) fn clear_deferred_no_select(&mut self) {
        self.context.deferred_no_select.clear();
    }

    /// Gets the root canvas for crate-level post-render hooks.
    pub(crate) fn root_canvas_mut(&mut self) -> &mut Canvas {
        self.context.canvas
    }

    /// Gets the region of the canvas that the component should be drawn to.
    pub fn canvas(&mut self) -> CanvasSubviewMut<'_> {
        self.context.canvas.subview_mut(
            self.node_position.x as _,
            self.node_position.y as _,
            self.clip_rect.left as _,
            self.clip_rect.top as _,
            self.clip_rect.right.saturating_sub(self.clip_rect.left) as _,
            self.clip_rect.bottom.saturating_sub(self.clip_rect.top) as _,
        )
    }

    /// Prepares to begin drawing a node by moving to the node's position and invoking the given
    /// closure.
    pub(crate) fn for_child_node_layout<F>(&mut self, node_id: NodeId, f: F)
    where
        F: FnOnce(&mut Self),
    {
        let old_node_id = self.node_id;
        let old_node_position = self.node_position;
        let old_node_size = self.node_size;
        let old_skip_children = self.skip_children;
        self.skip_children = false;
        self.node_id = node_id;
        let layout = self.layout();
        let mut next_position = Point {
            x: self.node_position.x + layout.location.x as i16,
            y: self.node_position.y + layout.location.y as i16,
        };
        if next_position.y < 0
            && self.style().position == Position::Absolute
            && !self.vertical_clip_active
        {
            // CC Ink's render-node-to-output.ts clamps negative screen-space Y
            // for absolute overlays so menus/tooltips that extend above the
            // viewport keep their top rows visible instead of clipping them.
            // iocraft's ScrollView uses an absolute child translated upward
            // inside an overflow-hidden viewport, so retain negative Y while
            // vertical clipping is active to preserve scroll-window semantics.
            next_position.y = 0;
        }
        self.node_position = next_position;
        self.node_size = Size {
            width: layout.size.width as u16,
            height: layout.size.height as u16,
        };
        f(self);
        let requested_skip_children = self.skip_children;
        self.node_id = old_node_id;
        self.node_position = old_node_position;
        self.node_size = old_node_size;
        self.skip_children = old_skip_children || requested_skip_children;
    }

    /// Returns whether this component is a zero-height box sharing a row with a sibling.
    ///
    /// CC Ink's `render-node-to-output.ts` uses this as a ghost-text guard for
    /// boxes whose Yoga height is squeezed to zero: if a later sibling paints on
    /// the same row, the zero-height box's children must not leave tail glyphs
    /// behind that sibling.
    pub(crate) fn zero_height_sibling_shares_y(&self) -> bool {
        let layout = self.layout();
        if layout.size.height != 0.0 {
            return false;
        }
        let Some(parent) = self.context.layout_engine.parent(self.node_id) else {
            return false;
        };
        let Ok(siblings) = self.context.layout_engine.children(parent) else {
            return false;
        };
        let Some(index) = siblings.iter().position(|sibling| *sibling == self.node_id) else {
            return false;
        };
        let my_top = layout.location.y;
        siblings[index + 1..]
            .iter()
            .chain(siblings[..index].iter().rev())
            .any(|sibling| {
                self.context
                    .layout_engine
                    .layout(*sibling)
                    .is_ok_and(|sibling_layout| sibling_layout.location.y == my_top)
            })
    }

    /// Prepares to begin drawing a node's children by shrinking the clipping rectangle if necessary.
    pub(crate) fn with_clip_rect_for_children<F>(&mut self, f: F)
    where
        F: FnOnce(&mut Self),
    {
        let overflow = self.style().overflow;
        if overflow.x == Overflow::Visible && overflow.y == Overflow::Visible {
            // No need to do anything.
            f(self);
            return;
        }

        let old_clip_rect = self.clip_rect;
        let old_vertical_clip_active = self.vertical_clip_active;
        let layout = self.layout();
        if overflow.x != Overflow::Visible {
            self.clip_rect.left = self
                .clip_rect
                .left
                .max((self.node_position.x + layout.border.left as i16).max(0) as u16);
            self.clip_rect.right = self.clip_rect.right.min(
                (self.node_position.x + self.node_size.width as i16 - layout.border.right as i16)
                    .max(0) as u16,
            );
        }
        if overflow.y != Overflow::Visible {
            self.vertical_clip_active = true;
            self.clip_rect.top = self
                .clip_rect
                .top
                .max((self.node_position.y + layout.border.top as i16).max(0) as u16);
            self.clip_rect.bottom = self.clip_rect.bottom.min(
                (self.node_position.y + self.node_size.height as i16 - layout.border.bottom as i16)
                    .max(0) as u16,
            );
        }
        f(self);
        self.clip_rect = old_clip_rect;
        self.vertical_clip_active = old_vertical_clip_active;
    }
}

/// The measure function of the current component, which is invoked to calculate the area that the
/// component's content should occupy.
pub type MeasureFunc =
    Box<dyn Fn(Size<Option<f32>>, Size<AvailableSpace>, &Style) -> Size<f32> + Send>;

#[derive(Default)]
pub(crate) struct LayoutEngineNodeContext {
    measure_func: Option<MeasureFunc>,
}

/// Newtype around [`TaffyTree`] that restores `Send`.
///
/// SAFETY: `TaffyTree` lost its `Send` implementation in taffy 0.8, solely because the
/// `calc()` feature stores type-erased `*const ()` handles inside style values
/// (`CompactLength`'s tagged-pointer representation). iocraft never constructs `calc()`
/// values — `TaffyTree`'s high-level API offers no way to do so, and every style we set
/// comes from `LayoutStyle`, which only produces `length`/`percent`/`auto` variants.
/// Therefore no actual pointer is ever stored, let alone dereferenced across threads.
///
/// This approach was endorsed by the taffy maintainer in
/// <https://github.com/ccbrown/iocraft/issues/119>. If iocraft ever exposes `calc()`,
/// this impl must be replaced with the generic-`Calc` mechanism tracked in
/// <https://github.com/DioxusLabs/taffy/pull/855>.
pub(crate) struct LayoutEngine(TaffyTree<LayoutEngineNodeContext>);

unsafe impl Send for LayoutEngine {}

impl LayoutEngine {
    pub fn new() -> Self {
        Self(TaffyTree::new())
    }
}

impl std::ops::Deref for LayoutEngine {
    type Target = TaffyTree<LayoutEngineNodeContext>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for LayoutEngine {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LayoutSnapshot {
    x: i16,
    y: i16,
    width: u16,
    height: u16,
}

struct Tree<'a> {
    layout_engine: LayoutEngine,
    wrapper_node_id: NodeId,
    root_component: InstantiatedComponent,
    root_component_props: AnyProps<'a>,
    system_context: SystemContext,
    root_view_event_context: crate::components::ViewFocusParentContext,
    prev_layout_snapshots: HashMap<NodeId, LayoutSnapshot>,
}

struct RenderOutput {
    canvas: Canvas,
    did_clear_terminal_output: bool,
    alternate_screen_changed: bool,
    phase_profile: RenderPhaseProfile,
}

impl<'a> Tree<'a> {
    fn new(mut props: AnyProps<'a>, helper: Box<dyn ComponentHelperExt>) -> Self {
        let mut layout_engine = LayoutEngine::new();
        let root_node_id = layout_engine
            .new_leaf_with_context(Style::default(), LayoutEngineNodeContext::default())
            .expect("we should be able to add the root");
        let wrapper_node_id = layout_engine
            .new_with_children(Style::default(), &[root_node_id])
            .expect("we should be able to add the root");
        Self {
            layout_engine,
            wrapper_node_id,
            root_component: InstantiatedComponent::new(root_node_id, props.borrow(), helper),
            root_component_props: props,
            system_context: SystemContext::new(),
            root_view_event_context: crate::components::ViewFocusParentContext::shared_root(),
            prev_layout_snapshots: HashMap::new(),
        }
    }

    fn render(
        &mut self,
        max_width: Option<usize>,
        mut terminal: Option<&mut Terminal<'_>>,
        profile_enabled: bool,
    ) -> RenderOutput {
        self.system_context.begin_render_frame();
        let update_start = profile_enabled.then(std::time::Instant::now);
        let exit_on_ctrl_c = terminal
            .as_deref()
            .map(Terminal::exit_on_ctrl_c)
            .unwrap_or(true);
        let mut wrapper_child_node_ids = vec![self.root_component.node_id()];
        let (did_clear_terminal_output, force_full_repaint, invalidate_prev_frame) = {
            let mut context = UpdateContext {
                terminal: terminal.as_deref_mut(),
                layout_engine: &mut self.layout_engine,
                did_clear_terminal_output: false,
                force_full_repaint: false,
                invalidate_prev_frame: false,
            };
            let mut component_context_stack = ContextStack::root(&mut self.system_context);
            // CC Ink dispatches hit-test/focus events from a single DOM root.
            // Seed an equivalent root event context for the whole iocraft tree so
            // transparent root components (Fragment, ContextProvider, etc.) share
            // one topmost click/hover registry instead of acting like independent
            // roots. The hovered set persists across frames like CC Ink's
            // `hoveredNodes`, while the per-frame hit registries are rebuilt below.
            self.root_view_event_context.begin_root_event_frame();
            component_context_stack.with_context(
                Some(Context::owned(ExitOnCtrlCContext(exit_on_ctrl_c))),
                |component_context_stack| {
                    component_context_stack.with_context(
                        Some(Context::owned(self.root_view_event_context.clone())),
                        |component_context_stack| {
                            self.root_component.update(
                                &mut context,
                                &mut wrapper_child_node_ids,
                                component_context_stack,
                                self.root_component_props.borrow(),
                            );
                        },
                    );
                },
            );
            (
                context.did_clear_terminal_output,
                context.force_full_repaint,
                context.invalidate_prev_frame,
            )
        };
        let update_duration = update_start.map_or(Duration::ZERO, |start| start.elapsed());
        let layout_start = profile_enabled.then(std::time::Instant::now);
        let alternate_screen_changed = terminal
            .as_deref_mut()
            .map(|term| {
                term.set_dynamic_alternate_screen(self.system_context.alternate_screen_request())
                    .unwrap()
            })
            .unwrap_or(false);
        let fullscreen_size = terminal.as_ref().and_then(|term| {
            if term.is_fullscreen() {
                term.size()
                    .map(|(cols, rows)| (cols as usize, rows as usize))
            } else {
                None
            }
        });
        self.layout_engine
            .set_children(self.wrapper_node_id, &wrapper_child_node_ids)
            .expect("we should be able to set the children");
        let mut wrapper_style = Style::default();
        if let Some(max_width) = max_width {
            // Mirror CC Ink's root layout contract: when rendering to a
            // terminal, the root is laid out against the terminal columns, so
            // percentage widths and right/center alignment resolve against the
            // viewport instead of shrink-wrapping to content.
            wrapper_style.size.width = taffy::style::Dimension::length(max_width as f32);
        }
        if let Some((_, rows)) = fullscreen_size {
            // Alt-screen's screen buffer is exactly terminalRows tall. Make the
            // root layout definite as well, so percentage heights resolve
            // against the viewport just like CC Ink's <AlternateScreen> Box.
            wrapper_style.size.height = taffy::style::Dimension::length(rows as f32);
        }
        self.layout_engine
            .set_style(self.wrapper_node_id, wrapper_style)
            .expect("we should be able to set the wrapper style");

        self.layout_engine
            .compute_layout_with_measure(
                self.wrapper_node_id,
                Size {
                    width: max_width
                        .map(|w| AvailableSpace::Definite(w as _))
                        .unwrap_or(AvailableSpace::MaxContent),
                    height: fullscreen_size
                        .map(|(_, rows)| AvailableSpace::Definite(rows as _))
                        .unwrap_or(AvailableSpace::MaxContent),
                },
                |known_dimensions, available_space, _node_id, node_context, style| {
                    match node_context.and_then(|cx| cx.measure_func.as_ref()) {
                        Some(f) => f(known_dimensions, available_space, style),
                        None => Size::ZERO,
                    }
                },
            )
            .expect("we should be able to compute the layout");

        debug_dump_layout_tree(&self.layout_engine, self.wrapper_node_id);
        let layout_shifted = self.update_layout_shift_state();
        let layout_duration = layout_start.map_or(Duration::ZERO, |start| start.elapsed());
        let draw_start = profile_enabled.then(std::time::Instant::now);
        let wrapper_layout = self
            .layout_engine
            .layout(self.wrapper_node_id)
            .expect("we should be able to get the wrapper layout");
        let canvas_width = fullscreen_size
            .map(|(cols, _)| cols)
            .unwrap_or(wrapper_layout.size.width as usize);
        let canvas_height = fullscreen_size
            .map(|(_, rows)| rows)
            .unwrap_or(wrapper_layout.size.height as usize);
        let mut canvas = Canvas::new(canvas_width, canvas_height);
        let mut deferred_no_select = Vec::new();
        let root_layout = self
            .layout_engine
            .layout(self.root_component.node_id())
            .expect("we should be able to get the root layout");
        {
            let mut drawer = ComponentDrawer {
                node_id: self.root_component.node_id(),
                node_position: Point {
                    x: root_layout.location.x as _,
                    y: root_layout.location.y as _,
                },
                node_size: Size {
                    width: root_layout.size.width as _,
                    height: root_layout.size.height as _,
                },
                clip_rect: Rect {
                    left: 0,
                    right: canvas_width as _,
                    top: 0,
                    bottom: canvas_height as _,
                },
                vertical_clip_active: false,
                skip_children: false,
                context: DrawContext {
                    layout_engine: &self.layout_engine,
                    canvas: &mut canvas,
                    deferred_no_select: &mut deferred_no_select,
                },
            };
            self.root_component.draw(&mut drawer);
            drawer.replay_deferred_no_select();
            drawer.clear_deferred_no_select();
        }
        if fullscreen_size.is_none() {
            canvas.trim_trailing_blank_rows();
        }
        if force_full_repaint {
            canvas.force_full_repaint();
        }
        if invalidate_prev_frame || layout_shifted {
            canvas.mark_damage(DamageRegion {
                x: 0,
                y: 0,
                width: canvas.width(),
                height: canvas.height(),
            });
        }
        let draw_duration = draw_start.map_or(Duration::ZERO, |start| start.elapsed());
        RenderOutput {
            canvas,
            did_clear_terminal_output,
            alternate_screen_changed,
            phase_profile: RenderPhaseProfile {
                update: update_duration,
                layout: layout_duration,
                draw: draw_duration,
            },
        }
    }

    fn update_layout_shift_state(&mut self) -> bool {
        let mut current = HashMap::new();
        self.collect_layout_snapshots(self.wrapper_node_id, &mut current);
        let shifted = !self.prev_layout_snapshots.is_empty()
            && (self.prev_layout_snapshots.len() != current.len()
                || current.iter().any(|(node_id, snapshot)| {
                    self.prev_layout_snapshots.get(node_id) != Some(snapshot)
                }));
        self.prev_layout_snapshots = current;
        shifted
    }

    fn collect_layout_snapshots(&self, node_id: NodeId, out: &mut HashMap<NodeId, LayoutSnapshot>) {
        if let Ok(layout) = self.layout_engine.layout(node_id) {
            out.insert(
                node_id,
                LayoutSnapshot {
                    x: layout.location.x as i16,
                    y: layout.location.y as i16,
                    width: layout.size.width as u16,
                    height: layout.size.height as u16,
                },
            );
        }
        if let Ok(children) = self.layout_engine.children(node_id) {
            for child in children {
                self.collect_layout_snapshots(child, out);
            }
        }
    }

    async fn terminal_render_loop(
        &mut self,
        mut term: Terminal<'_>,
        throttle: Option<std::time::Duration>,
        mut frame_profile: Option<FrameProfileCallback<'_>>,
    ) -> io::Result<()> {
        term.start_event_stream()?;
        let mut prev_canvas: Option<Canvas> = None;
        let mut prev_terminal_size: Option<(u16, u16)> = None;
        let mut mouse_capture_enabled: Option<bool> = None;
        loop {
            // Self-healing: if the process was just resumed from suspension (Ctrl+Z
            // followed by `fg`), the shell has reset the terminal modes and overwritten
            // parts of the screen. Re-apply our modes and forget the previous canvas so
            // the next write is a full redraw rather than a row diff against content
            // that no longer exists.
            if term.take_resumed() {
                term.reinitialize_after_resume()?;
                prev_canvas = None;
                prev_terminal_size = None;
            }
            term.refresh_size();
            let terminal_size = term.size();
            let terminal_size_changed =
                prev_canvas.is_some() && prev_terminal_size != terminal_size;
            let profile_enabled = frame_profile.is_some();
            let frame_start = profile_enabled.then(std::time::Instant::now);
            let mut frame_phases = RenderFramePhases::default();
            let mut repaint = None;
            term.synchronized_update(|mut term| {
                let mut output = self.render(
                    terminal_size.map(|(w, _)| w as usize),
                    Some(&mut term),
                    profile_enabled,
                );
                let changed_cells = if profile_enabled {
                    count_changed_cells(prev_canvas.as_ref(), &output.canvas)
                } else {
                    0
                };
                debug_log_render_frame(
                    &output,
                    prev_canvas.as_ref(),
                    terminal_size,
                    terminal_size_changed,
                    changed_cells,
                );
                let should_repaint = output.did_clear_terminal_output
                    || output.alternate_screen_changed
                    || terminal_size_changed
                    || output.canvas.should_force_full_repaint()
                    || output.canvas.has_damage()
                    || prev_canvas
                        .as_ref()
                        .is_some_and(|canvas| canvas.has_damage())
                    || prev_canvas.as_ref() != Some(&output.canvas);
                let previous_damage = prev_canvas.as_ref().and_then(Canvas::damage_region);
                if should_repaint {
                    let reason = classify_debug_repaint_reason(
                        &output,
                        prev_canvas.as_ref(),
                        terminal_size_changed,
                    );
                    repaint = Some(DebugRepaintInfo {
                        reason,
                        damage: output.canvas.damage_region(),
                        previous_damage,
                        changed_cells,
                        canvas_width: output.canvas.width(),
                        canvas_height: output.canvas.height(),
                    });
                    let write_start = profile_enabled.then(std::time::Instant::now);
                    let prev =
                        if output.did_clear_terminal_output || output.alternate_screen_changed {
                            None
                        } else {
                            prev_canvas.as_ref()
                        };
                    term.write_canvas(prev, &output.canvas)?;
                    // Position (or hide) the physical cursor after the frame has been
                    // committed, so IMEs and screen readers can anchor to the caret of
                    // a focused text input.
                    term.position_cursor(output.canvas.cursor_declaration())?;
                    frame_phases.terminal_write =
                        write_start.map_or(Duration::ZERO, |start| start.elapsed());
                }
                frame_phases.update = output.phase_profile.update;
                frame_phases.layout = output.phase_profile.layout;
                frame_phases.draw = output.phase_profile.draw;
                frame_phases.changed_cells = changed_cells;
                frame_phases.canvas_width = output.canvas.width();
                frame_phases.canvas_height = output.canvas.height();
                output.canvas.clear_force_full_repaint();
                // Keep damage on the retained previous canvas. CC Ink's
                // diffEach unions prev.damage with next.damage so cleanup
                // frames still scan regions dirtied by the last render, while
                // Canvas equality ignores damage so this does not wake idle
                // renders by itself.
                prev_canvas = Some(output.canvas);
                if let Some(canvas) = prev_canvas.as_ref() {
                    term.set_event_cell_snapshot(canvas);
                }
                prev_terminal_size = terminal_size;
                Ok(())
            })?;
            if let (Some(callback), Some(start)) = (frame_profile.as_mut(), frame_start) {
                callback(RenderFrameProfile {
                    duration: start.elapsed(),
                    phases: frame_phases,
                    repaint,
                });
            }
            let last_frame = std::time::Instant::now();
            if let Some(requested) = self.system_context.mouse_capture() {
                if mouse_capture_enabled != Some(requested) {
                    if requested {
                        term.enable_mouse_capture()?;
                    } else {
                        term.disable_mouse_capture()?;
                    }
                    mouse_capture_enabled = Some(requested);
                }
            }
            if let Some(flags) = self.system_context.keyboard_enhancement_flags() {
                term.set_keyboard_enhancement_flags(flags)?;
            }
            if let Some(title) = self.system_context.terminal_title() {
                let _ = crate::ansi::terminal_title(term.render_output(), title);
                let _ = term.render_output().flush();
            }
            if self.system_context.should_exit() || term.received_ctrl_c() {
                break;
            }
            select(self.root_component.wait().boxed(), term.wait().boxed()).await;
            if term.take_resumed() {
                term.reinitialize_after_resume()?;
                prev_canvas = None;
                prev_terminal_size = None;
                continue;
            }
            term.resolve_pending_ctrl_c();
            if term.received_ctrl_c() {
                break;
            }
            // Frame throttling: after the first change, keep absorbing further changes
            // until the frame interval has elapsed, so high-frequency state updates
            // (animations, streaming output, progress ticks) coalesce into one frame.
            //
            // Crucially this keeps polling the component tree the whole time: component
            // futures (use_future et al) are driven by this loop, so a plain sleep
            // would freeze them and merely slow the app down instead of coalescing.
            // Input latency is bounded by the interval (≤ ~17ms at the default 60fps).
            if let Some(throttle) = throttle {
                let last = last_frame;
                let mut resumed_during_throttle = false;
                loop {
                    let remaining = throttle.saturating_sub(last.elapsed());
                    if remaining.is_zero() {
                        break;
                    }
                    let timed_out = {
                        let timer = futures_timer::Delay::new(remaining);
                        let timed_out = match futures::future::select(
                            timer,
                            select(self.root_component.wait().boxed(), term.wait().boxed()),
                        )
                        .await
                        {
                            futures::future::Either::Left(_) => true,
                            futures::future::Either::Right(_) => false,
                        };
                        if term.take_resumed() {
                            term.reinitialize_after_resume()?;
                            prev_canvas = None;
                            prev_terminal_size = None;
                            resumed_during_throttle = true;
                        }
                        term.resolve_pending_ctrl_c();
                        Ok::<bool, io::Error>(timed_out)
                    }?;
                    if resumed_during_throttle {
                        break;
                    }
                    if timed_out || term.received_ctrl_c() {
                        break;
                    }
                }
                if resumed_during_throttle {
                    continue;
                }
                if term.received_ctrl_c() {
                    break;
                }
            }
        }
        Ok(())
    }
}

fn count_changed_cells(prev: Option<&Canvas>, next: &Canvas) -> usize {
    let mut count = 0;
    if let Some(prev) = prev {
        prev.diff_each(next, |_| {
            count += 1;
            false
        });
    } else {
        let empty = Canvas::new(0, 0);
        empty.diff_each(next, |_| {
            count += 1;
            false
        });
    }
    count
}

fn debug_dump_layout_tree(layout_engine: &LayoutEngine, root_node_id: NodeId) {
    let Ok(path) = std::env::var("IOCRAFT_LAYOUT_DUMP") else {
        return;
    };
    if path.is_empty() {
        return;
    }
    let mut buf = String::new();
    fn rec(layout_engine: &LayoutEngine, node_id: NodeId, depth: usize, buf: &mut String) {
        if let Ok(layout) = layout_engine.layout(node_id) {
            let child_count = layout_engine
                .children(node_id)
                .map(|c| c.len())
                .unwrap_or(0);
            let style = layout_engine.style(node_id).ok();
            let _ = std::fmt::Write::write_fmt(
                buf,
                format_args!(
                    "{indent}{:?} x={:.1} y={:.1} w={:.1} h={:.1} children={} style={:?}\n",
                    node_id,
                    layout.location.x,
                    layout.location.y,
                    layout.size.width,
                    layout.size.height,
                    child_count,
                    style.map(|s| (
                        &s.display,
                        &s.flex_direction,
                        &s.size,
                        &s.min_size,
                        &s.max_size,
                        &s.gap,
                        &s.padding,
                        &s.margin,
                        s.flex_grow,
                        s.flex_shrink
                    )),
                    indent = "  ".repeat(depth),
                ),
            );
        }
        if depth >= 12 {
            return;
        }
        if let Ok(children) = layout_engine.children(node_id) {
            for child in children {
                rec(layout_engine, child, depth + 1, buf);
            }
        }
    }
    buf.push_str("--- layout ---\n");
    rec(layout_engine, root_node_id, 0, &mut buf);
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut file| std::io::Write::write_all(&mut file, buf.as_bytes()));
}

fn debug_log_render_frame(
    output: &RenderOutput,
    prev_canvas: Option<&Canvas>,
    terminal_size: Option<(u16, u16)>,
    terminal_size_changed: bool,
    changed_cells: usize,
) {
    let Ok(path) = std::env::var("IOCRAFT_FRAME_LOG") else {
        return;
    };

    let mut visible_rows = 0usize;
    let mut last_nonblank_row = 0usize;
    let canvas_text = output.canvas.to_string();
    for (idx, line) in canvas_text.lines().enumerate() {
        visible_rows = idx + 1;
        if !line.trim().is_empty() {
            last_nonblank_row = idx + 1;
        }
    }
    let trailing_blank_rows = visible_rows.saturating_sub(last_nonblank_row);

    let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    else {
        return;
    };

    use std::io::Write as _;
    let _ = writeln!(
        file,
        "canvas={}x{} term={:?} prev={} cursor={:?} changed={} clear={} alt={} resize={} last_nonblank={} trailing_blank={}",
        output.canvas.width(),
        output.canvas.height(),
        terminal_size,
        prev_canvas.map(Canvas::height).unwrap_or(0),
        output.canvas.cursor_declaration(),
        changed_cells,
        output.did_clear_terminal_output,
        output.alternate_screen_changed,
        terminal_size_changed,
        last_nonblank_row,
        trailing_blank_rows,
    );

    if trailing_blank_rows > 20 {
        if let Ok(dump_path) = std::env::var("IOCRAFT_FRAME_DUMP") {
            if let Ok(mut dump) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(dump_path)
            {
                let _ = writeln!(
                    dump,
                    "\n--- canvas {}x{} last_nonblank={} trailing_blank={} ---",
                    output.canvas.width(),
                    output.canvas.height(),
                    last_nonblank_row,
                    trailing_blank_rows,
                );
                let _ = writeln!(dump, "{}", canvas_text);
            }
        }
    }
}

fn classify_debug_repaint_reason(
    output: &RenderOutput,
    prev_canvas: Option<&Canvas>,
    terminal_size_changed: bool,
) -> DebugRepaintReason {
    if output.did_clear_terminal_output {
        DebugRepaintReason::TerminalCleared
    } else if output.alternate_screen_changed {
        DebugRepaintReason::AlternateScreenChanged
    } else if terminal_size_changed {
        DebugRepaintReason::TerminalResized
    } else if output.canvas.should_force_full_repaint() {
        DebugRepaintReason::ForceFullRepaint
    } else if output.canvas.has_damage() {
        DebugRepaintReason::CurrentDamage
    } else if prev_canvas.is_some_and(Canvas::has_damage) {
        DebugRepaintReason::PreviousDamage
    } else if prev_canvas.is_none() {
        DebugRepaintReason::FirstFrame
    } else {
        DebugRepaintReason::CanvasChanged
    }
}

pub(crate) fn render<E: ElementExt>(mut e: E, max_width: Option<usize>) -> Canvas {
    let h = e.helper();
    let mut tree = Tree::new(e.props_mut(), h);
    tree.render(max_width, None, false).canvas
}

pub(crate) async fn terminal_render_loop<E>(
    e: &mut E,
    term: Terminal<'_>,
    throttle: Option<std::time::Duration>,
    frame_profile: Option<FrameProfileCallback<'_>>,
) -> io::Result<()>
where
    E: ElementExt,
{
    let h = e.helper();
    let mut tree = Tree::new(e.props_mut(), h);
    tree.terminal_render_loop(term, throttle, frame_profile)
        .await
}

pub(crate) struct MockTerminalRenderLoop<'a> {
    output: MockTerminalOutputStream,
    render_loop: LocalBoxFuture<'a, io::Result<()>>,
    render_loop_is_done: bool,
}

impl Stream for MockTerminalRenderLoop<'_> {
    type Item = Canvas;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut task::Context<'_>) -> Poll<Option<Self::Item>> {
        let mut this = self.as_mut();

        if !this.render_loop_is_done && this.render_loop.poll_unpin(cx).is_ready() {
            this.render_loop_is_done = true;
        }

        this.output.poll_next_unpin(cx)
    }
}

pub(crate) fn mock_terminal_render_loop<'a, E>(
    e: &'a mut E,
    config: MockTerminalConfig,
) -> MockTerminalRenderLoop<'a>
where
    E: ElementExt + 'a,
{
    mock_terminal_render_loop_with_profile(e, config, None)
}

pub(crate) fn mock_terminal_render_loop_with_profile<'a, E>(
    e: &'a mut E,
    config: MockTerminalConfig,
    frame_profile: Option<FrameProfileCallback<'a>>,
) -> MockTerminalRenderLoop<'a>
where
    E: ElementExt + 'a,
{
    let (term, output) = Terminal::mock(config);
    MockTerminalRenderLoop {
        // No throttling for mock terminals: tests rely on deterministic, immediate
        // frame production.
        render_loop: terminal_render_loop(e, term, None, frame_profile).boxed_local(),
        render_loop_is_done: false,
        output,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prelude::*;
    use core::future::Future;
    use macro_rules_attribute::apply;
    use smol_macros::test;

    #[test]
    fn test_renderer_node_generation_state_prevents_key_reuse_blits() {
        let mut generations = RendererNodeGenerationState::<&'static str>::new();
        let mut cache = RendererNodeCache::<RendererStableNodeId<&'static str>>::new();
        let layout = CachedLayoutBounds {
            x: 1,
            y: 2,
            width: 10,
            height: 1,
            top: Some(2),
        };

        let row0 = generations.current_id("row");
        assert_eq!(row0.generation, 0);
        cache.set_layout(row0.clone(), layout);
        assert!(cache.can_blit(&row0, layout));

        let removed = generations.remove(&"row").unwrap();
        assert_eq!(removed, row0);
        cache.remove_layout(&removed);
        let row1 = generations.current_id("row");
        assert_eq!(row1.generation, 1);
        assert_ne!(row1, removed);
        assert!(!cache.can_blit(&row1, layout));

        let remounted = generations.remount("row");
        assert_eq!(remounted.generation, 2);
        assert_eq!(generations.id(&"row"), Some(remounted.clone()));

        let other = generations.current_id("other");
        cache.set_layout(other.clone(), CachedLayoutBounds { y: 3, ..layout });
        let removed = generations.bump_unretained_keys(["row"]);
        assert_eq!(removed, vec![other.clone()]);
        cache.remove_layout(&other);
        assert_eq!(generations.current_id("other").generation, 1);
        assert_eq!(
            generations.len(),
            2,
            "generation tombstones prevent stale key reuse"
        );

        generations.clear();
        assert!(generations.is_empty());
    }

    #[test]
    fn test_renderer_retained_tree_reconciler_maps_logical_keys_to_stable_ids() {
        let mut reconciler = RendererRetainedTreeReconciler::<&'static str>::new();
        let root_id = reconciler.register_root("root");
        let (row_id, parent_id) = reconciler.attach("row", "root");
        let (leaf_id, row_parent_id) = reconciler.attach("leaf", "row");
        assert_eq!(parent_id, root_id);
        assert_eq!(row_parent_id, row_id);
        reconciler.clear_dirty();

        assert!(reconciler.mark_dirty(&"leaf", true));
        assert!(reconciler.is_dirty(&"leaf"));
        assert!(reconciler.is_dirty(&"row"));
        assert!(reconciler.is_dirty(&"root"));

        let root_layout = CachedLayoutBounds {
            x: 0,
            y: 0,
            width: 20,
            height: 4,
            top: Some(0),
        };
        let row_layout = CachedLayoutBounds {
            x: 0,
            y: 1,
            width: 20,
            height: 2,
            top: Some(1),
        };
        let leaf_layout = CachedLayoutBounds {
            x: 0,
            y: 2,
            width: 20,
            height: 1,
            top: Some(2),
        };

        reconciler.begin_frame();
        for (key, layout) in [
            ("root", root_layout),
            ("row", row_layout),
            ("leaf", leaf_layout),
        ] {
            let plan = reconciler.plan_node(RetainedLogicalTreeNodeInput {
                key,
                current_layout: layout,
                skip_self_blit: false,
                pending_scroll_delta: false,
                previous_screen_available: true,
                hidden: false,
                absolute: false,
            });
            assert_eq!(plan.plan.action, RetainedNodeRenderAction::Render);
            reconciler.commit_node_plan(&plan);
        }
        assert_eq!(
            reconciler
                .retained_state()
                .frame_state()
                .cache()
                .layout(&leaf_id),
            Some(leaf_layout)
        );

        let removed = reconciler.remove_subtree(&"row", true);
        assert!(removed.contains(&row_id));
        assert!(removed.contains(&leaf_id));
        assert_eq!(reconciler.current_id("row").generation, 1);
        assert_eq!(reconciler.current_id("leaf").generation, 1);
        assert!(reconciler.is_dirty(&"root"));
        assert!(
            reconciler.begin_frame(),
            "absolute subtree removal poisons next frame blits"
        );

        let root_after_remove = reconciler.plan_node(RetainedLogicalTreeNodeInput {
            key: "root",
            current_layout: root_layout,
            skip_self_blit: false,
            pending_scroll_delta: false,
            previous_screen_available: true,
            hidden: false,
            absolute: false,
        });
        assert_eq!(
            root_after_remove.plan.action,
            RetainedNodeRenderAction::Render
        );
        assert_eq!(
            root_after_remove.plan.pending_clear_regions,
            vec![row_layout.into()]
        );
        assert!(root_after_remove.plan.has_removed_child);

        reconciler.clear();
        assert!(reconciler.generations().is_empty());
    }

    #[test]
    fn test_scroll_fast_path_plan_matches_cc_ink_scrollbox_guards_and_regions() {
        let viewport = CachedClearRegion {
            x: 2,
            y: 10,
            width: 20,
            height: 5,
        };

        assert_eq!(plan_scroll_fast_path(viewport, 0, []), None);
        assert_eq!(plan_scroll_fast_path(viewport, 5, []), None);
        assert_eq!(
            is_scroll_fast_path_content_delta_safe(3, 0),
            true,
            "pure scroll is safe"
        );
        assert_eq!(
            is_scroll_fast_path_content_delta_safe(3, 3),
            true,
            "bottom append matching the scroll delta is safe"
        );
        assert_eq!(
            is_scroll_fast_path_content_delta_safe(-3, -3),
            false,
            "scroll-up plus shrink/removal must fall back to full render"
        );
        assert_eq!(is_scroll_fast_path_content_delta_safe(3, 1), false);

        let plan = plan_scroll_fast_path(
            viewport,
            2,
            [
                CachedClearRegion {
                    x: 0,
                    y: 12,
                    width: 6,
                    height: 1,
                },
                CachedClearRegion {
                    x: 0,
                    y: 15,
                    width: 6,
                    height: 1,
                },
            ],
        )
        .expect("delta smaller than viewport should plan a fast path");

        assert_eq!(plan.blit_region, viewport);
        assert_eq!(plan.delta, 2);
        assert_eq!(
            plan.edge_region,
            CachedClearRegion {
                x: 2,
                y: 13,
                width: 20,
                height: 2,
            },
            "positive delta repaints the bottom edge rows"
        );
        assert_eq!(
            plan.absolute_repair_regions,
            vec![CachedClearRegion {
                x: 2,
                y: 10,
                width: 20,
                height: 1,
            }],
            "absolute overlay pixels shifted into stable rows need full-width repair"
        );

        let up = plan_scroll_fast_path(
            viewport,
            -2,
            [CachedClearRegion {
                x: 0,
                y: 11,
                width: 6,
                height: 2,
            }],
        )
        .unwrap();
        assert_eq!(
            up.edge_region,
            CachedClearRegion {
                x: 2,
                y: 10,
                width: 20,
                height: 2,
            },
            "negative delta repaints the top edge rows"
        );
        assert_eq!(
            up.absolute_repair_regions,
            vec![CachedClearRegion {
                x: 2,
                y: 13,
                width: 20,
                height: 2,
            }]
        );
    }

    #[test]
    fn test_apply_scroll_fast_path_to_canvas_blits_shifts_and_clears_repairs() {
        let mut previous = Canvas::new(6, 4);
        for (row, text) in ["aaaaaa", "bbbbbb", "cccccc", "dddddd"]
            .into_iter()
            .enumerate()
        {
            previous.subview_mut(0, 0, 0, 0, 6, 4).set_text(
                0,
                row as isize,
                text,
                CanvasTextStyle::default(),
            );
        }

        let plan = plan_scroll_fast_path(
            CachedClearRegion {
                x: 0,
                y: 0,
                width: 6,
                height: 4,
            },
            2,
            [CachedClearRegion {
                x: 2,
                y: 3,
                width: 2,
                height: 1,
            }],
        )
        .unwrap();
        assert_eq!(
            plan.absolute_repair_regions,
            vec![CachedClearRegion {
                x: 0,
                y: 1,
                width: 6,
                height: 1,
            }]
        );

        let mut next = Canvas::new(6, 4);
        assert!(apply_scroll_fast_path_to_canvas(
            &mut next, &previous, &plan
        ));
        assert_eq!(next.get_text(0, 0, 6, 1), "cccccc");
        for y in 1..4 {
            for x in 0..6 {
                assert!(next.cell(x, y).is_some_and(|cell| cell.is_empty()));
            }
        }
        assert_eq!(
            next.damage_region(),
            Some(DamageRegion {
                x: 0,
                y: 0,
                width: 6,
                height: 4,
            })
        );
        assert_eq!(
            next.scroll_hint(),
            Some(ScrollHint {
                top: 0,
                bottom: 3,
                delta: 2,
            })
        );
        assert_eq!(scroll_fast_path_plan_to_scroll_hint(&plan, 5), None);

        let empty_plan = ScrollFastPathPlan {
            blit_region: CachedClearRegion {
                x: 0,
                y: 10,
                width: 6,
                height: 1,
            },
            delta: 1,
            edge_region: CachedClearRegion::default(),
            absolute_repair_regions: Vec::new(),
        };
        assert!(!apply_scroll_fast_path_to_canvas(
            &mut next,
            &previous,
            &empty_plan
        ));
    }

    #[test]
    fn test_scroll_fast_path_child_repairs_match_cc_ink_second_pass_cases() {
        let viewport = CachedClearRegion {
            x: 2,
            y: 10,
            width: 20,
            height: 5,
        };
        let edge = CachedClearRegion {
            x: 2,
            y: 13,
            width: 20,
            height: 2,
        };
        let repairs = plan_scroll_fast_path_child_repairs(
            viewport,
            8,
            2,
            2,
            edge,
            [
                ScrollFastPathChild {
                    key: "clean-before",
                    top: 2,
                    height: 1,
                    cached_y: Some(10),
                    cached_height: Some(1),
                    dirty: false,
                },
                ScrollFastPathChild {
                    key: "dirty-stable",
                    top: 3,
                    height: 1,
                    cached_y: Some(11),
                    cached_height: Some(1),
                    dirty: true,
                },
                ScrollFastPathChild {
                    key: "uncached-stable",
                    top: 4,
                    height: 1,
                    cached_y: None,
                    cached_height: None,
                    dirty: false,
                },
                ScrollFastPathChild {
                    key: "dirty-edge",
                    top: 5,
                    height: 1,
                    cached_y: Some(13),
                    cached_height: Some(1),
                    dirty: true,
                },
            ],
        );

        assert_eq!(
            repairs,
            vec![
                ScrollFastPathChildRepair {
                    key: "dirty-stable",
                    region: CachedClearRegion {
                        x: 2,
                        y: 11,
                        width: 20,
                        height: 1,
                    },
                },
                ScrollFastPathChildRepair {
                    key: "uncached-stable",
                    region: CachedClearRegion {
                        x: 2,
                        y: 12,
                        width: 20,
                        height: 1,
                    },
                },
            ]
        );
    }

    #[test]
    fn test_scroll_fast_path_frame_state_tracks_previous_content_and_repairs() {
        let viewport = CachedClearRegion {
            x: 0,
            y: 5,
            width: 20,
            height: 6,
        };
        let mut state = ScrollFastPathFrameState::new();

        state.begin_frame();
        let first = state.plan_frame(ScrollFastPathFrameInput::<&'static str> {
            viewport,
            content_y: 5,
            scroll_top: 0,
            content_height: 30,
            children: Vec::new(),
        });
        assert_eq!(
            first.fast_path, None,
            "no previous frame means no shift plan"
        );
        assert!(!first.viewport_stable);
        state.record_absolute_rect(CachedClearRegion {
            x: 3,
            y: 7,
            width: 4,
            height: 1,
        });
        state.commit_frame(viewport, 5, 30);

        state.begin_frame();
        assert_eq!(state.previous_absolute_rects().len(), 1);
        let plan = state.plan_frame(ScrollFastPathFrameInput {
            viewport,
            content_y: 3,
            scroll_top: 2,
            content_height: 30,
            children: vec![
                ScrollFastPathChild {
                    key: "dirty-stable",
                    top: 3,
                    height: 1,
                    cached_y: Some(8),
                    cached_height: Some(1),
                    dirty: true,
                },
                ScrollFastPathChild {
                    key: "clean-edge",
                    top: 7,
                    height: 1,
                    cached_y: Some(10),
                    cached_height: Some(1),
                    dirty: false,
                },
            ],
        });
        assert!(plan.viewport_stable);
        assert!(plan.content_delta_safe);
        assert_eq!(plan.delta, 2);
        let fast = plan
            .fast_path
            .expect("stable small scroll should use fast path");
        assert_eq!(fast.edge_region.y, 9);
        assert_eq!(fast.absolute_repair_regions.len(), 1);
        assert_eq!(
            plan.child_repairs,
            vec![ScrollFastPathChildRepair {
                key: "dirty-stable",
                region: CachedClearRegion {
                    x: 0,
                    y: 6,
                    width: 20,
                    height: 1,
                },
            }]
        );
        state.commit_frame(viewport, 3, 30);

        state.begin_frame();
        let unsafe_growth = state.plan_frame(ScrollFastPathFrameInput::<&'static str> {
            viewport,
            content_y: 1,
            scroll_top: 4,
            content_height: 31,
            children: Vec::new(),
        });
        assert_eq!(unsafe_growth.delta, 2);
        assert_eq!(unsafe_growth.content_height_delta, 1);
        assert!(!unsafe_growth.content_delta_safe);
        assert_eq!(unsafe_growth.fast_path, None);
    }

    #[test]
    fn test_apply_scroll_fast_path_frame_plan_to_canvas_clears_child_repairs() {
        let mut previous = Canvas::new(8, 5);
        {
            let mut view = previous.subview_mut(0, 0, 0, 0, 8, 5);
            for row in 0..5 {
                view.set_text(
                    0,
                    row as isize,
                    &format!("row{row}"),
                    CanvasTextStyle::default(),
                );
            }
        }
        previous.clear_damage();

        let fast_path = ScrollFastPathPlan {
            blit_region: CachedClearRegion {
                x: 0,
                y: 0,
                width: 8,
                height: 5,
            },
            delta: 1,
            edge_region: CachedClearRegion {
                x: 0,
                y: 4,
                width: 8,
                height: 1,
            },
            absolute_repair_regions: vec![CachedClearRegion {
                x: 0,
                y: 1,
                width: 8,
                height: 1,
            }],
        };
        let frame_plan = ScrollFastPathFramePlan {
            delta: 1,
            content_height_delta: 0,
            viewport_stable: true,
            content_delta_safe: true,
            fast_path: Some(fast_path),
            child_repairs: vec![ScrollFastPathChildRepair {
                key: "dirty-stable",
                region: CachedClearRegion {
                    x: 0,
                    y: 2,
                    width: 8,
                    height: 1,
                },
            }],
        };

        let mut next = Canvas::new(8, 5);
        assert!(apply_scroll_fast_path_frame_plan_to_canvas(
            &mut next,
            &previous,
            &frame_plan
        ));
        assert_eq!(next.get_text(0, 1, 8, 1), "");
        assert_eq!(next.get_text(0, 2, 8, 1), "");
        assert_eq!(next.get_text(0, 4, 8, 1), "");
        assert_eq!(
            next.scroll_hint(),
            Some(ScrollHint {
                top: 0,
                bottom: 4,
                delta: 1,
            })
        );

        let no_fast_path = ScrollFastPathFramePlan::<&'static str> {
            fast_path: None,
            child_repairs: Vec::new(),
            delta: 0,
            content_height_delta: 0,
            viewport_stable: true,
            content_delta_safe: true,
        };
        assert!(!apply_scroll_fast_path_frame_plan_to_canvas(
            &mut next,
            &previous,
            &no_fast_path
        ));
    }

    #[test]
    fn test_scroll_fast_path_frame_plan_to_terminal_patch_is_fullscreen_opt_in_bridge() {
        let frame_plan = ScrollFastPathFramePlan {
            delta: 1,
            content_height_delta: 0,
            viewport_stable: true,
            content_delta_safe: true,
            fast_path: Some(ScrollFastPathPlan {
                blit_region: CachedClearRegion {
                    x: 0,
                    y: 1,
                    width: 8,
                    height: 4,
                },
                delta: 1,
                edge_region: CachedClearRegion {
                    x: 0,
                    y: 4,
                    width: 8,
                    height: 1,
                },
                absolute_repair_regions: vec![CachedClearRegion {
                    x: 0,
                    y: 2,
                    width: 8,
                    height: 1,
                }],
            }),
            child_repairs: vec![ScrollFastPathChildRepair {
                key: "dirty-stable",
                region: CachedClearRegion {
                    x: 0,
                    y: 3,
                    width: 8,
                    height: 1,
                },
            }],
        };

        let patch = scroll_fast_path_frame_plan_to_terminal_patch(
            &frame_plan,
            8,
            TerminalScrollHintBounds {
                previous_screen_height: 6,
                next_screen_height: 6,
            },
        )
        .unwrap()
        .expect("full-width small scroll should produce a terminal patch");
        assert_eq!(
            patch.scroll_hint,
            ScrollHint {
                top: 1,
                bottom: 4,
                delta: 1,
            }
        );
        assert_eq!(patch.scroll_patch_ansi, "\x1b[2;5r\x1b[1S\x1b[r\x1b[H");
        assert_eq!(patch.edge_region.y, 4);
        assert_eq!(patch.absolute_repair_regions.len(), 1);
        assert_eq!(patch.child_repairs[0].key, "dirty-stable");

        assert!(
            scroll_fast_path_frame_plan_to_terminal_patch(
                &frame_plan,
                9,
                TerminalScrollHintBounds {
                    previous_screen_height: 6,
                    next_screen_height: 6,
                },
            )
            .unwrap()
            .is_none(),
            "partial-width retained scrolls must not emit DECSTBM"
        );
        assert_eq!(
            scroll_fast_path_frame_plan_to_terminal_patch(
                &frame_plan,
                8,
                TerminalScrollHintBounds {
                    previous_screen_height: 4,
                    next_screen_height: 6,
                },
            ),
            Err(TerminalScrollHintRejection::OutOfBounds)
        );

        assert_eq!(
            plan_scroll_fast_path_frame_terminal_patch(
                &frame_plan,
                8,
                ScrollFastPathTerminalFramePatchRequest {
                    bounds: TerminalScrollHintBounds {
                        previous_screen_height: 4,
                        next_screen_height: 6,
                    },
                    options: TerminalScrollHintPatchOptions::default(),
                },
            ),
            Ok(Some(ScrollFastPathTerminalFramePatchPlan::Skip(
                TerminalScrollHintPatchSkipReason::NotFullscreen,
            ))),
            "safety gate skips before validating bounds, matching log-update's altScreen/decstbmSafe branch"
        );
    }

    #[test]
    fn test_apply_scroll_fast_path_frame_plan_combines_canvas_and_terminal_patch() {
        let mut previous = Canvas::new(6, 4);
        {
            let mut view = previous.subview_mut(0, 0, 0, 0, 6, 4);
            for row in 0..4 {
                view.set_text(
                    0,
                    row as isize,
                    &format!("r{row}"),
                    CanvasTextStyle::default(),
                );
            }
        }
        previous.clear_damage();

        let frame_plan = ScrollFastPathFramePlan {
            delta: 1,
            content_height_delta: 0,
            viewport_stable: true,
            content_delta_safe: true,
            fast_path: Some(ScrollFastPathPlan {
                blit_region: CachedClearRegion {
                    x: 0,
                    y: 0,
                    width: 6,
                    height: 4,
                },
                delta: 1,
                edge_region: CachedClearRegion {
                    x: 0,
                    y: 3,
                    width: 6,
                    height: 1,
                },
                absolute_repair_regions: Vec::new(),
            }),
            child_repairs: vec![ScrollFastPathChildRepair {
                key: "dirty-stable",
                region: CachedClearRegion {
                    x: 0,
                    y: 1,
                    width: 6,
                    height: 1,
                },
            }],
        };

        let mut next = Canvas::new(6, 4);
        let application = apply_scroll_fast_path_frame_plan(
            &mut next,
            &previous,
            &frame_plan,
            Some(ScrollFastPathTerminalFramePatchRequest {
                bounds: TerminalScrollHintBounds {
                    previous_screen_height: 4,
                    next_screen_height: 4,
                },
                options: TerminalScrollHintPatchOptions::fullscreen_synchronized(),
            }),
        )
        .unwrap();
        assert!(application.canvas_applied);
        assert_eq!(next.get_text(0, 1, 6, 1), "");
        assert_eq!(next.get_text(0, 3, 6, 1), "");
        assert_eq!(
            application
                .terminal_patch
                .as_ref()
                .unwrap()
                .scroll_patch_ansi,
            "\x1b[1;4r\x1b[1S\x1b[r\x1b[H"
        );
        assert_eq!(application.terminal_patch_skip_reason, None);

        let mut previous_for_diff = previous.clone();
        assert!(application.shift_previous_canvas_for_terminal_diff(&mut previous_for_diff));
        assert_eq!(previous_for_diff.get_text(0, 0, 6, 1), "r1");
        assert_eq!(previous_for_diff.get_text(0, 3, 6, 1), "");

        let mut skipped = Canvas::new(6, 4);
        let application = apply_scroll_fast_path_frame_plan(
            &mut skipped,
            &previous,
            &frame_plan,
            Some(ScrollFastPathTerminalFramePatchRequest {
                bounds: TerminalScrollHintBounds {
                    previous_screen_height: 0,
                    next_screen_height: 0,
                },
                options: TerminalScrollHintPatchOptions::default(),
            }),
        )
        .unwrap();
        assert!(application.canvas_applied);
        assert!(application.terminal_patch.is_none());
        assert_eq!(
            application.terminal_patch_skip_reason,
            Some(TerminalScrollHintPatchSkipReason::NotFullscreen)
        );

        let mut canvas_only = Canvas::new(6, 4);
        let application =
            apply_scroll_fast_path_frame_plan(&mut canvas_only, &previous, &frame_plan, None)
                .unwrap();
        assert!(application.canvas_applied);
        assert!(application.terminal_patch.is_none());
        assert_eq!(application.terminal_patch_skip_reason, None);
    }

    #[test]
    fn test_retained_child_blit_plan_matches_cc_ink_contamination_guards() {
        let decisions = plan_retained_child_blits(
            false,
            [
                RetainedChildBlitInput {
                    key: "clean-before",
                    dirty: false,
                    clips_both_axes: false,
                    absolute: false,
                    opaque: false,
                    has_background: false,
                },
                RetainedChildBlitInput {
                    key: "dirty-clipped",
                    dirty: true,
                    clips_both_axes: true,
                    absolute: false,
                    opaque: false,
                    has_background: false,
                },
                RetainedChildBlitInput {
                    key: "clean-normal-after-clipped",
                    dirty: false,
                    clips_both_axes: false,
                    absolute: false,
                    opaque: false,
                    has_background: false,
                },
                RetainedChildBlitInput {
                    key: "absolute-transparent-after-clipped",
                    dirty: false,
                    clips_both_axes: false,
                    absolute: true,
                    opaque: false,
                    has_background: false,
                },
                RetainedChildBlitInput {
                    key: "absolute-opaque-after-clipped",
                    dirty: false,
                    clips_both_axes: false,
                    absolute: true,
                    opaque: true,
                    has_background: false,
                },
                RetainedChildBlitInput {
                    key: "dirty-unclipped",
                    dirty: true,
                    clips_both_axes: false,
                    absolute: false,
                    opaque: false,
                    has_background: false,
                },
                RetainedChildBlitInput {
                    key: "after-unclipped",
                    dirty: false,
                    clips_both_axes: false,
                    absolute: false,
                    opaque: false,
                    has_background: false,
                },
            ],
        );

        assert_eq!(
            decisions,
            vec![
                RetainedChildBlitDecision {
                    key: "clean-before",
                    allow_previous_screen: true,
                    skip_self_blit: false,
                },
                RetainedChildBlitDecision {
                    key: "dirty-clipped",
                    allow_previous_screen: true,
                    skip_self_blit: false,
                },
                RetainedChildBlitDecision {
                    key: "clean-normal-after-clipped",
                    allow_previous_screen: true,
                    skip_self_blit: false,
                },
                RetainedChildBlitDecision {
                    key: "absolute-transparent-after-clipped",
                    allow_previous_screen: true,
                    skip_self_blit: true,
                },
                RetainedChildBlitDecision {
                    key: "absolute-opaque-after-clipped",
                    allow_previous_screen: true,
                    skip_self_blit: false,
                },
                RetainedChildBlitDecision {
                    key: "dirty-unclipped",
                    allow_previous_screen: true,
                    skip_self_blit: false,
                },
                RetainedChildBlitDecision {
                    key: "after-unclipped",
                    allow_previous_screen: false,
                    skip_self_blit: false,
                },
            ]
        );

        let removed = plan_retained_child_blits(
            true,
            [RetainedChildBlitInput {
                key: "removed-parent-child",
                dirty: false,
                clips_both_axes: false,
                absolute: false,
                opaque: false,
                has_background: false,
            }],
        );
        assert_eq!(
            removed,
            vec![RetainedChildBlitDecision {
                key: "removed-parent-child",
                allow_previous_screen: false,
                skip_self_blit: false,
            }]
        );
    }

    #[test]
    fn test_scroll_viewport_child_render_plan_matches_cc_ink_culling_cache_rules() {
        let decisions = plan_scroll_viewport_child_render(
            0,
            6,
            false,
            false,
            [
                ScrollViewportChildInput {
                    key: "clean-cached-visible",
                    top: 99,
                    height: 99,
                    cached_top: Some(0),
                    cached_height: Some(1),
                    dirty: false,
                },
                ScrollViewportChildInput {
                    key: "dirty-culled-above",
                    top: -4,
                    height: 3,
                    cached_top: Some(-4),
                    cached_height: Some(1),
                    dirty: true,
                },
                ScrollViewportChildInput {
                    key: "clean-after-height-shift",
                    top: 4,
                    height: 1,
                    cached_top: Some(2),
                    cached_height: Some(1),
                    dirty: false,
                },
                ScrollViewportChildInput {
                    key: "dirty-visible",
                    top: 5,
                    height: 1,
                    cached_top: Some(5),
                    cached_height: Some(1),
                    dirty: true,
                },
                ScrollViewportChildInput {
                    key: "clean-after-rendered-dirty",
                    top: 5,
                    height: 1,
                    cached_top: Some(5),
                    cached_height: Some(1),
                    dirty: false,
                },
                ScrollViewportChildInput {
                    key: "clean-culled-below",
                    top: 6,
                    height: 1,
                    cached_top: Some(6),
                    cached_height: Some(1),
                    dirty: false,
                },
            ],
        );

        assert_eq!(
            decisions,
            vec![
                ScrollViewportChildDecision {
                    key: "clean-cached-visible",
                    visible: true,
                    top: 0,
                    height: 1,
                    used_cached_layout: true,
                    allow_previous_screen: true,
                    refresh_cached_top: None,
                    drop_subtree_cache: false,
                },
                ScrollViewportChildDecision {
                    key: "dirty-culled-above",
                    visible: false,
                    top: -4,
                    height: 3,
                    used_cached_layout: false,
                    allow_previous_screen: false,
                    refresh_cached_top: Some(-4),
                    drop_subtree_cache: true,
                },
                ScrollViewportChildDecision {
                    key: "clean-after-height-shift",
                    visible: true,
                    top: 4,
                    height: 1,
                    used_cached_layout: false,
                    allow_previous_screen: true,
                    refresh_cached_top: Some(4),
                    drop_subtree_cache: false,
                },
                ScrollViewportChildDecision {
                    key: "dirty-visible",
                    visible: true,
                    top: 5,
                    height: 1,
                    used_cached_layout: false,
                    allow_previous_screen: true,
                    refresh_cached_top: Some(5),
                    drop_subtree_cache: false,
                },
                ScrollViewportChildDecision {
                    key: "clean-after-rendered-dirty",
                    visible: true,
                    top: 5,
                    height: 1,
                    used_cached_layout: false,
                    allow_previous_screen: false,
                    refresh_cached_top: Some(5),
                    drop_subtree_cache: false,
                },
                ScrollViewportChildDecision {
                    key: "clean-culled-below",
                    visible: false,
                    top: 6,
                    height: 1,
                    used_cached_layout: false,
                    allow_previous_screen: false,
                    refresh_cached_top: Some(6),
                    drop_subtree_cache: true,
                },
            ]
        );

        let preserve = plan_scroll_viewport_child_render(
            0,
            1,
            true,
            true,
            [ScrollViewportChildInput {
                key: "removed-parent-visible",
                top: 0,
                height: 1,
                cached_top: None,
                cached_height: None,
                dirty: false,
            }],
        );
        assert_eq!(
            preserve,
            vec![ScrollViewportChildDecision {
                key: "removed-parent-visible",
                visible: true,
                top: 0,
                height: 1,
                used_cached_layout: false,
                allow_previous_screen: false,
                refresh_cached_top: None,
                drop_subtree_cache: false,
            }]
        );
    }

    #[test]
    fn test_escaping_absolute_descendant_blits_match_cc_ink_parent_blit_repair() {
        let parent = CachedClearRegion {
            x: 10,
            y: 5,
            width: 8,
            height: 4,
        };
        let blits = plan_escaping_absolute_descendant_blits(
            parent,
            [
                AbsoluteDescendantRect {
                    key: "inside",
                    rect: CachedClearRegion {
                        x: 11,
                        y: 6,
                        width: 2,
                        height: 1,
                    },
                },
                AbsoluteDescendantRect {
                    key: "left",
                    rect: CachedClearRegion {
                        x: 8,
                        y: 6,
                        width: 3,
                        height: 1,
                    },
                },
                AbsoluteDescendantRect {
                    key: "right",
                    rect: CachedClearRegion {
                        x: 17,
                        y: 6,
                        width: 3,
                        height: 1,
                    },
                },
                AbsoluteDescendantRect {
                    key: "above",
                    rect: CachedClearRegion {
                        x: 12,
                        y: 4,
                        width: 2,
                        height: 2,
                    },
                },
                AbsoluteDescendantRect {
                    key: "below",
                    rect: CachedClearRegion {
                        x: 12,
                        y: 8,
                        width: 2,
                        height: 2,
                    },
                },
                AbsoluteDescendantRect {
                    key: "empty",
                    rect: CachedClearRegion {
                        x: 0,
                        y: 0,
                        width: 0,
                        height: 1,
                    },
                },
            ],
        );

        assert_eq!(
            blits,
            vec![
                EscapingAbsoluteDescendantBlit {
                    key: "left",
                    rect: CachedClearRegion {
                        x: 8,
                        y: 6,
                        width: 3,
                        height: 1,
                    },
                },
                EscapingAbsoluteDescendantBlit {
                    key: "right",
                    rect: CachedClearRegion {
                        x: 17,
                        y: 6,
                        width: 3,
                        height: 1,
                    },
                },
                EscapingAbsoluteDescendantBlit {
                    key: "above",
                    rect: CachedClearRegion {
                        x: 12,
                        y: 4,
                        width: 2,
                        height: 2,
                    },
                },
                EscapingAbsoluteDescendantBlit {
                    key: "below",
                    rect: CachedClearRegion {
                        x: 12,
                        y: 8,
                        width: 2,
                        height: 2,
                    },
                },
            ]
        );
    }

    #[test]
    fn test_apply_escaping_absolute_descendant_blits_to_canvas_restores_overflow_cells() {
        let parent = CachedClearRegion {
            x: 10,
            y: 5,
            width: 8,
            height: 4,
        };
        let blits = plan_escaping_absolute_descendant_blits(
            parent,
            [
                AbsoluteDescendantRect {
                    key: "inside",
                    rect: CachedClearRegion {
                        x: 11,
                        y: 6,
                        width: 2,
                        height: 1,
                    },
                },
                AbsoluteDescendantRect {
                    key: "left",
                    rect: CachedClearRegion {
                        x: 8,
                        y: 6,
                        width: 3,
                        height: 1,
                    },
                },
                AbsoluteDescendantRect {
                    key: "above",
                    rect: CachedClearRegion {
                        x: 12,
                        y: 4,
                        width: 2,
                        height: 2,
                    },
                },
            ],
        );

        let mut previous = Canvas::new(20, 10);
        {
            let mut view = previous.subview_mut(0, 0, 0, 0, 20, 10);
            view.set_text(10, 6, "parent", CanvasTextStyle::default());
            view.set_text(8, 6, "LFT", CanvasTextStyle::default());
            view.set_text(12, 4, "AB", CanvasTextStyle::default());
        }
        previous.clear_damage();

        let mut next = Canvas::new(20, 10);
        next.blit_region_from(
            &previous,
            parent.x as usize,
            parent.y as usize,
            parent.width as usize,
            parent.height as usize,
        );
        assert_eq!(next.get_text(8, 6, 2, 1), "");
        assert_eq!(next.get_text(12, 4, 2, 1), "");

        let applied =
            apply_escaping_absolute_descendant_blits_to_canvas(&mut next, &previous, blits);
        assert_eq!(next.get_text(8, 6, 3, 1), "LFT");
        assert_eq!(next.get_text(12, 4, 2, 1), "AB");
        assert_eq!(
            applied,
            vec![
                EscapingAbsoluteDescendantCanvasBlit {
                    key: "left",
                    region: DamageRegion {
                        x: 8,
                        y: 6,
                        width: 3,
                        height: 1,
                    },
                },
                EscapingAbsoluteDescendantCanvasBlit {
                    key: "above",
                    region: DamageRegion {
                        x: 12,
                        y: 4,
                        width: 2,
                        height: 2,
                    },
                },
            ]
        );
    }

    #[test]
    fn test_retained_node_render_plan_matches_cc_ink_node_blit_and_clear_guards() {
        let current = CachedLayoutBounds {
            x: 1,
            y: 2,
            width: 10,
            height: 3,
            top: Some(2),
        };
        let moved = CachedLayoutBounds { y: 4, ..current };

        let clean_blit = plan_retained_node_render(RetainedNodeRenderInput {
            current_layout: current,
            cached_layout: Some(current),
            dirty: false,
            skip_self_blit: false,
            pending_scroll_delta: false,
            previous_screen_available: true,
            hidden: false,
            absolute: true,
            pending_clears: Vec::new(),
        });
        assert_eq!(
            clean_blit,
            RetainedNodeRenderPlan {
                action: RetainedNodeRenderAction::Blit,
                blit_region: Some(current.into()),
                clear_old_region: None,
                clear_old_from_absolute: false,
                pending_clear_regions: Vec::new(),
                has_removed_child: false,
                position_changed: false,
                layout_shifted: false,
                drop_subtree_cache: false,
                record_absolute_rect: true,
            }
        );

        let dirty_same_position = plan_retained_node_render(RetainedNodeRenderInput {
            current_layout: current,
            cached_layout: Some(current),
            dirty: true,
            skip_self_blit: false,
            pending_scroll_delta: false,
            previous_screen_available: true,
            hidden: false,
            absolute: false,
            pending_clears: Vec::new(),
        });
        assert_eq!(dirty_same_position.action, RetainedNodeRenderAction::Render);
        assert_eq!(dirty_same_position.clear_old_region, Some(current.into()));
        assert!(!dirty_same_position.layout_shifted);

        let moved_clean = plan_retained_node_render(RetainedNodeRenderInput {
            current_layout: moved,
            cached_layout: Some(current),
            dirty: false,
            skip_self_blit: false,
            pending_scroll_delta: false,
            previous_screen_available: true,
            hidden: false,
            absolute: false,
            pending_clears: Vec::new(),
        });
        assert_eq!(moved_clean.action, RetainedNodeRenderAction::Render);
        assert_eq!(moved_clean.clear_old_region, Some(current.into()));
        assert!(moved_clean.position_changed);
        assert!(moved_clean.layout_shifted);

        let pending_clear = CachedClearRegion {
            x: 0,
            y: 1,
            width: 2,
            height: 1,
        };
        let removed_child = plan_retained_node_render(RetainedNodeRenderInput {
            current_layout: current,
            cached_layout: Some(current),
            dirty: false,
            skip_self_blit: false,
            pending_scroll_delta: true,
            previous_screen_available: true,
            hidden: false,
            absolute: false,
            pending_clears: vec![pending_clear],
        });
        assert_eq!(removed_child.action, RetainedNodeRenderAction::Render);
        assert_eq!(removed_child.pending_clear_regions, vec![pending_clear]);
        assert!(removed_child.has_removed_child);
        assert!(removed_child.layout_shifted);

        let hidden = plan_retained_node_render(RetainedNodeRenderInput {
            current_layout: current,
            cached_layout: Some(current),
            dirty: true,
            skip_self_blit: false,
            pending_scroll_delta: false,
            previous_screen_available: true,
            hidden: true,
            absolute: true,
            pending_clears: vec![pending_clear],
        });
        assert_eq!(hidden.action, RetainedNodeRenderAction::Hidden);
        assert_eq!(hidden.clear_old_region, Some(current.into()));
        assert!(hidden.clear_old_from_absolute);
        assert!(hidden.drop_subtree_cache);
        assert!(hidden.layout_shifted);
        assert!(hidden.pending_clear_regions.is_empty());
    }

    #[test]
    fn test_apply_retained_node_render_plan_to_canvas_blits_and_clears() {
        let mut previous = Canvas::new(8, 4);
        {
            let mut view = previous.subview_mut(0, 0, 0, 0, 8, 4);
            for row in 0..4 {
                view.set_text(
                    0,
                    row as isize,
                    &format!("row{row}"),
                    CanvasTextStyle::default(),
                );
            }
        }
        previous.clear_damage();

        let pending_clear = CachedClearRegion {
            x: 0,
            y: 1,
            width: 8,
            height: 1,
        };
        let blit_plan = RetainedNodeRenderPlan {
            action: RetainedNodeRenderAction::Blit,
            blit_region: Some(CachedClearRegion {
                x: 0,
                y: 0,
                width: 8,
                height: 4,
            }),
            clear_old_region: None,
            clear_old_from_absolute: false,
            pending_clear_regions: vec![pending_clear],
            has_removed_child: true,
            position_changed: false,
            layout_shifted: true,
            drop_subtree_cache: false,
            record_absolute_rect: false,
        };

        let mut next = Canvas::new(8, 4);
        let applied = apply_retained_node_render_plan_to_canvas(&mut next, &previous, &blit_plan);
        assert_eq!(next.get_text(0, 0, 8, 1), "row0");
        assert_eq!(next.get_text(0, 1, 8, 1), "");
        assert_eq!(next.get_text(0, 2, 8, 1), "row2");
        assert_eq!(
            applied.blitted_region,
            Some(DamageRegion {
                x: 0,
                y: 0,
                width: 8,
                height: 4,
            })
        );
        assert_eq!(
            applied.pending_clear_regions,
            vec![DamageRegion {
                x: 0,
                y: 1,
                width: 8,
                height: 1,
            }]
        );

        let clear_plan = RetainedNodeRenderPlan {
            action: RetainedNodeRenderAction::Render,
            blit_region: None,
            clear_old_region: Some(CachedClearRegion {
                x: 0,
                y: 2,
                width: 8,
                height: 1,
            }),
            clear_old_from_absolute: false,
            pending_clear_regions: Vec::new(),
            has_removed_child: false,
            position_changed: true,
            layout_shifted: true,
            drop_subtree_cache: false,
            record_absolute_rect: false,
        };
        let mut dirty_next = previous.clone();
        let applied =
            apply_retained_node_render_plan_to_canvas(&mut dirty_next, &previous, &clear_plan);
        assert_eq!(dirty_next.get_text(0, 2, 8, 1), "");
        assert_eq!(
            applied.cleared_old_region,
            Some(DamageRegion {
                x: 0,
                y: 2,
                width: 8,
                height: 1,
            })
        );
        assert_eq!(applied.blitted_region, None);
    }

    #[test]
    fn test_renderer_layout_shift_tracker_matches_cc_ink_layout_shift_flag() {
        let mut tracker = RendererLayoutShiftTracker::<&'static str>::new();
        let root = RendererLayoutSnapshot {
            x: 0,
            y: 0,
            width: 10,
            height: 3,
        };
        let child = RendererLayoutSnapshot {
            x: 0,
            y: 1,
            width: 10,
            height: 1,
        };

        assert!(!tracker.update([("root", root), ("child", child)]));
        assert_eq!(tracker.len(), 2);
        assert_eq!(tracker.snapshot(&"child"), Some(child));
        assert!(!tracker.update([("root", root), ("child", child)]));

        assert!(tracker.update([
            ("root", root),
            ("child", RendererLayoutSnapshot { y: 2, ..child },),
        ]));
        assert!(
            tracker.update([("root", root)]),
            "removed child shifts layout"
        );
        assert!(
            tracker.update([("root", root), ("child", child)]),
            "added child shifts layout"
        );

        tracker.clear();
        assert!(tracker.is_empty());
        assert!(!tracker.update_from_layouts([(
            "root",
            CachedLayoutBounds {
                x: 0,
                y: 0,
                width: 10,
                height: 3,
                top: Some(0),
            },
        )]));
        assert_eq!(
            tracker.snapshot(&"root"),
            Some(RendererLayoutSnapshot {
                x: 0,
                y: 0,
                width: 10,
                height: 3,
            })
        );
    }

    #[test]
    fn test_renderer_node_cache_matches_cc_node_cache_semantics() {
        let mut cache = RendererNodeCache::<&'static str>::new();
        let layout = CachedLayoutBounds {
            x: 1,
            y: 2,
            width: 10,
            height: 3,
            top: Some(2),
        };

        assert!(!cache.can_blit(&"child", layout));
        cache.set_layout("child", layout);
        assert_eq!(cache.layout(&"child"), Some(layout));
        assert!(cache.can_blit(&"child", layout));
        assert!(!cache.can_blit(&"child", CachedLayoutBounds { y: 3, ..layout }));

        cache.add_pending_clear("parent", layout.into(), false);
        assert!(!cache.consume_absolute_removed_flag());
        assert_eq!(cache.take_pending_clears(&"parent"), vec![layout.into()]);
        assert!(cache.take_pending_clears(&"parent").is_empty());

        let negative_clear = CachedClearRegion {
            x: 4,
            y: -1,
            width: 6,
            height: 2,
        };
        assert_eq!(
            negative_clear.clipped_to_canvas(8, 4),
            Some(DamageRegion {
                x: 4,
                y: 0,
                width: 4,
                height: 1,
            })
        );

        cache.add_pending_clear("parent", negative_clear, true);
        assert!(cache.consume_absolute_removed_flag());
        assert!(!cache.consume_absolute_removed_flag());
        assert_eq!(cache.remove_layout(&"child"), Some(layout));

        cache.set_layout("root", layout);
        cache.set_layout("branch", CachedLayoutBounds { x: 2, ..layout });
        cache.set_layout("leaf", CachedLayoutBounds { x: 3, ..layout });
        cache.set_layout("sibling", CachedLayoutBounds { x: 4, ..layout });
        cache.add_pending_clear("branch", layout.into(), false);
        let children = std::collections::HashMap::from([
            ("root", vec!["branch", "sibling"]),
            ("branch", vec!["leaf"]),
        ]);
        cache.remove_subtree(&"branch", |node| {
            children.get(node).cloned().unwrap_or_default()
        });
        assert_eq!(cache.layout(&"branch"), None);
        assert_eq!(cache.layout(&"leaf"), None);
        assert_eq!(cache.layout(&"root"), Some(layout));
        assert_eq!(cache.layout(&"sibling").map(|bounds| bounds.x), Some(4));
        assert!(cache.take_pending_clears(&"branch").is_empty());

        cache.clear();
        assert_eq!(cache.layout(&"child"), None);
        assert!(cache.take_pending_clears(&"parent").is_empty());
        assert!(!cache.consume_absolute_removed_flag());
    }

    #[test]
    fn test_renderer_retained_frame_state_integrates_cache_plan_and_commit() {
        let mut state = RendererRetainedFrameState::<&'static str>::new();
        let root = CachedLayoutBounds {
            x: 0,
            y: 0,
            width: 10,
            height: 3,
            top: Some(0),
        };
        let child = CachedLayoutBounds {
            x: 1,
            y: 1,
            width: 4,
            height: 1,
            top: Some(1),
        };

        assert!(!state.begin_frame());
        let first = state.plan_node(RetainedFrameNodeInput {
            key: "root",
            current_layout: root,
            dirty: true,
            skip_self_blit: false,
            pending_scroll_delta: false,
            previous_screen_available: true,
            hidden: false,
            absolute: false,
        });
        assert_eq!(first.plan.action, RetainedNodeRenderAction::Render);
        assert!(!state.layout_shifted());
        state.commit_node_plan(&first);
        assert_eq!(state.cache().layout(&"root"), Some(root));

        assert!(!state.begin_frame());
        let clean = state.plan_node(RetainedFrameNodeInput {
            key: "root",
            current_layout: root,
            dirty: false,
            skip_self_blit: false,
            pending_scroll_delta: false,
            previous_screen_available: true,
            hidden: false,
            absolute: false,
        });
        assert_eq!(clean.plan.action, RetainedNodeRenderAction::Blit);
        assert_eq!(clean.plan.blit_region, Some(root.into()));
        state.commit_node_plan(&clean);

        state.queue_child_clear("root", child.into(), true);
        assert!(
            state.begin_frame(),
            "absolute child clear should poison next-frame blits"
        );
        let with_removed_child = state.plan_node(RetainedFrameNodeInput {
            key: "root",
            current_layout: root,
            dirty: false,
            skip_self_blit: false,
            pending_scroll_delta: false,
            previous_screen_available: true,
            hidden: false,
            absolute: false,
        });
        assert_eq!(
            with_removed_child.plan.pending_clear_regions,
            vec![child.into()]
        );
        assert!(with_removed_child.plan.has_removed_child);
        assert!(state.layout_shifted());
        state.commit_node_plan(&with_removed_child);

        state.cache_mut().set_layout("overlay", child);
        state
            .cache_mut()
            .set_layout("leaf", CachedLayoutBounds { x: 2, ..child });
        assert!(!state.begin_frame());
        let hidden_overlay = state.plan_node(RetainedFrameNodeInput {
            key: "overlay",
            current_layout: child,
            dirty: true,
            skip_self_blit: false,
            pending_scroll_delta: false,
            previous_screen_available: true,
            hidden: true,
            absolute: true,
        });
        assert_eq!(hidden_overlay.plan.action, RetainedNodeRenderAction::Hidden);
        assert!(hidden_overlay.plan.drop_subtree_cache);
        assert!(state.absolute_clear_this_frame());
        let children = std::collections::HashMap::from([("overlay", vec!["leaf"])]);
        state.commit_node_plan_with_children(&hidden_overlay, |node| {
            children.get(node).cloned().unwrap_or_default()
        });
        assert_eq!(state.cache().layout(&"overlay"), None);
        assert_eq!(state.cache().layout(&"leaf"), None);
        assert_eq!(state.cache().layout(&"root"), Some(root));
    }

    #[test]
    fn test_renderer_retained_tree_state_integrates_dirty_tree_and_cache() {
        let mut state = RendererRetainedTreeState::<&'static str>::new();
        let root = CachedLayoutBounds {
            x: 0,
            y: 0,
            width: 20,
            height: 5,
            top: Some(0),
        };
        let branch = CachedLayoutBounds {
            x: 0,
            y: 1,
            width: 20,
            height: 3,
            top: Some(1),
        };
        let leaf = CachedLayoutBounds {
            x: 0,
            y: 2,
            width: 20,
            height: 1,
            top: Some(2),
        };
        let overlay = CachedLayoutBounds {
            x: 2,
            y: 1,
            width: 5,
            height: 1,
            top: Some(1),
        };

        state.register_root("root");
        state.attach("branch", "root");
        state.attach("leaf", "branch");
        state.attach("overlay", "root");
        state.clear_dirty();

        state.mark_dirty(&"leaf", true);
        assert!(state.is_dirty(&"leaf"));
        assert!(state.is_dirty(&"branch"));
        assert!(state.is_dirty(&"root"));
        assert!(!state.is_dirty(&"overlay"));

        state.begin_frame();
        let root_plan = state.plan_node(RetainedTreeNodeInput {
            key: "root",
            current_layout: root,
            skip_self_blit: false,
            pending_scroll_delta: false,
            previous_screen_available: true,
            hidden: false,
            absolute: false,
        });
        assert_eq!(root_plan.plan.action, RetainedNodeRenderAction::Render);
        state.commit_node_plan(&root_plan);
        assert!(!state.is_dirty(&"root"));
        assert!(state.is_dirty(&"branch"));

        let branch_plan = state.plan_node(RetainedTreeNodeInput {
            key: "branch",
            current_layout: branch,
            skip_self_blit: false,
            pending_scroll_delta: false,
            previous_screen_available: true,
            hidden: false,
            absolute: false,
        });
        let leaf_plan = state.plan_node(RetainedTreeNodeInput {
            key: "leaf",
            current_layout: leaf,
            skip_self_blit: false,
            pending_scroll_delta: false,
            previous_screen_available: true,
            hidden: false,
            absolute: false,
        });
        assert_eq!(branch_plan.plan.action, RetainedNodeRenderAction::Render);
        assert_eq!(leaf_plan.plan.action, RetainedNodeRenderAction::Render);
        state.commit_node_plan(&branch_plan);
        state.commit_node_plan(&leaf_plan);
        assert!(!state.is_dirty(&"branch"));
        assert!(!state.is_dirty(&"leaf"));
        assert_eq!(state.frame_state().cache().layout(&"leaf"), Some(leaf));

        state
            .frame_state_mut()
            .cache_mut()
            .set_layout("overlay", overlay);
        let removed = state.remove_subtree(&"overlay", true);
        assert_eq!(removed, vec!["overlay"]);
        assert!(state.is_dirty(&"root"));
        assert!(
            state.begin_frame(),
            "absolute removal poisons next frame blits"
        );
        let root_after_remove = state.plan_node(RetainedTreeNodeInput {
            key: "root",
            current_layout: root,
            skip_self_blit: false,
            pending_scroll_delta: false,
            previous_screen_available: true,
            hidden: false,
            absolute: false,
        });
        assert_eq!(
            root_after_remove.plan.action,
            RetainedNodeRenderAction::Render
        );
        assert_eq!(
            root_after_remove.plan.pending_clear_regions,
            vec![overlay.into()]
        );
        assert!(root_after_remove.plan.has_removed_child);
        assert!(state.frame_state().layout_shifted());
    }

    #[test]
    fn test_renderer_dirty_tree_marks_ancestors_and_removes_subtrees() {
        let mut tree = RendererDirtyTree::<&'static str>::new();
        tree.register_root("root");
        tree.attach("branch", "root");
        tree.attach("leaf", "branch");
        tree.attach("sibling", "root");
        assert_eq!(tree.child_keys(&"root"), vec!["branch", "sibling"]);

        tree.mark_dirty(&"leaf", true);
        assert!(tree.is_dirty(&"leaf"));
        assert!(tree.is_dirty(&"branch"));
        assert!(tree.is_dirty(&"root"));
        assert!(!tree.is_dirty(&"sibling"));
        assert!(tree.is_measure_dirty(&"leaf"));
        assert!(!tree.is_measure_dirty(&"branch"));

        tree.clear_node(&"branch");
        assert!(!tree.is_dirty(&"branch"));
        assert!(tree.is_dirty(&"root"));
        tree.clear_dirty();
        assert!(tree.dirty_nodes().next().is_none());

        let mut removed = tree.remove_subtree(&"branch");
        removed.sort_unstable();
        assert_eq!(removed, vec!["branch", "leaf"]);
        assert_eq!(tree.parent(&"leaf"), None);
        assert!(
            tree.is_dirty(&"root"),
            "removing a child dirties the parent"
        );
        assert!(!tree.is_dirty(&"branch"));
        assert!(!tree.is_dirty(&"leaf"));

        tree.attach("sibling", "branch");
        assert_eq!(tree.parent(&"sibling"), Some(&"branch"));
        assert_eq!(tree.child_keys(&"branch"), vec!["sibling"]);
        tree.register_root("sibling");
        assert_eq!(tree.parent(&"sibling"), None);
        tree.clear();
        assert!(tree.dirty_nodes().next().is_none());
        assert_eq!(tree.parent(&"sibling"), None);
    }

    #[derive(Default, Props)]
    struct MyInnerComponentProps {
        label: String,
    }

    #[component]
    fn MyInnerComponent(
        mut hooks: Hooks,
        props: &MyInnerComponentProps,
    ) -> impl Into<AnyElement<'static>> {
        let mut counter = hooks.use_state(|| 0);
        counter += 1;
        element! {
            Text(content: format!("render count ({}): {}", props.label, counter))
        }
    }

    #[component]
    fn MyComponent(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let mut tick = hooks.use_state(|| 0);

        hooks.use_future(async move {
            tick += 1;
        });

        if tick == 1 {
            system.exit();
        }

        element! {
            View(flex_direction: FlexDirection::Column) {
                Text(content: format!("tick: {}", tick))
                MyInnerComponent(label: "a")
                #((0..2).map(|i| element! { MyInnerComponent(label: format!("b{}", i)) }))
                #((0..2).map(|i| element! { MyInnerComponent(key: i, label: format!("c{}", i)) }))
            }
        }
    }

    #[apply(test!)]
    async fn test_terminal_render_loop() {
        let canvases: Vec<_> =
            mock_terminal_render_loop(&mut element!(MyComponent), MockTerminalConfig::default())
                .collect()
                .await;
        let actual = canvases.iter().map(|c| c.to_string()).collect::<Vec<_>>();
        let expected = vec![
            "tick: 0\nrender count (a): 1\nrender count (b0): 1\nrender count (b1): 1\nrender count (c0): 1\nrender count (c1): 1\n",
            "tick: 1\nrender count (a): 2\nrender count (b0): 2\nrender count (b1): 2\nrender count (c0): 2\nrender count (c1): 2\n",
        ];
        assert_eq!(actual, expected);
    }

    #[derive(Default, Props)]
    struct ContentSizeProbeProps;

    #[derive(Default)]
    struct ContentSizeProbe;

    impl Component for ContentSizeProbe {
        type Props<'a> = ContentSizeProbeProps;

        fn new(_props: &Self::Props<'_>) -> Self {
            Self
        }

        fn update(
            &mut self,
            _props: &mut Self::Props<'_>,
            _hooks: Hooks,
            updater: &mut ComponentUpdater,
        ) {
            updater.set_layout_style(taffy::style::Style {
                size: taffy::Size {
                    width: taffy::style::Dimension::length(10.0),
                    height: taffy::style::Dimension::length(5.0),
                },
                padding: taffy::Rect {
                    left: taffy::style::LengthPercentage::length(1.0),
                    right: taffy::style::LengthPercentage::length(1.0),
                    top: taffy::style::LengthPercentage::length(1.0),
                    bottom: taffy::style::LengthPercentage::length(1.0),
                },
                border: taffy::Rect {
                    left: taffy::style::LengthPercentage::length(1.0),
                    right: taffy::style::LengthPercentage::length(1.0),
                    top: taffy::style::LengthPercentage::length(1.0),
                    bottom: taffy::style::LengthPercentage::length(1.0),
                },
                ..Default::default()
            });
        }

        fn draw(&mut self, drawer: &mut ComponentDrawer<'_>) {
            let content = drawer.content_size();
            let visible = drawer.visible_size();
            let remaining = drawer.remaining_canvas_size();
            drawer.canvas().set_text(
                0,
                0,
                &format!("c{}v{}r{}", content.width, visible.width, remaining.width),
                CanvasTextStyle::default(),
            );
        }
    }

    #[test]
    fn test_component_drawer_content_and_visible_size_match_ink_helpers() {
        let canvas = element!(ContentSizeProbe).render(Some(8));
        assert_eq!(canvas.to_string().lines().next(), Some("c4v8r8"));
    }

    #[derive(Default)]
    struct RenderWakeCounter {
        renders: u32,
    }

    impl Hook for RenderWakeCounter {}

    #[component]
    fn ResizeWakeComponent(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let counter = hooks.use_hook(RenderWakeCounter::default);
        counter.renders += 1;
        let renders = counter.renders;
        if renders >= 2 {
            system.exit();
        }
        element!(Text(content: format!("render: {renders}")))
    }

    #[apply(test!)]
    async fn test_resize_event_wakes_render_loop_without_subscriber() {
        let canvases: Vec<_> = element!(ResizeWakeComponent)
            .mock_terminal_render_loop(MockTerminalConfig::with_events(futures::stream::iter(
                vec![TerminalEvent::Resize(100, 40)],
            )))
            .collect()
            .await;
        let actual = canvases.iter().map(|c| c.to_string()).collect::<Vec<_>>();
        assert_eq!(actual, vec!["render: 1\n", "render: 2\n"]);
    }

    #[component]
    fn StaticResizeComponent(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let counter = hooks.use_hook(RenderWakeCounter::default);
        counter.renders += 1;
        if counter.renders >= 2 {
            system.exit();
        }
        element!(Text(content: "static"))
    }

    #[apply(test!)]
    async fn test_resize_event_repaints_unchanged_canvas() {
        let canvases: Vec<_> = element!(StaticResizeComponent)
            .mock_terminal_render_loop(MockTerminalConfig::with_events(futures::stream::iter(
                vec![TerminalEvent::Resize(100, 40)],
            )))
            .collect()
            .await;
        let actual = canvases.iter().map(|c| c.to_string()).collect::<Vec<_>>();
        assert_eq!(actual, vec!["static\n", "static\n"]);
    }

    #[component]
    fn TallStaticComponent(hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        system.exit();
        element!(Text(content: "one\ntwo\nthree\nfour"))
    }

    #[apply(test!)]
    async fn test_fullscreen_render_clamps_canvas_to_terminal_rows() {
        let canvases: Vec<_> = element!(TallStaticComponent)
            .mock_terminal_render_loop(
                MockTerminalConfig::default()
                    .with_fullscreen(true)
                    .with_size(10, 2),
            )
            .collect()
            .await;

        assert_eq!(canvases.len(), 1);
        assert_eq!(canvases[0].width(), 10);
        assert_eq!(canvases[0].height(), 2);
        assert_eq!(canvases[0].to_string(), "one\ntwo\n");
    }

    #[component]
    fn ShortStaticComponent(hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        system.exit();
        element!(Text(content: "one"))
    }

    #[apply(test!)]
    async fn test_fullscreen_render_extends_canvas_to_terminal_rows() {
        let canvases: Vec<_> = element!(ShortStaticComponent)
            .mock_terminal_render_loop(
                MockTerminalConfig::default()
                    .with_fullscreen(true)
                    .with_size(10, 4),
            )
            .collect()
            .await;

        assert_eq!(canvases.len(), 1);
        assert_eq!(canvases[0].width(), 10);
        assert_eq!(canvases[0].height(), 4);
        assert_eq!(canvases[0].to_string(), "one\n\n\n\n");
    }

    struct PercentWidthProbe;

    impl Component for PercentWidthProbe {
        type Props<'a> = crate::props::NoProps;

        fn new(_props: &Self::Props<'_>) -> Self {
            Self
        }

        fn update(
            &mut self,
            _props: &mut Self::Props<'_>,
            _hooks: Hooks,
            updater: &mut ComponentUpdater,
        ) {
            updater.set_layout_style(taffy::style::Style {
                size: taffy::geometry::Size {
                    width: taffy::style::Dimension::percent(1.0),
                    height: taffy::style::Dimension::length(1.0),
                },
                ..Default::default()
            });
        }

        fn draw(&mut self, drawer: &mut ComponentDrawer<'_>) {
            let width = drawer.size().width.saturating_sub(1) as isize;
            drawer
                .canvas()
                .set_text(width, 0, "x", CanvasTextStyle::default());
        }
    }

    #[component]
    fn PercentWidthProbeApp(hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        system.exit();
        element!(PercentWidthProbe)
    }

    #[apply(test!)]
    async fn test_fullscreen_layout_percent_width_resolves_against_terminal_columns() {
        let canvases: Vec<_> = element!(PercentWidthProbeApp)
            .mock_terminal_render_loop(
                MockTerminalConfig::default()
                    .with_fullscreen(true)
                    .with_size(10, 4),
            )
            .collect()
            .await;

        assert_eq!(canvases.len(), 1);
        assert_eq!(canvases[0].to_string(), "         x\n\n\n\n");
    }

    struct PercentHeightProbe;

    impl Component for PercentHeightProbe {
        type Props<'a> = crate::props::NoProps;

        fn new(_props: &Self::Props<'_>) -> Self {
            Self
        }

        fn update(
            &mut self,
            _props: &mut Self::Props<'_>,
            _hooks: Hooks,
            updater: &mut ComponentUpdater,
        ) {
            updater.set_layout_style(taffy::style::Style {
                size: taffy::geometry::Size {
                    width: taffy::style::Dimension::length(1.0),
                    height: taffy::style::Dimension::percent(1.0),
                },
                ..Default::default()
            });
        }

        fn draw(&mut self, drawer: &mut ComponentDrawer<'_>) {
            let y = drawer.size().height.saturating_sub(1) as isize;
            drawer
                .canvas()
                .set_text(0, y, "x", CanvasTextStyle::default());
        }
    }

    #[component]
    fn PercentHeightProbeApp(hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        system.exit();
        element!(PercentHeightProbe)
    }

    #[apply(test!)]
    async fn test_fullscreen_layout_percent_height_resolves_against_terminal_rows() {
        let canvases: Vec<_> = element!(PercentHeightProbeApp)
            .mock_terminal_render_loop(
                MockTerminalConfig::default()
                    .with_fullscreen(true)
                    .with_size(10, 4),
            )
            .collect()
            .await;

        assert_eq!(canvases.len(), 1);
        assert_eq!(canvases[0].to_string(), "\n\n\nx\n");
    }

    #[apply(test!)]
    async fn test_main_screen_render_does_not_clamp_canvas_to_terminal_rows() {
        let canvases: Vec<_> = element!(TallStaticComponent)
            .mock_terminal_render_loop(MockTerminalConfig::default().with_size(10, 2))
            .collect()
            .await;

        assert_eq!(canvases.len(), 1);
        assert_eq!(canvases[0].height(), 4);
        assert_eq!(canvases[0].to_string(), "one\ntwo\nthree\nfour\n");
    }

    #[component]
    fn LayoutShiftComponent(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let mut expanded = hooks.use_state(|| false);

        hooks.use_future(async move {
            expanded.set(true);
        });

        if expanded.get() {
            system.exit();
        }

        element! {
            View(flex_direction: FlexDirection::Column) {
                Text(content: if expanded.get() { "top\nextra" } else { "top" })
                Text(content: "bottom")
            }
        }
    }

    #[derive(Default)]
    struct ForceFullRepaintOnSecondUpdateHook {
        updates: u32,
    }

    impl Hook for ForceFullRepaintOnSecondUpdateHook {
        fn post_component_update(&mut self, updater: &mut ComponentUpdater) {
            self.updates += 1;
            if self.updates >= 2 {
                updater.force_full_repaint();
            }
        }
    }

    #[component]
    fn ForceFullRepaintComponent(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let mut tick = hooks.use_state(|| 0u32);
        let _ = hooks.use_hook(ForceFullRepaintOnSecondUpdateHook::default);

        hooks.use_future(async move {
            tick += 1;
        });

        if tick > 0 {
            system.exit();
        }

        element!(Text(content: "static"))
    }

    #[apply(test!)]
    async fn test_component_updater_can_force_full_repaint_for_identical_canvas() {
        let canvases: Vec<_> = element!(ForceFullRepaintComponent)
            .mock_terminal_render_loop(MockTerminalConfig::default())
            .collect()
            .await;

        assert_eq!(canvases.len(), 2);
        assert_eq!(canvases[0].to_string(), "static\n");
        assert_eq!(canvases[1].to_string(), "static\n");
        assert!(!canvases[0].should_force_full_repaint());
        assert!(
            canvases[1].should_force_full_repaint(),
            "ComponentUpdater::force_full_repaint should be one-shot render metadata"
        );
    }

    #[derive(Default)]
    struct InvalidatePrevFrameOnSecondUpdateHook {
        updates: u32,
    }

    impl Hook for InvalidatePrevFrameOnSecondUpdateHook {
        fn post_component_update(&mut self, updater: &mut ComponentUpdater) {
            self.updates += 1;
            if self.updates >= 2 {
                updater.invalidate_previous_frame();
            }
        }
    }

    #[component]
    fn InvalidatePrevFrameComponent(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let mut tick = hooks.use_state(|| 0u32);
        let _ = hooks.use_hook(InvalidatePrevFrameOnSecondUpdateHook::default);

        hooks.use_future(async move {
            tick += 1;
        });

        if tick > 0 {
            system.exit();
        }

        element!(Text(content: "static"))
    }

    #[apply(test!)]
    async fn test_component_updater_invalidate_previous_frame_marks_full_damage() {
        let canvases: Vec<_> = element!(InvalidatePrevFrameComponent)
            .mock_terminal_render_loop(MockTerminalConfig::default())
            .collect()
            .await;

        assert_eq!(canvases.len(), 2);
        assert_eq!(canvases[0].to_string(), "static\n");
        assert_eq!(canvases[1].to_string(), "static\n");
        assert_eq!(canvases[0].damage_region(), None);
        assert_eq!(
            canvases[1].damage_region(),
            Some(DamageRegion {
                x: 0,
                y: 0,
                width: canvases[1].width(),
                height: canvases[1].height(),
            }),
            "invalidate_previous_frame should map to CC Ink-style full-screen damage"
        );
        assert!(
            !canvases[1].should_force_full_repaint(),
            "prev-frame invalidation should not disable scroll optimizations"
        );
    }

    #[derive(Default)]
    struct MarkDamageOnSecondDrawHook {
        draws: u32,
    }

    impl Hook for MarkDamageOnSecondDrawHook {
        fn post_component_draw(&mut self, drawer: &mut ComponentDrawer) {
            self.draws += 1;
            if self.draws >= 2 {
                drawer.mark_damage();
            }
        }
    }

    #[component]
    fn DamageMarkerComponent(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let mut tick = hooks.use_state(|| 0u32);
        let _ = hooks.use_hook(MarkDamageOnSecondDrawHook::default);

        hooks.use_future(async move {
            tick += 1;
        });

        if tick > 0 {
            system.exit();
        }

        element!(Text(content: "static"))
    }

    #[apply(test!)]
    async fn test_component_drawer_damage_wakes_identical_canvas() {
        let canvases: Vec<_> = element!(DamageMarkerComponent)
            .mock_terminal_render_loop(MockTerminalConfig::default())
            .collect()
            .await;

        assert_eq!(canvases.len(), 2);
        assert_eq!(canvases[0].to_string(), "static\n");
        assert_eq!(canvases[1].to_string(), "static\n");
        assert_eq!(canvases[0].damage_region(), None);
        assert!(
            canvases[1].damage_region().is_some(),
            "ComponentDrawer::mark_damage should be one-shot render metadata that wakes the frame"
        );
    }

    #[derive(Default)]
    struct MarkNoSelectHook;

    impl Hook for MarkNoSelectHook {
        fn post_component_draw(&mut self, drawer: &mut ComponentDrawer) {
            drawer.mark_no_select_region(1, 0, 2, 1);
        }
    }

    #[component]
    fn NoSelectMarkerComponent(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let _ = hooks.use_hook(MarkNoSelectHook::default);
        system.exit();
        element!(Text(content: "abcd"))
    }

    #[apply(test!)]
    async fn test_component_drawer_no_select_marks_canvas_metadata() {
        let canvases: Vec<_> = element!(NoSelectMarkerComponent)
            .mock_terminal_render_loop(MockTerminalConfig::default())
            .collect()
            .await;

        assert_eq!(canvases.len(), 1);
        assert_eq!(canvases[0].to_string(), "abcd\n");
        assert!(!canvases[0].is_no_select(0, 0));
        assert!(canvases[0].is_no_select(1, 0));
        assert!(canvases[0].is_no_select(2, 0));
        assert!(!canvases[0].is_no_select(3, 0));
        assert_eq!(
            canvases[0].damage_region(),
            None,
            "noSelect is selection metadata, not terminal-output damage"
        );
    }

    #[derive(Default)]
    struct MarkDamageOnlyOnSecondDrawHook {
        draws: u32,
    }

    impl Hook for MarkDamageOnlyOnSecondDrawHook {
        fn post_component_draw(&mut self, drawer: &mut ComponentDrawer) {
            self.draws += 1;
            if self.draws == 2 {
                drawer.mark_damage();
            }
        }
    }

    #[component]
    fn PrevDamageCarryComponent(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let mut tick = hooks.use_state(|| 0u32);
        let _ = hooks.use_hook(MarkDamageOnlyOnSecondDrawHook::default);

        hooks.use_future(async move {
            tick += 1;
            futures_timer::Delay::new(std::time::Duration::from_millis(1)).await;
            tick += 1;
        });

        if tick >= 2 {
            system.exit();
        }

        element!(Text(content: "static"))
    }

    #[apply(test!)]
    async fn test_prev_damage_wakes_next_identical_render_once() {
        let canvases: Vec<_> = element!(PrevDamageCarryComponent)
            .mock_terminal_render_loop(MockTerminalConfig::default())
            .collect()
            .await;

        assert_eq!(
            canvases.len(),
            3,
            "previous-frame damage should force one cleanup diff even when the next canvas cells are identical"
        );
        assert_eq!(canvases[0].damage_region(), None);
        assert!(canvases[1].damage_region().is_some());
        assert_eq!(canvases[2].damage_region(), None);
        assert!(canvases
            .iter()
            .all(|canvas| canvas.to_string() == "static\n"));
    }

    #[derive(Default)]
    struct MarkDamageRegionOnSecondDrawHook {
        draws: u32,
    }

    impl Hook for MarkDamageRegionOnSecondDrawHook {
        fn post_component_draw(&mut self, drawer: &mut ComponentDrawer) {
            self.draws += 1;
            if self.draws >= 2 {
                drawer.mark_damage_region(1, 0, 2, 1);
            }
        }
    }

    #[component]
    fn DamageRegionMarkerComponent(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let mut tick = hooks.use_state(|| 0u32);
        let _ = hooks.use_hook(MarkDamageRegionOnSecondDrawHook::default);

        hooks.use_future(async move {
            tick += 1;
        });

        if tick > 0 {
            system.exit();
        }

        element!(Text(content: "static"))
    }

    #[apply(test!)]
    async fn test_component_drawer_can_mark_local_damage_region() {
        let canvases: Vec<_> = element!(DamageRegionMarkerComponent)
            .mock_terminal_render_loop(MockTerminalConfig::default())
            .collect()
            .await;

        assert_eq!(canvases.len(), 2);
        assert_eq!(
            canvases[1].damage_region(),
            Some(DamageRegion {
                x: 1,
                y: 0,
                width: 2,
                height: 1,
            })
        );
    }

    #[derive(Default)]
    struct MarkOutOfBoundsDamageRegionOnSecondDrawHook {
        draws: u32,
    }

    impl Hook for MarkOutOfBoundsDamageRegionOnSecondDrawHook {
        fn post_component_draw(&mut self, drawer: &mut ComponentDrawer) {
            self.draws += 1;
            if self.draws >= 2 {
                drawer.mark_damage_region(5, 0, 10, 10);
            }
        }
    }

    #[component]
    fn ClippedDamageRegionMarkerComponent(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let mut tick = hooks.use_state(|| 0u32);
        let _ = hooks.use_hook(MarkOutOfBoundsDamageRegionOnSecondDrawHook::default);

        hooks.use_future(async move {
            tick += 1;
        });

        if tick > 0 {
            system.exit();
        }

        element!(Text(content: "static"))
    }

    #[apply(test!)]
    async fn test_component_drawer_damage_region_clips_to_component_bounds() {
        let canvases: Vec<_> = element!(ClippedDamageRegionMarkerComponent)
            .mock_terminal_render_loop(MockTerminalConfig::default())
            .collect()
            .await;

        assert_eq!(canvases.len(), 2);
        assert_eq!(
            canvases[1].damage_region(),
            Some(DamageRegion {
                x: 5,
                y: 0,
                width: 1,
                height: 1,
            })
        );
    }

    #[component]
    fn ClipRectDamageChild(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let _ = hooks.use_hook(MarkDamageOnSecondDrawHook::default);
        element!(Text(content: "static"))
    }

    #[component]
    fn ClipRectDamageParent(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let mut tick = hooks.use_state(|| 0u32);

        hooks.use_future(async move {
            tick += 1;
        });

        if tick > 0 {
            system.exit();
        }

        element! {
            View(width: 4, overflow: Overflow::Hidden) {
                ClipRectDamageChild
            }
        }
    }

    #[apply(test!)]
    async fn test_component_drawer_damage_region_clips_to_parent_clip_rect() {
        let canvases: Vec<_> = element!(ClipRectDamageParent)
            .mock_terminal_render_loop(MockTerminalConfig::default())
            .collect()
            .await;

        assert_eq!(canvases.len(), 2);
        assert_eq!(
            canvases[1].damage_region(),
            Some(DamageRegion {
                x: 0,
                y: 0,
                width: 4,
                height: 2,
            })
        );
    }

    #[derive(Default)]
    struct ScrollHintOnDrawHook;

    impl Hook for ScrollHintOnDrawHook {
        fn post_component_draw(&mut self, drawer: &mut ComponentDrawer) {
            drawer.set_scroll_hint(1);
        }
    }

    #[component]
    fn ScrollHintChild(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let _ = hooks.use_hook(ScrollHintOnDrawHook::default);
        element! {
            View(width: 8, height: 4) {
                Text(content: "one\ntwo\nthree\nfour")
            }
        }
    }

    #[component]
    fn ClippedScrollHintParent(hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        system.exit();

        element! {
            View(width: 8, height: 2, overflow: Overflow::Hidden) {
                ScrollHintChild
            }
        }
    }

    #[apply(test!)]
    async fn test_component_drawer_scroll_hint_clips_to_parent_clip_rect() {
        let canvases: Vec<_> = element!(ClippedScrollHintParent)
            .mock_terminal_render_loop(MockTerminalConfig::default())
            .collect()
            .await;

        assert_eq!(canvases.len(), 1);
        assert_eq!(
            canvases[0].scroll_hint(),
            Some(ScrollHint {
                top: 0,
                bottom: 1,
                delta: 1,
            })
        );
    }

    struct PartialWidthScrollHintBox;

    impl Component for PartialWidthScrollHintBox {
        type Props<'a> = crate::props::NoProps;

        fn new(_props: &Self::Props<'_>) -> Self {
            Self
        }

        fn update(
            &mut self,
            _props: &mut Self::Props<'_>,
            _hooks: Hooks,
            updater: &mut ComponentUpdater,
        ) {
            updater.set_layout_style(taffy::style::Style {
                size: taffy::geometry::Size {
                    width: taffy::style::Dimension::length(4.0),
                    height: taffy::style::Dimension::length(2.0),
                },
                ..Default::default()
            });
        }

        fn draw(&mut self, drawer: &mut ComponentDrawer<'_>) {
            drawer.set_scroll_hint(1);
            drawer
                .canvas()
                .set_text(0, 0, "box", CanvasTextStyle::default());
        }
    }

    #[component]
    fn PartialWidthScrollHintParent(hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        system.exit();

        element! {
            View(width: 10, height: 2) {
                PartialWidthScrollHintBox
            }
        }
    }

    #[apply(test!)]
    async fn test_component_drawer_scroll_hint_requires_full_canvas_width() {
        let canvases: Vec<_> = element!(PartialWidthScrollHintParent)
            .mock_terminal_render_loop(MockTerminalConfig::default())
            .collect()
            .await;

        assert_eq!(canvases.len(), 1);
        assert_eq!(
            canvases[0].scroll_hint(),
            None,
            "DECSTBM scroll regions move full terminal rows, so partial-width components must not emit hints"
        );
    }

    #[apply(test!)]
    async fn test_main_screen_layout_shift_marks_next_canvas_damage() {
        let canvases: Vec<_> = element!(LayoutShiftComponent)
            .mock_terminal_render_loop(MockTerminalConfig::default())
            .collect()
            .await;

        assert_eq!(canvases.len(), 2);
        assert_eq!(
            canvases[0].damage_region(),
            None,
            "first frame has no previous layout to compare"
        );
        assert_eq!(
            canvases[1].damage_region(),
            Some(DamageRegion {
                x: 0,
                y: 0,
                width: canvases[1].width(),
                height: canvases[1].height(),
            }),
            "CC Ink applies full-screen damage layout-shift backstop on main-screen too"
        );
        assert!(
            !canvases[1].should_force_full_repaint(),
            "layout shifts should use damage metadata rather than disabling scroll optimizations"
        );
    }

    /// A component that updates state rapidly (every poll) until 20 updates have
    /// occurred. With throttling enabled, many updates coalesce into few frames.
    #[component]
    fn RapidComponent(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let mut tick = hooks.use_state(|| 0u32);

        hooks.use_future(async move {
            for _ in 0..20 {
                // Yield so each increment lands in a separate poll cycle, then bump.
                futures_timer::Delay::new(std::time::Duration::from_millis(1)).await;
                tick += 1;
            }
        });

        if tick >= 20 {
            system.exit();
        }

        element!(Text(content: format!("tick: {}", tick)))
    }

    #[test]
    fn test_frame_profile_stats_accumulates_benchmark_metrics() {
        let mut stats = RenderFrameProfileStats::default();
        stats.record(&RenderFrameProfile {
            duration: Duration::from_millis(10),
            phases: RenderFramePhases {
                update: Duration::from_millis(1),
                layout: Duration::from_millis(2),
                draw: Duration::from_millis(3),
                terminal_write: Duration::from_millis(4),
                changed_cells: 5,
                canvas_width: 10,
                canvas_height: 2,
            },
            repaint: Some(DebugRepaintInfo {
                reason: DebugRepaintReason::FirstFrame,
                damage: None,
                previous_damage: None,
                changed_cells: 5,
                canvas_width: 10,
                canvas_height: 2,
            }),
        });
        stats.record(&RenderFrameProfile {
            duration: Duration::from_millis(30),
            phases: RenderFramePhases {
                update: Duration::from_millis(3),
                layout: Duration::from_millis(4),
                draw: Duration::from_millis(5),
                terminal_write: Duration::from_millis(6),
                changed_cells: 9,
                canvas_width: 10,
                canvas_height: 2,
            },
            repaint: None,
        });

        assert_eq!(stats.frames, 2);
        assert_eq!(stats.repaint_frames, 1);
        assert_eq!(stats.max_duration, Duration::from_millis(30));
        assert_eq!(stats.average_duration(), Duration::from_millis(20));
        assert_eq!(stats.average_update(), Duration::from_millis(2));
        assert_eq!(stats.average_layout(), Duration::from_millis(3));
        assert_eq!(stats.average_draw(), Duration::from_millis(4));
        assert_eq!(stats.average_terminal_write(), Duration::from_millis(5));
        assert_eq!(stats.total_changed_cells, 14);
        assert_eq!(stats.max_changed_cells, 9);
        assert_eq!(stats.average_changed_cells(), 7.0);
        assert_eq!(stats.repaint_ratio(), 0.5);
    }

    #[apply(test!)]
    async fn test_frame_profile_callback_reports_repaint_phases() {
        let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let events_for_callback = events.clone();
        let mut element = element!(MyComponent);
        let h = element.helper();
        let (term, output) = Terminal::mock(MockTerminalConfig::default());
        let render_loop = async {
            let mut tree = Tree::new(element.props_mut(), h);
            tree.terminal_render_loop(
                term,
                None,
                Some(Box::new(move |event| {
                    events_for_callback.lock().unwrap().push(event);
                })),
            )
            .await
            .unwrap();
        };
        let collect = output.collect::<Vec<_>>();
        let (_, canvases) = futures::join!(render_loop, collect);

        assert!(!canvases.is_empty());
        let events = events.lock().unwrap();
        assert_eq!(events.len(), canvases.len());
        let first = events
            .iter()
            .find(|event| event.repaint.is_some())
            .expect("at least the first frame should repaint");
        assert_eq!(
            first.repaint.as_ref().map(|repaint| repaint.reason),
            Some(DebugRepaintReason::FirstFrame)
        );
        assert!(first.phases.canvas_width > 0);
        assert!(first.phases.canvas_height > 0);
        assert!(
            first.phases.changed_cells > 0,
            "profile should include a retained-canvas change count"
        );
    }

    #[apply(test!)]
    async fn test_mock_terminal_render_loop_with_profile_reports_events() {
        let stats = std::sync::Arc::new(std::sync::Mutex::new(RenderFrameProfileStats::default()));
        let stats_for_callback = stats.clone();
        let canvases: Vec<_> = element!(MyComponent)
            .mock_terminal_render_loop_with_profile(MockTerminalConfig::default(), move |event| {
                stats_for_callback.lock().unwrap().record(&event);
            })
            .collect()
            .await;

        let stats = stats.lock().unwrap();
        assert_eq!(stats.frames, canvases.len());
        assert!(stats.frames > 0);
        assert!(stats.repaint_frames > 0);
        assert!(stats.max_changed_cells > 0);
    }

    #[apply(test!)]
    async fn test_render_loop_throttling_coalesces_frames() {
        // Without throttling: ~one frame per tick (21 frames including the initial).
        let unthrottled: Vec<_> = {
            let mut element = element!(RapidComponent);
            let (term, output) = Terminal::mock(MockTerminalConfig::default());
            let mut h = element.helper();
            let render_loop = async {
                let mut tree = Tree::new(element.props_mut(), h);
                tree.terminal_render_loop(term, None, None).await.unwrap();
            };
            let collect = output.collect::<Vec<_>>();
            let (_, canvases) = futures::join!(render_loop, collect);
            h = element.helper();
            let _ = h;
            canvases
        };

        // With a 50ms throttle: the 20 ticks (at ~1ms apart) coalesce into far fewer
        // frames — at most a handful of throttle windows pass during the run.
        let throttled: Vec<_> = {
            let mut element = element!(RapidComponent);
            let h = element.helper();
            let (term, output) = Terminal::mock(MockTerminalConfig::default());
            let render_loop = async {
                let mut tree = Tree::new(element.props_mut(), h);
                tree.terminal_render_loop(term, Some(std::time::Duration::from_millis(50)), None)
                    .await
                    .unwrap();
            };
            let collect = output.collect::<Vec<_>>();
            let (_, canvases) = futures::join!(render_loop, collect);
            canvases
        };

        assert!(
            unthrottled.len() > throttled.len(),
            "throttling should reduce frame count: unthrottled={} throttled={}",
            unthrottled.len(),
            throttled.len()
        );
        // Conservative bound to avoid CI timing flakiness: 20 ticks at 1ms within
        // 50ms windows should need well under half the unthrottled frame count.
        assert!(
            throttled.len() <= unthrottled.len() / 2,
            "expected at most half the frames: unthrottled={} throttled={}",
            unthrottled.len(),
            throttled.len()
        );
        // Both runs must end on the final state.
        assert!(throttled.last().unwrap().to_string().contains("tick: 20"));
        assert!(unthrottled.last().unwrap().to_string().contains("tick: 20"));
    }

    async fn await_send_future<F: Future<Output = io::Result<()>> + Send>(f: F) {
        f.await.unwrap();
    }

    // Make sure terminal_render_loop can be sent across threads.
    #[apply(test!)]
    async fn test_terminal_render_loop_send() {
        let (term, _output) = Terminal::mock(MockTerminalConfig::default());
        await_send_future(terminal_render_loop(
            &mut element!(MyComponent),
            term,
            None,
            None,
        ))
        .await;
    }

    #[component]
    fn FullWidthComponent() -> impl Into<AnyElement<'static>> {
        element! {
            View(height: 2, width: 100pct, border_style: BorderStyle::Classic)
        }
    }

    #[test]
    fn test_transparent_layout() {
        // For layout purposes, components defined with #[component] should not introduce a new
        // node in between its parent and child.
        let actual = element! {
            View(width: 10) {
                FullWidthComponent
            }
        }
        .to_string();
        assert_eq!(actual, "+--------+\n+--------+\n",);
    }

    #[derive(Default, Props)]
    struct AsyncTickerProps {
        ticks: Option<State<i32>>,
    }

    #[component]
    fn AsyncTicker<'a>(
        props: &mut AsyncTickerProps,
        mut hooks: Hooks,
    ) -> impl Into<AnyElement<'a>> {
        let mut ticks = props.ticks.unwrap();
        hooks.use_future(async move {
            ticks += 1;
        });
        element!(View)
    }

    #[component]
    fn AsyncTickerContainer(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let child_ticks = hooks.use_state(|| 0);
        let mut tick = hooks.use_state(|| 0);

        hooks.use_future(async move {
            tick += 1;
        });

        if tick == 5 {
            // make sure our children have all ticked exactly 10 times
            assert_eq!(child_ticks, 10);
            system.exit();
        } else {
            // do a few more render passes
            tick += 1;
        }

        element! {
            View {
                #((0..10).map(|_| {
                    element! {
                        AsyncTicker(ticks: child_ticks)
                    }
                }))
            }
        }
    }

    // This is a regression test for an issue where elements added via iterator without keys would
    // be re-created on every render instead of being recycled.
    #[apply(test!)]
    async fn test_async_ticker_container() {
        let canvases: Vec<_> = mock_terminal_render_loop(
            &mut element!(AsyncTickerContainer),
            MockTerminalConfig::default(),
        )
        .collect()
        .await;
        assert!(!canvases.is_empty());
    }

    #[test]
    fn test_negative_dimensions() {
        let actual = element! {
            View(width: 10, height: 5, position: Position::Relative) {
                View(position: Position::Absolute, left: 10, top: 10, right: 10, bottom: 10, overflow: Overflow::Hidden) {
                    Text(content: "Hello!")
                }
            }
        }
        .to_string();
        assert_eq!(actual, "\n\n\n\n\n",);
    }
}
