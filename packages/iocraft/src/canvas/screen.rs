use super::*;

/// Tracks whether a cell is a standalone character, the first column of a wide
/// (double-width) character, or a spacer placeholder used by wide-character
/// layout. See the module-level documentation for why this matters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum CellWidth {
    /// A normal single-column character.
    #[default]
    Normal,
    /// The first column of a double-width character (CJK, emoji, etc.).
    Wide,
    /// The second column occupied by a [`Wide`](CellWidth::Wide) character in the
    /// preceding cell. This cell carries no independent content — its `character`
    /// field is `None`.
    WidthTail,
    /// A blank placeholder at the right edge where a wide grapheme would have
    /// crossed the viewport boundary. CC Ink calls this `SpacerHead` and skips
    /// it during terminal output so the real cursor never enters pending-wrap.
    SpacerHead,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Character {
    pub(super) value: String,
    pub(super) style: CanvasTextStyle,
    pub(super) hyperlink: Option<String>,
}

impl Character {
    pub(super) fn required_padding(&self) -> usize {
        if self.value.contains('\u{fe0f}') {
            if handles_vs16_incorrectly() {
                string_display_width(&self.value).saturating_sub(1)
            } else {
                0
            }
        } else {
            0
        }
    }

    pub(super) fn needs_width_compensation(&self) -> bool {
        // Mirrors CC Ink's log-update.ts `needsWidthCompensation`: newer
        // emoji and text-default emoji with VS16 may advance by only one column
        // on terminals with stale wcwidth tables. ANSI output compensates with
        // cursor addressing so the fix is harmless on terminals with correct
        // width tables.
        let Some(cp) = self.value.chars().next().map(|ch| ch as u32) else {
            return false;
        };
        if (0x1fa70..=0x1faff).contains(&cp) || (0x1fb00..=0x1fbff).contains(&cp) {
            return true;
        }
        self.value.len() >= 2 && self.value.contains('\u{fe0f}')
    }
}

/// Overlays are the mechanism behind cursor inversion, search highlighting, and selection
/// rendering. They are applied **after** the component tree has finished drawing, so
/// components do not need to know whether they are "selected" or "under the cursor".
///
/// See `docs/design-post-render-style-overlay.md` for the full design rationale.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct StyleOverlay {
    /// Override the foreground color. `Some(None)` resets to default; `None` keeps original.
    pub color: Option<Option<Color>>,
    /// Override the background color. `Some(None)` resets to default; `None` keeps original.
    pub background_color: Option<Option<Color>>,
    /// Override the text weight.
    pub weight: Option<Weight>,
    /// Force underline on or off.
    pub underline: Option<bool>,
    /// Override the underline variant.
    pub underline_style: Option<UnderlineStyle>,
    /// Override the underline color. `Some(None)` resets to default; `None` keeps original.
    pub underline_color: Option<Option<Color>>,
    /// Force italic on or off.
    pub italic: Option<bool>,
    /// Force blink on or off.
    pub blink: Option<bool>,
    /// Force hidden/conceal on or off.
    pub hidden: Option<bool>,
    /// Force strikethrough on or off.
    pub strikethrough: Option<bool>,
    /// Force overline on or off.
    pub overline: Option<bool>,
    /// Force color inversion on or off. This is the primary field for cursor / search / selection.
    pub invert: Option<bool>,
}

impl StyleOverlay {
    /// Composes this overlay on top of an existing lower-priority overlay.
    ///
    /// `Some(...)` fields in `self` override the lower overlay, while `None`
    /// fields preserve it. This mirrors CC Ink's post-render style-id layering:
    /// selection is applied first, search matches are applied on top, and the
    /// current-match highlight can then override only the fields it owns.
    pub fn compose_over(self, lower: Self) -> Self {
        Self {
            color: self.color.or(lower.color),
            background_color: self.background_color.or(lower.background_color),
            weight: self.weight.or(lower.weight),
            underline: self.underline.or(lower.underline),
            underline_style: self.underline_style.or(lower.underline_style),
            underline_color: self.underline_color.or(lower.underline_color),
            italic: self.italic.or(lower.italic),
            blink: self.blink.or(lower.blink),
            hidden: self.hidden.or(lower.hidden),
            strikethrough: self.strikethrough.or(lower.strikethrough),
            overline: self.overline.or(lower.overline),
            invert: self.invert.or(lower.invert),
        }
    }

    /// Creates an inverse-video overlay, useful for cursors and generic search
    /// matches.
    pub fn inverse() -> Self {
        Self {
            invert: Some(true),
            ..Default::default()
        }
    }

    /// Creates a solid selection-background overlay that preserves foreground
    /// colors and disables inverse-video.
    ///
    /// This mirrors CC Ink's selection style: a theme-provided background color
    /// replaces the cell background while syntax/highlight foregrounds remain
    /// readable.
    pub fn selection_background(color: Color) -> Self {
        Self {
            background_color: Some(Some(color)),
            invert: Some(false),
            ..Default::default()
        }
    }

    /// Creates an overlay for the current search match.
    ///
    /// CC Ink renders the current match as yellow-fg plus inverse-video, with
    /// bold and underline on top. The inverse turns the yellow foreground into a
    /// yellow background while `background_color: Some(None)` strips any lower
    /// selection/syntax background so the marker is unambiguous.
    pub fn current_match(background_color: Color) -> Self {
        Self {
            color: Some(Some(background_color)),
            background_color: Some(None),
            weight: Some(Weight::Bold),
            underline: Some(true),
            invert: Some(true),
            ..Default::default()
        }
    }
}

/// A single cell on a [`Canvas`], containing optional text and background color.
#[non_exhaustive]
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CanvasCell {
    /// The background color of this cell, if set.
    pub background_color: Option<Color>,
    pub(super) character: Option<Character>,
    /// Whether this cell is a normal character, the first column of a wide character,
    /// or a trailing placeholder.
    pub(crate) cell_width: CellWidth,
}

fn clear_cell_width_relationship(row: &mut [CanvasCell], x: usize) {
    let Some(width) = row.get(x).map(|cell| cell.cell_width) else {
        return;
    };
    match width {
        CellWidth::Wide => {
            row[x].character = None;
            row[x].cell_width = CellWidth::Normal;
            if x + 1 < row.len() && row[x + 1].cell_width == CellWidth::WidthTail {
                row[x + 1].character = None;
                row[x + 1].cell_width = CellWidth::Normal;
            }
        }
        CellWidth::WidthTail => {
            row[x].character = None;
            row[x].cell_width = CellWidth::Normal;
            if x > 0 && row[x - 1].cell_width == CellWidth::Wide {
                row[x - 1].character = None;
                row[x - 1].cell_width = CellWidth::Normal;
            }
        }
        CellWidth::SpacerHead | CellWidth::Normal => {
            row[x].character = None;
            row[x].cell_width = CellWidth::Normal;
        }
    }
}

impl CanvasCell {
    /// Returns the text content of this cell, or `None` if empty.
    pub fn text(&self) -> Option<&str> {
        self.character.as_ref().map(|ch| ch.value.as_str())
    }

    /// Returns the text style of this cell, or `None` if the cell is empty.
    pub fn text_style(&self) -> Option<&CanvasTextStyle> {
        self.character.as_ref().map(|ch| &ch.style)
    }

    /// Returns this cell's OSC 8 hyperlink target, if any.
    pub fn hyperlink(&self) -> Option<&str> {
        self.character
            .as_ref()
            .and_then(|ch| ch.hyperlink.as_deref())
    }

    /// Returns `true` if the cell has no content and no background color.
    pub fn is_empty(&self) -> bool {
        self.background_color.is_none() && self.character.is_none()
    }

    fn is_row_trim_empty(&self, overlay: Option<&StyleOverlay>) -> bool {
        let effective_bg = match overlay.and_then(|o| o.background_color) {
            Some(bg) => bg,
            None => self.background_color,
        };
        if effective_bg.is_some() {
            return false;
        }

        let effective_style = match (&self.character, overlay) {
            (Some(c), Some(ov)) => c.style.with_overlay(ov),
            (Some(c), None) => c.style,
            (None, Some(ov)) => CanvasTextStyle::default().with_overlay(ov),
            (None, None) => CanvasTextStyle::default(),
        };
        if effective_style.invert
            || effective_style.underline
            || effective_style.strikethrough
            || effective_style.overline
        {
            return false;
        }

        match &self.character {
            Some(c) => c.value == " " && c.hyperlink.is_none(),
            None => true,
        }
    }
}

