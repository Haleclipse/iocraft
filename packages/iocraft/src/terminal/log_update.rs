use super::{fullscreen::packed_canvas_row_ansi_from_col, *};

/// Reason a CC Ink-style terminal diff should fall back to a full clear.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminalClearReason {
    /// The viewport changed size.
    Resize,
    /// The previous or current frame reaches native terminal scrollback.
    Offscreen,
    /// Caller-requested clear/reset outside the automatic heuristic.
    Clear,
}

/// Minimal frame geometry used by [`should_clear_terminal_screen`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TerminalFrameBounds {
    /// Rendered screen height in terminal rows.
    pub screen_height: usize,
    /// Terminal viewport width in columns.
    pub viewport_width: usize,
    /// Terminal viewport height in rows.
    pub viewport_height: usize,
}

/// Returns whether a CC Ink-style terminal diff should clear before rendering.
///
/// This mirrors `frame.ts::shouldClearScreen(...)`: resize wins first, then
/// current or previous frames whose screen height is at least the viewport
/// height are treated as offscreen/scrollback-producing and require a clear.
/// It is a mode-neutral helper for custom renderers; iocraft's built-in
/// retained-canvas renderer has additional inline/fullscreen-specific guards.
pub fn should_clear_terminal_screen(
    prev: TerminalFrameBounds,
    next: TerminalFrameBounds,
) -> Option<TerminalClearReason> {
    if next.viewport_height != prev.viewport_height || next.viewport_width != prev.viewport_width {
        return Some(TerminalClearReason::Resize);
    }

    if next.screen_height >= next.viewport_height || prev.screen_height >= prev.viewport_height {
        return Some(TerminalClearReason::Offscreen);
    }

    None
}

/// Main-screen geometry used by [`analyze_terminal_inline_diff`].
///
/// This is intentionally separate from [`TerminalFrameBounds`]: CC Ink's
/// `log-update.ts` main-screen diff has stricter cursor-reachability rules than
/// the mode-neutral `frame.ts::shouldClearScreen(...)` helper. Width shrink or
/// any width change invalidates wrap assumptions, while height growth alone does
/// not force a clear.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TerminalInlineDiffBounds {
    /// Previous rendered screen height in terminal rows.
    pub prev_screen_height: usize,
    /// Next rendered screen height in terminal rows.
    pub next_screen_height: usize,
    /// Previous terminal viewport width in columns.
    pub prev_viewport_width: usize,
    /// Previous terminal viewport height in rows.
    pub prev_viewport_height: usize,
    /// Next terminal viewport width in columns.
    pub next_viewport_width: usize,
    /// Next terminal viewport height in rows.
    pub next_viewport_height: usize,
}

/// Result of [`analyze_terminal_inline_diff`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TerminalInlineDiffAnalysis {
    /// Immediate full-clear reason, if geometry alone proves sparse diffing is unsafe.
    pub clear_reason: Option<TerminalClearReason>,
    /// Number of top rows that cannot be reached safely with relative cursor movement.
    ///
    /// If any changed cell falls within this prefix, a custom main-screen
    /// renderer should fall back to a full clear with [`TerminalClearReason::Offscreen`].
    /// This includes CC Ink's `cursorRestoreScroll` extra row when the previous
    /// frame filled or overflowed the viewport.
    pub unreachable_rows: usize,
    /// Whether the next frame is taller than the previous frame.
    pub growing: bool,
    /// Whether the next frame is shorter than the previous frame.
    pub shrinking: bool,
}

