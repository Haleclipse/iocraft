use super::*;

impl Canvas {
    pub(super) fn write_row_impl<W: Write>(
        &self,
        y: usize,
        mut w: W,
        ansi: bool,
        start_col: usize,
    ) -> io::Result<()> {
        let row = self.row(y);
        let overlay_row = self.overlays.get(y);

        let mut background_color = None;
        let mut text_style = CanvasTextStyle::default();
        let mut active_hyperlink: Option<String> = None;
        let mut col = start_col.min(row.len());
        while col < row.len() {
            let cell_start_col = col;
            let cell = &row[col];
            let overlay = overlay_row
                .and_then(|r| r.get(col))
                .and_then(|o| o.as_ref());

            // Compute the effective text style: base character style merged with overlay.
            // For empty cells with an overlay, start from default and merge the overlay
            // so that e.g. a cursor overlay on an empty cell still emits SGR 7.
            let (effective_style, has_style) = match (&cell.character, overlay) {
                (Some(c), Some(ov)) => (c.style.with_overlay(ov), true),
                (Some(c), None) => (c.style, true),
                (None, Some(ov)) => (CanvasTextStyle::default().with_overlay(ov), true),
                (None, None) => (CanvasTextStyle::default(), false),
            };

            // Effective background: overlay can override the cell's background.
            let effective_bg = match overlay.and_then(|ov| ov.background_color) {
                Some(bg) => bg,
                None => cell.background_color,
            };

            if ansi && has_style {
                let mut needs_reset = false;
                if effective_style.weight != text_style.weight
                    && effective_style.weight == Weight::Normal
                {
                    needs_reset = true;
                }
                if !effective_style.underline && text_style.underline {
                    needs_reset = true;
                }
                if !effective_style.italic && text_style.italic {
                    needs_reset = true;
                }
                if !effective_style.blink && text_style.blink {
                    needs_reset = true;
                }
                if !effective_style.hidden && text_style.hidden {
                    needs_reset = true;
                }
                if !effective_style.strikethrough && text_style.strikethrough {
                    needs_reset = true;
                }
                if !effective_style.overline && text_style.overline {
                    needs_reset = true;
                }
                if !effective_style.invert && text_style.invert {
                    needs_reset = true;
                }
                if needs_reset {
                    sgr_reset(&mut w)?;
                    background_color = None;
                    text_style = CanvasTextStyle::default();
                }

                if effective_style.color != text_style.color {
                    sgr_fg(&mut w, effective_style.color.unwrap_or(Color::Reset))?;
                }

                if effective_style.underline_color != text_style.underline_color {
                    sgr_underline_color(
                        &mut w,
                        effective_style.underline_color.unwrap_or(Color::Reset),
                    )?;
                }

                if effective_style.weight != text_style.weight {
                    match effective_style.weight {
                        Weight::Bold => sgr_attr(&mut w, Attribute::Bold)?,
                        Weight::Normal => {}
                        Weight::Light => sgr_attr(&mut w, Attribute::Dim)?,
                    }
                }

                if effective_style.underline
                    && (!text_style.underline
                        || effective_style.underline_style != text_style.underline_style)
                {
                    sgr_attr(&mut w, effective_style.underline_style.attribute())?;
                }

                if effective_style.italic && !text_style.italic {
                    sgr_attr(&mut w, Attribute::Italic)?;
                }

                if effective_style.blink && !text_style.blink {
                    sgr_attr(&mut w, Attribute::SlowBlink)?;
                }

                if effective_style.hidden && !text_style.hidden {
                    sgr_attr(&mut w, Attribute::Hidden)?;
                }

                if effective_style.strikethrough && !text_style.strikethrough {
                    sgr_attr(&mut w, Attribute::CrossedOut)?;
                }

                if effective_style.overline && !text_style.overline {
                    sgr_attr(&mut w, Attribute::OverLined)?;
                }

                if effective_style.invert && !text_style.invert {
                    sgr_attr(&mut w, Attribute::Reverse)?;
                }

                text_style = effective_style;
            } else if ansi && !has_style {
                // Empty cell without overlay — reset active attributes if needed.
                if text_style.underline
                    || text_style.underline_color.is_some()
                    || text_style.blink
                    || text_style.hidden
                    || text_style.strikethrough
                    || text_style.overline
                    || text_style.invert
                {
                    sgr_reset(&mut w)?;
                    background_color = None;
                    text_style = CanvasTextStyle::default();
                }
            }

            // Spacer cells are placeholders for wide-character layout. The
            // terminal cursor either already advanced past WidthTail from the
            // preceding Wide cell, or must not enter pending-wrap for SpacerHead
            // at the right edge. Skip them entirely.
            if matches!(
                cell.cell_width,
                CellWidth::WidthTail | CellWidth::SpacerHead
            ) {
                col += 1;
                continue;
            }

            let cell_display_width = if let Some(c) = &cell.character {
                grapheme_width(&c.value).max(1)
            } else {
                1
            };
            col += cell_display_width;

            if ansi && effective_bg != background_color {
                sgr_bg(&mut w, effective_bg.unwrap_or(Color::Reset))?;
                background_color = effective_bg;
            }

            // OSC 8 hyperlink: emit open/close sequences around the character.
            if ansi {
                if let Some(c) = &cell.character {
                    if c.hyperlink.as_deref() != active_hyperlink.as_deref() {
                        if active_hyperlink.is_some() {
                            hyperlink_close(&mut w)?;
                        }
                        if let Some(href) = &c.hyperlink {
                            hyperlink_open(&mut w, href)?;
                        }
                        active_hyperlink = c.hyperlink.clone();
                    }
                } else if active_hyperlink.is_some() {
                    hyperlink_close(&mut w)?;
                    active_hyperlink = None;
                }
            }

            if let Some(c) = &cell.character {
                if ansi
                    && cell_display_width == 2
                    && c.needs_width_compensation()
                    && cell_start_col + 1 < self.width
                {
                    // CC Ink's robust emoji compensation: prefill the second
                    // cell with a styled/background-colored space, return to
                    // the emoji start, write the emoji, then force the cursor
                    // to the expected post-wide-cell column. On correct
                    // terminals the emoji overwrites the prefilled space; on
                    // stale-width terminals the space fills the gap.
                    write!(
                        w,
                        "\x1b[{}G \x1b[{}G{}\x1b[{}G",
                        cell_start_col + 2,
                        cell_start_col + 1,
                        c.value,
                        cell_start_col + cell_display_width + 1
                    )?;
                } else {
                    write!(w, "{}{}", c.value, " ".repeat(c.required_padding()))?;
                }
            } else {
                w.write_all(b" ")?;
            }
        }
        // Row-end: single exit path for erase-to-EOL. Reset only the
        // attributes that would bleed into the erased area, then clear.
        if ansi {
            if active_hyperlink.is_some() {
                hyperlink_close(&mut w)?;
            }
            if background_color.is_some()
                || text_style.underline
                || text_style.underline_color.is_some()
                || text_style.blink
                || text_style.hidden
                || text_style.strikethrough
                || text_style.overline
                || text_style.invert
                || text_style.weight != Weight::Normal
            {
                sgr_reset(&mut w)?;
            }
            erase_to_eol(&mut w)?;
            sgr_reset(&mut w)?;
        }
        Ok(())
    }

