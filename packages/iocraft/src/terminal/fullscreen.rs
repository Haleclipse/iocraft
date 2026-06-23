use super::*;

/// Options for planning fullscreen/alternate-screen cursor anchor patches.
///
/// CC Ink anchors every non-empty alt-screen diff with `CSI H` and parks the
/// cursor at the terminal bottom after the diff. That self-heals out-of-band
/// cursor drift in tmux/iTerm2 without affecting main-screen scrollback. This
/// option bag exposes the same behavior for custom patch-list renderers while
/// keeping it explicitly fullscreen-only and opt-in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TerminalFullscreenDiffPatchOptions {
    /// Whether the caller is rendering in fullscreen/alternate-screen mode.
    pub fullscreen: bool,
    /// Whether the optimized diff contains any terminal writes.
    pub has_diff: bool,
    /// Whether to erase the alt-screen display before painting this diff.
    ///
    /// This mirrors CC Ink's resize path, where `CSI 2 J` is prepended inside
    /// the same synchronized output block as the repaint so stale wide-line
    /// tails disappear atomically.
    pub erase_before_paint: bool,
    /// Terminal row count used to park the cursor at `row;1H` after the diff.
    ///
    /// Use `None` when the size is unknown; the pre-diff anchor is still useful
    /// for relative diff correctness, but no post-diff park patch is emitted.
    pub terminal_rows: Option<u16>,
}

/// Fullscreen cursor anchor patches for a custom terminal diff.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TerminalFullscreenDiffPatchPlan {
    /// Patch to prepend before the caller's diff, usually `CSI H` or
    /// `CSI 2 J` + `CSI H` after resize.
    pub pre_diff_patch: Option<TerminalPatch>,
    /// Patch to append after the caller's diff to park the cursor at the bottom
    /// row and column 1.
    pub post_diff_patch: Option<TerminalPatch>,
}

impl TerminalFullscreenDiffPatchPlan {
    /// Returns whether no anchor or park patch is required.
    pub fn is_empty(&self) -> bool {
        self.pre_diff_patch.is_none() && self.post_diff_patch.is_none()
    }

    /// Prepends/appends this plan to an existing optimized diff.
    ///
    /// The caller should compute `has_diff` from the optimized diff before
    /// planning, matching CC Ink's order: optimize first, then add the
    /// fullscreen-only cursor preamble/postamble only when there is actual work.
    pub fn apply_to(&self, diff: &mut Vec<TerminalPatch>) {
        if let Some(pre) = &self.pre_diff_patch {
            diff.insert(0, pre.clone());
        }
        if let Some(post) = &self.post_diff_patch {
            diff.push(post.clone());
        }
    }
}

/// Plans CC Ink-style fullscreen cursor anchor/park patches for a terminal diff.
///
/// Returns an empty plan unless `fullscreen && has_diff`. The erase variant uses
/// `CSI 2 J` + `CSI H` rather than [`TerminalPatch::ClearTerminal`] because the
/// alt-screen resize path must erase the visible display without issuing the
/// main-screen scrollback-clear sequence. No terminal I/O is performed.
pub fn plan_terminal_fullscreen_diff_patches(
    options: TerminalFullscreenDiffPatchOptions,
) -> TerminalFullscreenDiffPatchPlan {
    if !options.fullscreen || !options.has_diff {
        return TerminalFullscreenDiffPatchPlan::default();
    }

    let pre_diff_patch = Some(TerminalPatch::Stdout(if options.erase_before_paint {
        "\x1b[2J\x1b[H".to_string()
    } else {
        "\x1b[H".to_string()
    }));
    let post_diff_patch = options
        .terminal_rows
        .filter(|row| *row > 0)
        .map(|row| TerminalPatch::Stdout(format!("\x1b[{row};1H")));

    TerminalFullscreenDiffPatchPlan {
        pre_diff_patch,
        post_diff_patch,
    }
}