/// Analyzes CC Ink `log-update.ts` main-screen diff geometry.
///
/// This helper is **main-screen safe** and mode-neutral: it does not write to
/// the terminal, clear output, or enter fullscreen. It packages the geometry
/// guards used before sparse inline row-diffing:
///
/// - shorter viewport height or changed nonzero viewport width → `Resize`
/// - shrinking from a scrollback-producing previous frame to a frame that fits
///   the viewport → `Offscreen`
/// - clearing more rows than fit in the viewport → `Offscreen`
/// - otherwise return the top-row prefix that is unreachable; changed cells in
///   that prefix should trigger an `Offscreen` clear.
///
/// iocraft's built-in renderer applies equivalent internal guards. Custom
/// renderers that produce [`TerminalPatch`] diffs can use this to preserve CC
/// Ink's main-screen scrollback safety without adopting Claude Code policy.
pub fn analyze_terminal_inline_diff(
    bounds: TerminalInlineDiffBounds,
) -> TerminalInlineDiffAnalysis {
    let growing = bounds.next_screen_height > bounds.prev_screen_height;
    let shrinking = bounds.next_screen_height < bounds.prev_screen_height;
    let prev_had_scrollback = bounds.prev_viewport_height > 0
        && bounds.prev_screen_height > 0
        && bounds.prev_screen_height >= bounds.prev_viewport_height;

    let resize_requires_clear = bounds.next_viewport_height < bounds.prev_viewport_height
        || (bounds.prev_viewport_width != 0
            && bounds.next_viewport_width != bounds.prev_viewport_width);

    let shrink_to_fits_requires_clear = prev_had_scrollback
        && shrinking
        && bounds.next_screen_height <= bounds.prev_viewport_height;

    let shrink_clear_count = bounds
        .prev_screen_height
        .saturating_sub(bounds.next_screen_height);
    let shrink_clear_exceeds_viewport = shrinking
        && bounds.prev_viewport_height > 0
        && shrink_clear_count > bounds.prev_viewport_height;

    let clear_reason = if resize_requires_clear {
        Some(TerminalClearReason::Resize)
    } else if shrink_to_fits_requires_clear || shrink_clear_exceeds_viewport {
        Some(TerminalClearReason::Offscreen)
    } else {
        None
    };

    let cursor_restore_scroll = usize::from(prev_had_scrollback);
    let reference_height = if growing {
        bounds.prev_screen_height
    } else {
        bounds.prev_screen_height.max(bounds.next_screen_height)
    };
    let reference_viewport = if growing {
        bounds.prev_viewport_height
    } else {
        bounds.next_viewport_height
    };
    let unreachable_rows = if reference_viewport == 0 || reference_height == 0 {
        0
    } else {
        reference_height
            .saturating_sub(reference_viewport)
            .saturating_add(cursor_restore_scroll)
    };

    TerminalInlineDiffAnalysis {
        clear_reason,
        unreachable_rows,
        growing,
        shrinking,
    }
}

/// Debug metadata for a main-screen sparse diff that must fall back to clear.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalInlineDiffResetDebug {
    /// First changed row that is not safely reachable with relative cursor movement.
    pub trigger_y: usize,
    /// Previous retained-canvas text on the trigger row.
    pub prev_line: String,
    /// Next retained-canvas text on the trigger row.
    pub next_line: String,
}

/// Decision returned by [`plan_terminal_inline_canvas_diff`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalInlineCanvasDiffDecision {
    /// Geometry analysis used to make this decision.
    pub analysis: TerminalInlineDiffAnalysis,
    /// Whether a custom main-screen renderer should clear and fully repaint
    /// instead of attempting a sparse row diff.
    pub clear_reason: Option<TerminalClearReason>,
    /// Optional row-level debug information for an offscreen/unreachable change.
    pub debug: Option<TerminalInlineDiffResetDebug>,
}

/// Decides whether a main-screen retained-canvas sparse diff is safe.
///
/// This extends [`analyze_terminal_inline_diff`] with the actual canvas scan
/// from CC Ink `log-update.ts`: after geometry says sparse diffing might be
/// possible, any changed cell in the unreachable top-row prefix triggers an
/// `Offscreen` clear. The helper is mode-neutral and performs no terminal I/O;
/// it only packages the clear-vs-sparse decision and optional debug row text for
/// custom renderers.
pub fn plan_terminal_inline_canvas_diff(
    previous: &Canvas,
    next: &Canvas,
    bounds: TerminalInlineDiffBounds,
) -> TerminalInlineCanvasDiffDecision {
    let analysis = analyze_terminal_inline_diff(bounds);
    if let Some(clear_reason) = analysis.clear_reason {
        return TerminalInlineCanvasDiffDecision {
            analysis,
            clear_reason: Some(clear_reason),
            debug: None,
        };
    }

    let mut trigger_y = None;
    if analysis.unreachable_rows > 0 {
        previous.diff_each(next, |change| {
            if change.y < analysis.unreachable_rows {
                trigger_y = Some(change.y);
                true
            } else {
                false
            }
        });
    }

    let debug = trigger_y.map(|trigger_y| {
        let width = previous.width().max(next.width());
        TerminalInlineDiffResetDebug {
            trigger_y,
            prev_line: previous.get_text(0, trigger_y, width, 1),
            next_line: next.get_text(0, trigger_y, width, 1),
        }
    });

    TerminalInlineCanvasDiffDecision {
        analysis,
        clear_reason: debug.as_ref().map(|_| TerminalClearReason::Offscreen),
        debug,
    }
}

