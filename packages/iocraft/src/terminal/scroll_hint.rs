use super::*;

/// Screen bounds used to validate a fullscreen DECSTBM scroll hint patch.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TerminalScrollHintBounds {
    /// Previous retained screen height in rows.
    pub previous_screen_height: usize,
    /// Next retained screen height in rows.
    pub next_screen_height: usize,
}

/// Options used before emitting a fullscreen DECSTBM scroll hint patch.
///
/// CC Ink only emits the `DECSTBM + SU/SD + reset + home` fast path when it is
/// running in the alternate screen **and** the scroll+diff sequence can be made
/// atomic (`decstbmSafe`). This typed gate lets custom renderers keep the same
/// boundary without duplicating policy checks or accidentally using DECSTBM in
/// main-screen output.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TerminalScrollHintPatchOptions {
    /// Whether the caller is currently rendering in fullscreen/alternate-screen mode.
    pub fullscreen: bool,
    /// Whether the caller will write the scroll patch and following row repairs
    /// inside an atomic/synchronized terminal update.
    pub synchronized_output: bool,
}

impl TerminalScrollHintPatchOptions {
    /// Returns options for a caller that already knows it is in fullscreen and
    /// inside an atomic update scope.
    pub fn fullscreen_synchronized() -> Self {
        Self {
            fullscreen: true,
            synchronized_output: true,
        }
    }

    /// Returns whether emitting a DECSTBM scroll patch is safe under these options.
    pub fn is_decstbm_safe(self) -> bool {
        self.fullscreen && self.synchronized_output
    }
}

/// Reason a fullscreen DECSTBM scroll hint patch is skipped before validation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminalScrollHintPatchSkipReason {
    /// DECSTBM is an alternate-screen/fullscreen optimization and must not be
    /// used for main-screen scrollback-preserving renderers.
    NotFullscreen,
    /// Without atomic synchronized output, users can see the intermediate
    /// hardware-scrolled region before edge rows are repainted.
    NotSynchronized,
}

/// Result of planning a guarded fullscreen DECSTBM scroll hint patch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TerminalScrollHintPatchPlan {
    /// Emit this serialized DECSTBM patch before the sparse row diff/repair pass.
    Emit(String),
    /// Do not emit DECSTBM; fall back to a normal diff path.
    Skip(TerminalScrollHintPatchSkipReason),
}

/// Reason a fullscreen DECSTBM scroll hint cannot be serialized safely.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminalScrollHintRejection {
    /// The hint has `top > bottom`.
    InvalidRegion,
    /// The hinted region is outside either the previous or next retained screen.
    OutOfBounds,
    /// The hint has a zero scroll delta.
    ZeroDelta,
    /// The absolute delta is at least the scroll-region height.
    DeltaTooLarge,
}

/// Validates a fullscreen DECSTBM scroll hint against retained screen bounds.
///
/// This mirrors the guard used by CC Ink `log-update.ts` before emitting
/// `DECSTBM + SU/SD + reset-region + cursor-home`: the region must exist in
/// both previous and next screens, the delta must be non-zero, and the shift
/// must be smaller than the region height. Callers must still gate this helper
/// on fullscreen/alternate-screen mode and atomic-update safety; it does not
/// write to the terminal or change screen mode.
pub fn validate_terminal_scroll_hint(
    hint: crate::canvas::ScrollHint,
    bounds: TerminalScrollHintBounds,
) -> Result<crate::canvas::ScrollHint, TerminalScrollHintRejection> {
    if hint.top > hint.bottom {
        return Err(TerminalScrollHintRejection::InvalidRegion);
    }
    if hint.bottom >= bounds.previous_screen_height || hint.bottom >= bounds.next_screen_height {
        return Err(TerminalScrollHintRejection::OutOfBounds);
    }

    let region_height = hint.bottom - hint.top + 1;
    let abs_delta = hint.delta.unsigned_abs() as usize;
    if abs_delta == 0 {
        return Err(TerminalScrollHintRejection::ZeroDelta);
    }
    if abs_delta >= region_height {
        return Err(TerminalScrollHintRejection::DeltaTooLarge);
    }

    Ok(hint)
}

/// Serializes a fullscreen DECSTBM scroll hint patch.
///
/// The returned string is the same defensive sequence CC Ink emits before the
/// sparse row diff: set a 1-indexed inclusive scroll region, scroll it up (`S`)
/// or down (`T`), reset the scroll region, then home the cursor. This is a
/// fullscreen-only optimization helper for custom renderers; main-screen
/// renderers should not use DECSTBM because it mutates the terminal scroll
/// region and can visibly jump without synchronized output.
pub fn terminal_scroll_hint_to_ansi(
    hint: crate::canvas::ScrollHint,
    bounds: TerminalScrollHintBounds,
) -> Result<String, TerminalScrollHintRejection> {
    let hint = validate_terminal_scroll_hint(hint, bounds)?;
    let mut out = String::new();
    out.push_str(&format!("\x1b[{};{}r", hint.top + 1, hint.bottom + 1));
    let abs_delta = hint.delta.unsigned_abs();
    if hint.delta > 0 {
        out.push_str(&format!("\x1b[{abs_delta}S"));
    } else {
        out.push_str(&format!("\x1b[{abs_delta}T"));
    }
    out.push_str("\x1b[r\x1b[H");
    Ok(out)
}

/// Plans a fullscreen DECSTBM scroll hint patch with CC Ink-style safety gates.
///
/// This checks `fullscreen` and atomic synchronized-output safety before
/// validating geometry. That ordering matches CC Ink `log-update.ts`: when
/// `altScreen` or `decstbmSafe` is false, the renderer simply falls back to its
/// ordinary diff path and does not care whether the scroll hint would otherwise
/// fit terminal bounds.
pub fn plan_terminal_scroll_hint_patch(
    hint: crate::canvas::ScrollHint,
    bounds: TerminalScrollHintBounds,
    options: TerminalScrollHintPatchOptions,
) -> Result<TerminalScrollHintPatchPlan, TerminalScrollHintRejection> {
    if !options.fullscreen {
        return Ok(TerminalScrollHintPatchPlan::Skip(
            TerminalScrollHintPatchSkipReason::NotFullscreen,
        ));
    }
    if !options.synchronized_output {
        return Ok(TerminalScrollHintPatchPlan::Skip(
            TerminalScrollHintPatchSkipReason::NotSynchronized,
        ));
    }

    terminal_scroll_hint_to_ansi(hint, bounds).map(TerminalScrollHintPatchPlan::Emit)
}

/// Writes terminal-output patches to any writer.
///
/// This is the writer form of [`terminal_patches_to_ansi`]. It is useful for
/// custom renderers that already have a patch list and want CC Ink-style
/// serialization without depending on iocraft's built-in retained-canvas
/// terminal renderer.
pub fn write_terminal_patches(
    writer: &mut (impl Write + ?Sized),
    diff: &[TerminalPatch],
    skip_sync_markers: bool,
) -> io::Result<()> {
    let output = terminal_patches_to_ansi(diff, skip_sync_markers);
    writer.write_all(output.as_bytes())
}
