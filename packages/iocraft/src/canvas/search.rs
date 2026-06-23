use super::*;

impl Canvas {
    fn searchable_row_text_in_cols(
        &self,
        row: usize,
        start_col: usize,
        end_col: usize,
    ) -> (String, Vec<usize>, Vec<usize>) {
        let mut text = String::new();
        let mut col_of_cell = Vec::new();
        let mut byte_to_cell = Vec::new();
        let end_col = end_col.min(self.width);
        for col in start_col.min(end_col)..end_col {
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

            let lower = cell
                .character
                .as_ref()
                .map(|character| character.value.to_lowercase())
                .unwrap_or_else(|| " ".to_string());
            let cell_idx = col_of_cell.len();
            byte_to_cell.extend(std::iter::repeat_n(cell_idx, lower.len()));
            text.push_str(&lower);
            col_of_cell.push(col);
        }
        (text, col_of_cell, byte_to_cell)
    }

    fn scan_text_positions_absolute(
        &self,
        query: &str,
        start_x: usize,
        start_y: usize,
        max_x: usize,
        max_y: usize,
        relative_to_region: bool,
    ) -> Vec<TextMatchPosition> {
        let query = query.to_lowercase();
        if query.is_empty() || self.width == 0 || self.height() == 0 {
            return Vec::new();
        }

        let mut positions = Vec::new();
        for row in start_y.min(max_y)..max_y.min(self.height()) {
            let (text, col_of_cell, byte_to_cell) =
                self.searchable_row_text_in_cols(row, start_x, max_x);
            let mut search_from = 0;
            while search_from <= text.len() {
                let Some(relative_pos) = text[search_from..].find(&query) else {
                    break;
                };
                let pos = search_from + relative_pos;
                let end_byte = pos + query.len() - 1;
                let (Some(&start_cell), Some(&end_cell)) =
                    (byte_to_cell.get(pos), byte_to_cell.get(end_byte))
                else {
                    break;
                };
                let (Some(&start_col), Some(&end_col)) =
                    (col_of_cell.get(start_cell), col_of_cell.get(end_cell))
                else {
                    break;
                };
                let end_width = self
                    .cell(end_col, row)
                    .map(|cell| usize::from(cell.cell_width == CellWidth::Wide) + 1)
                    .unwrap_or(1);
                let absolute_end = end_col.saturating_add(end_width).min(max_x);
                let (out_row, out_col) = if relative_to_region {
                    (
                        row.saturating_sub(start_y),
                        start_col.saturating_sub(start_x),
                    )
                } else {
                    (row, start_col)
                };
                positions.push(TextMatchPosition {
                    row: out_row,
                    col: out_col,
                    len: absolute_end.saturating_sub(start_col),
                });
                search_from = pos + query.len();
            }
        }
        positions
    }

    /// Scans the rendered canvas for non-overlapping case-insensitive matches.
    ///
    /// This mirrors CC Ink's `scanPositions(...)` / `applySearchHighlight(...)`
    /// screen-space search: it searches what is rendered, skips `noSelect` cells
    /// and wide-character tails, and reports match spans in terminal cells.
    pub fn scan_text_positions(&self, query: &str) -> Vec<TextMatchPosition> {
        self.scan_text_positions_absolute(query, 0, 0, self.width, self.height(), false)
    }

    /// Scans a rectangular rendered region and returns match positions relative
    /// to that region's top-left corner.
    ///
    /// This is the Canvas-level counterpart to CC Ink's `scanElementSubtree`:
    /// callers can render or identify a subtree/viewport region, scan exactly
    /// what is visible there, and later feed the returned relative positions to
    /// [`Canvas::apply_positioned_highlight`] with an appropriate row offset.
    pub fn scan_text_positions_region(
        &self,
        x: usize,
        y: usize,
        width: usize,
        height: usize,
        query: &str,
    ) -> Vec<TextMatchPosition> {
        if width == 0 || height == 0 || x >= self.width || y >= self.height() {
            return Vec::new();
        }
        self.scan_text_positions_absolute(
            query,
            x,
            y,
            x.saturating_add(width).min(self.width),
            y.saturating_add(height).min(self.height()),
            true,
        )
    }

    fn apply_overlay_to_match(
        &mut self,
        position: TextMatchPosition,
        overlay: StyleOverlay,
    ) -> bool {
        if position.row >= self.height() || position.len == 0 {
            return false;
        }
        let mut damage: Option<DamageRegion> = None;
        let end = position.col.saturating_add(position.len).min(self.width);
        for col in position.col..end {
            if self.is_no_select(col, position.row) {
                continue;
            }
            let Some(cell) = self.cell(col, position.row) else {
                continue;
            };
            if matches!(
                cell.cell_width,
                CellWidth::WidthTail | CellWidth::SpacerHead
            ) {
                continue;
            }
            self.set_overlay(col, position.row, overlay);
            let cell_damage = DamageRegion {
                x: col,
                y: position.row,
                width: 1,
                height: 1,
            };
            damage = Some(match damage {
                Some(existing) => existing.union(cell_damage),
                None => cell_damage,
            });
        }

        if let Some(region) = damage {
            self.mark_damage(region);
            true
        } else {
            false
        }
    }

    /// Applies a search-highlight overlay to all visible matches of `query`.
    ///
    /// Returns `true` if at least one cell was highlighted. Matches are
    /// non-overlapping and case-insensitive, and `noSelect` cells are not search
    /// targets, matching CC Ink's screen-space search behavior.
    pub fn apply_search_highlight(&mut self, query: &str, overlay: StyleOverlay) -> bool {
        let positions = self.scan_text_positions(query);
        let mut applied = false;
        for position in positions {
            applied |= self.apply_overlay_to_match(position, overlay);
        }
        applied
    }

    /// Applies an overlay to a pre-scanned match position plus a row offset.
    ///
    /// This is the iocraft counterpart to CC Ink's `applyPositionedHighlight`:
    /// positions can be relative to a message/subtree and then translated into
    /// the current screen by adding `row_offset`.
    pub fn apply_positioned_highlight(
        &mut self,
        positions: &[TextMatchPosition],
        row_offset: isize,
        current_idx: usize,
        overlay: StyleOverlay,
    ) -> bool {
        let Some(position) = positions.get(current_idx).copied() else {
            return false;
        };
        let row = position.row as isize + row_offset;
        if row < 0 {
            return false;
        }
        self.apply_overlay_to_match(
            TextMatchPosition {
                row: row as usize,
                ..position
            },
            overlay,
        )
    }
}
