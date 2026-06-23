use super::*;

/// A point in canvas/screen-buffer coordinates used by selection helpers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SelectionPoint {
    /// Column.
    pub col: usize,
    /// Row.
    pub row: usize,
}

/// A linear text selection in canvas/screen-buffer coordinates.
///
/// The range is inclusive at both ends and is normalized in reading order by
/// the helper methods. This mirrors CC Ink's `SelectionState` bounds model while
/// keeping iocraft's core renderer independent from mouse/keyboard policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SelectionRange {
    /// Where the selection started.
    pub anchor: SelectionPoint,
    /// Where the selection currently ends.
    pub focus: SelectionPoint,
}

/// Position of a text match within a rendered canvas.
///
/// `len` is measured in terminal cells, so a match containing a wide character
/// spans the wide character's leading cell plus its tail cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextMatchPosition {
    /// Row of the match.
    pub row: usize,
    /// First column of the match.
    pub col: usize,
    /// Width of the match in terminal cells.
    pub len: usize,
}

impl SelectionRange {
    /// Constructs a new inclusive selection range.
    pub fn new(anchor: SelectionPoint, focus: SelectionPoint) -> Self {
        Self { anchor, focus }
    }

    pub(super) fn normalized(self) -> (SelectionPoint, SelectionPoint) {
        let anchor_first = self.anchor.row < self.focus.row
            || (self.anchor.row == self.focus.row && self.anchor.col <= self.focus.col);
        if anchor_first {
            (self.anchor, self.focus)
        } else {
            (self.focus, self.anchor)
        }
    }

    /// Returns whether `point` is inside this inclusive linear selection.
    pub fn contains(self, point: SelectionPoint) -> bool {
        let (start, end) = self.normalized();
        if point.row < start.row || point.row > end.row {
            return false;
        }
        if point.row == start.row && point.col < start.col {
            return false;
        }
        if point.row == end.row && point.col > end.col {
            return false;
        }
        true
    }
}

/// Side of the viewport whose rows are being captured before scrolling out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectionCaptureSide {
    /// Rows are leaving above the viewport; captured rows are prepended before
    /// the current on-screen selection text.
    Above,
    /// Rows are leaving below the viewport; captured rows are appended after
    /// the current on-screen selection text.
    Below,
}

/// Multi-click selection granularity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectionSpanKind {
    /// Select and extend by same-class word runs.
    Word,
    /// Select and extend by whole screen rows.
    Line,
}

/// Semantic keyboard selection focus movement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectionFocusMove {
    /// Move one cell left, wrapping to the previous row when possible.
    Left,
    /// Move one cell right, wrapping to the next row when possible.
    Right,
    /// Move one row up, keeping the current column.
    Up,
    /// Move one row down, keeping the current column.
    Down,
    /// Move to column 0 on the current row.
    LineStart,
    /// Move to the last column on the current row.
    LineEnd,
}

/// Multi-click selection count handled on mouse press.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectionClickCount {
    /// Double-click selects a same-class word run.
    Double,
    /// Triple-click selects a whole screen row.
    Triple,
}

/// Classified mouse press for fullscreen text selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectionMousePressKind {
    /// A single click/press starts char-mode selection.
    Single,
    /// A double click/press selects a word run.
    Double,
    /// A triple-or-later click/press selects a whole row.
    Triple,
}

impl SelectionMousePressKind {
    /// Returns the multi-click selection count, or `None` for a single press.
    pub fn click_count(self) -> Option<SelectionClickCount> {
        match self {
            Self::Single => None,
            Self::Double => Some(SelectionClickCount::Double),
            Self::Triple => Some(SelectionClickCount::Triple),
        }
    }
}

/// CC Ink-compatible multi-click timeout in milliseconds.
pub const SELECTION_MULTI_CLICK_TIMEOUT_MS: u64 = 500;

/// CC Ink-compatible multi-click cell-distance tolerance.
pub const SELECTION_MULTI_CLICK_DISTANCE: usize = 1;

/// CC Ink-compatible drag autoscroll step size in rows.
pub const SELECTION_AUTOSCROLL_LINES: usize = 2;

/// CC Ink-compatible drag autoscroll timer interval in milliseconds.
pub const SELECTION_AUTOSCROLL_INTERVAL_MS: u64 = 50;

/// CC Ink-compatible drag autoscroll lost-release failsafe tick limit.
pub const SELECTION_AUTOSCROLL_MAX_TICKS: usize = 200;

/// Direction of drag autoscroll while extending a fullscreen selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectionDragScrollDirection {
    /// Focus is above the scroll viewport; scroll upward, moving content down.
    Up,
    /// Focus is below the scroll viewport; scroll downward, moving content up.
    Down,
}

/// Tracks double/triple-click presses for fullscreen selection.
///
/// This mirrors CC Ink's `App.tsx` bookkeeping: a press within 500ms and
/// within one terminal cell of the previous press increments the click count;
/// otherwise it resets to a single click. Counts greater than three are
/// reported as triple-clicks so repeated clicks keep selecting the line.
#[derive(Clone, Debug, Default)]
pub struct SelectionClickTracker {
    last_click_time_ms: Option<u64>,
    last_click_col: usize,
    last_click_row: usize,
    click_count: u8,
}

impl SelectionClickTracker {
    /// Creates a fresh multi-click tracker.
    pub fn new() -> Self {
        Self::default()
    }

