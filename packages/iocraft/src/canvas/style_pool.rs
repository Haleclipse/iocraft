use super::*;

/// Terminal underline style variant.
///
/// This mirrors CC Ink's termio `UnderlineStyle` parser. The plain
/// [`CanvasTextStyle::underline`] boolean remains the high-level on/off switch;
/// this enum selects which SGR 4 variant is emitted when underline is enabled.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum UnderlineStyle {
    /// SGR 4 / 4:1 single underline.
    #[default]
    Single,
    /// SGR 21 / 4:2 double underline.
    Double,
    /// SGR 4:3 curly underline / undercurl.
    Curly,
    /// SGR 4:4 dotted underline.
    Dotted,
    /// SGR 4:5 dashed underline.
    Dashed,
}

impl UnderlineStyle {
    pub(super) fn attribute(self) -> Attribute {
        match self {
            Self::Single => Attribute::Underlined,
            Self::Double => Attribute::DoubleUnderlined,
            Self::Curly => Attribute::Undercurled,
            Self::Dotted => Attribute::Underdotted,
            Self::Dashed => Attribute::Underdashed,
        }
    }
}

/// Describes the style of text to be rendered via a [`Canvas`].
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct CanvasTextStyle {
    /// The color of the text.
    pub color: Option<Color>,

    /// The weight of the text.
    pub weight: Weight,

    /// Whether the text is underlined.
    pub underline: bool,

    /// Which underline variant to use when [`Self::underline`] is enabled.
    pub underline_style: UnderlineStyle,

    /// Optional underline color (SGR 58/59).
    pub underline_color: Option<Color>,

    /// Whether the text is italicized.
    pub italic: bool,

    /// Whether the text should blink.
    pub blink: bool,

    /// Whether the text should be concealed/hidden.
    pub hidden: bool,

    /// Whether the text is struck through.
    pub strikethrough: bool,

    /// Whether the text is overlined.
    pub overline: bool,

    /// Whether the foreground and background colors should be inverted.
    pub invert: bool,
}

impl CanvasTextStyle {
    /// Produce a new style by merging an overlay on top of `self`.
    /// `None` fields in the overlay leave the original value; `Some` fields override.
    pub fn with_overlay(&self, o: &StyleOverlay) -> Self {
        Self {
            color: o.color.unwrap_or(self.color),
            weight: o.weight.unwrap_or(self.weight),
            underline: o.underline.unwrap_or(self.underline),
            underline_style: o.underline_style.unwrap_or(self.underline_style),
            underline_color: o.underline_color.unwrap_or(self.underline_color),
            italic: o.italic.unwrap_or(self.italic),
            blink: o.blink.unwrap_or(self.blink),
            hidden: o.hidden.unwrap_or(self.hidden),
            strikethrough: o.strikethrough.unwrap_or(self.strikethrough),
            overline: o.overline.unwrap_or(self.overline),
            invert: o.invert.unwrap_or(self.invert),
        }
    }
}

/// Fully resolved terminal style for ANSI transition serialization.
///
/// This is the Rust-native counterpart to CC Ink's `StylePool.transition(...)`
/// cache key: text attributes and background are explicit typed fields instead
/// of packed style IDs. It intentionally does not include OSC 8 hyperlinks;
/// hyperlink transitions remain separate terminal operations.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct CanvasResolvedStyle {
    /// Resolved text attributes for the cell.
    pub text: CanvasTextStyle,
    /// Resolved background color for the cell.
    pub background_color: Option<Color>,
}

impl CanvasResolvedStyle {
    /// Applies a post-render style overlay and returns the resolved style.
    ///
    /// This mirrors CC Ink's style-pool overlay helpers (`withInverse`,
    /// `withSelectionBg`, and `withCurrentMatch`) while keeping iocraft's public
    /// API typed instead of exposing ANSI-token arrays or packed bit flags.
    pub fn with_overlay(self, overlay: StyleOverlay) -> Self {
        Self {
            text: self.text.with_overlay(&overlay),
            background_color: overlay.background_color.unwrap_or(self.background_color),
        }
    }

    /// Returns whether this style has a visible effect on a space cell.
    ///
    /// CC Ink encodes this as bit 0 of packed `StylePool` IDs so sparse row
    /// renderers can skip foreground-only spaces while preserving spaces with
    /// backgrounds, inverse, underline, strikethrough, or overline. iocraft
    /// keeps style IDs opaque and exposes the same decision as a typed helper.
    pub fn is_visible_on_space(self) -> bool {
        self.background_color.is_some()
            || self.text.invert
            || self.text.underline
            || self.text.strikethrough
            || self.text.overline
    }
}

impl From<CanvasTextStyle> for CanvasResolvedStyle {
    fn from(text: CanvasTextStyle) -> Self {
        Self {
            text,
            background_color: None,
        }
    }
}

