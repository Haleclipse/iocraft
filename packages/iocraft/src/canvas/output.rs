use super::*;

/// Represents a writeable region of a [`Canvas`]. All coordinates provided to functions of this
/// type are relative to the subview origin and clipped to its bounds.
pub struct CanvasSubviewMut<'a> {
    pub(super) x: isize,
    pub(super) y: isize,
    pub(super) clip_x: isize,
    pub(super) clip_y: isize,
    pub(super) clip_width: usize,
    pub(super) clip_height: usize,
    pub(super) canvas: &'a mut Canvas,
}

impl CanvasSubviewMut<'_> {
    /// Returns a reference to a cell at the given **relative** subview position.
    ///
    /// Returns `None` if the resulting absolute position is out of bounds or
    /// outside the clip region.
    pub fn cell(&self, x: isize, y: isize) -> Option<&CanvasCell> {
        let abs_x = self.x + x;
        let abs_y = self.y + y;
        if abs_x < self.clip_x
            || abs_y < self.clip_y
            || abs_x < 0
            || abs_y < 0
            || abs_x >= self.clip_x + self.clip_width as isize
            || abs_y >= self.clip_y + self.clip_height as isize
        {
            return None;
        }
        self.canvas.cell(abs_x as usize, abs_y as usize)
    }

    /// Extracts plain text from a rectangular region using **relative** subview
    /// coordinates. The region is clamped to the clip bounds.
    pub fn get_text(&self, x: isize, y: isize, w: usize, h: usize) -> String {
        let mut left = self.x + x;
        let mut top = self.y + y;
        let mut right = left + w as isize;
        let mut bottom = top + h as isize;

        left = left.max(self.clip_x).max(0);
        top = top.max(self.clip_y).max(0);
        right = right.min(self.clip_x + self.clip_width as isize).max(0);
        bottom = bottom.min(self.clip_y + self.clip_height as isize).max(0);

        self.canvas.get_text(
            left as _,
            top as _,
            (right - left).max(0) as _,
            (bottom - top).max(0) as _,
        )
    }

    /// Fills the region with the given color.
    pub fn set_background_color(&mut self, x: isize, y: isize, w: usize, h: usize, color: Color) {
        let mut left = self.x + x;
        let mut top = self.y + y;
        let mut right = left + w as isize;
        let mut bottom = top + h as isize;

        left = left.max(self.clip_x).max(0);
        top = top.max(self.clip_y).max(0);
        right = right.min(self.clip_x + self.clip_width as isize).max(0);
        bottom = bottom.min(self.clip_y + self.clip_height as isize).max(0);

        self.canvas.set_background_color(
            left as _,
            top as _,
            (right - left).max(0) as _,
            (bottom - top).max(0) as _,
            color,
        );
    }

    /// Removes text from the region without touching overlay/damage metadata.
    pub fn clear_text(&mut self, x: isize, y: isize, w: usize, h: usize) {
        let mut left = self.x + x;
        let mut top = self.y + y;
        let mut right = left + w as isize;
        let mut bottom = top + h as isize;

        left = left.max(self.clip_x).max(0);
        top = top.max(self.clip_y).max(0);
        right = right.min(self.clip_x + self.clip_width as isize).max(0);
        bottom = bottom.min(self.clip_y + self.clip_height as isize).max(0);

        self.canvas.clear_text(
            left as _,
            top as _,
            (right - left).max(0) as _,
            (bottom - top).max(0) as _,
        );
    }

    /// Clears cells/styles/hyperlinks/overlays in this subview and marks the
    /// affected rectangle damaged.
    ///
    /// This is the component-local counterpart to [`Canvas::clear_region`],
    /// mirroring CC Ink's `screen.clearRegion(...)` operation. Coordinates are
    /// relative to the subview and clipped to its clip rect before wide-glyph
    /// boundary repair and damage calculation are delegated to the root canvas.
    pub fn clear_region(&mut self, x: isize, y: isize, w: usize, h: usize) {
        let mut left = self.x + x;
        let mut top = self.y + y;
        let mut right = left + w as isize;
        let mut bottom = top + h as isize;

        left = left.max(self.clip_x).max(0);
        top = top.max(self.clip_y).max(0);
        right = right.min(self.clip_x + self.clip_width as isize).max(0);
        bottom = bottom.min(self.clip_y + self.clip_height as isize).max(0);

        self.canvas.clear_region(
            left as _,
            top as _,
            (right - left).max(0) as _,
            (bottom - top).max(0) as _,
        );
    }

    /// Marks a rectangular region as excluded from fullscreen text selection.
    /// Coordinates are relative to the subview and clipped to the subview's clip bounds.
    pub fn mark_no_select_region(&mut self, x: isize, y: isize, w: usize, h: usize) {
        let mut left = self.x + x;
        let mut top = self.y + y;
        let mut right = left + w as isize;
        let mut bottom = top + h as isize;

        left = left.max(self.clip_x).max(0);
        top = top.max(self.clip_y).max(0);
        right = right.min(self.clip_x + self.clip_width as isize).max(0);
        bottom = bottom.min(self.clip_y + self.clip_height as isize).max(0);

        self.canvas.mark_no_select_region(
            left as _,
            top as _,
            (right - left).max(0) as _,
            (bottom - top).max(0) as _,
        );
    }

    /// Marks a relative row as a soft-wrap continuation of the previous row.
    /// `prev_content_end` is relative to this subview and is translated to an
    /// absolute canvas column before being stored.
    pub fn mark_soft_wrap_continuation(&mut self, y: isize, prev_content_end: usize) {
        let abs_y = self.y + y;
        if abs_y < self.clip_y || abs_y < 0 || abs_y >= self.clip_y + self.clip_height as isize {
            return;
        }
        let abs_prev_content_end = (self.x + prev_content_end as isize).max(0) as usize;
        self.canvas
            .mark_soft_wrap_continuation(abs_y as usize, abs_prev_content_end);
    }

    /// Copies a rectangular region from `src` into this subview.
    ///
    /// This is useful for custom retained-screen components that produce an
    /// offscreen [`Canvas`] with metadata such as selection overlays,
    /// `noSelect`, and `softWrap`, then blit it into the component's allocated
    /// layout box. Coordinates are clipped to both the subview clip rect and the
    /// source canvas. Copied cells are marked damaged so terminal diff writers
    /// repaint post-render overlays even when the underlying text is unchanged.
    pub fn blit_region_from(
        &mut self,
        src: &Canvas,
        dst_x: isize,
        dst_y: isize,
        src_x: usize,
        src_y: usize,
        width: usize,
        height: usize,
    ) {
        self.blit_region_from_impl(src, dst_x, dst_y, src_x, src_y, width, height, true);
    }

    /// Copies a rectangular region from `src` without marking terminal-output damage.
    ///
    /// This is the clean-blit counterpart to [`Self::blit_region_from`]. Use it
    /// only when the restored cells are known to match the previous terminal
    /// frame; otherwise the terminal writer may skip a repaint that is required
    /// to repair stale physical output.
    pub fn blit_region_from_clean(
        &mut self,
        src: &Canvas,
        dst_x: isize,
        dst_y: isize,
        src_x: usize,
        src_y: usize,
        width: usize,
        height: usize,
    ) {
        self.blit_region_from_impl(src, dst_x, dst_y, src_x, src_y, width, height, false);
    }

    fn blit_region_from_impl(
        &mut self,
        src: &Canvas,
        dst_x: isize,
        dst_y: isize,
        src_x: usize,
        src_y: usize,
        width: usize,
        height: usize,
        mark_damage: bool,
    ) {
        if width == 0 || height == 0 || src_x >= src.width() || src_y >= src.height() {
            return;
        }

        let mut src_left = src_x as isize;
        let mut src_top = src_y as isize;
        let mut dst_left = self.x + dst_x;
        let mut dst_top = self.y + dst_y;
        let mut copy_width = width as isize;
        let mut copy_height = height as isize;

        let clip_left = self.clip_x.max(0);
        let clip_top = self.clip_y.max(0);
        let clip_right = (self.clip_x + self.clip_width as isize)
            .min(self.canvas.width() as isize)
            .max(0);
        let clip_bottom = (self.clip_y + self.clip_height as isize)
            .min(self.canvas.height() as isize)
            .max(0);

        if dst_left < clip_left {
            let delta = clip_left - dst_left;
            dst_left += delta;
            src_left += delta;
            copy_width -= delta;
        }
        if dst_top < clip_top {
            let delta = clip_top - dst_top;
            dst_top += delta;
            src_top += delta;
            copy_height -= delta;
        }

        copy_width = copy_width
            .min(clip_right - dst_left)
            .min(src.width() as isize - src_left);
        copy_height = copy_height
            .min(clip_bottom - dst_top)
            .min(src.height() as isize - src_top);

        if copy_width <= 0 || copy_height <= 0 {
            return;
        }

        let src_left = src_left as usize;
        let src_top = src_top as usize;
        let dst_left = dst_left as usize;
        let dst_top = dst_top as usize;
        let copy_width = copy_width as usize;
        let copy_height = copy_height as usize;
        let mut damage_width = copy_width;

        for row_offset in 0..copy_height {
            let src_row = src_top + row_offset;
            let dst_row = dst_top + row_offset;
            let src_right = src_left + copy_width;
            let dst_right = dst_left + copy_width;

            self.canvas.cells[dst_row][dst_left..dst_right]
                .clone_from_slice(&src.cells[src_row][src_left..src_right]);
            self.canvas.overlays[dst_row][dst_left..dst_right]
                .clone_from_slice(&src.overlays[src_row][src_left..src_right]);
            self.canvas.no_select[dst_row][dst_left..dst_right]
                .clone_from_slice(&src.no_select[src_row][src_left..src_right]);

            let src_soft_wrap = src.soft_wrap[src_row];
            self.canvas.soft_wrap[dst_row] = if src_soft_wrap > 0 {
                let translated = if src_soft_wrap <= src_left {
                    dst_left
                } else {
                    dst_left + src_soft_wrap.saturating_sub(src_left)
                };
                translated.min(self.canvas.width())
            } else {
                0
            };

            if src_right < src.width()
                && dst_right < self.canvas.width()
                && (dst_right as isize) < clip_right
                && src.cells[src_row][src_right - 1].cell_width == CellWidth::Wide
            {
                self.canvas.cells[dst_row][dst_right] = CanvasCell {
                    cell_width: CellWidth::WidthTail,
                    ..Default::default()
                };
                self.canvas.overlays[dst_row][dst_right] = None;
                damage_width = damage_width.max(copy_width + 1);
            }
        }

        if mark_damage {
            self.canvas.mark_damage(DamageRegion {
                x: dst_left,
                y: dst_top,
                width: damage_width,
                height: copy_height,
            });
        }
    }

    /// Declares the physical cursor position at the given **relative** subview position.
    /// Out-of-bounds or outside-clip positions are silently ignored.
    /// See [`Canvas::declare_cursor`].
    pub fn declare_cursor(&mut self, x: isize, y: isize, visible: bool) {
        let abs_x = self.x + x;
        let abs_y = self.y + y;
        if abs_x < self.clip_x
            || abs_y < self.clip_y
            || abs_x < 0
            || abs_y < 0
            || abs_x >= self.clip_x + self.clip_width as isize
            || abs_y >= self.clip_y + self.clip_height as isize
        {
            return;
        }
        self.canvas
            .declare_cursor(abs_x as usize, abs_y as usize, visible);
    }

    /// Sets a style overlay on a cell at the given **relative** subview position.
    /// Out-of-bounds or outside-clip positions are silently ignored.
    pub fn set_overlay(&mut self, x: isize, y: isize, overlay: StyleOverlay) {
        let abs_x = self.x + x;
        let abs_y = self.y + y;
        if abs_x < self.clip_x
            || abs_y < self.clip_y
            || abs_x < 0
            || abs_y < 0
            || abs_x >= self.clip_x + self.clip_width as isize
            || abs_y >= self.clip_y + self.clip_height as isize
        {
            return;
        }
        self.canvas
            .set_overlay(abs_x as usize, abs_y as usize, overlay);
    }

    /// Writes text to the region.
    pub fn set_text(&mut self, x: isize, y: isize, text: &str, style: CanvasTextStyle) {
        self.set_text_with_link(x, y, text, style, None);
    }

    /// Writes text to the region, optionally wrapping it in an OSC 8 hyperlink.
    pub fn set_text_with_link(
        &mut self,
        x: isize,
        y: isize,
        text: &str,
        style: CanvasTextStyle,
        hyperlink: Option<&str>,
    ) {
        let x = self.x + x;
        let min_x = self.clip_x.max(0);
        let max_x = self.clip_x + self.clip_width as isize - 1;
        let min_y = self.clip_y.max(0);
        let max_y = (self.clip_y + self.clip_height as isize).min(self.canvas.height() as _) - 1;
        let mut y = self.y + y;
        for line in text.lines() {
            if y >= min_y && y <= max_y {
                self.canvas
                    .set_text_row_str_clipped(x, y as usize, min_x, max_x, line, style, hyperlink);
            }
            y += 1;
        }
    }
}