    /// Clears the tracked click chain.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Records a press and returns whether it is a single, double, or triple
    /// selection click. `now_ms` should be a monotonic timestamp in
    /// milliseconds; it is intentionally passed in by the caller to keep this
    /// type deterministic and easy to test.
    pub fn record_press(&mut self, col: usize, row: usize, now_ms: u64) -> SelectionMousePressKind {
        let near_last = self.last_click_time_ms.is_some_and(|last| {
            now_ms.saturating_sub(last) < SELECTION_MULTI_CLICK_TIMEOUT_MS
                && col.abs_diff(self.last_click_col) <= SELECTION_MULTI_CLICK_DISTANCE
                && row.abs_diff(self.last_click_row) <= SELECTION_MULTI_CLICK_DISTANCE
        });
        self.click_count = if near_last {
            self.click_count.saturating_add(1).min(3)
        } else {
            1
        };
        self.last_click_time_ms = Some(now_ms);
        self.last_click_col = col;
        self.last_click_row = row;

        match self.click_count {
            1 => SelectionMousePressKind::Single,
            2 => SelectionMousePressKind::Double,
            _ => SelectionMousePressKind::Triple,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct SelectionSpan {
    pub(super) lo: SelectionPoint,
    pub(super) hi: SelectionPoint,
    kind: SelectionSpanKind,
}

/// Mutable fullscreen text selection state.
///
/// This is a small Rust counterpart to CC Ink's `SelectionState`: it tracks an
/// anchor/focus linear selection plus text captured from rows that scroll out of
/// the viewport during drag/keyboard scrolling.
#[derive(Clone, Debug, Default)]
pub struct SelectionState {
    anchor: Option<SelectionPoint>,
    focus: Option<SelectionPoint>,
    is_dragging: bool,
    pub(super) anchor_span: Option<SelectionSpan>,
    pub(super) virtual_anchor_row: Option<isize>,
    virtual_focus_row: Option<isize>,
    pub(super) scrolled_off_above: Vec<String>,
    pub(super) scrolled_off_below: Vec<String>,
    pub(super) scrolled_off_above_soft_wrap: Vec<bool>,
    pub(super) scrolled_off_below_soft_wrap: Vec<bool>,
    last_press_had_alt: bool,
}

impl SelectionState {
    /// Creates an empty selection state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the selection anchor, if any.
    pub fn anchor(&self) -> Option<SelectionPoint> {
        self.anchor
    }

    /// Returns the current focus endpoint, if any.
    pub fn focus(&self) -> Option<SelectionPoint> {
        self.focus
    }

    /// Returns whether a drag is in progress.
    pub fn is_dragging(&self) -> bool {
        self.is_dragging
    }

    /// Returns whether the mouse press that started the current selection had
    /// the Alt modifier set. CC Ink uses this to show the correct copy hint in
    /// VS Code/macOS terminals where Alt-click may be intercepted natively.
    pub fn last_press_had_alt(&self) -> bool {
        self.last_press_had_alt
    }

    /// Updates the Alt-modifier marker for the press that just started the
    /// selection. This mirrors CC Ink's SGR-button-bit handling, where the
    /// parser writes the flag immediately after `startSelection(...)`.
    pub fn set_last_press_had_alt(&mut self, value: bool) {
        self.last_press_had_alt = value;
    }

    /// Starts a drag selection at the given cell. The focus is intentionally
    /// unset until the first real drag motion, so a click without drag does not
    /// create a one-cell selection.
    pub fn start(&mut self, col: usize, row: usize) {
        self.start_with_alt(col, row, false);
    }

    /// Starts a drag selection and records whether the initiating press had
    /// the Alt modifier set.
    pub fn start_with_alt(&mut self, col: usize, row: usize, last_press_had_alt: bool) {
        self.anchor = Some(SelectionPoint { col, row });
        self.focus = None;
        self.is_dragging = true;
        self.anchor_span = None;
        self.virtual_anchor_row = None;
        self.virtual_focus_row = None;
        self.scrolled_off_above.clear();
        self.scrolled_off_below.clear();
        self.scrolled_off_above_soft_wrap.clear();
        self.scrolled_off_below_soft_wrap.clear();
        self.last_press_had_alt = last_press_had_alt;
    }

    fn compare_points(a: SelectionPoint, b: SelectionPoint) -> std::cmp::Ordering {
        a.row.cmp(&b.row).then(a.col.cmp(&b.col))
    }

    /// Selects the word/same-class run at a canvas cell, as used by
    /// double-click selection. Returns `false` when the target is out of bounds
    /// or inside a noSelect region.
    pub fn select_word_at(&mut self, canvas: &Canvas, col: usize, row: usize) -> bool {
        let Some((lo_col, hi_col)) = canvas.word_bounds_at(col, row) else {
            return false;
        };
        self.start(lo_col, row);
        let lo = SelectionPoint { col: lo_col, row };
        let hi = SelectionPoint { col: hi_col, row };
        self.anchor = Some(lo);
        self.focus = Some(hi);
        self.is_dragging = true;
        self.anchor_span = Some(SelectionSpan {
            lo,
            hi,
            kind: SelectionSpanKind::Word,
        });
        true
    }

    /// Packed-screen variant of [`SelectionState::select_word_at`].
    pub fn select_word_at_packed(
        &mut self,
        screen: &CanvasPackedScreen,
        pools: &CanvasPackedCellPools,
        col: usize,
        row: usize,
    ) -> bool {
        let Some((lo_col, hi_col)) = screen.word_bounds_at(pools, col, row) else {
            return false;
        };
        self.start(lo_col, row);
        let lo = SelectionPoint { col: lo_col, row };
        let hi = SelectionPoint { col: hi_col, row };
        self.anchor = Some(lo);
        self.focus = Some(hi);
        self.is_dragging = true;
        self.anchor_span = Some(SelectionSpan {
            lo,
            hi,
            kind: SelectionSpanKind::Word,
        });
        true
    }

    /// Selects an entire row, as used by triple-click selection.
    pub fn select_line_at(&mut self, canvas: &Canvas, row: usize) -> bool {
        if row >= canvas.height() || canvas.width == 0 {
            return false;
        }
        self.start(0, row);
        let lo = SelectionPoint { col: 0, row };
        let hi = SelectionPoint {
            col: canvas.width - 1,
            row,
        };
        self.anchor = Some(lo);
        self.focus = Some(hi);
        self.is_dragging = true;
        self.anchor_span = Some(SelectionSpan {
            lo,
            hi,
            kind: SelectionSpanKind::Line,
        });
        true
    }

    /// Packed-screen variant of [`SelectionState::select_line_at`].
    pub fn select_line_at_packed(&mut self, screen: &CanvasPackedScreen, row: usize) -> bool {
        if row >= screen.height || screen.width == 0 {
            return false;
        }
        self.start(0, row);
        let lo = SelectionPoint { col: 0, row };
        let hi = SelectionPoint {
            col: screen.width - 1,
            row,
        };
        self.anchor = Some(lo);
        self.focus = Some(hi);
        self.is_dragging = true;
        self.anchor_span = Some(SelectionSpan {
            lo,
            hi,
            kind: SelectionSpanKind::Line,
        });
        true
    }

    /// Handles a double- or triple-click press.
    ///
    /// The press first seeds a char-mode selection; if word/line snapping finds
    /// nothing selectable (for example a double-click in a `noSelect` gutter),
    /// focus is set to the anchor so the click is treated as a selection press
    /// rather than falling through to normal click dispatch.
    pub fn start_multi_click(
        &mut self,
        canvas: &Canvas,
        col: usize,
        row: usize,
        count: SelectionClickCount,
    ) {
        self.start(col, row);
        match count {
            SelectionClickCount::Double => {
                self.select_word_at(canvas, col, row);
            }
            SelectionClickCount::Triple => {
                self.select_line_at(canvas, row);
            }
        }
        if self.focus.is_none() {
            self.focus = self.anchor;
        }
    }

    /// Packed-screen variant of [`SelectionState::start_multi_click`].
    pub fn start_multi_click_packed(
        &mut self,
        screen: &CanvasPackedScreen,
        pools: &CanvasPackedCellPools,
        col: usize,
        row: usize,
        count: SelectionClickCount,
    ) {
        self.start(col, row);
        match count {
            SelectionClickCount::Double => {
                self.select_word_at_packed(screen, pools, col, row);
            }
            SelectionClickCount::Triple => {
                self.select_line_at_packed(screen, row);
            }
        }
        if self.focus.is_none() {
            self.focus = self.anchor;
        }
    }

    /// Extends an active word/line selection to a new mouse position.
    ///
    /// Word mode falls back to the raw cell when the mouse is over noSelect or
    /// out of bounds, matching CC Ink's gutter-drag behavior.
    pub fn extend_span_selection(&mut self, canvas: &Canvas, col: usize, row: usize) {
        if !self.is_dragging {
            return;
        }
        let Some(span) = self.anchor_span else {
            return;
        };
        let (m_lo, m_hi) = match span.kind {
            SelectionSpanKind::Word => match canvas.word_bounds_at(col, row) {
                Some((lo, hi)) => (
                    SelectionPoint { col: lo, row },
                    SelectionPoint { col: hi, row },
                ),
                None => (SelectionPoint { col, row }, SelectionPoint { col, row }),
            },
            SelectionSpanKind::Line => {
                if canvas.width == 0 || canvas.height() == 0 {
                    return;
                }
                let row = row.min(canvas.height() - 1);
                (
                    SelectionPoint { col: 0, row },
                    SelectionPoint {
                        col: canvas.width - 1,
                        row,
                    },
                )
            }
        };

        if Self::compare_points(m_hi, span.lo) == std::cmp::Ordering::Less {
            self.anchor = Some(span.hi);
            self.focus = Some(m_lo);
        } else if Self::compare_points(m_lo, span.hi) == std::cmp::Ordering::Greater {
            self.anchor = Some(span.lo);
            self.focus = Some(m_hi);
        } else {
            self.anchor = Some(span.lo);
            self.focus = Some(span.hi);
        }
    }

    /// Packed-screen variant of [`SelectionState::extend_span_selection`].
    pub fn extend_span_selection_packed(
        &mut self,
        screen: &CanvasPackedScreen,
        pools: &CanvasPackedCellPools,
        col: usize,
        row: usize,
    ) {
        if !self.is_dragging {
            return;
        }
        let Some(span) = self.anchor_span else {
            return;
        };
        let (m_lo, m_hi) = match span.kind {
            SelectionSpanKind::Word => match screen.word_bounds_at(pools, col, row) {
                Some((lo, hi)) => (
                    SelectionPoint { col: lo, row },
                    SelectionPoint { col: hi, row },
                ),
                None => (SelectionPoint { col, row }, SelectionPoint { col, row }),
            },
            SelectionSpanKind::Line => {
                if screen.width == 0 || screen.height == 0 {
                    return;
                }
                let row = row.min(screen.height - 1);
                (
                    SelectionPoint { col: 0, row },
                    SelectionPoint {
                        col: screen.width - 1,
                        row,
                    },
                )
            }
        };

        if Self::compare_points(m_hi, span.lo) == std::cmp::Ordering::Less {
            self.anchor = Some(span.hi);
            self.focus = Some(m_lo);
        } else if Self::compare_points(m_lo, span.hi) == std::cmp::Ordering::Greater {
            self.anchor = Some(span.lo);
            self.focus = Some(m_hi);
        } else {
            self.anchor = Some(span.lo);
            self.focus = Some(span.hi);
        }
    }

    /// Moves the focus endpoint explicitly, as used by keyboard selection
    /// extension. This leaves the anchor fixed, clears word/line span mode,
    /// and preserves any scrolled-off row accumulators.
    pub fn move_focus(&mut self, col: usize, row: usize) {
        if self.focus.is_none() {
            return;
        }
        self.anchor_span = None;
        self.focus = Some(SelectionPoint { col, row });
        self.virtual_focus_row = None;
    }

    /// Moves the focus endpoint using terminal-style keyboard selection rules.
    ///
    /// Left/right wrap across row boundaries; up/down clamp at the viewport
    /// edges; lineStart/lineEnd move horizontally on the current row. Returns
    /// `true` when the focus changed.
    pub fn move_focus_by(
        &mut self,
        movement: SelectionFocusMove,
        width: usize,
        height: usize,
    ) -> bool {
        let Some(focus) = self.focus else {
            return false;
        };
        if width == 0 || height == 0 {
            return false;
        }
        let max_col = width - 1;
        let max_row = height - 1;
        let (mut col, mut row) = (focus.col.min(max_col), focus.row.min(max_row));
        match movement {
            SelectionFocusMove::Left => {
                if col > 0 {
                    col -= 1;
                } else if row > 0 {
                    col = max_col;
                    row -= 1;
                }
            }
            SelectionFocusMove::Right => {
                if col < max_col {
                    col += 1;
                } else if row < max_row {
                    col = 0;
                    row += 1;
                }
            }
            SelectionFocusMove::Up => {
                row = row.saturating_sub(1);
            }
            SelectionFocusMove::Down => {
                row = (row + 1).min(max_row);
            }
            SelectionFocusMove::LineStart => {
                col = 0;
            }
            SelectionFocusMove::LineEnd => {
                col = max_col;
            }
        }
        if col == focus.col && row == focus.row {
            return false;
        }
        self.move_focus(col, row);
        true
    }

    /// Updates the focus endpoint while dragging.
    pub fn update(&mut self, col: usize, row: usize) {
        if !self.is_dragging {
            return;
        }
        if self.focus.is_none()
            && self
                .anchor
                .is_some_and(|anchor| anchor.col == col && anchor.row == row)
        {
            return;
        }
        self.focus = Some(SelectionPoint { col, row });
        self.anchor_span = None;
        self.virtual_focus_row = None;
    }

    /// Finishes the drag while keeping the selection highlight/copy range.
    pub fn finish(&mut self) {
        self.is_dragging = false;
    }

    /// Clears the selection and all captured off-screen rows.
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// Returns `true` when both anchor and focus are present.
    pub fn has_selection(&self) -> bool {
        self.anchor.is_some() && self.focus.is_some()
    }

    /// Returns whether no rows are currently captured outside the viewport.
    pub fn captured_rows_empty(&self) -> bool {
        self.scrolled_off_above.is_empty() && self.scrolled_off_below.is_empty()
    }

    /// Clears rows captured outside the viewport while keeping the on-screen
    /// anchor/focus. Used when drag autoscroll detects an opposite-edge
    /// reversal, matching CC Ink's accumulator reset before stopping.
    pub fn clear_captured_rows(&mut self) {
        self.scrolled_off_above.clear();
        self.scrolled_off_below.clear();
        self.scrolled_off_above_soft_wrap.clear();
        self.scrolled_off_below_soft_wrap.clear();
    }

    /// Returns the current inclusive selection range, if active.
    pub fn range(&self) -> Option<SelectionRange> {
        Some(SelectionRange::new(self.anchor?, self.focus?))
    }

    /// Returns whether a cell is within the current selection bounds.
    pub fn is_cell_selected(&self, col: usize, row: usize) -> bool {
        self.range()
            .is_some_and(|range| range.contains(SelectionPoint { col, row }))
    }

    /// Applies a post-render overlay for this selection to a canvas.
    ///
    /// This is the stateful counterpart to CC Ink's
    /// `applySelectionOverlay(screen, selection, stylePool)`: callers do not
    /// need to manually extract the range, and the canvas records damage for the
    /// affected cells.
    pub fn apply_overlay(&self, canvas: &mut Canvas, overlay: StyleOverlay) -> bool {
        let Some(range) = self.range() else {
            return false;
        };
        canvas.apply_selection_overlay(range, overlay)
    }

    /// Packed-screen variant of [`SelectionState::apply_overlay`].
    pub fn apply_overlay_packed(
        &self,
        screen: &mut CanvasPackedScreen,
        pools: &mut CanvasPackedCellPools,
        overlay: StyleOverlay,
    ) -> bool {
        let Some(range) = self.range() else {
            return false;
        };
        screen.apply_selection_overlay(pools, range, overlay)
    }

    fn join_row(lines: &mut Vec<String>, text: String, soft_wrap: bool) {
        if soft_wrap && !lines.is_empty() {
            lines.last_mut().unwrap().push_str(&text);
        } else {
            lines.push(text);
        }
    }

    /// Extracts selected text from `canvas`, including rows captured earlier by
    /// [`SelectionState::capture_scrolled_rows`].
    pub fn selected_text(&self, canvas: &Canvas) -> String {
        let Some(range) = self.range() else {
            return String::new();
        };
        let (start, end) = range.normalized();
        if canvas.width == 0 || canvas.height() == 0 || start.row >= canvas.height() {
            return String::new();
        }

        let mut lines = Vec::<String>::new();
        for (text, soft_wrap) in self
            .scrolled_off_above
            .iter()
            .cloned()
            .zip(self.scrolled_off_above_soft_wrap.iter().copied())
        {
            Self::join_row(&mut lines, text, soft_wrap);
        }

        let last_row = end.row.min(canvas.height() - 1);
        for row in start.row..=last_row {
            let col_start = if row == start.row { start.col } else { 0 };
            let col_end = if row == end.row {
                end.col.min(canvas.width - 1)
            } else {
                canvas.width - 1
            };
            let text = if col_start >= canvas.width || col_start > col_end {
                String::new()
            } else {
                canvas.extract_selected_row(row, col_start, col_end)
            };
            Self::join_row(&mut lines, text, canvas.soft_wrap_continuation(row) > 0);
        }

        for (text, soft_wrap) in self
            .scrolled_off_below
            .iter()
            .cloned()
            .zip(self.scrolled_off_below_soft_wrap.iter().copied())
        {
            Self::join_row(&mut lines, text, soft_wrap);
        }

        lines.join("\n")
    }

    /// Packed-screen variant of [`SelectionState::selected_text`].
    pub fn selected_text_packed(
        &self,
        screen: &CanvasPackedScreen,
        pools: &CanvasPackedCellPools,
    ) -> String {
        let Some(range) = self.range() else {
            return String::new();
        };
        let (start, end) = range.normalized();
        if screen.width == 0 || screen.height == 0 || start.row >= screen.height {
            return String::new();
        }

        let mut lines = Vec::<String>::new();
        for (text, soft_wrap) in self
            .scrolled_off_above
            .iter()
            .cloned()
            .zip(self.scrolled_off_above_soft_wrap.iter().copied())
        {
            Self::join_row(&mut lines, text, soft_wrap);
        }

        let last_row = end.row.min(screen.height - 1);
        for row in start.row..=last_row {
            let col_start = if row == start.row { start.col } else { 0 };
            let col_end = if row == end.row {
                end.col.min(screen.width - 1)
            } else {
                screen.width - 1
            };
            let text = if col_start >= screen.width || col_start > col_end {
                String::new()
            } else {
                screen.extract_selected_row(pools, row, col_start, col_end)
            };
            Self::join_row(&mut lines, text, screen.soft_wrap_continuation(row) > 0);
        }

        for (text, soft_wrap) in self
            .scrolled_off_below
            .iter()
            .cloned()
            .zip(self.scrolled_off_below_soft_wrap.iter().copied())
        {
            Self::join_row(&mut lines, text, soft_wrap);
        }

        lines.join("\n")
    }

    /// Captures selected rows before a scroll operation overwrites or shifts
    /// them out of the visible canvas.
    pub fn capture_scrolled_rows(
        &mut self,
        canvas: &Canvas,
        first_row: usize,
        last_row: usize,
        side: SelectionCaptureSide,
    ) {
        let Some(range) = self.range() else {
            return;
        };
        if canvas.width == 0 || canvas.height() == 0 || first_row > last_row {
            return;
        }
        let (start, end) = range.normalized();
        let lo = first_row.max(start.row);
        let hi = last_row.min(end.row).min(canvas.height() - 1);
        if lo > hi {
            return;
        }

        let mut captured = Vec::new();
        let mut captured_soft_wrap = Vec::new();
        for row in lo..=hi {
            let col_start = if row == start.row { start.col } else { 0 };
            let col_end = if row == end.row {
                end.col.min(canvas.width - 1)
            } else {
                canvas.width - 1
            };
            captured.push(canvas.extract_selected_row(row, col_start, col_end));
            captured_soft_wrap.push(canvas.soft_wrap_continuation(row) > 0);
        }

        match side {
            SelectionCaptureSide::Above => {
                self.scrolled_off_above.extend(captured);
                self.scrolled_off_above_soft_wrap.extend(captured_soft_wrap);
                if self.anchor.is_some_and(|anchor| anchor.row == start.row) && lo == start.row {
                    if let Some(anchor) = &mut self.anchor {
                        anchor.col = 0;
                    }
                    if let Some(span) = &mut self.anchor_span {
                        span.lo.col = 0;
                        span.hi.col = canvas.width.saturating_sub(1);
                    }
                }
            }
            SelectionCaptureSide::Below => {
                self.scrolled_off_below.splice(0..0, captured);
                self.scrolled_off_below_soft_wrap
                    .splice(0..0, captured_soft_wrap);
                if self.anchor.is_some_and(|anchor| anchor.row == end.row) && hi == end.row {
                    if let Some(anchor) = &mut self.anchor {
                        anchor.col = canvas.width.saturating_sub(1);
                    }
                    if let Some(span) = &mut self.anchor_span {
                        span.lo.col = 0;
                        span.hi.col = canvas.width.saturating_sub(1);
                    }
                }
            }
        }
    }

    /// Packed-screen variant of [`SelectionState::capture_scrolled_rows`].
    pub fn capture_scrolled_rows_packed(
        &mut self,
        screen: &CanvasPackedScreen,
        pools: &CanvasPackedCellPools,
        first_row: usize,
        last_row: usize,
        side: SelectionCaptureSide,
    ) {
        let Some(range) = self.range() else {
            return;
        };
        if screen.width == 0 || screen.height == 0 || first_row > last_row {
            return;
        }
        let (start, end) = range.normalized();
        let lo = first_row.max(start.row);
        let hi = last_row.min(end.row).min(screen.height - 1);
        if lo > hi {
            return;
        }

        let mut captured = Vec::new();
        let mut captured_soft_wrap = Vec::new();
        for row in lo..=hi {
            let col_start = if row == start.row { start.col } else { 0 };
            let col_end = if row == end.row {
                end.col.min(screen.width - 1)
            } else {
                screen.width - 1
            };
            captured.push(screen.extract_selected_row(pools, row, col_start, col_end));
            captured_soft_wrap.push(screen.soft_wrap_continuation(row) > 0);
        }

        match side {
            SelectionCaptureSide::Above => {
                self.scrolled_off_above.extend(captured);
                self.scrolled_off_above_soft_wrap.extend(captured_soft_wrap);
                if self.anchor.is_some_and(|anchor| anchor.row == start.row) && lo == start.row {
                    if let Some(anchor) = &mut self.anchor {
                        anchor.col = 0;
                    }
                    if let Some(span) = &mut self.anchor_span {
                        span.lo.col = 0;
                        span.hi.col = screen.width.saturating_sub(1);
                    }
                }
            }
            SelectionCaptureSide::Below => {
                self.scrolled_off_below.splice(0..0, captured);
                self.scrolled_off_below_soft_wrap
                    .splice(0..0, captured_soft_wrap);
                if self.anchor.is_some_and(|anchor| anchor.row == end.row) && hi == end.row {
                    if let Some(anchor) = &mut self.anchor {
                        anchor.col = screen.width.saturating_sub(1);
                    }
                    if let Some(span) = &mut self.anchor_span {
                        span.lo.col = 0;
                        span.hi.col = screen.width.saturating_sub(1);
                    }
                }
            }
        }
    }

    /// Shifts only the anchor row after drag-to-scroll. Focus remains at the
    /// live mouse position, while the anchor follows the text under it.
    pub fn shift_anchor(&mut self, delta: isize, min_row: usize, max_row: usize) {
        let Some(anchor) = self.anchor else {
            return;
        };
        let min_row_i = min_row as isize;
        let max_row_i = max_row as isize;
        let raw = self.virtual_anchor_row.unwrap_or(anchor.row as isize) + delta;
        self.anchor = Some(SelectionPoint {
            col: anchor.col,
            row: raw.clamp(min_row_i, max_row_i) as usize,
        });
        self.virtual_anchor_row = if raw < min_row_i || raw > max_row_i {
            Some(raw)
        } else {
            None
        };
        if let Some(span) = &mut self.anchor_span {
            let shift = |point: SelectionPoint| SelectionPoint {
                col: point.col,
                row: (point.row as isize + delta).clamp(min_row_i, max_row_i) as usize,
            };
            span.lo = shift(span.lo);
            span.hi = shift(span.hi);
        }
    }

    /// Shifts anchor and focus during streaming follow-scroll. Returns `true`
    /// when the selection was cleared because it scrolled entirely above the
    /// viewport.
    pub fn shift_for_follow(&mut self, delta: isize, min_row: usize, max_row: usize) -> bool {
        let Some(anchor) = self.anchor else {
            return false;
        };
        let raw_anchor = self.virtual_anchor_row.unwrap_or(anchor.row as isize) + delta;
        let raw_focus = self
            .focus
            .map(|focus| self.virtual_focus_row.unwrap_or(focus.row as isize) + delta);
        let min_row_i = min_row as isize;
        let max_row_i = max_row as isize;
        if raw_anchor < min_row_i && raw_focus.is_some_and(|row| row < min_row_i) {
            self.clear();
            return true;
        }

        self.anchor = Some(SelectionPoint {
            col: anchor.col,
            row: raw_anchor.clamp(min_row_i, max_row_i) as usize,
        });
        self.virtual_anchor_row = if raw_anchor < min_row_i || raw_anchor > max_row_i {
            Some(raw_anchor)
        } else {
            None
        };
        if let (Some(focus), Some(raw_focus)) = (self.focus, raw_focus) {
            self.focus = Some(SelectionPoint {
                col: focus.col,
                row: raw_focus.clamp(min_row_i, max_row_i) as usize,
            });
            self.virtual_focus_row = if raw_focus < min_row_i || raw_focus > max_row_i {
                Some(raw_focus)
            } else {
                None
            };
        }

        if let Some(span) = &mut self.anchor_span {
            let shift = |point: SelectionPoint| SelectionPoint {
                col: point.col,
                row: (point.row as isize + delta).clamp(min_row_i, max_row_i) as usize,
            };
            span.lo = shift(span.lo);
            span.hi = shift(span.hi);
        }
        false
    }

    /// Shifts the current anchor/focus rows after the canvas content scrolls.
    /// Points that leave the viewport are clamped to the corresponding edge.
    /// Virtual rows are retained so reverse scrolls can restore the true
    /// position and trim stale scrolled-off row accumulators.
    pub fn shift_rows(&mut self, delta: isize, min_row: usize, max_row: usize, width: usize) {
        let (Some(anchor), Some(focus)) = (self.anchor, self.focus) else {
            return;
        };
        let min_row_i = min_row as isize;
        let max_row_i = max_row as isize;
        let old_anchor = self.virtual_anchor_row.unwrap_or(anchor.row as isize);
        let old_focus = self.virtual_focus_row.unwrap_or(focus.row as isize);
        let raw_anchor = old_anchor + delta;
        let raw_focus = old_focus + delta;
        if (raw_anchor < min_row_i && raw_focus < min_row_i)
            || (raw_anchor > max_row_i && raw_focus > max_row_i)
        {
            self.clear();
            return;
        }

        let old_min = old_anchor.min(old_focus);
        let old_max = old_anchor.max(old_focus);
        let new_min = raw_anchor.min(raw_focus);
        let new_max = raw_anchor.max(raw_focus);
        let old_above_debt = (min_row_i - old_min).max(0) as usize;
        let old_below_debt = (old_max - max_row_i).max(0) as usize;
        let new_above_debt = (min_row_i - new_min).max(0) as usize;
        let new_below_debt = (new_max - max_row_i).max(0) as usize;

        if new_above_debt < old_above_debt {
            let drop = old_above_debt - new_above_debt;
            let keep = self.scrolled_off_above.len().saturating_sub(drop);
            self.scrolled_off_above.truncate(keep);
            self.scrolled_off_above_soft_wrap.truncate(keep);
        }
        if new_below_debt < old_below_debt {
            let drop = (old_below_debt - new_below_debt).min(self.scrolled_off_below.len());
            self.scrolled_off_below.drain(0..drop);
            self.scrolled_off_below_soft_wrap.drain(0..drop);
        }
        if self.scrolled_off_above.len() > new_above_debt {
            let drop = self.scrolled_off_above.len() - new_above_debt;
            self.scrolled_off_above.drain(0..drop);
            self.scrolled_off_above_soft_wrap.drain(0..drop);
        }
        if self.scrolled_off_below.len() > new_below_debt {
            self.scrolled_off_below.truncate(new_below_debt);
            self.scrolled_off_below_soft_wrap.truncate(new_below_debt);
        }

        let clamp_point = |point: SelectionPoint, raw_row: isize| {
            if raw_row < min_row_i {
                SelectionPoint {
                    col: 0,
                    row: min_row,
                }
            } else if raw_row > max_row_i {
                SelectionPoint {
                    col: width.saturating_sub(1),
                    row: max_row,
                }
            } else {
                SelectionPoint {
                    col: point.col,
                    row: raw_row as usize,
                }
            }
        };
        self.anchor = Some(clamp_point(anchor, raw_anchor));
        self.focus = Some(clamp_point(focus, raw_focus));
        self.virtual_anchor_row = if raw_anchor < min_row_i || raw_anchor > max_row_i {
            Some(raw_anchor)
        } else {
            None
        };
        self.virtual_focus_row = if raw_focus < min_row_i || raw_focus > max_row_i {
            Some(raw_focus)
        } else {
            None
        };

        if let Some(span) = &mut self.anchor_span {
            let shift = |point: SelectionPoint| {
                let raw_row = point.row as isize + delta;
                if raw_row < min_row_i {
                    SelectionPoint {
                        col: 0,
                        row: min_row,
                    }
                } else if raw_row > max_row_i {
                    SelectionPoint {
                        col: width.saturating_sub(1),
                        row: max_row,
                    }
                } else {
                    SelectionPoint {
                        col: point.col,
                        row: raw_row as usize,
                    }
                }
            };
            span.lo = shift(span.lo);
            span.hi = shift(span.hi);
        }
    }
}

/// Result of handling a fullscreen left-mouse press.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SelectionPressOutcome {
    /// How the press was classified by the multi-click tracker.
    pub kind: SelectionMousePressKind,
    /// Whether an in-progress drag had to be finished before this press was
    /// applied. This is the CC Ink lost-release fallback: a fresh press while
    /// `isDragging` is true means the previous release was likely dropped.
    pub finished_previous_drag: bool,
    /// Whether callers should cancel a pending deferred hyperlink open from a
    /// previous single click. CC Ink cancels that timer as soon as a double- or
    /// triple-click press is recognized.
    pub cancel_pending_hyperlink: bool,
}

/// Result of handling no-button mouse motion in fullscreen mode.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SelectionHoverOutcome {
    /// Whether a drag was active and got finished as lost-release recovery.
    pub finished_drag: bool,
    /// A deduplicated hover target. Repeated no-button motion at the same cell
    /// returns `None`, matching CC Ink's `lastHoverCol/lastHoverRow` guard.
    pub hover: Option<SelectionPoint>,
}

/// Result of translating a selection for an app-level scroll jump.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SelectionScrollOutcome {
    /// Whether the selection was within the scroll viewport and got translated.
    pub translated: bool,
    /// Whether the translation moved the whole selection out of the viewport and
    /// cleared it.
    pub cleared: bool,
}