fn packed_screen_plain_row(
    screen: &CanvasPackedScreen,
    pools: &CanvasPackedCellPools,
    y: usize,
    width: usize,
) -> String {
    if y >= screen.height {
        return String::new();
    }

    let mut line = String::new();
    for x in 0..width.min(screen.width) {
        line.push_str(screen.char_in_cell(pools, x, y).unwrap_or(" "));
    }
    line.trim_end().to_string()
}

/// Decides whether a main-screen packed-screen sparse diff is safe.
///
/// This is the packed counterpart to [`plan_terminal_inline_canvas_diff`]. It
/// mirrors the CC Ink `log-update.ts` unreachable-row scan by using
/// [`CanvasPackedScreen::diff_each`] against the caller's packed buffers and by
/// reporting trimmed debug lines via [`CanvasPackedScreen::char_in_cell`]. The
/// helper remains mode-neutral: it performs no terminal I/O and does not make
/// packed screens the default renderer representation.
pub fn plan_terminal_inline_packed_canvas_diff(
    previous: &CanvasPackedScreen,
    next: &CanvasPackedScreen,
    pools: &CanvasPackedCellPools,
    bounds: TerminalInlineDiffBounds,
) -> TerminalInlineCanvasDiffDecision {
    let analysis = analyze_terminal_inline_diff(bounds);
    if let Some(clear_reason) = analysis.clear_reason {
        return TerminalInlineCanvasDiffDecision {
            analysis,
            clear_reason: Some(clear_reason),
            debug: None,
        };
    }

    let mut trigger_y = None;
    if analysis.unreachable_rows > 0 {
        previous.diff_each(next, |change| {
            if change.y < analysis.unreachable_rows {
                trigger_y = Some(change.y);
                true
            } else {
                false
            }
        });
    }

    let debug = trigger_y.map(|trigger_y| {
        let width = previous.width.max(next.width);
        TerminalInlineDiffResetDebug {
            trigger_y,
            prev_line: packed_screen_plain_row(previous, pools, trigger_y, width),
            next_line: packed_screen_plain_row(next, pools, trigger_y, width),
        }
    });

    TerminalInlineCanvasDiffDecision {
        analysis,
        clear_reason: debug.as_ref().map(|_| TerminalClearReason::Offscreen),
        debug,
    }
}

/// Patch plan for a main-screen inline retained-canvas frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalInlineCanvasFramePatchPlan {
    /// Clear-vs-sparse decision used to build this plan.
    pub decision: TerminalInlineCanvasDiffDecision,
    /// Patches to write when `decision.clear_reason` is `Some(_)`.
    ///
    /// An empty list means the sparse row diff is safe and should be produced by
    /// the caller's custom renderer. This helper deliberately does not expose a
    /// default main-screen sparse patch generator because iocraft's built-in
    /// writer keeps that path Rust-native and cursor-stateful.
    pub patches: Vec<TerminalPatch>,
}

impl TerminalInlineCanvasFramePatchPlan {
    /// Returns whether this plan requires a clear + full repaint fallback.
    pub fn requires_clear_repaint(&self) -> bool {
        self.decision.clear_reason.is_some()
    }

    /// Returns whether the caller may continue with its sparse row diff path.
    pub fn sparse_diff_safe(&self) -> bool {
        self.decision.clear_reason.is_none()
    }
}

fn canvas_ansi_without_final_newline(canvas: &Canvas) -> String {
    let mut output = Vec::new();
    canvas
        .write_ansi_without_final_newline(&mut output)
        .expect("Vec writes cannot fail");
    String::from_utf8(output).expect("canvas ANSI output is valid UTF-8")
}

/// Plans the CC Ink main-screen clear + full-repaint fallback for a canvas diff.
///
/// This combines [`plan_terminal_inline_canvas_diff`] with the full reset branch
/// from `log-update.ts`: when geometry or unreachable-row changes make sparse
/// cursor movement unsafe, the returned patches start with
/// [`TerminalPatch::ClearTerminal`] and then repaint the whole next canvas from
/// the terminal origin. When sparse diffing is safe, `patches` is empty so a
/// custom renderer can continue with its own row-diff path. The helper performs
/// no terminal I/O and does not change default renderer behavior.
pub fn plan_terminal_inline_canvas_frame_patches(
    previous: &Canvas,
    next: &Canvas,
    bounds: TerminalInlineDiffBounds,
) -> TerminalInlineCanvasFramePatchPlan {
    let decision = plan_terminal_inline_canvas_diff(previous, next, bounds);
    let patches = if decision.clear_reason.is_some() {
        vec![
            TerminalPatch::ClearTerminal,
            TerminalPatch::Stdout(canvas_ansi_without_final_newline(next)),
        ]
    } else {
        Vec::new()
    };

    TerminalInlineCanvasFramePatchPlan { decision, patches }
}

