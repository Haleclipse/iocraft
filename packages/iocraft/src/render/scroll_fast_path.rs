use super::*;

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