/// Result of handling a fullscreen mouse release.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SelectionReleaseOutcome {
    /// Whether a drag/press was active and got finished by this release.
    pub was_dragging: bool,
    /// A single-click target when the press/release did not create a text
    /// selection. Callers dispatch normal click handlers at this point.
    pub click: Option<SelectionPoint>,
    /// Hyperlink target under the release cell, populated only for an
    /// unconsumed single click. Opening it is intentionally left to callers so
    /// they can defer/cancel on a following double-click like CC Ink does.
    pub hyperlink: Option<String>,
}

/// Small owner for fullscreen selection state plus CC Ink-style mouse press
/// lifecycle bookkeeping.
///
/// It intentionally does not depend on a concrete terminal event type. Callers
/// translate mouse events to `left_press` / `drag` / `release` and pass screen
/// coordinates in 0-indexed cell space. That keeps the screen-buffer selection
/// model reusable for terminal backends and tests while matching the behavior
/// implemented by CC Ink's `App.tsx` mouse handler.
#[derive(Clone, Debug, Default)]
pub struct SelectionController {
    pub(super) selection: SelectionState,
    click_tracker: SelectionClickTracker,
    last_hover: Option<SelectionPoint>,
    pub(super) last_drag_scroll_dir: Option<SelectionDragScrollDirection>,
    copy_on_select_copied: bool,
}