/// Options for producing fullscreen/alternate-screen canvas diff patches.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TerminalFullscreenCanvasDiffOptions {
    /// Zero-indexed terminal row offset for the retained canvas origin.
    pub top_row: u16,
    /// Rewrite every retained row even when the canvases compare equal.
    ///
    /// The helper also honors `Canvas::force_full_repaint()` internally; this
    /// flag lets custom renderers request the same behavior without mutating
    /// the canvas metadata.
    pub force_full_repaint: bool,
}

pub(super) fn canvas_row_ansi_from_col(canvas: &Canvas, y: usize, start_col: usize) -> String {
    let mut row = Vec::new();
    canvas
        .write_ansi_row_from_col_without_newline(y, start_col, &mut row)
        .expect("Vec writes cannot fail");
    String::from_utf8(row).expect("canvas ANSI rows are valid UTF-8")
}

/// Produces absolute fullscreen canvas diff patches for a custom renderer.
///
/// This is an opt-in patch-list counterpart to iocraft's built-in fullscreen
/// retained writer. It mirrors the CC Ink alt-screen pattern of absolute row
/// addressing from a known origin: unchanged rows are skipped, changed rows are
/// written from their first changed column through EOL, damaged rows are honored
/// through [`Canvas::row_change_start`], and rows removed by a shorter next
/// canvas are cleared with `CSI 2 K`. The function performs no terminal I/O and
/// does not add cursor anchor/park or synchronized-output wrappers; pair it with
/// [`plan_terminal_fullscreen_diff_patches`], [`optimize_terminal_patches`], and
/// [`terminal_patches_to_ansi`] as needed.
///
/// For a DECSTBM scroll fast path, shift the previous canvas baseline first
/// (for example via
/// `ScrollFastPathFrameApplication::shift_previous_canvas_for_terminal_diff`)
/// and pass the shifted baseline as `previous` so this sparse diff only emits
/// edge and repair rows.
pub fn terminal_fullscreen_canvas_diff_patches(
    previous: Option<&Canvas>,
    next: &Canvas,
    options: TerminalFullscreenCanvasDiffOptions,
) -> Vec<TerminalPatch> {
    let mut diff = Vec::new();
    let force_full_repaint = options.force_full_repaint || next.should_force_full_repaint();

    let max_height = previous
        .map(|previous| previous.height().max(next.height()))
        .unwrap_or_else(|| next.height());

    for y in 0..max_height {
        let start_col = match previous {
            None => {
                if y < next.height() {
                    Some(0)
                } else {
                    None
                }
            }
            Some(_) if force_full_repaint || y >= next.height() => Some(0),
            Some(previous) => previous.row_change_start(next, y),
        };
        let Some(start_col) = start_col else {
            continue;
        };

        let row = usize::from(options.top_row) + y + 1;
        let col = start_col + 1;
        diff.push(TerminalPatch::Stdout(format!("\x1b[{row};{col}H")));
        if y < next.height() {
            diff.push(TerminalPatch::Stdout(canvas_row_ansi_from_col(
                next, y, start_col,
            )));
        } else {
            diff.push(TerminalPatch::Stdout("\x1b[2K".to_string()));
        }
    }

    diff
}

pub(super) fn packed_canvas_row_ansi_from_col(
    next: &CanvasPackedScreen,
    pools: &CanvasPackedCellPools,
    style_cache: &mut CanvasStyleTransitionCache,
    y: usize,
    start_col: usize,
) -> String {
    next.ansi_row_with_style_cache(pools, style_cache, y, start_col)
        .expect("packed canvas ANSI rows are valid UTF-8")
}