fn packed_canvas_ansi_without_final_newline(
    screen: &CanvasPackedScreen,
    pools: &CanvasPackedCellPools,
    style_cache: &mut CanvasStyleTransitionCache,
) -> String {
    if screen.height == 0 {
        return String::new();
    }

    let mut output = String::from("\x1b[0m");
    for y in 0..screen.height {
        if y > 0 {
            output.push_str("\r\n");
        }
        output.push_str(&packed_canvas_row_ansi_from_col(
            screen,
            pools,
            style_cache,
            y,
            0,
        ));
    }
    output
}

/// Plans the CC Ink main-screen clear + full-repaint fallback for a packed-screen diff.
///
/// This mirrors [`plan_terminal_inline_canvas_frame_patches`] for custom
/// renderers that already produce packed screens. When the CC Ink geometry or
/// unreachable-row scan says sparse cursor movement is unsafe, the returned
/// patch list clears the terminal and repaints the entire packed next screen
/// from the origin. When sparse diffing is safe, no patches are returned so the
/// caller can continue with its own cursor-stateful packed row diff path.
pub fn plan_terminal_inline_packed_canvas_frame_patches(
    previous: &CanvasPackedScreen,
    next: &CanvasPackedScreen,
    pools: &CanvasPackedCellPools,
    style_cache: &mut CanvasStyleTransitionCache,
    bounds: TerminalInlineDiffBounds,
) -> TerminalInlineCanvasFramePatchPlan {
    let decision = plan_terminal_inline_packed_canvas_diff(previous, next, pools, bounds);
    let patches = if decision.clear_reason.is_some() {
        vec![
            TerminalPatch::ClearTerminal,
            TerminalPatch::Stdout(packed_canvas_ansi_without_final_newline(
                next,
                pools,
                style_cache,
            )),
        ]
    } else {
        Vec::new()
    };

    TerminalInlineCanvasFramePatchPlan { decision, patches }
}

/// A terminal-output patch used by CC Ink-style diff optimizers.
///
/// iocraft's built-in terminal renderer writes retained [`Canvas`] rows
/// directly, but custom renderers and tests can use this mode-neutral patch
/// representation with [`optimize_terminal_patches`] to mirror CC Ink's
/// `optimizer.ts` rules without changing main-screen or fullscreen policy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TerminalPatch {
    /// Raw stdout payload.
    Stdout(String),
    /// Clear `count` previously-rendered rows.
    Clear {
        /// Number of rows to clear.
        count: usize,
    },
    /// Full terminal clear/reset patch.
    ClearTerminal,
    /// Hide the terminal cursor.
    CursorHide,
    /// Show the terminal cursor.
    CursorShow,
    /// Move the cursor relative to its current position.
    CursorMove {
        /// Horizontal delta; positive moves right, negative moves left.
        x: i32,
        /// Vertical delta; positive moves down, negative moves up.
        y: i32,
    },
    /// Move the cursor to an absolute 1-indexed column.
    CursorTo {
        /// Target 1-indexed terminal column.
        col: u16,
    },
    /// Emit a carriage return.
    CarriageReturn,
    /// Set the current OSC 8 hyperlink target.
    Hyperlink {
        /// Hyperlink URI.
        uri: String,
    },
    /// Pre-serialized ANSI style transition.
    StyleStr(String),
}