impl SelectionController {
    /// Creates an empty selection controller.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the underlying selection state.
    pub fn selection(&self) -> &SelectionState {
        &self.selection
    }

    /// Returns the underlying selection state mutably for advanced callers that
    /// need to apply scroll shifts/captures directly.
    pub fn selection_mut(&mut self) -> &mut SelectionState {
        &mut self.selection
    }

    /// Returns whether text is currently selected.
    pub fn has_selection(&self) -> bool {
        self.selection.has_selection()
    }

    /// Clears selection and resets multi-click bookkeeping.
    pub fn clear(&mut self) {
        self.selection.clear();
        self.click_tracker.reset();
        self.last_hover = None;
        self.last_drag_scroll_dir = None;
        self.copy_on_select_copied = false;
    }

    /// Handles a non-left mouse press. CC Ink uses this to break the multi-click
    /// chain without otherwise touching the active selection.
    pub fn handle_non_left_press(&mut self) {
        self.click_tracker.reset();
    }

    /// Handles a fresh left-button press at a screen cell.
    ///
    /// Single clicks start char-mode selection and record `last_press_had_alt`.
    /// Double/triple clicks select word/line immediately on press. If a previous
    /// drag is still active, it is finished first so copy-on-select callers can
    /// observe `finished_previous_drag` and copy before the new press replaces
    /// the selection.
    pub fn handle_left_press(
        &mut self,
        canvas: &Canvas,
        col: usize,
        row: usize,
        now_ms: u64,
        alt: bool,
    ) -> SelectionPressOutcome {
        let finished_previous_drag = self.finish_if_dragging();
        self.copy_on_select_copied = false;
        let kind = self.click_tracker.record_press(col, row, now_ms);
        match kind.click_count() {
            Some(count) => self.selection.start_multi_click(canvas, col, row, count),
            None => self.selection.start_with_alt(col, row, alt),
        }
        SelectionPressOutcome {
            kind,
            finished_previous_drag,
            cancel_pending_hyperlink: kind.click_count().is_some(),
        }
    }