/// Produces absolute fullscreen packed-screen diff patches for custom renderers.
///
/// This is the packed counterpart to [`terminal_fullscreen_canvas_diff_patches`]
/// and CC Ink's packed `screen.diff(...)` + sparse row writer path. It uses
/// [`CanvasPackedScreen::row_change_start`] to honor damage/shrink regions,
/// serializes changed rows with [`CanvasPackedScreen::write_ansi_row_with_style_cache`],
/// and preserves the same fullscreen absolute row addressing convention. The
/// helper performs no terminal I/O, does not enter fullscreen, and keeps packed
/// screen usage opt-in for custom retained renderers and benchmarks.
pub fn terminal_fullscreen_packed_canvas_diff_patches(
    previous: Option<&CanvasPackedScreen>,
    next: &CanvasPackedScreen,
    pools: &CanvasPackedCellPools,
    style_cache: &mut CanvasStyleTransitionCache,
    options: TerminalFullscreenCanvasDiffOptions,
) -> Vec<TerminalPatch> {
    let mut diff = Vec::new();
    let max_height = previous
        .map(|previous| previous.height.max(next.height))
        .unwrap_or(next.height);

    for y in 0..max_height {
        let start_col = match previous {
            None => (y < next.height).then_some(0),
            Some(_) if options.force_full_repaint || y >= next.height => Some(0),
            Some(previous) => previous.row_change_start(next, y),
        };
        let Some(start_col) = start_col else {
            continue;
        };

        let row = usize::from(options.top_row) + y + 1;
        let col = start_col + 1;
        diff.push(TerminalPatch::Stdout(format!("\x1b[{row};{col}H")));
        if y < next.height {
            diff.push(TerminalPatch::Stdout(packed_canvas_row_ansi_from_col(
                next,
                pools,
                style_cache,
                y,
                start_col,
            )));
        } else {
            diff.push(TerminalPatch::Stdout("\x1b[2K".to_string()));
        }
    }

    diff
}

/// Options for composing a complete fullscreen canvas frame patch list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalFullscreenCanvasFramePatchOptions {
    /// Options used for absolute retained-canvas row diffing.
    pub canvas_diff: TerminalFullscreenCanvasDiffOptions,
    /// Optional fullscreen DECSTBM scroll patch to prepend to the content diff.
    ///
    /// This should already have passed [`plan_terminal_scroll_hint_patch`] or an
    /// equivalent fullscreen/atomic safety gate. It is placed before row repairs,
    /// matching CC Ink's `scrollPatch + screen.diff` ordering.
    pub scroll_patch_ansi: Option<String>,
    /// Whether the final fullscreen cursor preamble should erase the visible
    /// alt-screen before painting. This is the CC Ink resize path.
    pub erase_before_paint: bool,
    /// Terminal row count used to park the cursor after the diff.
    pub terminal_rows: Option<u16>,
    /// Whether to apply [`optimize_terminal_patches`] before adding fullscreen
    /// cursor anchor/park patches.
    ///
    /// CC Ink optimizes the content diff first, then prepends/appends the
    /// fullscreen cursor patches. Set this to `false` only when callers need to
    /// inspect the raw patch boundaries for tests or instrumentation.
    pub optimize: bool,
}

impl Default for TerminalFullscreenCanvasFramePatchOptions {
    fn default() -> Self {
        Self {
            canvas_diff: TerminalFullscreenCanvasDiffOptions::default(),
            scroll_patch_ansi: None,
            erase_before_paint: false,
            terminal_rows: None,
            optimize: true,
        }
    }
}

/// Result of [`plan_terminal_fullscreen_canvas_frame_patches`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TerminalFullscreenCanvasFramePatchPlan {
    /// Final patch list: cursor anchor, optional DECSTBM scroll patch, canvas row
    /// diff/repairs, then cursor park.
    pub patches: Vec<TerminalPatch>,
    /// Cursor pre/post plan that was applied to `patches`.
    pub cursor_plan: TerminalFullscreenDiffPatchPlan,
    /// Number of non-cursor patches after optional optimization.
    pub content_patch_count: usize,
    /// Whether a non-empty DECSTBM scroll patch was included.
    pub had_scroll_patch: bool,
}

impl TerminalFullscreenCanvasFramePatchPlan {
    /// Returns whether the plan has no terminal patches to write.
    pub fn is_empty(&self) -> bool {
        self.patches.is_empty()
    }
}