pub(super) fn write_canvas_style_transition<W: Write>(
    mut w: W,
    from: CanvasResolvedStyle,
    to: CanvasResolvedStyle,
) -> io::Result<()> {
    let mut text_style = from.text;
    let mut background_color = from.background_color;
    let effective_style = to.text;
    let effective_bg = to.background_color;

    let mut needs_reset = false;
    if effective_style.weight != text_style.weight && effective_style.weight == Weight::Normal {
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
        && (!text_style.underline || effective_style.underline_style != text_style.underline_style)
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

    if effective_bg != background_color {
        sgr_bg(&mut w, effective_bg.unwrap_or(Color::Reset))?;
    }

    Ok(())
}

/// Serializes the ANSI SGR transition between two resolved canvas styles.
///
/// This mirrors iocraft's row writer and CC Ink's `StylePool.transition(...)`
/// semantics as a mode-neutral helper for custom renderers. It is purely a
/// serialization utility: it does not write text, manage hyperlinks, cache by
/// default, or change terminal/screen mode.
pub fn canvas_style_transition_to_ansi(
    from: CanvasResolvedStyle,
    to: CanvasResolvedStyle,
) -> String {
    if from == to {
        return String::new();
    }

    let mut out = Vec::new();
    write_canvas_style_transition(&mut out, from, to).expect("Vec writes cannot fail");
    String::from_utf8(out).expect("SGR escape sequences are valid UTF-8")
}

/// Opt-in cache for repeated canvas style transition serialization.
///
/// CC Ink caches `StylePool.transition(fromId, toId)` because terminal diffing
/// often repeats the same SGR transitions. This Rust-first helper keeps the
/// cache explicit and typed for custom renderers/benchmarks; iocraft's default
/// `Canvas` writer remains straightforward and does not expose packed style IDs.
#[derive(Clone, Debug, Default)]
pub struct CanvasStyleTransitionCache {
    transitions: HashMap<(CanvasResolvedStyle, CanvasResolvedStyle), String>,
}

impl CanvasStyleTransitionCache {
    /// Creates an empty transition cache.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the cached ANSI transition from `from` to `to`, computing it on first use.
    pub fn transition(&mut self, from: CanvasResolvedStyle, to: CanvasResolvedStyle) -> &str {
        self.transitions
            .entry((from, to))
            .or_insert_with(|| canvas_style_transition_to_ansi(from, to))
            .as_str()
    }

    /// Number of cached transition pairs.
    pub fn len(&self) -> usize {
        self.transitions.len()
    }

    /// Whether the cache is empty.
    pub fn is_empty(&self) -> bool {
        self.transitions.is_empty()
    }

    /// Clears all cached transition strings.
    pub fn clear(&mut self) {
        self.transitions.clear();
    }
}

#[derive(Clone, Debug, PartialEq)]
struct CanvasAnsiRowCacheEntry {
    row: Vec<CanvasCell>,
    overlays: Vec<Option<StyleOverlay>>,
    ansi: Vec<u8>,
}

/// Opt-in cache for repeated ANSI row serialization.
///
/// CC Ink keeps an `Output.charCache` across renderer frames so unchanged lines
/// avoid repeated tokenization, grapheme clustering, style resolution, and ANSI
/// generation. This Rust helper keeps the same optimization explicit and
/// typed: it snapshots the trimmed row cells plus post-render overlays and
/// reuses the serialized ANSI row only while that snapshot is unchanged. It
/// does not change the default canvas writer or expose packed screen/style IDs.
#[derive(Clone, Debug, Default)]
pub struct CanvasAnsiRowCache {
    rows: HashMap<(usize, usize), CanvasAnsiRowCacheEntry>,
}

impl CanvasAnsiRowCache {
    /// Creates an empty row serialization cache.
    pub fn new() -> Self {
        Self::default()
    }

    /// Writes a full ANSI row, reusing cached bytes when the row is unchanged.
    pub fn write_row<W: Write>(&mut self, canvas: &Canvas, y: usize, w: W) -> io::Result<()> {
        self.write_row_from_col(canvas, y, 0, w)
    }

    /// Writes an ANSI row from `start_col`, reusing cached bytes when unchanged.
    ///
    /// The caller must position the terminal cursor at `start_col` and ensure
    /// SGR state is reset, matching the built-in row writer contract.
    pub fn write_row_from_col<W: Write>(
        &mut self,
        canvas: &Canvas,
        y: usize,
        start_col: usize,
        mut w: W,
    ) -> io::Result<()> {
        let key = (y, start_col);
        let row = canvas.row(y).to_vec();
        let overlays = canvas
            .overlays
            .get(y)
            .map(|overlays| overlays.iter().take(row.len()).copied().collect::<Vec<_>>())
            .unwrap_or_else(|| vec![None; row.len()]);

        if let Some(entry) = self.rows.get(&key) {
            if entry.row == row && entry.overlays == overlays {
                w.write_all(&entry.ansi)?;
                return Ok(());
            }
        }

        let mut ansi = Vec::new();
        canvas.write_row_impl(y, &mut ansi, true, start_col)?;
        w.write_all(&ansi)?;
        self.rows.insert(
            key,
            CanvasAnsiRowCacheEntry {
                row,
                overlays,
                ansi,
            },
        );
        Ok(())
    }

    /// Number of cached row/start-column entries.
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Whether the cache is empty.
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Clears all cached rows.
    pub fn clear(&mut self) {
        self.rows.clear();
    }
}