    /// Packed-screen variant of [`SelectionController::handle_left_press`].
    pub fn handle_left_press_packed(
        &mut self,
        screen: &CanvasPackedScreen,
        pools: &CanvasPackedCellPools,
        col: usize,
        row: usize,
        now_ms: u64,
        alt: bool,
    ) -> SelectionPressOutcome {
        let finished_previous_drag = self.finish_if_dragging();
        self.copy_on_select_copied = false;
        let kind = self.click_tracker.record_press(col, row, now_ms);
        match kind.click_count() {
            Some(count) => self
                .selection
                .start_multi_click_packed(screen, pools, col, row, count),
            None => self.selection.start_with_alt(col, row, alt),
        }
        SelectionPressOutcome {
            kind,
            finished_previous_drag,
            cancel_pending_hyperlink: kind.click_count().is_some(),
        }
    }

    /// Returns whether no-button motion at `col,row` would mutate selection
    /// controller state. App-level owners use this to avoid waking/rendering on
    /// duplicate hover events that CC Ink's `subscribeToSelectionChange` would
    /// not report as selection mutations.
    pub fn no_button_motion_would_change(&self, col: usize, row: usize) -> bool {
        self.selection.is_dragging() || self.last_hover != Some(SelectionPoint { col, row })
    }

    /// Handles no-button motion (mode 1003 hover) in fullscreen mode.
    ///
    /// If a drag is still active, this finishes it as lost-release recovery;
    /// then it returns a hover target only when the cell changed.
    pub fn handle_no_button_motion(&mut self, col: usize, row: usize) -> SelectionHoverOutcome {
        let finished_drag = self.finish_if_dragging();
        let point = SelectionPoint { col, row };
        let hover = if self.last_hover == Some(point) {
            None
        } else {
            self.last_hover = Some(point);
            Some(point)
        };
        SelectionHoverOutcome {
            finished_drag,
            hover,
        }
    }