/// Composes fullscreen retained-canvas frame patches for custom renderers.
///
/// This is the opt-in bridge that packages the CC Ink fullscreen patch-list
/// sequence in Rust-native pieces:
///
/// 1. optional DECSTBM scroll patch (already safety-gated by the caller),
/// 2. absolute retained-canvas sparse row diff/repair patches,
/// 3. optional CC Ink-style optimization,
/// 4. fullscreen cursor anchor/resize-erase preamble and bottom-row park.
///
/// The function never writes to the terminal, never enters fullscreen, and does
/// not make DECSTBM decisions itself. For scroll fast paths, callers should pass
/// a previous canvas baseline that has already been shifted to mirror the
/// hardware scroll.
pub fn plan_terminal_fullscreen_canvas_frame_patches(
    previous: Option<&Canvas>,
    next: &Canvas,
    options: TerminalFullscreenCanvasFramePatchOptions,
) -> TerminalFullscreenCanvasFramePatchPlan {
    let mut content = Vec::new();
    let mut had_scroll_patch = false;

    if let Some(scroll_patch_ansi) = options.scroll_patch_ansi {
        if !scroll_patch_ansi.is_empty() {
            content.push(TerminalPatch::Stdout(scroll_patch_ansi));
            had_scroll_patch = true;
        }
    }

    content.extend(terminal_fullscreen_canvas_diff_patches(
        previous,
        next,
        options.canvas_diff,
    ));

    if options.optimize {
        content = optimize_terminal_patches(content);
    }

    let content_patch_count = content.len();
    let cursor_plan = plan_terminal_fullscreen_diff_patches(TerminalFullscreenDiffPatchOptions {
        fullscreen: true,
        has_diff: !content.is_empty(),
        erase_before_paint: options.erase_before_paint,
        terminal_rows: options.terminal_rows,
    });
    let mut patches = content;
    cursor_plan.apply_to(&mut patches);

    TerminalFullscreenCanvasFramePatchPlan {
        patches,
        cursor_plan,
        content_patch_count,
        had_scroll_patch,
    }
}

/// Composes fullscreen packed-screen frame patches for custom renderers.
///
/// This mirrors [`plan_terminal_fullscreen_canvas_frame_patches`] while keeping
/// the packed `Screen`/`CharPool`/`StylePool` path opt-in. It is useful for
/// retained renderers that already produced [`CanvasPackedScreen`] snapshots and
/// want CC Ink-style absolute fullscreen row diffs without converting back to
/// typed [`Canvas`] rows.
pub fn plan_terminal_fullscreen_packed_canvas_frame_patches(
    previous: Option<&CanvasPackedScreen>,
    next: &CanvasPackedScreen,
    pools: &CanvasPackedCellPools,
    style_cache: &mut CanvasStyleTransitionCache,
    options: TerminalFullscreenCanvasFramePatchOptions,
) -> TerminalFullscreenCanvasFramePatchPlan {
    let mut content = Vec::new();
    let mut had_scroll_patch = false;

    if let Some(scroll_patch_ansi) = options.scroll_patch_ansi {
        if !scroll_patch_ansi.is_empty() {
            content.push(TerminalPatch::Stdout(scroll_patch_ansi));
            had_scroll_patch = true;
        }
    }

    content.extend(terminal_fullscreen_packed_canvas_diff_patches(
        previous,
        next,
        pools,
        style_cache,
        options.canvas_diff,
    ));

    if options.optimize {
        content = optimize_terminal_patches(content);
    }

    let content_patch_count = content.len();
    let cursor_plan = plan_terminal_fullscreen_diff_patches(TerminalFullscreenDiffPatchOptions {
        fullscreen: true,
        has_diff: !content.is_empty(),
        erase_before_paint: options.erase_before_paint,
        terminal_rows: options.terminal_rows,
    });
    let mut patches = content;
    cursor_plan.apply_to(&mut patches);

    TerminalFullscreenCanvasFramePatchPlan {
        patches,
        cursor_plan,
        content_patch_count,
        had_scroll_patch,
    }
}