/// `Canvas` is the medium that output is drawn to before being rendered to the terminal or other
/// destinations.
///
/// Typical use of the library doesn't require direct interaction with this struct. It is primarily useful for two cases:
///
/// - When implementing low-level components, you'll need to utilize the `Canvas` drawing methods.
/// - When implementing unit tests for components, you may want to render to a `Canvas` for inspection.
#[derive(Clone)]
pub struct Canvas {
    pub(super) width: usize,
    pub(super) cells: Vec<Vec<CanvasCell>>,
    pub(super) overlays: Vec<Vec<Option<StyleOverlay>>>,
    pub(super) no_select: Vec<Vec<bool>>,
    pub(super) soft_wrap: Vec<usize>,
    pub(super) cursor_declaration: Option<CursorDeclaration>,
    pub(super) scroll_hint: Option<ScrollHint>,
    pub(super) force_full_repaint: bool,
    pub(super) damage_region: Option<DamageRegion>,
}

impl PartialEq for Canvas {
    fn eq(&self, other: &Self) -> bool {
        self.width == other.width
            && self.cells == other.cells
            && self.overlays == other.overlays
            && self.cursor_declaration == other.cursor_declaration
    }
}

/// DECSTBM scroll optimization hint for fullscreen/alternate-screen rendering.
///
/// `top` and `bottom` are 0-indexed inclusive terminal rows. `delta > 0`
/// means content moved up (scroll offset increased, CSI `S`); `delta < 0`
/// means content moved down (CSI `T`). The hint is intentionally ignored by
/// [`Canvas`] equality so a transient optimization marker does not by itself
/// wake or dirty subsequent unchanged frames.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScrollHint {
    /// Top row of the scroll region, 0-indexed and inclusive.
    pub top: usize,
    /// Bottom row of the scroll region, 0-indexed and inclusive.
    pub bottom: usize,
    /// Signed row delta. Positive scrolls content up; negative scrolls content down.
    pub delta: i32,
}

/// One-shot damage marker for terminal rendering.
///
/// The rectangle mirrors CC Ink's screen damage model: terminal backends use the
/// vertical extent to wake otherwise-identical rows, and the horizontal extent to
/// start sparse row rewrites at the first dirty column instead of repainting the
/// whole row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DamageRegion {
    /// Left column, 0-indexed.
    pub x: usize,
    /// Top row, 0-indexed.
    pub y: usize,
    /// Width in cells.
    pub width: usize,
    /// Height in rows.
    pub height: usize,
}

impl DamageRegion {
    pub(super) fn union(self, other: Self) -> Self {
        let left = self.x.min(other.x);
        let top = self.y.min(other.y);
        let right = self
            .x
            .saturating_add(self.width)
            .max(other.x.saturating_add(other.width));
        let bottom = self
            .y
            .saturating_add(self.height)
            .max(other.y.saturating_add(other.height));
        Self {
            x: left,
            y: top,
            width: right.saturating_sub(left),
            height: bottom.saturating_sub(top),
        }
    }

    pub(super) fn intersects_row(self, row: usize) -> bool {
        self.height > 0 && row >= self.y && row < self.y.saturating_add(self.height)
    }
}

/// Owned cell snapshot emitted by [`Canvas::diff`].
///
/// This is a mode-neutral retained-canvas analogue of CC Ink's `screen.ts`
/// `Cell` view: the base cell is kept separate from iocraft's post-render
/// [`StyleOverlay`] layer so custom renderers can decide how to serialize the
/// composed style.
#[derive(Clone, Debug, PartialEq)]
pub struct CanvasDiffCell {
    /// Retained cell contents and base style.
    pub cell: CanvasCell,
    /// Post-render overlay for this cell, if any.
    pub overlay: Option<StyleOverlay>,
}

/// Borrowed cell view emitted by [`Canvas::diff_each`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CanvasDiffCellRef<'a> {
    /// Retained cell contents and base style.
    pub cell: &'a CanvasCell,
    /// Post-render overlay for this cell, if any.
    pub overlay: Option<StyleOverlay>,
}

impl CanvasDiffCellRef<'_> {
    /// Converts this borrowed view into an owned [`CanvasDiffCell`].
    pub fn to_owned(self) -> CanvasDiffCell {
        CanvasDiffCell {
            cell: self.cell.clone(),
            overlay: self.overlay,
        }
    }

    fn is_empty(self) -> bool {
        self.cell.is_empty() && self.overlay.is_none()
    }
}

/// Owned per-cell change emitted by [`Canvas::diff`].
#[derive(Clone, Debug, PartialEq)]
pub struct CanvasDiffChange {
    /// Column of the changed cell.
    pub x: usize,
    /// Row of the changed cell.
    pub y: usize,
    /// Previous cell, or `None` when the next canvas grew into this position.
    pub removed: Option<CanvasDiffCell>,
    /// New cell, or `None` when the next canvas shrank away from this position.
    pub added: Option<CanvasDiffCell>,
}

/// Borrowed per-cell change emitted by [`Canvas::diff_each`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CanvasDiffChangeRef<'a> {
    /// Column of the changed cell.
    pub x: usize,
    /// Row of the changed cell.
    pub y: usize,
    /// Previous cell, or `None` when the next canvas grew into this position.
    pub removed: Option<CanvasDiffCellRef<'a>>,
    /// New cell, or `None` when the next canvas shrank away from this position.
    pub added: Option<CanvasDiffCellRef<'a>>,
}

impl CanvasDiffChangeRef<'_> {
    /// Converts this borrowed change into an owned [`CanvasDiffChange`].
    pub fn to_owned(self) -> CanvasDiffChange {
        CanvasDiffChange {
            x: self.x,
            y: self.y,
            removed: self.removed.map(CanvasDiffCellRef::to_owned),
            added: self.added.map(CanvasDiffCellRef::to_owned),
        }
    }
}

/// Physical terminal cursor declaration for a single frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CursorDeclaration {
    /// Column.
    pub x: usize,
    /// Row.
    pub y: usize,
    /// Whether the physical cursor should be made visible. When `false` the
    /// cursor is positioned (for IME / screen readers) but not shown — the
    /// visual cursor is rendered entirely via [`StyleOverlay`] (ink model).
    /// When `true` the terminal's native cursor is displayed (ratatui model).
    pub visible: bool,
}

impl Canvas {
    /// Constructs a new canvas with the given dimensions.
    pub fn new(width: usize, height: usize) -> Self {
        Self {
            width,
            cells: vec![vec![CanvasCell::default(); width]; height],
            overlays: vec![vec![None; width]; height],
            no_select: vec![vec![false; width]; height],
            soft_wrap: vec![0; height],
            cursor_declaration: None,
            scroll_hint: None,
            force_full_repaint: false,
            damage_region: None,
        }
    }

    /// Marks a rectangular region as excluded from fullscreen text selection.
    ///
    /// This is render metadata only: it does not affect terminal output, canvas
    /// equality, or damage. It mirrors CC Ink's `screen.noSelect` bitmap, which
    /// is consumed by selection/copy/highlight code rather than the diff writer.
    pub fn mark_no_select_region(&mut self, x: usize, y: usize, width: usize, height: usize) {
        if width == 0 || height == 0 {
            return;
        }

        let left = x.min(self.width);
        let top = y.min(self.height());
        let right = x.saturating_add(width).min(self.width);
        let bottom = y.saturating_add(height).min(self.height());
        if right <= left || bottom <= top {
            return;
        }

        for row in &mut self.no_select[top..bottom] {
            row[left..right].fill(true);
        }
    }

    /// Returns whether a cell is marked as excluded from text selection.
    pub fn is_no_select(&self, x: usize, y: usize) -> bool {
        self.no_select
            .get(y)
            .and_then(|row| row.get(x))
            .copied()
            .unwrap_or(false)
    }