    /// Handles terminal focus loss while a drag may still be active. Some
    /// emulators drop release events when the pointer leaves the window; CC Ink
    /// finishes the selection on focus-out to stop drag-to-scroll timers and let
    /// copy-on-select observe the final range.
    pub fn handle_focus_lost(&mut self) -> bool {
        self.finish_if_dragging()
    }

    /// Translates selection during sticky/auto-follow scrolling.
    ///
    /// `delta` is the number of rows by which the scrollbox followed new
    /// content at the bottom; content moves up, so rows leaving the top are
    /// captured and selection coordinates shift upward. During an active drag
    /// only the anchor follows the text while focus stays at the mouse. After
    /// release, both endpoints follow as a block. A released selection whose
    /// focus is outside the scroll viewport is left unchanged, matching CC
    /// Ink's guard against teleporting footer/header selections into content.
    pub fn translate_for_follow_scroll(
        &mut self,
        canvas: &Canvas,
        delta: usize,
        viewport_top: usize,
        viewport_bottom: usize,
    ) -> SelectionScrollOutcome {
        if delta == 0 || viewport_top > viewport_bottom || canvas.width() == 0 {
            return SelectionScrollOutcome::default();
        }
        let Some(anchor) = self.selection.anchor() else {
            return SelectionScrollOutcome::default();
        };
        if anchor.row < viewport_top || anchor.row > viewport_bottom {
            return SelectionScrollOutcome::default();
        }
        let focus = self.selection.focus();
        if !self.selection.is_dragging()
            && focus.is_some_and(|focus| focus.row < viewport_top || focus.row > viewport_bottom)
        {
            return SelectionScrollOutcome::default();
        }

        let viewport_height = viewport_bottom - viewport_top + 1;
        let capture_rows = delta.min(viewport_height);
        if self.selection.has_selection() && capture_rows > 0 {
            self.selection.capture_scrolled_rows(
                canvas,
                viewport_top,
                viewport_top + capture_rows - 1,
                SelectionCaptureSide::Above,
            );
        }

        if self.selection.is_dragging() {
            self.selection
                .shift_anchor(-(delta as isize), viewport_top, viewport_bottom);
            SelectionScrollOutcome {
                translated: true,
                cleared: false,
            }
        } else {
            let had_selection = self.selection.has_selection();
            let cleared =
                self.selection
                    .shift_for_follow(-(delta as isize), viewport_top, viewport_bottom);
            SelectionScrollOutcome {
                translated: true,
                cleared: had_selection && cleared,
            }
        }
    }

    /// Packed-screen variant of [`SelectionController::translate_for_follow_scroll`].
    pub fn translate_for_follow_scroll_packed(
        &mut self,
        screen: &CanvasPackedScreen,
        pools: &CanvasPackedCellPools,
        delta: usize,
        viewport_top: usize,
        viewport_bottom: usize,
    ) -> SelectionScrollOutcome {
        if delta == 0 || viewport_top > viewport_bottom || screen.width == 0 {
            return SelectionScrollOutcome::default();
        }
        let Some(anchor) = self.selection.anchor() else {
            return SelectionScrollOutcome::default();
        };
        if anchor.row < viewport_top || anchor.row > viewport_bottom {
            return SelectionScrollOutcome::default();
        }
        let focus = self.selection.focus();
        if !self.selection.is_dragging()
            && focus.is_some_and(|focus| focus.row < viewport_top || focus.row > viewport_bottom)
        {
            return SelectionScrollOutcome::default();
        }

        let viewport_height = viewport_bottom - viewport_top + 1;
        let capture_rows = delta.min(viewport_height);
        if self.selection.has_selection() && capture_rows > 0 {
            self.selection.capture_scrolled_rows_packed(
                screen,
                pools,
                viewport_top,
                viewport_top + capture_rows - 1,
                SelectionCaptureSide::Above,
            );
        }

        if self.selection.is_dragging() {
            self.selection
                .shift_anchor(-(delta as isize), viewport_top, viewport_bottom);
            SelectionScrollOutcome {
                translated: true,
                cleared: false,
            }
        } else {
            let had_selection = self.selection.has_selection();
            let cleared =
                self.selection
                    .shift_for_follow(-(delta as isize), viewport_top, viewport_bottom);
            SelectionScrollOutcome {
                translated: true,
                cleared: had_selection && cleared,
            }
        }
    }