    /// Writes a single row's ANSI representation without a trailing newline.
    ///
    /// The caller must ensure SGR state is reset before this is called (the
    /// terminal's default state qualifies). The function leaves SGR state
    /// reset on return, so a sequence of calls — separated only by cursor
    /// movement — will each start from a clean state.
    pub(crate) fn write_ansi_row_without_newline<W: Write>(
        &self,
        y: usize,
        w: W,
    ) -> io::Result<()> {
        self.write_row_impl(y, w, true, 0)
    }

    /// Writes a single row's ANSI representation from `start_col` through EOL.
    ///
    /// The caller must position the terminal cursor at `start_col` first and
    /// ensure SGR state is reset. The function leaves SGR state reset on return.
    pub(crate) fn write_ansi_row_from_col_without_newline<W: Write>(
        &self,
        y: usize,
        start_col: usize,
        w: W,
    ) -> io::Result<()> {
        self.write_row_impl(y, w, true, start_col)
    }

    fn write_impl<W: Write>(
        &self,
        mut w: W,
        ansi: bool,
        omit_final_newline: bool,
    ) -> io::Result<()> {
        if ansi {
            sgr_reset(&mut w)?;
        }
        for y in 0..self.cells.len() {
            self.write_row_impl(y, &mut w, ansi, 0)?;
            let is_final_line = y == self.cells.len() - 1;
            if !omit_final_newline || !is_final_line {
                if ansi {
                    // add a carriage return in case we're in raw mode
                    w.write_all(b"\r\n")?;
                } else {
                    w.write_all(b"\n")?;
                }
            }
        }
        w.flush()?;
        Ok(())
    }

    /// Writes the canvas to the given writer with ANSI escape codes.
    pub fn write_ansi<W: Write>(&self, w: W) -> io::Result<()> {
        self.write_impl(w, true, false)
    }

    pub(crate) fn write_ansi_without_final_newline<W: Write>(&self, w: W) -> io::Result<()> {
        self.write_impl(w, true, true)
    }

    /// Writes the canvas to the given writer as unstyled text, without ANSI escape codes.
    pub fn write<W: Write>(&self, w: W) -> io::Result<()> {
        self.write_impl(w, false, false)
    }
}

impl Display for Canvas {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut buf = Vec::with_capacity(self.width * self.cells.len());
        self.write(&mut buf).unwrap();
        f.write_str(&String::from_utf8_lossy(&buf))?;
        Ok(())
    }
}