    fn selection_char_class(value: &str) -> u8 {
        if value.is_empty() || value == " " {
            return 0;
        }
        if value.chars().all(|ch| {
            ch.is_alphanumeric() || matches!(ch, '_' | '/' | '.' | '-' | '+' | '~' | '\\')
        }) {
            1
        } else {
            2
        }
    }

    fn cell_selection_text(&self, x: usize, y: usize) -> Option<&str> {
        self.cell(x, y)
            .and_then(|cell| {
                cell.character
                    .as_ref()
                    .map(|character| character.value.as_str())
            })
            .or(Some(" "))
    }

    pub(super) fn word_bounds_at(&self, col: usize, row: usize) -> Option<(usize, usize)> {
        if row >= self.height() || self.width == 0 {
            return None;
        }

        let mut c = col;
        if c > 0
            && self
                .cell(c, row)
                .is_some_and(|cell| cell.cell_width == CellWidth::WidthTail)
        {
            c -= 1;
        }
        if c >= self.width || self.is_no_select(c, row) {
            return None;
        }
        let cls = Self::selection_char_class(self.cell_selection_text(c, row)?);

        let mut lo = c;
        while lo > 0 {
            let prev = lo - 1;
            if self.is_no_select(prev, row) {
                break;
            }
            let Some(prev_cell) = self.cell(prev, row) else {
                break;
            };
            if prev_cell.cell_width == CellWidth::WidthTail {
                if prev == 0 || self.is_no_select(prev - 1, row) {
                    break;
                }
                let Some(head_text) = self.cell_selection_text(prev - 1, row) else {
                    break;
                };
                if Self::selection_char_class(head_text) != cls {
                    break;
                }
                lo = prev - 1;
                continue;
            }
            let Some(prev_text) = self.cell_selection_text(prev, row) else {
                break;
            };
            if Self::selection_char_class(prev_text) != cls {
                break;
            }
            lo = prev;
        }

        let mut hi = c;
        while hi + 1 < self.width {
            let next = hi + 1;
            if self.is_no_select(next, row) {
                break;
            }
            let Some(next_cell) = self.cell(next, row) else {
                break;
            };
            if next_cell.cell_width == CellWidth::WidthTail {
                hi = next;
                continue;
            }
            let Some(next_text) = self.cell_selection_text(next, row) else {
                break;
            };
            if Self::selection_char_class(next_text) != cls {
                break;
            }
            hi = next;
        }

        Some((lo, hi))
    }

    fn osc8_hyperlink_at(&self, col: usize, row: usize) -> Option<&str> {
        let cell = self.cell(col, row)?;
        if let Some(href) = cell.hyperlink() {
            return Some(href);
        }
        if col > 0 && cell.cell_width == CellWidth::WidthTail {
            return self.cell(col - 1, row).and_then(CanvasCell::hyperlink);
        }
        None
    }

    fn is_plain_url_char(value: &str) -> bool {
        if value.len() != 1 {
            return false;
        }
        let byte = value.as_bytes()[0];
        (0x21..=0x7e).contains(&byte) && !matches!(byte, b'<' | b'>' | b'"' | b'\'' | b'`' | b' ')
    }

    fn plain_url_char_at(&self, col: usize, row: usize) -> Option<&str> {
        let cell = self.cell(col, row)?;
        if cell.cell_width != CellWidth::Normal {
            return None;
        }
        let value = cell.text()?;
        Self::is_plain_url_char(value).then_some(value)
    }

    /// Finds a plain-text URL at a cell, respecting `noSelect` boundaries.
    ///
    /// This mirrors CC Ink's fallback used when fullscreen mouse tracking
    /// intercepts native terminal URL detection. OSC 8 links are handled by
    /// [`Canvas::hyperlink_at`]; this method scans ASCII URL-like text runs for
    /// `http://`, `https://`, or `file://` schemes and strips trailing sentence
    /// punctuation.
    pub fn plain_text_url_at(&self, col: usize, row: usize) -> Option<String> {
        if row >= self.height() || self.width == 0 {
            return None;
        }
        let mut c = col;
        if c > 0
            && self
                .cell(c, row)
                .is_some_and(|cell| cell.cell_width == CellWidth::WidthTail)
        {
            c -= 1;
        }
        if c >= self.width || self.is_no_select(c, row) || self.plain_url_char_at(c, row).is_none()
        {
            return None;
        }

        let mut lo = c;
        while lo > 0 {
            let prev = lo - 1;
            if self.is_no_select(prev, row) || self.plain_url_char_at(prev, row).is_none() {
                break;
            }
            lo = prev;
        }

        let mut hi = c;
        while hi + 1 < self.width {
            let next = hi + 1;
            if self.is_no_select(next, row) || self.plain_url_char_at(next, row).is_none() {
                break;
            }
            hi = next;
        }

        let mut token = String::new();
        for col in lo..=hi {
            token.push_str(self.plain_url_char_at(col, row)?);
        }
        let click_idx = c - lo;

        let mut url_start = None;
        let mut url_end = token.len();
        let mut search_from = 0;
        while search_from < token.len() {
            let rest = &token[search_from..];
            let next_scheme = ["http://", "https://", "file://"]
                .into_iter()
                .filter_map(|scheme| rest.find(scheme).map(|idx| search_from + idx))
                .min();
            let Some(idx) = next_scheme else {
                break;
            };
            if idx > click_idx {
                url_end = idx;
                break;
            }
            url_start = Some(idx);
            search_from = idx + 1;
        }

        let url_start = url_start?;
        let mut url = token[url_start..url_end].to_string();
        while let Some(last) = url.chars().last() {
            if ".,;:!?".contains(last) {
                url.pop();
                continue;
            }
            let opener = match last {
                ')' => Some('('),
                ']' => Some('['),
                '}' => Some('{'),
                _ => None,
            };
            let Some(opener) = opener else {
                break;
            };
            let opens = url.chars().filter(|ch| *ch == opener).count();
            let closes = url.chars().filter(|ch| *ch == last).count();
            if closes > opens {
                url.pop();
            } else {
                break;
            }
        }

        if url.is_empty() || click_idx >= url_start + url.len() {
            return None;
        }
        Some(url)
    }

    /// Returns the hyperlink target at a cell.
    ///
    /// OSC 8 hyperlinks are preferred, including when the queried cell is the
    /// tail of a wide linked grapheme. If no OSC 8 hyperlink is present, this
    /// falls back to [`Canvas::plain_text_url_at`], matching CC Ink's fullscreen
    /// click handling.
    pub fn hyperlink_at(&self, col: usize, row: usize) -> Option<String> {
        self.osc8_hyperlink_at(col, row)
            .map(str::to_string)
            .or_else(|| self.plain_text_url_at(col, row))
    }

    /// Marks a row as a soft-wrap continuation of the previous row.
    ///
    /// `prev_content_end` is the exclusive terminal column where the previous
    /// row's rendered content ended. This mirrors CC Ink's `screen.softWrap`:
    /// selection copy joins continuation rows without inserting `\n`, and uses
    /// the previous row's content end to avoid copying unwritten padding.
    pub fn mark_soft_wrap_continuation(&mut self, row: usize, prev_content_end: usize) {
        if let Some(slot) = self.soft_wrap.get_mut(row) {
            *slot = prev_content_end.min(self.width);
        }
    }

    /// Returns the previous-row content end for a soft-wrap continuation row,
    /// or 0 when the row starts a hard/new logical line.
    pub fn soft_wrap_continuation(&self, row: usize) -> usize {
        self.soft_wrap.get(row).copied().unwrap_or(0)
    }

    pub(super) fn extract_selected_row(
        &self,
        row: usize,
        col_start: usize,
        col_end: usize,
    ) -> String {
        let content_end = self.soft_wrap_continuation(row + 1);
        let last_col = if content_end > 0 {
            col_end.min(content_end.saturating_sub(1))
        } else {
            col_end
        };
        let mut line = String::new();
        for col in col_start..=last_col {
            if self.is_no_select(col, row) {
                continue;
            }
            let Some(cell) = self.cell(col, row) else {
                continue;
            };
            if matches!(
                cell.cell_width,
                CellWidth::WidthTail | CellWidth::SpacerHead
            ) {
                continue;
            }
            if let Some(character) = &cell.character {
                line.push_str(&character.value);
            } else {
                line.push(' ');
            }
        }
        if content_end > 0 {
            line
        } else {
            line.trim_end().to_string()
        }
    }