    /// Translates an existing selection for a synchronous scroll jump.
    ///
    /// `delta > 0` means the scroll offset increased and content moved up;
    /// rows leaving the top are captured before the caller mutates the canvas.
    /// `delta < 0` means content moved down and rows leaving the bottom are
    /// captured. Both anchor and focus must be inside the scroll viewport; a
    /// selection straddling static footer/header content is intentionally left
    /// unchanged to avoid teleporting the static endpoint into the scrollbox.
    pub fn translate_for_scroll_jump(
        &mut self,
        canvas: &Canvas,
        delta: isize,
        viewport_top: usize,
        viewport_bottom: usize,
    ) -> SelectionScrollOutcome {
        if delta == 0 || viewport_top > viewport_bottom || canvas.width() == 0 {
            return SelectionScrollOutcome::default();
        }
        let (Some(anchor), Some(focus)) = (self.selection.anchor(), self.selection.focus()) else {
            return SelectionScrollOutcome::default();
        };
        if anchor.row < viewport_top
            || anchor.row > viewport_bottom
            || focus.row < viewport_top
            || focus.row > viewport_bottom
        {
            return SelectionScrollOutcome::default();
        }

        let had_selection = self.selection.has_selection();
        let viewport_height = viewport_bottom - viewport_top + 1;
        if delta > 0 {
            let actual = delta as usize;
            let capture_rows = actual.min(viewport_height);
            if had_selection && capture_rows > 0 {
                self.selection.capture_scrolled_rows(
                    canvas,
                    viewport_top,
                    viewport_top + capture_rows - 1,
                    SelectionCaptureSide::Above,
                );
            }
            self.selection
                .shift_rows(-delta, viewport_top, viewport_bottom, canvas.width());
        } else {
            let actual = (-delta) as usize;
            let capture_rows = actual.min(viewport_height);
            if had_selection && capture_rows > 0 {
                self.selection.capture_scrolled_rows(
                    canvas,
                    viewport_bottom - capture_rows + 1,
                    viewport_bottom,
                    SelectionCaptureSide::Below,
                );
            }
            self.selection.shift_rows(
                actual as isize,
                viewport_top,
                viewport_bottom,
                canvas.width(),
            );
        }

        SelectionScrollOutcome {
            translated: true,
            cleared: had_selection && !self.selection.has_selection(),
        }
    }

    /// Packed-screen variant of [`SelectionController::translate_for_scroll_jump`].
    pub fn translate_for_scroll_jump_packed(
        &mut self,
        screen: &CanvasPackedScreen,
        pools: &CanvasPackedCellPools,
        delta: isize,
        viewport_top: usize,
        viewport_bottom: usize,
    ) -> SelectionScrollOutcome {
        if delta == 0 || viewport_top > viewport_bottom || screen.width == 0 {
            return SelectionScrollOutcome::default();
        }
        let (Some(anchor), Some(focus)) = (self.selection.anchor(), self.selection.focus()) else {
            return SelectionScrollOutcome::default();
        };
        if anchor.row < viewport_top
            || anchor.row > viewport_bottom
            || focus.row < viewport_top
            || focus.row > viewport_bottom
        {
            return SelectionScrollOutcome::default();
        }

        let had_selection = self.selection.has_selection();
        let viewport_height = viewport_bottom - viewport_top + 1;
        if delta > 0 {
            let actual = delta as usize;
            let capture_rows = actual.min(viewport_height);
            if had_selection && capture_rows > 0 {
                self.selection.capture_scrolled_rows_packed(
                    screen,
                    pools,
                    viewport_top,
                    viewport_top + capture_rows - 1,
                    SelectionCaptureSide::Above,
                );
            }
            self.selection
                .shift_rows(-delta, viewport_top, viewport_bottom, screen.width);
        } else {
            let actual = (-delta) as usize;
            let capture_rows = actual.min(viewport_height);
            if had_selection && capture_rows > 0 {
                self.selection.capture_scrolled_rows_packed(
                    screen,
                    pools,
                    viewport_bottom - capture_rows + 1,
                    viewport_bottom,
                    SelectionCaptureSide::Below,
                );
            }
            self.selection
                .shift_rows(actual as isize, viewport_top, viewport_bottom, screen.width);
        }

        SelectionScrollOutcome {
            translated: true,
            cleared: had_selection && !self.selection.has_selection(),
        }
    }

    /// Computes drag autoscroll direction relative to a scroll viewport.
    ///
    /// This follows CC Ink's `dragScrollDirection(...)`: a fresh autoscroll can
    /// start only when the anchor is inside the scrollbox, but once rows have
    /// been captured the same direction may resume even if the anchor is clamped
    /// to an edge. Moving focus to the opposite edge clears captured rows and
    /// returns `None`, preventing duplicated copied text on blocked reversals.
    pub fn drag_scroll_direction(
        &mut self,
        viewport_top: usize,
        viewport_bottom: usize,
    ) -> Option<SelectionDragScrollDirection> {
        if viewport_top > viewport_bottom {
            self.last_drag_scroll_dir = None;
            return None;
        }
        let (Some(anchor), Some(focus)) = (self.selection.anchor(), self.selection.focus()) else {
            self.last_drag_scroll_dir = None;
            return None;
        };
        if !self.selection.is_dragging() {
            self.last_drag_scroll_dir = None;
            return None;
        }
        if self.selection.captured_rows_empty() {
            self.last_drag_scroll_dir = None;
        }

        let want = if focus.row < viewport_top {
            Some(SelectionDragScrollDirection::Up)
        } else if focus.row > viewport_bottom {
            Some(SelectionDragScrollDirection::Down)
        } else {
            None
        };

        if let Some(active) = self.last_drag_scroll_dir {
            if want == Some(active) {
                return want;
            }
            if want.is_some() {
                self.selection.clear_captured_rows();
                self.last_drag_scroll_dir = None;
            }
            return None;
        }

        if anchor.row < viewport_top || anchor.row > viewport_bottom {
            return None;
        }
        if let Some(direction) = want {
            self.last_drag_scroll_dir = Some(direction);
        }
        want
    }

    /// Captures rows and shifts the anchor for one drag-autoscroll tick.
    ///
    /// `direction` is the result of [`SelectionController::drag_scroll_direction`].
    /// `actual_lines` should be clamped by the caller to the real scroll amount
    /// available at the boundary. This method pairs capture+shift so copied text
    /// and visible highlight stay in sync.
    pub fn translate_for_drag_autoscroll(
        &mut self,
        canvas: &Canvas,
        direction: SelectionDragScrollDirection,
        actual_lines: usize,
        viewport_top: usize,
        viewport_bottom: usize,
    ) -> SelectionScrollOutcome {
        if actual_lines == 0 || viewport_top > viewport_bottom || canvas.width() == 0 {
            return SelectionScrollOutcome::default();
        }
        let Some(_) = self.selection.anchor() else {
            return SelectionScrollOutcome::default();
        };
        let viewport_height = viewport_bottom - viewport_top + 1;
        let lines = actual_lines.min(viewport_height);
        if self.selection.has_selection() {
            match direction {
                SelectionDragScrollDirection::Up => {
                    self.selection.capture_scrolled_rows(
                        canvas,
                        viewport_bottom - lines + 1,
                        viewport_bottom,
                        SelectionCaptureSide::Below,
                    );
                }
                SelectionDragScrollDirection::Down => {
                    self.selection.capture_scrolled_rows(
                        canvas,
                        viewport_top,
                        viewport_top + lines - 1,
                        SelectionCaptureSide::Above,
                    );
                }
            }
        }

        match direction {
            SelectionDragScrollDirection::Up => {
                self.selection
                    .shift_anchor(lines as isize, 0, viewport_bottom);
            }
            SelectionDragScrollDirection::Down => {
                self.selection
                    .shift_anchor(-(lines as isize), viewport_top, viewport_bottom);
            }
        }
        self.last_drag_scroll_dir = Some(direction);
        SelectionScrollOutcome {
            translated: true,
            cleared: false,
        }
    }