/// Optimizes terminal-output patches using CC Ink `optimizer.ts` rules.
///
/// For multi-patch diffs, this removes no-op stdout/clear/cursor moves,
/// merges adjacent relative cursor moves, collapses consecutive `cursorTo`
/// patches to the last target, concatenates adjacent style transition strings,
/// deduplicates consecutive hyperlinks with the same URI, and cancels adjacent
/// cursor hide/show pairs. Matching CC Ink, zero- or one-patch diffs are
/// returned unchanged. It is an opt-in optimization utility; it does not write
/// to the terminal.
pub fn optimize_terminal_patches(diff: Vec<TerminalPatch>) -> Vec<TerminalPatch> {
    if diff.len() <= 1 {
        return diff;
    }

    let mut result = Vec::with_capacity(diff.len());

    for patch in diff {
        match &patch {
            TerminalPatch::Stdout(content) if content.is_empty() => continue,
            TerminalPatch::CursorMove { x: 0, y: 0 } => continue,
            TerminalPatch::Clear { count: 0 } => continue,
            _ => {}
        }

        if let Some(last) = result.last_mut() {
            if let (
                TerminalPatch::CursorMove {
                    x: last_x,
                    y: last_y,
                },
                TerminalPatch::CursorMove { x, y },
            ) = (&mut *last, &patch)
            {
                *last_x += *x;
                *last_y += *y;
                continue;
            }

            if matches!(&*last, TerminalPatch::CursorTo { .. })
                && matches!(&patch, TerminalPatch::CursorTo { .. })
            {
                *last = patch;
                continue;
            }

            if let (TerminalPatch::StyleStr(last_str), TerminalPatch::StyleStr(str)) =
                (&mut *last, &patch)
            {
                last_str.push_str(str);
                continue;
            }

            if let (TerminalPatch::Hyperlink { uri: last_uri }, TerminalPatch::Hyperlink { uri }) =
                (&*last, &patch)
            {
                if last_uri == uri {
                    continue;
                }
            }

            if matches!(
                (&*last, &patch),
                (TerminalPatch::CursorShow, TerminalPatch::CursorHide)
                    | (TerminalPatch::CursorHide, TerminalPatch::CursorShow)
            ) {
                result.pop();
                continue;
            }
        }

        result.push(patch);
    }

    result
}

fn csi_sequence(body: impl std::fmt::Display) -> String {
    format!("\x1b[{body}")
}

fn cursor_move_sequence(x: i32, y: i32) -> String {
    let mut out = String::new();
    if x < 0 {
        out.push_str(&csi_sequence(format!("{}D", -x)));
    } else if x > 0 {
        out.push_str(&csi_sequence(format!("{x}C")));
    }
    if y < 0 {
        out.push_str(&csi_sequence(format!("{}A", -y)));
    } else if y > 0 {
        out.push_str(&csi_sequence(format!("{y}B")));
    }
    out
}

fn erase_lines_sequence(count: usize) -> String {
    if count == 0 {
        return String::new();
    }

    let mut out = String::new();
    for i in 0..count {
        out.push_str("\x1b[2K");
        if i < count - 1 {
            out.push_str("\x1b[1A");
        }
    }
    out.push_str("\x1b[G");
    out
}

fn hyperlink_patch_sequence(uri: &str) -> String {
    let mut out = Vec::new();
    if uri.is_empty() {
        crate::ansi::hyperlink_close(&mut out).expect("Vec writes cannot fail");
    } else {
        crate::ansi::hyperlink_open(&mut out, uri).expect("Vec writes cannot fail");
    }
    String::from_utf8(out).expect("hyperlink escape sequences are valid UTF-8")
}

/// Serializes terminal-output patches to ANSI, mirroring CC Ink
/// `writeDiffToTerminal(...)`.
///
/// Empty diffs serialize to an empty string. Non-empty diffs are wrapped in DEC
/// 2026 synchronized-output markers unless `skip_sync_markers` is `true`, so
/// callers can gate atomic writes on [`is_synchronized_output_supported`] or on
/// their own terminal capability probe. This is an opt-in serialization helper:
/// it does not write to the terminal or change terminal modes.
pub fn terminal_patches_to_ansi(diff: &[TerminalPatch], skip_sync_markers: bool) -> String {
    if diff.is_empty() {
        return String::new();
    }

    let mut out = String::new();
    if !skip_sync_markers {
        out.push_str("\x1b[?2026h");
    }

    for patch in diff {
        match patch {
            TerminalPatch::Stdout(content) => out.push_str(content),
            TerminalPatch::Clear { count } => out.push_str(&erase_lines_sequence(*count)),
            TerminalPatch::ClearTerminal => out.push_str(clear_terminal_sequence()),
            TerminalPatch::CursorHide => out.push_str("\x1b[?25l"),
            TerminalPatch::CursorShow => out.push_str("\x1b[?25h"),
            TerminalPatch::CursorMove { x, y } => out.push_str(&cursor_move_sequence(*x, *y)),
            TerminalPatch::CursorTo { col } => out.push_str(&csi_sequence(format!("{col}G"))),
            TerminalPatch::CarriageReturn => out.push('\r'),
            TerminalPatch::Hyperlink { uri } => out.push_str(&hyperlink_patch_sequence(uri)),
            TerminalPatch::StyleStr(str) => out.push_str(str),
        }
    }

    if !skip_sync_markers {
        out.push_str("\x1b[?2026l");
    }
    out
}