    /// Extracts text from a linear selection range, skipping cells marked with
    /// [`Canvas::mark_no_select_region`].
    ///
    /// Soft-wrap continuation rows (see [`Canvas::mark_soft_wrap_continuation`])
    /// are joined back onto the previous row without `\n`. Wide-character tail
    /// cells are skipped because the head cell already owns the full grapheme.
    /// This is the iocraft foundation for CC Ink's `getSelectedText(...)`.
    pub fn selected_text(&self, range: SelectionRange) -> String {
        let (start, end) = range.normalized();
        if self.width == 0 || self.height() == 0 || start.row >= self.height() {
            return String::new();
        }

        let mut lines = Vec::<String>::new();
        let last_row = end.row.min(self.height() - 1);
        for row in start.row..=last_row {
            let col_start = if row == start.row { start.col } else { 0 };
            let col_end = if row == end.row {
                end.col.min(self.width - 1)
            } else {
                self.width - 1
            };
            let line = if col_start >= self.width || col_start > col_end {
                String::new()
            } else {
                self.extract_selected_row(row, col_start, col_end)
            };

            if self.soft_wrap_continuation(row) > 0 && !lines.is_empty() {
                lines.last_mut().unwrap().push_str(&line);
            } else {
                lines.push(line);
            }
        }

        lines.join("\n")
    }

    /// Applies a style overlay to every selectable cell in a linear selection
    /// range, skipping `noSelect` cells.
    ///
    /// This mirrors CC Ink's `applySelectionOverlay(...)`: selection/highlight is
    /// applied after ordinary rendering by mutating screen-cell style metadata,
    /// leaving the diff writer unaware of selection policy. The affected cells
    /// are marked damaged so otherwise-identical selected rows are repainted.
    pub fn apply_selection_overlay(
        &mut self,
        range: SelectionRange,
        overlay: StyleOverlay,
    ) -> bool {
        let (start, end) = range.normalized();
        if self.width == 0 || self.height() == 0 || start.row >= self.height() {
            return false;
        }

        let mut damage: Option<DamageRegion> = None;
        let last_row = end.row.min(self.height() - 1);
        for row in start.row..=last_row {
            let col_start = if row == start.row { start.col } else { 0 };
            let col_end = if row == end.row {
                end.col.min(self.width - 1)
            } else {
                self.width - 1
            };
            if col_start >= self.width || col_start > col_end {
                continue;
            }

            for col in col_start..=col_end {
                if self.is_no_select(col, row) {
                    continue;
                }
                let Some(cell) = self.cell(col, row) else {
                    continue;
                };
                if matches!(
                    cell.cell_width,
                    CellWidth::WidthTail | CellWidth::SpacerHead
                ) {
                    continue;
                }
                self.set_overlay(col, row, overlay);
                let cell_damage = DamageRegion {
                    x: col,
                    y: row,
                    width: 1,
                    height: 1,
                };
                damage = Some(match damage {
                    Some(existing) => existing.union(cell_damage),
                    None => cell_damage,
                });
            }
        }

        if let Some(region) = damage {
            self.mark_damage(region);
            true
        } else {
            false
        }
    }

    /// Declares the physical terminal cursor position with explicit visibility.
    ///
    /// - `visible: true` — **ratatui model**: the terminal's native cursor is shown
    ///   at the given position. Best when you want the terminal to guarantee cursor
    ///   contrast and respect the user's cursor preferences.
    /// - `visible: false` — **ink model**: the cursor is positioned for IME and
    ///   screen readers but stays hidden. The visual cursor is rendered via
    ///   [`StyleOverlay`] inversion or explicit colors.
    ///
    /// Only one declaration can be active per frame — the last writer wins. When no
    /// component declares a cursor, the physical cursor remains hidden.
    pub fn declare_cursor(&mut self, x: usize, y: usize, visible: bool) {
        if y < self.cells.len() && x < self.width {
            self.cursor_declaration = Some(CursorDeclaration { x, y, visible });
        }
    }

    /// Returns the declared cursor for this frame, if any.
    pub fn cursor_declaration(&self) -> Option<CursorDeclaration> {
        self.cursor_declaration
    }

    /// Declares that a fullscreen scroll region moved between the previous and
    /// current frame. Terminal backends may use this to emit DECSTBM + SU/SD and
    /// then diff against a virtually shifted previous canvas, mirroring CC Ink's
    /// ScrollBox fast path. Main-screen renderers ignore the hint.
    pub fn set_scroll_hint(&mut self, hint: ScrollHint) {
        self.scroll_hint = Some(hint);
    }

    /// Returns the scroll optimization hint for this frame, if any.
    pub fn scroll_hint(&self) -> Option<ScrollHint> {
        self.scroll_hint
    }

    /// Requests a full repaint for this canvas even if its cells are identical
    /// to the previous frame. This is the iocraft equivalent of CC Ink's
    /// `prevFrameContaminated`/full-damage backstop: when terminal contents or
    /// post-render overlays may have made the retained previous frame
    /// untrustworthy, the next write must refresh every row instead of relying
    /// on sparse row equality.
    pub fn force_full_repaint(&mut self) {
        self.force_full_repaint = true;
    }

    pub(crate) fn should_force_full_repaint(&self) -> bool {
        self.force_full_repaint
    }

    pub(crate) fn clear_force_full_repaint(&mut self) {
        self.force_full_repaint = false;
    }

    /// Marks a rectangle as damaged for this frame. Damaged rows are repainted
    /// even when their retained cells compare equal to the previous frame.
    /// Damage is render metadata and is intentionally ignored by [`Canvas`]
    /// equality so callers can opt into repaint without changing logical output.
    pub fn mark_damage(&mut self, region: DamageRegion) {
        if region.width == 0 || region.height == 0 {
            return;
        }

        let left = region.x.min(self.width);
        let top = region.y.min(self.height());
        let right = region.x.saturating_add(region.width).min(self.width);
        let bottom = region.y.saturating_add(region.height).min(self.height());
        if right <= left || bottom <= top {
            return;
        }

        let clipped = DamageRegion {
            x: left,
            y: top,
            width: right - left,
            height: bottom - top,
        };
        self.damage_region = Some(match self.damage_region {
            Some(existing) => existing.union(clipped),
            None => clipped,
        });
    }

    /// Returns the accumulated damage region for this canvas, if any.
    ///
    /// Damage is render metadata: it is ignored by [`Canvas`] equality but used
    /// by terminal diff writers to repaint otherwise-identical cells.
    pub fn damage_region(&self) -> Option<DamageRegion> {
        self.damage_region
    }

    pub(crate) fn has_damage(&self) -> bool {
        self.damage_region.is_some()
    }

    #[cfg(test)]
    pub(crate) fn row_is_damaged(&self, y: usize) -> bool {
        self.damage_region
            .is_some_and(|region| region.intersects_row(y))
    }

    /// Clears accumulated damage metadata.
    pub fn clear_damage(&mut self) {
        self.damage_region = None;
    }

    /// Clears a rectangular region of terminal-output cells and marks it damaged.
    ///
    /// This mirrors CC Ink's `clearRegion(...)`: it clears cells/styles/hyperlinks
    /// and overlay metadata while leaving selection-only metadata such as
    /// `noSelect`/`softWrap` untouched. If the clear cuts through a wide
    /// grapheme at either horizontal edge, the orphaned head/tail just outside
    /// the region is repaired and included in the damage bounds.
    pub fn clear_region(&mut self, x: usize, y: usize, width: usize, height: usize) {
        if width == 0 || height == 0 || x >= self.width || y >= self.height() {
            return;
        }
        let max_x = x.saturating_add(width).min(self.width);
        let max_y = y.saturating_add(height).min(self.height());
        if x >= max_x || y >= max_y {
            return;
        }

        let mut damage_min_x = x;
        let mut damage_max_x = max_x;
        for row in y..max_y {
            if x > 0
                && self.cells[row][x].cell_width == CellWidth::WidthTail
                && self.cells[row][x - 1].cell_width == CellWidth::Wide
            {
                self.cells[row][x - 1] = CanvasCell::default();
                self.overlays[row][x - 1] = None;
                damage_min_x = damage_min_x.min(x - 1);
            }
            if max_x < self.width
                && self.cells[row][max_x - 1].cell_width == CellWidth::Wide
                && self.cells[row][max_x].cell_width == CellWidth::WidthTail
            {
                self.cells[row][max_x] = CanvasCell::default();
                self.overlays[row][max_x] = None;
                damage_max_x = damage_max_x.max(max_x + 1);
            }

            for col in x..max_x {
                self.cells[row][col] = CanvasCell::default();
                self.overlays[row][col] = None;
            }
        }

        self.mark_damage(DamageRegion {
            x: damage_min_x,
            y,
            width: damage_max_x.saturating_sub(damage_min_x),
            height: max_y - y,
        });
    }