/// Result of composing a fullscreen canvas frame with a guarded DECSTBM scroll hint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalFullscreenCanvasScrollFramePatchPlan {
    /// Final frame patch plan after optional previous-baseline shifting.
    pub frame: TerminalFullscreenCanvasFramePatchPlan,
    /// Scroll-hint planning result. `None` means the caller supplied no hint;
    /// `Some(Skip(_))` means the fullscreen/atomic safety gate rejected DECSTBM
    /// and the frame fell back to an ordinary diff.
    pub scroll_hint_plan: Option<TerminalScrollHintPatchPlan>,
}

impl TerminalFullscreenCanvasScrollFramePatchPlan {
    /// Returns whether the final frame has no terminal patches to write.
    pub fn is_empty(&self) -> bool {
        self.frame.is_empty()
    }

    /// Returns whether the DECSTBM scroll patch was emitted and included in the frame.
    pub fn had_scroll_patch(&self) -> bool {
        self.frame.had_scroll_patch
    }
}

fn plan_scroll_hint_for_fullscreen_frame(
    hint: Option<crate::canvas::ScrollHint>,
    previous_height: usize,
    next_height: usize,
    options: TerminalScrollHintPatchOptions,
) -> Result<Option<TerminalScrollHintPatchPlan>, TerminalScrollHintRejection> {
    let Some(hint) = hint else {
        return Ok(None);
    };
    plan_terminal_scroll_hint_patch(
        hint,
        TerminalScrollHintBounds {
            previous_screen_height: previous_height,
            next_screen_height: next_height,
        },
        options,
    )
    .map(Some)
}

/// Composes a fullscreen retained-canvas frame and safely applies a scroll hint.
///
/// This is the direct opt-in counterpart to CC Ink `log-update.ts`'s
/// `altScreen && next.scrollHint && decstbmSafe` branch: the hint is first gated
/// by [`plan_terminal_scroll_hint_patch`]; when it emits, a previous-canvas
/// clone is shifted before row diffing so only edge/repair rows are repainted.
/// When the gate skips DECSTBM, the original previous canvas is diffed normally.
/// The function performs no terminal I/O and does not enter fullscreen.
///
/// When `hint` is `Some`, this helper owns the DECSTBM prefix: it overwrites
/// `frame_options.scroll_patch_ansi` on emit and clears it when the safety gate
/// skips. When `hint` is `None`, `frame_options` is passed through unchanged.
pub fn plan_terminal_fullscreen_canvas_scroll_frame_patches(
    previous: &Canvas,
    next: &Canvas,
    hint: Option<crate::canvas::ScrollHint>,
    scroll_options: TerminalScrollHintPatchOptions,
    mut frame_options: TerminalFullscreenCanvasFramePatchOptions,
) -> Result<TerminalFullscreenCanvasScrollFramePatchPlan, TerminalScrollHintRejection> {
    let hint_was_supplied = hint.is_some();
    let scroll_hint_plan = plan_scroll_hint_for_fullscreen_frame(
        hint,
        previous.height(),
        next.height(),
        scroll_options,
    )?;
    let mut shifted_previous = None;

    if let (Some(hint), Some(TerminalScrollHintPatchPlan::Emit(scroll_patch))) =
        (hint, scroll_hint_plan.as_ref())
    {
        let mut shifted = previous.clone();
        shifted.shift_rows(hint.top, hint.bottom, hint.delta);
        shifted_previous = Some(shifted);
        frame_options.scroll_patch_ansi = Some(scroll_patch.clone());
    } else if hint_was_supplied {
        frame_options.scroll_patch_ansi = None;
    }

    let previous_for_diff = shifted_previous.as_ref().unwrap_or(previous);
    let frame =
        plan_terminal_fullscreen_canvas_frame_patches(Some(previous_for_diff), next, frame_options);

    Ok(TerminalFullscreenCanvasScrollFramePatchPlan {
        frame,
        scroll_hint_plan,
    })
}