    /// Packed-screen variant of [`SelectionController::translate_for_drag_autoscroll`].
    pub fn translate_for_drag_autoscroll_packed(
        &mut self,
        screen: &CanvasPackedScreen,
        pools: &CanvasPackedCellPools,
        direction: SelectionDragScrollDirection,
        actual_lines: usize,
        viewport_top: usize,
        viewport_bottom: usize,
    ) -> SelectionScrollOutcome {
        if actual_lines == 0 || viewport_top > viewport_bottom || screen.width == 0 {
            return SelectionScrollOutcome::default();
        }
        let Some(_) = self.selection.anchor() else {
            return SelectionScrollOutcome::default();
        };
        let viewport_height = viewport_bottom - viewport_top + 1;
        let lines = actual_lines.min(viewport_height);
        if self.selection.has_selection() {
            match direction {
                SelectionDragScrollDirection::Up => {
                    self.selection.capture_scrolled_rows_packed(
                        screen,
                        pools,
                        viewport_bottom - lines + 1,
                        viewport_bottom,
                        SelectionCaptureSide::Below,
                    );
                }
                SelectionDragScrollDirection::Down => {
                    self.selection.capture_scrolled_rows_packed(
                        screen,
                        pools,
                        viewport_top,
                        viewport_top + lines - 1,
                        SelectionCaptureSide::Above,
                    );
                }
            }
        }

        match direction {
            SelectionDragScrollDirection::Up => {
                self.selection
                    .shift_anchor(lines as isize, 0, viewport_bottom);
            }
            SelectionDragScrollDirection::Down => {
                self.selection
                    .shift_anchor(-(lines as isize), viewport_top, viewport_bottom);
            }
        }
        self.last_drag_scroll_dir = Some(direction);
        SelectionScrollOutcome {
            translated: true,
            cleared: false,
        }
    }

    /// Handles mouse drag motion, extending by word/line if the current
    /// selection came from a multi-click, otherwise moving the raw focus cell.
    pub fn handle_drag(&mut self, canvas: &Canvas, col: usize, row: usize) {
        self.copy_on_select_copied = false;
        if self.selection.anchor_span.is_some() {
            self.selection.extend_span_selection(canvas, col, row);
        } else {
            self.selection.update(col, row);
        }
    }

    /// Packed-screen variant of [`SelectionController::handle_drag`].
    pub fn handle_drag_packed(
        &mut self,
        screen: &CanvasPackedScreen,
        pools: &CanvasPackedCellPools,
        col: usize,
        row: usize,
    ) {
        self.copy_on_select_copied = false;
        if self.selection.anchor_span.is_some() {
            self.selection
                .extend_span_selection_packed(screen, pools, col, row);
        } else {
            self.selection.update(col, row);
        }
    }

    /// Finishes a mouse drag/release while preserving the selected range.
    /// Returns `true` if a drag had been active.
    pub fn handle_release(&mut self) -> bool {
        self.finish_if_dragging()
    }

    /// Finishes a release and classifies the click/link fallback.
    ///
    /// This mirrors the release tail of CC Ink's `handleMouseEvent(...)`: if a
    /// press/release did not become a text selection, it is a normal click; if
    /// that click was not consumed by app-level handlers, fullscreen mouse
    /// tracking must emulate terminal hyperlink lookup from the screen buffer.
    pub fn handle_release_at(
        &mut self,
        canvas: &Canvas,
        col: usize,
        row: usize,
        click_consumed: bool,
    ) -> SelectionReleaseOutcome {
        let was_dragging = self.finish_if_dragging();
        let click = if !self.selection.has_selection() {
            self.selection.anchor().map(|_| SelectionPoint { col, row })
        } else {
            None
        };
        let hyperlink = if click.is_some() && !click_consumed {
            canvas.hyperlink_at(col, row)
        } else {
            None
        };
        SelectionReleaseOutcome {
            was_dragging,
            click,
            hyperlink,
        }
    }

    /// Packed-screen variant of [`SelectionController::handle_release_at`].
    pub fn handle_release_at_packed(
        &mut self,
        screen: &CanvasPackedScreen,
        pools: &CanvasPackedCellPools,
        col: usize,
        row: usize,
        click_consumed: bool,
    ) -> SelectionReleaseOutcome {
        let was_dragging = self.finish_if_dragging();
        let click = if !self.selection.has_selection() {
            self.selection.anchor().map(|_| SelectionPoint { col, row })
        } else {
            None
        };
        let hyperlink = if click.is_some() && !click_consumed {
            screen.hyperlink_at(pools, col, row)
        } else {
            None
        };
        SelectionReleaseOutcome {
            was_dragging,
            click,
            hyperlink,
        }
    }

    /// Finishes the active drag if one is in progress. This is used for
    /// focus-loss/no-button-motion lost-release recovery.
    pub fn finish_if_dragging(&mut self) -> bool {
        let was_dragging = self.selection.is_dragging();
        if was_dragging {
            self.selection.finish();
            // CC Ink resets drag-autoscroll continuation state on drag finish.
            // Otherwise a later drag could inherit a stale same-direction
            // bypass after a release/focus-loss/no-button lost-release repair.
            self.last_drag_scroll_dir = None;
        }
        was_dragging
    }

    /// Returns selected text without clearing the highlight.
    pub fn selected_text(&self, canvas: &Canvas) -> String {
        self.selection.selected_text(canvas)
    }

    /// Packed-screen variant of [`SelectionController::selected_text`].
    pub fn selected_text_packed(
        &self,
        screen: &CanvasPackedScreen,
        pools: &CanvasPackedCellPools,
    ) -> String {
        self.selection.selected_text_packed(screen, pools)
    }

    /// Returns whether [`SelectionController::copy_on_select_text`] would need
    /// to mutate copy-on-select bookkeeping.
    ///
    /// This lets app-level hooks avoid writing reactive state for repeated
    /// settled-selection notifications after the same range has already been
    /// consumed.
    pub fn copy_on_select_would_mutate(&self) -> bool {
        if self.selection.is_dragging() || !self.selection.has_selection() {
            return self.copy_on_select_copied;
        }
        !self.copy_on_select_copied
    }

    /// Returns text that should be copied for copy-on-select, at most once per
    /// settled selection.
    ///
    /// This is the state-machine counterpart to CC Ink's `useCopyOnSelect`:
    /// dragging resets the guard; no selection resets it; the first subsequent
    /// non-dragging selection returns selected text without clearing the
    /// highlight. Clipboard transport and user notifications remain caller
    /// policy.
    pub fn copy_on_select_text(&mut self, canvas: &Canvas) -> Option<String> {
        if self.selection.is_dragging() {
            self.copy_on_select_copied = false;
            return None;
        }
        if !self.selection.has_selection() {
            self.copy_on_select_copied = false;
            return None;
        }
        if self.copy_on_select_copied {
            return None;
        }
        self.copy_on_select_copied = true;
        let text = self.selection.selected_text(canvas);
        // Match CC Ink's useCopyOnSelect: blank-line/whitespace-only ranges
        // settle the guard so we do not retry on every notification, but they
        // are not worth an OSC 52 clipboard write or copied notification.
        (!text.trim().is_empty()).then_some(text)
    }

    /// Packed-screen variant of [`SelectionController::copy_on_select_text`].
    pub fn copy_on_select_text_packed(
        &mut self,
        screen: &CanvasPackedScreen,
        pools: &CanvasPackedCellPools,
    ) -> Option<String> {
        if self.selection.is_dragging() {
            self.copy_on_select_copied = false;
            return None;
        }
        if !self.selection.has_selection() {
            self.copy_on_select_copied = false;
            return None;
        }
        if self.copy_on_select_copied {
            return None;
        }
        self.copy_on_select_copied = true;
        let text = self.selection.selected_text_packed(screen, pools);
        (!text.trim().is_empty()).then_some(text)
    }

    /// Returns selected text and clears the selection, mirroring CC Ink's
    /// `copySelection()` state transition. Clipboard transport remains the
    /// caller's responsibility.
    pub fn take_selected_text(&mut self, canvas: &Canvas) -> String {
        let text = self.selection.selected_text(canvas);
        if self.selection.has_selection() {
            self.selection.clear();
            self.copy_on_select_copied = false;
        }
        text
    }

    /// Packed-screen variant of [`SelectionController::take_selected_text`].
    pub fn take_selected_text_packed(
        &mut self,
        screen: &CanvasPackedScreen,
        pools: &CanvasPackedCellPools,
    ) -> String {
        let text = self.selection.selected_text_packed(screen, pools);
        if self.selection.has_selection() {
            self.selection.clear();
            self.copy_on_select_copied = false;
        }
        text
    }
}