    /// Returns a new canvas containing a copy of a rectangular region.
    ///
    /// The returned canvas uses local coordinates (the source region's top-left
    /// becomes `(0, 0)`) and preserves cells plus render metadata (`overlays`,
    /// `noSelect`, and per-row `softWrap`). Damage metadata is cleared because
    /// the snapshot is intended for retained-subtree caches; blitting the
    /// snapshot later will mark the destination damaged.
    pub fn copy_region(&self, x: usize, y: usize, width: usize, height: usize) -> Canvas {
        if width == 0 || height == 0 || x >= self.width || y >= self.height() {
            return Canvas::new(0, 0);
        }
        let copy_width = width.min(self.width - x);
        let copy_height = height.min(self.height() - y);
        let mut out = Canvas::new(copy_width, copy_height);
        out.subview_mut(0, 0, 0, 0, copy_width, copy_height)
            .blit_region_from(self, 0, 0, x, y, copy_width, copy_height);
        out.clear_damage();
        out
    }

    /// Copies a rectangular region from another canvas at the same coordinates.
    ///
    /// This is the iocraft equivalent of CC Ink's `blitRegion(...)`: it copies
    /// cells plus render metadata (`overlays`, `noSelect`, and per-row
    /// `softWrap`) and marks the copied rectangle as damaged. If a copied region
    /// ends on the leading cell of a wide grapheme, the spacer tail just outside
    /// the region is repaired and included in damage.
    pub fn blit_region_from(
        &mut self,
        src: &Canvas,
        x: usize,
        y: usize,
        width: usize,
        height: usize,
    ) {
        if width == 0 || height == 0 || x >= self.width || x >= src.width {
            return;
        }
        let max_x = x.saturating_add(width).min(self.width).min(src.width);
        let max_y = y
            .saturating_add(height)
            .min(self.height())
            .min(src.height());
        if x >= max_x || y >= max_y {
            return;
        }

        for row in y..max_y {
            self.cells[row][x..max_x].clone_from_slice(&src.cells[row][x..max_x]);
            self.overlays[row][x..max_x].clone_from_slice(&src.overlays[row][x..max_x]);
            self.no_select[row][x..max_x].clone_from_slice(&src.no_select[row][x..max_x]);
            self.soft_wrap[row] = src.soft_wrap[row];
        }

        let mut damage_width = max_x - x;
        if max_x < self.width && max_x < src.width {
            let mut wrote_tail = false;
            for row in y..max_y {
                if src.cells[row][max_x - 1].cell_width == CellWidth::Wide {
                    self.cells[row][max_x] = CanvasCell {
                        cell_width: CellWidth::WidthTail,
                        ..Default::default()
                    };
                    self.overlays[row][max_x] = None;
                    wrote_tail = true;
                }
            }
            if wrote_tail {
                damage_width += 1;
            }
        }

        self.mark_damage(DamageRegion {
            x,
            y,
            width: damage_width,
            height: max_y - y,
        });
    }

    /// Copies a rectangular region while skipping rows covered by absolute clears.
    ///
    /// This mirrors the absolute-clear guard in CC Ink's `output.ts`: when a
    /// removed `position:absolute` node queued a clear over a row, that row in
    /// the previous retained screen can contain stale overlay pixels. A clean
    /// sibling blit must not restore those pixels. Rows are skipped only when a
    /// clear region fully covers the blit row segment (`x..max_x`), matching the
    /// official `startX >= clear.x && maxX <= clear.x + clear.width` rule.
    ///
    /// The clear itself is not applied here; callers should clear or mark damage
    /// for `excluded_clears` separately, then use this helper for subsequent
    /// prev-screen blits. The helper is mode-neutral and performs no terminal I/O.
    pub fn blit_region_from_excluding_clears(
        &mut self,
        src: &Canvas,
        x: usize,
        y: usize,
        width: usize,
        height: usize,
        excluded_clears: &[DamageRegion],
    ) {
        if excluded_clears.is_empty() {
            self.blit_region_from(src, x, y, width, height);
            return;
        }
        if width == 0 || height == 0 || x >= self.width || x >= src.width {
            return;
        }
        let max_x = x.saturating_add(width).min(self.width).min(src.width);
        let max_y = y
            .saturating_add(height)
            .min(self.height())
            .min(src.height());
        if x >= max_x || y >= max_y {
            return;
        }

        let row_is_excluded = |row: usize| {
            excluded_clears.iter().any(|clear| {
                clear.height > 0
                    && row >= clear.y
                    && row < clear.y.saturating_add(clear.height)
                    && x >= clear.x
                    && max_x <= clear.x.saturating_add(clear.width)
            })
        };

        let mut span_start = y;
        for row in y..=max_y {
            if row == max_y || row_is_excluded(row) {
                if row > span_start {
                    self.blit_region_from(src, x, span_start, width, row - span_start);
                }
                span_start = row.saturating_add(1);
            }
        }
    }

    pub(crate) fn shift_rows(&mut self, top: usize, bottom: usize, delta: i32) {
        if delta == 0 || top > bottom || bottom >= self.height() {
            return;
        }
        let height = bottom - top + 1;
        let abs_delta = delta.unsigned_abs() as usize;
        if abs_delta >= height {
            for y in top..=bottom {
                self.cells[y].fill(CanvasCell::default());
                self.overlays[y].fill(None);
                self.no_select[y].fill(false);
                self.soft_wrap[y] = 0;
            }
            return;
        }

        if delta > 0 {
            for y in top..=bottom - abs_delta {
                self.cells[y] = self.cells[y + abs_delta].clone();
                self.overlays[y] = self.overlays[y + abs_delta].clone();
                self.no_select[y] = self.no_select[y + abs_delta].clone();
                self.soft_wrap[y] = self.soft_wrap[y + abs_delta];
            }
            for y in bottom - abs_delta + 1..=bottom {
                self.cells[y].fill(CanvasCell::default());
                self.overlays[y].fill(None);
                self.no_select[y].fill(false);
                self.soft_wrap[y] = 0;
            }
        } else {
            for y in (top + abs_delta..=bottom).rev() {
                self.cells[y] = self.cells[y - abs_delta].clone();
                self.overlays[y] = self.overlays[y - abs_delta].clone();
                self.no_select[y] = self.no_select[y - abs_delta].clone();
                self.soft_wrap[y] = self.soft_wrap[y - abs_delta];
            }
            for y in top..top + abs_delta {
                self.cells[y].fill(CanvasCell::default());
                self.overlays[y].fill(None);
                self.no_select[y].fill(false);
                self.soft_wrap[y] = 0;
            }
        }
        self.scroll_hint = None;
    }