/// Composes a fullscreen packed-screen frame and safely applies a scroll hint.
///
/// This mirrors [`plan_terminal_fullscreen_canvas_scroll_frame_patches`] for
/// custom renderers that already use [`CanvasPackedScreen`]. A guarded emitted
/// DECSTBM hint shifts a packed previous-screen clone before sparse row diffing;
/// skipped hints fall back to the ordinary packed diff. Packed screen usage
/// remains opt-in and no terminal I/O is performed.
pub fn plan_terminal_fullscreen_packed_canvas_scroll_frame_patches(
    previous: &CanvasPackedScreen,
    next: &CanvasPackedScreen,
    pools: &CanvasPackedCellPools,
    style_cache: &mut CanvasStyleTransitionCache,
    hint: Option<crate::canvas::ScrollHint>,
    scroll_options: TerminalScrollHintPatchOptions,
    mut frame_options: TerminalFullscreenCanvasFramePatchOptions,
) -> Result<TerminalFullscreenCanvasScrollFramePatchPlan, TerminalScrollHintRejection> {
    let hint_was_supplied = hint.is_some();
    let scroll_hint_plan =
        plan_scroll_hint_for_fullscreen_frame(hint, previous.height, next.height, scroll_options)?;
    let mut shifted_previous = None;

    if let (Some(hint), Some(TerminalScrollHintPatchPlan::Emit(scroll_patch))) =
        (hint, scroll_hint_plan.as_ref())
    {
        let mut shifted = previous.clone();
        shifted.shift_rows(hint.top, hint.bottom, hint.delta);
        shifted_previous = Some(shifted);
        frame_options.scroll_patch_ansi = Some(scroll_patch.clone());
    } else if hint_was_supplied {
        frame_options.scroll_patch_ansi = None;
    }

    let previous_for_diff = shifted_previous.as_ref().unwrap_or(previous);
    let frame = plan_terminal_fullscreen_packed_canvas_frame_patches(
        Some(previous_for_diff),
        next,
        pools,
        style_cache,
        frame_options,
    );

    Ok(TerminalFullscreenCanvasScrollFramePatchPlan {
        frame,
        scroll_hint_plan,
    })
}

/// Stateful opt-in fullscreen retained-canvas frame planner.
///
/// CC Ink's `LogUpdate` owns the previous screen and mutates/shifts it before
/// diffing DECSTBM scroll frames. This Rust helper exposes the same state shape
/// without doing terminal I/O or changing modes: callers feed it successive
/// retained [`Canvas`] frames, receive patch plans, and choose when to write the
/// serialized ANSI themselves.
#[derive(Clone, Default)]
pub struct TerminalFullscreenCanvasFrameState {
    previous: Option<Canvas>,
}

impl TerminalFullscreenCanvasFrameState {
    /// Creates an empty state with no trusted previous canvas.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the trusted previous retained canvas, if any.
    pub fn previous(&self) -> Option<&Canvas> {
        self.previous.as_ref()
    }

    /// Drops the trusted previous canvas, e.g. after terminal resume or mode reset.
    pub fn reset(&mut self) {
        self.previous = None;
    }

    /// Plans a fullscreen frame diff and stores `next` as the new previous canvas.
    pub fn plan_frame(
        &mut self,
        next: &Canvas,
        options: TerminalFullscreenCanvasFramePatchOptions,
    ) -> TerminalFullscreenCanvasFramePatchPlan {
        let frame =
            plan_terminal_fullscreen_canvas_frame_patches(self.previous.as_ref(), next, options);
        self.previous = Some(next.clone());
        frame
    }