    /// Removes fully blank rows from the bottom of the canvas.
    ///
    /// Inline Ink-style renders should be sized to visible content. If a layout
    /// engine overestimates natural height, emitting those trailing empty rows
    /// scrolls useful content out of the terminal viewport. This keeps any row
    /// containing cells, overlays, or a declared native cursor.
    pub fn trim_trailing_blank_rows(&mut self) -> usize {
        let cursor_floor = self
            .cursor_declaration
            .map(|cursor| cursor.y.saturating_add(1))
            .unwrap_or(0);
        let min_height = cursor_floor.max(1);
        let original = self.height();
        while self.height() > min_height {
            let Some(row) = self.cells.last() else {
                break;
            };
            let row_idx = self.height() - 1;
            let row_blank = row.iter().all(CanvasCell::is_empty)
                && self
                    .overlays
                    .get(row_idx)
                    .is_none_or(|row| row.iter().all(Option::is_none));
            if !row_blank {
                break;
            }
            self.cells.pop();
            self.overlays.pop();
            self.no_select.pop();
            self.soft_wrap.pop();
        }
        let removed = original.saturating_sub(self.height());
        if removed > 0 {
            let height_now = self.height();
            if let Some(region) = self.damage_region.as_mut() {
                let bottom = region.y.saturating_add(region.height).min(height_now);
                region.height = bottom.saturating_sub(region.y);
                if region.height == 0 || region.width == 0 {
                    self.damage_region = None;
                }
            }
            if self
                .scroll_hint
                .is_some_and(|hint| hint.bottom >= height_now)
            {
                self.scroll_hint = None;
            }
        }
        removed
    }

    /// Returns the width of the canvas.
    pub fn width(&self) -> usize {
        self.width
    }

    /// Returns the height of the canvas.
    pub fn height(&self) -> usize {
        self.cells.len()
    }

    /// Returns a reference to the cell at the given position, or `None` if
    /// out of bounds.
    pub fn cell(&self, x: usize, y: usize) -> Option<&CanvasCell> {
        self.cells.get(y).and_then(|row| row.get(x))
    }