    /// Plans a fullscreen frame with an optional safety-gated DECSTBM scroll hint.
    ///
    /// If there is no trusted previous frame yet, the hint is ignored and a
    /// normal first-frame diff is produced. When a previous frame exists, this
    /// delegates to [`plan_terminal_fullscreen_canvas_scroll_frame_patches`], so
    /// emitted DECSTBM patches shift the previous baseline before sparse diffing
    /// and skipped hints fall back to an ordinary diff.
    pub fn plan_scroll_frame(
        &mut self,
        next: &Canvas,
        hint: Option<crate::canvas::ScrollHint>,
        scroll_options: TerminalScrollHintPatchOptions,
        mut frame_options: TerminalFullscreenCanvasFramePatchOptions,
    ) -> Result<TerminalFullscreenCanvasScrollFramePatchPlan, TerminalScrollHintRejection> {
        let plan = if let Some(previous) = self.previous.as_ref() {
            plan_terminal_fullscreen_canvas_scroll_frame_patches(
                previous,
                next,
                hint,
                scroll_options,
                frame_options,
            )?
        } else {
            if hint.is_some() {
                frame_options.scroll_patch_ansi = None;
            }
            TerminalFullscreenCanvasScrollFramePatchPlan {
                frame: plan_terminal_fullscreen_canvas_frame_patches(None, next, frame_options),
                scroll_hint_plan: None,
            }
        };
        self.previous = Some(next.clone());
        Ok(plan)
    }
}

/// Stateful opt-in fullscreen packed-screen frame planner.
///
/// This is the packed counterpart to [`TerminalFullscreenCanvasFrameState`]. It
/// retains the previous [`CanvasPackedScreen`] and owns a style-transition cache
/// for sparse row serialization, while the caller keeps the compatible
/// [`CanvasPackedCellPools`]. Packed IDs must remain valid for the supplied
/// pools across frames.
#[derive(Clone, Debug, Default)]
pub struct TerminalFullscreenPackedCanvasFrameState {
    previous: Option<CanvasPackedScreen>,
    style_cache: CanvasStyleTransitionCache,
}

impl TerminalFullscreenPackedCanvasFrameState {
    /// Creates an empty packed frame state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the trusted previous packed screen, if any.
    pub fn previous(&self) -> Option<&CanvasPackedScreen> {
        self.previous.as_ref()
    }

    /// Returns the internal style transition cache.
    pub fn style_cache(&self) -> &CanvasStyleTransitionCache {
        &self.style_cache
    }

    /// Returns the internal style transition cache mutably.
    pub fn style_cache_mut(&mut self) -> &mut CanvasStyleTransitionCache {
        &mut self.style_cache
    }

    /// Drops previous-screen trust and clears cached style transitions.
    pub fn reset(&mut self) {
        self.previous = None;
        self.style_cache.clear();
    }

    /// Plans a packed fullscreen frame diff and stores `next` as the new baseline.
    pub fn plan_frame(
        &mut self,
        next: &CanvasPackedScreen,
        pools: &CanvasPackedCellPools,
        options: TerminalFullscreenCanvasFramePatchOptions,
    ) -> TerminalFullscreenCanvasFramePatchPlan {
        let frame = plan_terminal_fullscreen_packed_canvas_frame_patches(
            self.previous.as_ref(),
            next,
            pools,
            &mut self.style_cache,
            options,
        );
        self.previous = Some(next.clone());
        frame
    }

    /// Plans a packed fullscreen frame with an optional safety-gated DECSTBM hint.
    pub fn plan_scroll_frame(
        &mut self,
        next: &CanvasPackedScreen,
        pools: &CanvasPackedCellPools,
        hint: Option<crate::canvas::ScrollHint>,
        scroll_options: TerminalScrollHintPatchOptions,
        mut frame_options: TerminalFullscreenCanvasFramePatchOptions,
    ) -> Result<TerminalFullscreenCanvasScrollFramePatchPlan, TerminalScrollHintRejection> {
        let plan = if let Some(previous) = self.previous.as_ref() {
            plan_terminal_fullscreen_packed_canvas_scroll_frame_patches(
                previous,
                next,
                pools,
                &mut self.style_cache,
                hint,
                scroll_options,
                frame_options,
            )?
        } else {
            if hint.is_some() {
                frame_options.scroll_patch_ansi = None;
            }
            TerminalFullscreenCanvasScrollFramePatchPlan {
                frame: plan_terminal_fullscreen_packed_canvas_frame_patches(
                    None,
                    next,
                    pools,
                    &mut self.style_cache,
                    frame_options,
                ),
                scroll_hint_plan: None,
            }
        };
        self.previous = Some(next.clone());
        Ok(plan)
    }
}