    fn diff_cell_ref(&self, x: usize, y: usize) -> Option<CanvasDiffCellRef<'_>> {
        Some(CanvasDiffCellRef {
            cell: self.cells.get(y)?.get(x)?,
            overlay: self.overlay_for_diff(y, x),
        })
    }

    /// Returns all per-cell differences needed to transform this canvas into `next`.
    ///
    /// This is the mode-neutral retained-canvas counterpart to CC Ink's
    /// `screen.ts` `diff(...)`: it compares cell content plus post-render
    /// overlays, reports removals when the next canvas shrinks, and reports
    /// additions only for non-empty next cells. It does not write terminal
    /// output, enter fullscreen, or mutate either canvas.
    pub fn diff(&self, next: &Self) -> Vec<CanvasDiffChange> {
        let mut changes = Vec::new();
        self.diff_each(next, |change| {
            changes.push(change.to_owned());
            false
        });
        changes
    }

    /// Calls `callback` for each per-cell difference between this canvas and `next`.
    ///
    /// The callback receives borrowed cell views and may return `true` to stop
    /// iteration early; this method returns whether such an early stop occurred.
    /// The scan includes explicit overlay-only differences so custom retained
    /// renderers can repaint cursor/search/selection changes even when the base
    /// text cells are identical. This is an optimization-only helper for custom
    /// renderers and never performs terminal I/O by itself.
    pub fn diff_each<F>(&self, next: &Self, callback: F) -> bool
    where
        F: FnMut(CanvasDiffChangeRef<'_>) -> bool,
    {
        let max_height = self.height().max(next.height());
        let max_width = self.width.max(next.width);
        self.diff_each_in_bounds(
            next,
            DamageRegion {
                x: 0,
                y: 0,
                width: max_width,
                height: max_height,
            },
            callback,
        )
    }

    /// Calls `callback` for differences inside a bounded canvas region.
    ///
    /// This is the opt-in damage-bounded counterpart to [`Self::diff_each`]:
    /// callers that already know a safe dirty rectangle can avoid scanning rows
    /// outside it. The region is clipped to the union of both canvas extents,
    /// row prefix skipping still uses [`Self::row_change_start`], and callback
    /// semantics match [`Self::diff_each`]. Damage metadata is a scan hint only;
    /// damage-only rows do not emit cell changes.
    pub fn diff_each_in_bounds<F>(&self, next: &Self, bounds: DamageRegion, mut callback: F) -> bool
    where
        F: FnMut(CanvasDiffChangeRef<'_>) -> bool,
    {
        let max_height = self.height().max(next.height());
        let max_width = self.width.max(next.width);
        if max_height == 0 || max_width == 0 || bounds.width == 0 || bounds.height == 0 {
            return false;
        }

        let top = bounds.y.min(max_height);
        let bottom = bounds.y.saturating_add(bounds.height).min(max_height);
        let left = bounds.x.min(max_width);
        let right = bounds.x.saturating_add(bounds.width).min(max_width);
        if bottom <= top || right <= left {
            return false;
        }

        for y in top..bottom {
            let row_start = if y < self.height() && y < next.height() && self.width == next.width {
                self.row_change_start(next, y).unwrap_or(max_width)
            } else {
                0
            }
            .max(left);
            if row_start >= right {
                continue;
            }

            for x in row_start..right {
                let removed = self.diff_cell_ref(x, y);
                let added = next.diff_cell_ref(x, y);
                let changed = match (removed, added) {
                    (Some(prev), Some(next)) => {
                        prev.cell != next.cell || prev.overlay != next.overlay
                    }
                    (None, Some(next)) => !next.is_empty(),
                    (Some(_), None) => true,
                    (None, None) => false,
                };
                if changed
                    && callback(CanvasDiffChangeRef {
                        x,
                        y,
                        removed,
                        added,
                    })
                {
                    return true;
                }
            }
        }

        false
    }

    /// Creates an opt-in packed snapshot of this canvas using caller-owned intern pools.
    ///
    /// This mirrors CC Ink's packed `Screen`/pool optimization at the API
    /// boundary without replacing iocraft's default typed [`Canvas`]. Cell text,
    /// composed post-render styles, OSC 8 links, wide-cell markers, no-select
    /// metadata, soft-wrap metadata, and current damage are copied into a compact
    /// row-major snapshot that custom retained renderers can diff or benchmark.
    pub fn pack_with(&self, pools: &mut CanvasPackedCellPools) -> CanvasPackedScreen {
        let height = self.height();
        let mut cells = Vec::with_capacity(self.width.saturating_mul(height));
        let mut no_select = Vec::with_capacity(self.width.saturating_mul(height));

        for y in 0..height {
            for x in 0..self.width {
                cells.push(pools.intern_cell(&self.cells[y][x], self.overlay_for_diff(y, x)));
                no_select.push(self.no_select[y][x]);
            }
        }

        CanvasPackedScreen {
            width: self.width,
            height,
            cells,
            no_select,
            soft_wrap: self.soft_wrap.clone(),
            damage_region: self.damage_region,
        }
    }

    /// Returns a clone of `next` with repaint/debug overlay applied to changed cells.
    ///
    /// This is a retained-canvas visualization helper inspired by CC Ink's
    /// `CLAUDE_CODE_DEBUG_REPAINTS`: it highlights cells that would be scanned
    /// by a retained diff, plus explicit current/previous damage regions and
    /// full-repaint requests. It is mode-neutral and does not write terminal
    /// output or change render-loop behavior.
    pub fn debug_repaint_overlay(
        previous: Option<&Self>,
        next: &Self,
        overlay: StyleOverlay,
    ) -> Self {
        let mut canvas = next.clone();

        if next.force_full_repaint {
            canvas.set_overlay_rect(0, 0, canvas.width(), canvas.height(), overlay);
            return canvas;
        }

        match previous {
            Some(previous) => {
                previous.diff_each(next, |change| {
                    if change.x < canvas.width() && change.y < canvas.height() {
                        canvas.set_overlay(change.x, change.y, overlay);
                    }
                    false
                });
                if let Some(region) = previous.damage_region() {
                    canvas.set_overlay_rect(
                        region.x,
                        region.y,
                        region.width,
                        region.height,
                        overlay,
                    );
                }
            }
            None => {
                let empty = Canvas::new(0, 0);
                empty.diff_each(next, |change| {
                    if change.x < canvas.width() && change.y < canvas.height() {
                        canvas.set_overlay(change.x, change.y, overlay);
                    }
                    false
                });
            }
        }

        if let Some(region) = next.damage_region() {
            canvas.set_overlay_rect(region.x, region.y, region.width, region.height, overlay);
        }

        canvas
    }

    /// Returns whether a screen cell is visually blank.
    ///
    /// This mirrors CC Ink's `isEmptyCellAt(...)` for click metadata: unwritten
    /// cells and plain default spaces count as blank, while backgrounds,
    /// inverse/underline overlays, hyperlinks, and non-space glyphs count as
    /// non-blank. Out-of-bounds coordinates are blank.
    pub fn cell_is_blank(&self, x: usize, y: usize) -> bool {
        let Some(row) = self.cells.get(y) else {
            return true;
        };
        let Some(cell) = row.get(x) else {
            return true;
        };
        let overlay = self
            .overlays
            .get(y)
            .and_then(|row| row.get(x))
            .and_then(|overlay| overlay.as_ref());
        cell.is_row_trim_empty(overlay)
    }

    /// Extracts plain text from a rectangular region of the canvas.
    ///
    /// Each row within the region produces one line in the result, separated
    /// by newlines. Trailing whitespace on each line is trimmed. Out-of-bounds
    /// coordinates are clamped silently.
    pub fn get_text(&self, x: usize, y: usize, w: usize, h: usize) -> String {
        let mut lines = Vec::with_capacity(h);
        for row_idx in y..y + h {
            let Some(row) = self.cells.get(row_idx) else {
                lines.push(String::new());
                continue;
            };
            let start = x.min(row.len());
            let end = (x + w).min(row.len());
            let slice = &row[start..end];
            let last_non_empty = slice.iter().rposition(|cell| cell.character.is_some());
            let trim_end = match last_non_empty {
                Some(i) => i + 1,
                None => {
                    lines.push(String::new());
                    continue;
                }
            };
            let mut s = String::with_capacity(trim_end);
            for cell in &slice[..trim_end] {
                // WidthTail cells are trailing placeholders of wide characters —
                // they carry no independent content and must be skipped during text
                // extraction. This fixes the phantom space that previously appeared
                // after every CJK character (e.g. "中 文" instead of "中文").
                if cell.cell_width == CellWidth::WidthTail {
                    continue;
                }
                match cell.character.as_ref() {
                    Some(ch) => s.push_str(&ch.value),
                    None => s.push(' '),
                }
            }
            lines.push(s);
        }
        lines.join(
            "
",
        )
    }

    pub(super) fn clear_text(&mut self, x: usize, y: usize, w: usize, h: usize) {
        for y in y..y + h {
            if let Some(row) = self.cells.get_mut(y) {
                for x in x..x + w {
                    if x < row.len() {
                        clear_cell_width_relationship(row, x);
                    }
                }
            }
        }
    }

    pub(super) fn set_background_color(
        &mut self,
        x: usize,
        y: usize,
        w: usize,
        h: usize,
        color: Color,
    ) {
        for y in y..y + h {
            if let Some(row) = self.cells.get_mut(y) {
                for x in x..x + w {
                    if x < row.len() {
                        row[x].background_color = Some(color);
                    }
                }
            }
        }
    }

    fn write_grapheme_to_row(
        row: &mut [CanvasCell],
        x: usize,
        grapheme: &str,
        width: usize,
        style: CanvasTextStyle,
        hyperlink: Option<&str>,
    ) {
        // Remove any stale wide-character relationship before overwriting this cell.
        // This prevents old WidthTail markers from surviving when a wide glyph is
        // replaced by a narrow one, or when writing starts in the tail cell.
        clear_cell_width_relationship(row, x);
        if width >= 2 && x + 1 < row.len() {
            clear_cell_width_relationship(row, x + 1);
        }

        row[x].character = Some(Character {
            value: grapheme.to_string(),
            hyperlink: hyperlink.map(|s| s.to_string()),
            style,
        });
        if width >= 2 {
            row[x].cell_width = CellWidth::Wide;
            if x + 1 < row.len() {
                row[x + 1].character = None;
                row[x + 1].cell_width = CellWidth::WidthTail;
            }
        } else {
            row[x].cell_width = CellWidth::Normal;
        }
    }

    pub(super) fn set_text_row_str_clipped(
        &mut self,
        mut x: isize,
        y: usize,
        min_x: isize,
        max_x: isize,
        text: &str,
        style: CanvasTextStyle,
        hyperlink: Option<&str>,
    ) {
        let row = &mut self.cells[y];
        if row.is_empty() {
            return;
        }
        let min_x = min_x.max(0);
        let max_x = max_x.min(row.len() as isize - 1);
        if min_x > max_x {
            return;
        }

        let graphemes = text.graphemes(true).collect::<Vec<_>>();
        let mut grapheme_idx = 0;
        while grapheme_idx < graphemes.len() {
            let grapheme = graphemes[grapheme_idx];
            if let Some(code) = single_ascii_byte(grapheme).filter(|code| *code <= 0x1f) {
                if code == b'\t' {
                    // POSIX/terminal tab stops are fixed at 8 columns. CC Ink's
                    // output layer expands tabs into blank cells at write time
                    // rather than letting a literal TAB move the physical cursor.
                    let spaces = 8 - x.rem_euclid(8) as usize;
                    for _ in 0..spaces {
                        if x >= min_x && x <= max_x {
                            Self::write_grapheme_to_row(
                                row,
                                x as usize,
                                " ",
                                1,
                                CanvasTextStyle::default(),
                                None,
                            );
                        }
                        x += 1;
                    }
                    grapheme_idx += 1;
                    continue;
                }

                // Mirror CC Ink output.ts: C0 controls must never enter the
                // retained buffer because terminals may move the cursor for
                // them while our virtual cursor treats them as zero-width.
                // ESC starts a control sequence; skip the whole sequence when
                // possible so raw cursor movement/title/string controls do not
                // leak visible bytes into the UI.
                grapheme_idx = if code == 0x1b {
                    skip_escape_sequence_graphemes(&graphemes, grapheme_idx)
                } else {
                    grapheme_idx + 1
                };
                continue;
            }

            let width = grapheme_width(grapheme);
            if width == 0 {
                // Zero-width graphemes (standalone combining marks, DEL/C1
                // controls, ZWJ/ZWS, etc.) must not occupy or mutate retained
                // cells. Base+combining clusters arrive as one grapheme with
                // width 1, so composed accents are still preserved.
                grapheme_idx += 1;
                continue;
            }

            let end_x = x + width as isize;
            if end_x <= min_x {
                x = end_x;
                grapheme_idx += 1;
                continue;
            }
            if x > max_x {
                break;
            }
            if x < min_x {
                // Do not render partial graphemes clipped at the left edge.
                x = end_x;
                grapheme_idx += 1;
                continue;
            }
            if end_x - 1 > max_x {
                // Do not render a partial wide grapheme at the right edge. VT
                // terminals wrap when a double-width glyph crosses the margin;
                // CC Ink's output layer writes a SpacerHead placeholder and
                // later skips it during terminal output, preserving the virtual
                // cursor model without leaving stale cells behind.
                let x = x as usize;
                clear_cell_width_relationship(row, x);
                row[x].character = None;
                row[x].background_color = None;
                row[x].cell_width = CellWidth::SpacerHead;
                break;
            }

            Self::write_grapheme_to_row(row, x as usize, grapheme, width, style, hyperlink);
            x = end_x;
            grapheme_idx += 1;
        }
    }

    fn compose_overlay_slot(slot: &mut Option<StyleOverlay>, overlay: StyleOverlay) {
        *slot = Some(match *slot {
            Some(existing) => overlay.compose_over(existing),
            None => overlay,
        });
    }

    /// Sets a style overlay on a single cell, without altering the cell's original text or style.
    ///
    /// If the cell already has an overlay, `overlay` is composed on top instead
    /// of replacing the lower layer. This matches CC Ink's post-render overlay
    /// pipeline where selection, query search, and current-match search styling
    /// are applied sequentially to the same screen cell.
    ///
    /// If the cell is [`CellWidth::Wide`], the overlay is automatically extended to the
    /// trailing [`CellWidth::WidthTail`] cell as well, preventing the "half-character
    /// inverted" artefact for CJK and emoji characters. Conversely, if the target cell
    /// is a `WidthTail`, the overlay is applied to both the tail and its leading `Wide`
    /// cell. Callers do not need to know whether a cell is wide.
    pub fn set_overlay(&mut self, x: usize, y: usize, overlay: StyleOverlay) {
        if let Some(row) = self.overlays.get_mut(y) {
            if let Some(slot) = row.get_mut(x) {
                Self::compose_overlay_slot(slot, overlay);
            }
        }
        // Auto-expand across the wide character's cells.
        if let Some(cell_row) = self.cells.get(y) {
            if let Some(cell) = cell_row.get(x) {
                match cell.cell_width {
                    CellWidth::Wide => {
                        if let Some(row) = self.overlays.get_mut(y) {
                            if let Some(slot) = row.get_mut(x + 1) {
                                Self::compose_overlay_slot(slot, overlay);
                            }
                        }
                    }
                    CellWidth::WidthTail if x > 0 => {
                        if let Some(row) = self.overlays.get_mut(y) {
                            if let Some(slot) = row.get_mut(x - 1) {
                                Self::compose_overlay_slot(slot, overlay);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    /// Sets a style overlay on every cell in a rectangular region.
    pub fn set_overlay_rect(
        &mut self,
        x: usize,
        y: usize,
        w: usize,
        h: usize,
        overlay: StyleOverlay,
    ) {
        for row_idx in y..y + h {
            if let Some(row) = self.overlays.get_mut(row_idx) {
                for col_idx in x..x + w {
                    if let Some(slot) = row.get_mut(col_idx) {
                        Self::compose_overlay_slot(slot, overlay);
                    }
                }
            }
        }
    }

    /// Clears the overlay on a single cell.
    pub fn clear_overlay(&mut self, x: usize, y: usize) {
        if let Some(row) = self.overlays.get_mut(y) {
            if let Some(slot) = row.get_mut(x) {
                *slot = None;
            }
        }
    }

    /// Clears all overlays on the canvas.
    pub fn clear_overlays(&mut self) {
        for row in &mut self.overlays {
            row.fill(None);
        }
    }

    /// Returns the text style of a cell with its overlay merged in, if the cell exists.
    pub fn resolved_text_style(&self, x: usize, y: usize) -> Option<CanvasTextStyle> {
        let base = self
            .cells
            .get(y)
            .and_then(|r| r.get(x))
            .and_then(|c| c.text_style())
            .copied()
            .unwrap_or_default();
        match self
            .overlays
            .get(y)
            .and_then(|r| r.get(x))
            .and_then(|o| o.as_ref())
        {
            Some(ov) => Some(base.with_overlay(ov)),
            None => self
                .cells
                .get(y)
                .and_then(|r| r.get(x))
                .and_then(|c| c.text_style())
                .copied(),
        }
    }

    /// Gets a subview of the canvas for writing.
    pub fn subview_mut(
        &mut self,
        x: isize,
        y: isize,
        clip_x: isize,
        clip_y: isize,
        clip_width: usize,
        clip_height: usize,
    ) -> CanvasSubviewMut<'_> {
        CanvasSubviewMut {
            y,
            x,
            clip_x,
            clip_y,
            clip_width,
            clip_height,
            canvas: self,
        }
    }

    pub(super) fn row(&self, y: usize) -> &[CanvasCell] {
        let Some(row) = self.cells.get(y) else {
            return &[];
        };
        let overlay_row = self.overlays.get(y);
        // A cell counts as render-significant if it has visible content, visible
        // background/style, a hyperlink, or a visible overlay. Plain trailing spaces
        // are skipped and cleared with erase-to-EOL, matching CC Ink's sparse writer
        // and avoiding right-margin pending-wrap from invisible padding.
        let last_non_empty = row.iter().enumerate().rposition(|(x, cell)| {
            let overlay = overlay_row.and_then(|r| r.get(x)).and_then(|o| o.as_ref());
            !cell.is_row_trim_empty(overlay)
        });
        &row[..last_non_empty.map_or(0, |i| i + 1)]
    }

    /// Returns the row's overlays with trailing `None` entries trimmed, so that an
    /// out-of-bounds row and an existing-but-overlay-free row compare as equal.
    pub(super) fn overlay_row(&self, y: usize) -> &[Option<StyleOverlay>] {
        let Some(row) = self.overlays.get(y) else {
            return &[];
        };
        let last_some = row.iter().rposition(|o| o.is_some());
        &row[..last_some.map_or(0, |i| i + 1)]
    }

    /// Returns the terminal column reached after rendering a row from column 0.
    ///
    /// Rows are rendered sparsely: trailing empty cells are skipped and then cleared
    /// with erase-to-EOL, while interior empty cells are written as spaces to preserve
    /// alignment. This helper mirrors `write_row_impl`'s printable-cell advancement so
    /// the inline renderer can track whether a write actually ended in VT autowrap's
    /// pending-wrap state instead of assuming `canvas.width()` was reached.
    pub(crate) fn ansi_row_rendered_width(&self, y: usize) -> usize {
        let row = self.row(y);
        let mut rendered_width = 0;
        for (x, cell) in row.iter().enumerate() {
            if matches!(
                cell.cell_width,
                CellWidth::WidthTail | CellWidth::SpacerHead
            ) {
                continue;
            }
            let cell_width = cell
                .character
                .as_ref()
                .map(|c| grapheme_width(&c.value).max(1))
                .unwrap_or(1);
            rendered_width = rendered_width.max(x + cell_width);
        }
        rendered_width
    }

    /// Compares a row of this canvas with the same row of another canvas, including
    /// any style overlays.
    ///
    /// Overlays MUST participate in this comparison: the row-level diff renderer uses
    /// `row_eq` to skip unchanged rows, and a moving cursor often changes *only* the
    /// overlay layer (the underlying cells stay identical). If overlays were ignored
    /// here, cursor movement would leave stale inverted cells on screen.
    pub(crate) fn row_eq(&self, other: &Self, y: usize) -> bool {
        self.width == other.width
            && self.row(y) == other.row(y)
            && self.overlay_row(y) == other.overlay_row(y)
    }

    fn row_damage_start(&self, y: usize) -> Option<usize> {
        let region = self.damage_region?;
        if !region.intersects_row(y) || region.width == 0 {
            return None;
        }
        Some(region.x.min(self.width))
    }

    fn cell_for_diff(&self, y: usize, x: usize) -> CanvasCell {
        self.cells
            .get(y)
            .and_then(|row| row.get(x))
            .cloned()
            .unwrap_or_default()
    }

    fn overlay_for_diff(&self, y: usize, x: usize) -> Option<StyleOverlay> {
        self.overlays
            .get(y)
            .and_then(|row| row.get(x))
            .and_then(|overlay| *overlay)
    }

    fn cell_width_for_diff(&self, y: usize, x: usize) -> CellWidth {
        self.cells
            .get(y)
            .and_then(|row| row.get(x))
            .map(|cell| cell.cell_width)
            .unwrap_or(CellWidth::Normal)
    }

    /// Returns the first column that must be repainted to transform `self` into
    /// `other` on row `y`, including overlay-only changes and explicit damage.
    ///
    /// This is the retained-canvas counterpart to CC Ink's `diffEach()` damage
    /// scan. The terminal backend rewrites from this column to EOL, which keeps
    /// shrink/clear semantics simple while avoiding repainting long unchanged
    /// prefixes. If the first difference is a wide-character tail, the start is
    /// expanded to the leading cell so terminal cursor accounting stays valid.
    pub fn row_change_start(&self, other: &Self, y: usize) -> Option<usize> {
        if self.width != other.width {
            return Some(0);
        }

        let damage_start = [self.row_damage_start(y), other.row_damage_start(y)]
            .into_iter()
            .flatten()
            .min();

        let max_len = self.row(y).len().max(other.row(y).len());
        let diff_start = (0..max_len).find(|&x| {
            self.cell_for_diff(y, x) != other.cell_for_diff(y, x)
                || self.overlay_for_diff(y, x) != other.overlay_for_diff(y, x)
        });

        let mut start = match (damage_start, diff_start) {
            (Some(a), Some(b)) => a.min(b),
            (Some(a), None) => a,
            (None, Some(b)) => b,
            (None, None) => return None,
        };

        if start > 0
            && (self.cell_width_for_diff(y, start) == CellWidth::WidthTail
                || other.cell_width_for_diff(y, start) == CellWidth::WidthTail)
        {
            start -= 1;
        }

        Some(start.min(self.width))
    }
}
