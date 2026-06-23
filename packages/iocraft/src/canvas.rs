use crate::ansi::{
    erase_to_eol, hyperlink_close, hyperlink_open, sgr_attr, sgr_bg, sgr_fg, sgr_reset,
    sgr_underline_color,
};
use crate::style::{Color, Weight};
use crossterm::style::Attribute;
use std::{
    collections::HashMap,
    env,
    fmt::{self, Display},
    io::{self, Write},
    sync::{Mutex, Once, OnceLock},
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthChar;

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
struct Character {
    value: String,
    style: CanvasTextStyle,
    hyperlink: Option<String>,
}

/// Compute the **terminal display width** of a string, measuring by grapheme clusters.
///
/// This is the replacement for `UnicodeWidthStr::width()` throughout the rendering
/// pipeline: it counts the same way terminals render, including ZWJ emoji families
/// and CJK characters.
pub(crate) fn string_display_width(s: &str) -> usize {
    string_display_width_from_col(s, 0)
}

/// Compute terminal display width starting at a specific terminal column.
///
/// Tabs expand to the next 8-column tab stop, matching CC Ink's output layer
/// and common terminal behavior. Other C0 controls have zero display width.
pub(crate) fn string_display_width_from_col(s: &str, start_col: usize) -> usize {
    if s.bytes().all(|byte| (0x20..=0x7e).contains(&byte)) {
        return s.len();
    }

    let mut col = start_col;
    let graphemes = s.graphemes(true).collect::<Vec<_>>();
    let mut idx = 0;
    while idx < graphemes.len() {
        let grapheme = graphemes[idx];
        if let Some(code) = single_ascii_byte(grapheme).filter(|code| *code <= 0x1f) {
            if code == b'\t' {
                col += 8 - (col % 8);
                idx += 1;
            } else if code == 0x1b {
                idx = skip_escape_sequence_graphemes(&graphemes, idx);
            } else {
                idx += 1;
            }
            continue;
        }
        col += grapheme_width(grapheme);
        idx += 1;
    }
    col.saturating_sub(start_col)
}

/// Expands TAB characters to spaces using terminal tab stops.
///
/// This mirrors CC Ink's `tabstops.ts` helper with the POSIX/default terminal
/// tab interval of 8 columns. ANSI/OSC/DCS escape sequences are preserved and
/// do not advance the measured column; newlines reset the column to zero.
pub fn expand_tabs(text: &str) -> String {
    expand_tabs_with_interval(text, 8)
}

/// Expands TAB characters to spaces using a custom tab-stop interval.
///
/// `interval == 0` is treated as `1` to avoid division by zero.
pub fn expand_tabs_with_interval(text: &str, interval: usize) -> String {
    if !text.contains('\t') {
        return text.to_string();
    }

    let interval = interval.max(1);
    let graphemes = text.graphemes(true).collect::<Vec<_>>();
    let mut result = String::with_capacity(text.len());
    let mut col = 0usize;
    let mut idx = 0usize;

    while idx < graphemes.len() {
        let grapheme = graphemes[idx];
        if let Some(code) = single_ascii_byte(grapheme).filter(|code| *code <= 0x1f) {
            match code {
                b'\t' => {
                    let spaces = interval - (col % interval);
                    result.extend(std::iter::repeat(' ').take(spaces));
                    col += spaces;
                    idx += 1;
                }
                b'\n' => {
                    result.push('\n');
                    col = 0;
                    idx += 1;
                }
                0x1b => {
                    let next = skip_escape_sequence_graphemes(&graphemes, idx);
                    for part in &graphemes[idx..next] {
                        result.push_str(part);
                    }
                    idx = next;
                }
                _ => {
                    result.push_str(grapheme);
                    idx += 1;
                }
            }
            continue;
        }

        result.push_str(grapheme);
        col += grapheme_width(grapheme);
        idx += 1;
    }

    result
}

const LINE_WIDTH_CACHE_SIZE: usize = 4096;
static LINE_WIDTH_CACHE: OnceLock<Mutex<HashMap<String, usize>>> = OnceLock::new();

/// Computes the terminal display width of a single line, with a small shared cache.
///
/// This is the Rust counterpart of CC Ink's `line-width-cache.ts`: streaming output
/// repeatedly re-measures completed lines, so caching line widths avoids expensive
/// Unicode/grapheme measurement for unchanged rows. The cache is intentionally simple
/// and clears once it reaches 4096 distinct lines, matching the fork's behavior.
pub fn line_width(line: &str) -> usize {
    let cache = LINE_WIDTH_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let Ok(mut cache) = cache.lock() else {
        return string_display_width(line);
    };

    if let Some(width) = cache.get(line) {
        return *width;
    }

    let width = string_display_width(line);
    if cache.len() >= LINE_WIDTH_CACHE_SIZE {
        cache.clear();
    }
    cache.insert(line.to_string(), width);
    width
}

/// Returns the widest line in terminal cells.
///
/// This mirrors CC Ink's `widest-line.ts`, including a `0` result for empty
/// input and measuring trailing empty lines after a final newline.
pub fn widest_line(text: &str) -> usize {
    let mut max_width = 0;
    for line in text.split('\n') {
        max_width = max_width.max(line_width(line));
    }
    max_width
}

/// Result returned by [`measure_text`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TextMeasurement {
    /// Widest line in terminal cells before wrapping height is applied.
    pub width: usize,
    /// Number of visual terminal rows needed to render the text.
    pub height: usize,
}

/// Measures terminal text width and visual row count in one pass.
///
/// This mirrors CC Ink's `measure-text.ts`: empty text measures to `0×0`,
/// trailing newlines reserve an empty row, and finite non-zero `max_width`
/// wraps each source line by terminal-cell width using ceiling division.
/// `None` or `Some(0)` disables wrapping. Width calculation uses iocraft's
/// terminal display-width rules, including grapheme clusters, C0/control
/// filtering, escape-sequence skipping, and 8-column tab stops.
pub fn measure_text(text: &str, max_width: Option<usize>) -> TextMeasurement {
    if text.is_empty() {
        return TextMeasurement::default();
    }

    let mut width = 0usize;
    let mut height = 0usize;
    let no_wrap = max_width.is_none_or(|max_width| max_width == 0);
    let max_width = max_width.unwrap_or(0);

    for line in text.split('\n') {
        let line_display_width = line_width(line);
        width = width.max(line_display_width);
        height += if no_wrap || line_display_width == 0 {
            1
        } else {
            line_display_width.div_ceil(max_width)
        };
    }

    TextMeasurement { width, height }
}

/// Compute the **terminal display width** of a single grapheme cluster.
///
/// This mirrors the logic of npm `string-width` / `Bun.stringWidth` that Claude Code
/// uses for layout measurement:
///
/// 1. Zero-width clusters (pure control/format/mark codepoints) → 0
/// 2. Multi-codepoint clusters containing at least two codepoints with nonzero width
///    (ZWJ emoji family sequences, keycap sequences, etc.) → 2
/// 3. Single visible codepoint → its `UnicodeWidthChar::width()` (East Asian Width)
///
/// This is a best-effort approximation. True terminal rendering width varies across
/// terminals and Unicode versions; the only authoritative answer would come from
/// querying the terminal itself. For CJK text and common emoji this is accurate on
/// modern terminals (kitty, iTerm2, WezTerm, Ghostty, Windows Terminal).
fn grapheme_width(grapheme: &str) -> usize {
    // Fast path: single ASCII byte.
    let bytes = grapheme.as_bytes();
    if bytes.len() == 1 {
        let b = bytes[0];
        return if (0x20..=0x7E).contains(&b) { 1 } else { 0 };
    }

    // Count codepoints with nonzero individual width. Some format controls
    // still report width 1 in unicode-width; filter them first using CC Ink's
    // stringWidth.ts zero-width table so retained cursor accounting matches the
    // official fork.
    let mut visible_cps = 0usize;
    let mut first_visible_width = 0usize;
    for ch in grapheme.chars() {
        if is_cc_zero_width_codepoint(ch) {
            continue;
        }
        let w = ch.width().unwrap_or(0);
        if w > 0 {
            visible_cps += 1;
            if first_visible_width == 0 {
                first_visible_width = w;
            }
        }
    }

    // Pure zero-width cluster (combining marks, control chars, etc.).
    if visible_cps == 0 {
        return 0;
    }

    // Multi-codepoint cluster with multiple visible codepoints: ZWJ emoji families,
    // flag sequences, keycap sequences. Modern terminals render these as a single
    // double-width glyph.
    if visible_cps >= 2 {
        return 2;
    }

    // Incomplete keycap sequences (digit/#/* + VS16 without U+20E3) stay one
    // column in CC Ink's stringWidth.ts. Complete keycaps fall through to the
    // VS16 emoji-width rule below.
    if grapheme.len() >= 4 && grapheme.contains('\u{fe0f}') && !grapheme.contains('\u{20e3}') {
        if let Some(first) = grapheme.chars().next() {
            if first.is_ascii_digit() || first == '#' || first == '*' {
                return 1;
            }
        }
    }

    // Single visible codepoint plus VS16 (emoji presentation selector): terminals
    // render the emoji presentation as 2 columns even when the base character's East
    // Asian Width says 1. Example: ☀ (U+2600, EAW:Ambiguous → 1) + VS16 → ☀️.
    // Other single-visible-codepoint clusters, such as `e` + combining acute accent,
    // keep the base character's width.
    if grapheme.contains('\u{fe0f}') {
        return 2;
    }

    // Single visible codepoint, optionally with combining marks: use its East Asian Width.
    first_visible_width
}

fn is_cc_zero_width_codepoint(ch: char) -> bool {
    let cp = ch as u32;
    // Mirrors CC Ink stringWidth.ts `isZeroWidth`. unicode-width already
    // handles many of these as zero, but not all Arabic formatting controls.
    if cp <= 0x1f || (0x7f..=0x9f).contains(&cp) {
        return true;
    }
    if (0x20..0x7f).contains(&cp) {
        return false;
    }
    if (0xa0..0x0300).contains(&cp) {
        return cp == 0x00ad;
    }
    (0x200b..=0x200d).contains(&cp)
        || cp == 0xfeff
        || (0x2060..=0x2064).contains(&cp)
        || (0xfe00..=0xfe0f).contains(&cp)
        || (0xe0100..=0xe01ef).contains(&cp)
        || (0x0300..=0x036f).contains(&cp)
        || (0x1ab0..=0x1aff).contains(&cp)
        || (0x1dc0..=0x1dff).contains(&cp)
        || (0x20d0..=0x20ff).contains(&cp)
        || (0xfe20..=0xfe2f).contains(&cp)
        || ((0x0900..=0x0d4f).contains(&cp)
            && matches!(cp & 0x7f, 0x00..=0x03 | 0x3a..=0x4f | 0x51..=0x57 | 0x62..=0x63))
        || cp == 0x0e31
        || (0x0e34..=0x0e3a).contains(&cp)
        || (0x0e47..=0x0e4e).contains(&cp)
        || cp == 0x0eb1
        || (0x0eb4..=0x0ebc).contains(&cp)
        || (0x0ec8..=0x0ecd).contains(&cp)
        || (0x0600..=0x0605).contains(&cp)
        || matches!(cp, 0x06dd | 0x070f | 0x08e2)
        || (0xd800..=0xdfff).contains(&cp)
        || (0xe0000..=0xe007f).contains(&cp)
}

fn single_ascii_byte(grapheme: &str) -> Option<u8> {
    let bytes = grapheme.as_bytes();
    (bytes.len() == 1 && bytes[0].is_ascii()).then_some(bytes[0])
}

fn skip_escape_sequence_graphemes(graphemes: &[&str], idx: usize) -> usize {
    let Some(next) = graphemes.get(idx + 1).and_then(|g| single_ascii_byte(g)) else {
        return idx + 1;
    };

    match next {
        b'(' | b')' | b'*' | b'+' => (idx + 3).min(graphemes.len()),
        b'[' => {
            let mut j = idx + 2;
            while j < graphemes.len() {
                if single_ascii_byte(graphemes[j]).is_some_and(|byte| (0x40..=0x7e).contains(&byte))
                {
                    return j + 1;
                }
                j += 1;
            }
            graphemes.len()
        }
        b']' | b'P' | b'_' | b'^' | b'X' => {
            let mut j = idx + 2;
            while j < graphemes.len() {
                if single_ascii_byte(graphemes[j]) == Some(0x07) {
                    return j + 1;
                }
                if single_ascii_byte(graphemes[j]) == Some(0x1b)
                    && graphemes.get(j + 1).and_then(|g| single_ascii_byte(g)) == Some(b'\\')
                {
                    return (j + 2).min(graphemes.len());
                }
                j += 1;
            }
            graphemes.len()
        }
        0x30..=0x7e => (idx + 2).min(graphemes.len()),
        _ => idx + 1,
    }
}

static mut HANDLES_VS16_INCORRECTLY: bool = false;
static INIT_HANDLES_VS16_INCORRECTLY: Once = Once::new();

// Some terminals incorrectly only advance the cursor one space for emoji with VS16, so we need to
// add whitespace to compensate.
//
// https://www.jeffquast.com/post/ucs-detect-test-results/
// https://darrenburns.net/posts/emoji-in-the-terminal/
//
// Windows and iTerm2 seem to do the right thing. We add exceptions below for the ones that don't.
// Hopefully one day we'll be able to remove this hack.
pub(crate) fn handles_vs16_incorrectly() -> bool {
    unsafe {
        INIT_HANDLES_VS16_INCORRECTLY.call_once(|| {
            HANDLES_VS16_INCORRECTLY = env::var("TERM_PROGRAM")
                .map(|s| s == "Apple_Terminal")
                .unwrap_or(false)
                || env::var("GNOME_TERMINAL_SCREEN").is_ok_and(|v| !v.is_empty())
        });
        HANDLES_VS16_INCORRECTLY
    }
}

impl Character {
    fn required_padding(&self) -> usize {
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

    fn needs_width_compensation(&self) -> bool {
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
    fn attribute(self) -> Attribute {
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

fn write_canvas_style_transition<W: Write>(
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

/// Cached grapheme cluster metadata for packed direct line writes.
///
/// CC Ink's `Output.charCache` stores grapheme clusters and terminal widths so
/// its hot write loop does not re-tokenize or re-measure unchanged lines. This
/// Rust-first helper keeps style/link IDs out of the cache because iocraft's
/// packed pools are caller-owned and may be generation-reset; styles and links
/// are interned per write while the expensive Unicode clustering/width work is
/// reused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CanvasPackedLineCluster {
    /// Grapheme text, including TAB/control/ESC graphemes that the writer may skip.
    pub text: String,
    /// Precomputed terminal display width of this grapheme cluster.
    pub width: usize,
}

/// One styled segment for packed direct line writes.
///
/// This is the Rust-native counterpart to one CC Ink styled text run after ANSI
/// tokenization and hyperlink extraction. The line writer interns the typed
/// style/link once for the run, then reuses [`CanvasPackedLineCache`] for the
/// expensive grapheme-width work inside `text`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CanvasPackedLineRun<'a> {
    /// Plain text for this run. ANSI/terminal control sequences are skipped by
    /// the packed writer, but callers that already parsed ANSI should pass only
    /// printable text here.
    pub text: &'a str,
    /// Resolved style applied to printable cells in this run.
    pub style: CanvasResolvedStyle,
    /// Optional OSC 8 hyperlink target for printable cells in this run.
    pub hyperlink: Option<&'a str>,
}

/// One pre-interned styled segment for packed direct line writes.
///
/// Use this variant when a custom renderer already owns stable packed style and
/// hyperlink IDs. The IDs must come from the same compatible
/// [`CanvasPackedCellPools`] used by the destination screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CanvasPackedLineRunIds<'a> {
    /// Plain text for this run.
    pub text: &'a str,
    /// Packed style ID from [`CanvasPackedCellPools::intern_style`].
    pub style_id: u32,
    /// Packed hyperlink ID from [`CanvasPackedCellPools::intern_hyperlink`].
    pub hyperlink_id: u32,
}

/// Owned styled segment used by [`CanvasPackedOutput`] write operations.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CanvasPackedLineOwnedRun {
    /// Plain text for this run.
    pub text: String,
    /// Resolved style applied to printable cells in this run.
    pub style: CanvasResolvedStyle,
    /// Optional OSC 8 hyperlink target for printable cells in this run.
    pub hyperlink: Option<String>,
}

impl<'a> From<CanvasPackedLineRun<'a>> for CanvasPackedLineOwnedRun {
    fn from(value: CanvasPackedLineRun<'a>) -> Self {
        Self {
            text: value.text.to_string(),
            style: value.style,
            hyperlink: value.hyperlink.map(str::to_string),
        }
    }
}

/// Opt-in cache for packed direct line writes.
#[derive(Clone, Debug)]
pub struct CanvasPackedLineCache {
    max_entries: usize,
    lines: HashMap<String, Vec<CanvasPackedLineCluster>>,
    scratch: Vec<CanvasPackedLineCluster>,
}

impl Default for CanvasPackedLineCache {
    fn default() -> Self {
        Self::with_max_entries(16_384)
    }
}

impl CanvasPackedLineCache {
    /// Creates a cache with the CC Ink `Output.charCache` reset cap.
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a cache with an explicit maximum number of retained lines.
    ///
    /// `max_entries == 0` disables retention while still using the same parser
    /// path, useful for benchmarks and tests.
    pub fn with_max_entries(max_entries: usize) -> Self {
        Self {
            max_entries,
            lines: HashMap::new(),
            scratch: Vec::new(),
        }
    }

    /// Returns cached grapheme clusters for one logical line.
    pub fn clusters(&mut self, line: &str) -> &[CanvasPackedLineCluster] {
        if self.max_entries == 0 {
            self.scratch = build_canvas_packed_line_clusters(line);
            return &self.scratch;
        }

        if !self.lines.contains_key(line) && self.lines.len() >= self.max_entries {
            self.lines.clear();
        }
        self.lines
            .entry(line.to_string())
            .or_insert_with(|| build_canvas_packed_line_clusters(line))
            .as_slice()
    }

    /// Number of retained line entries.
    pub fn len(&self) -> usize {
        self.lines.len()
    }

    /// Whether the retained cache is empty.
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// Clears retained line entries and scratch storage.
    pub fn clear(&mut self) {
        self.lines.clear();
        self.scratch.clear();
    }
}

fn build_canvas_packed_line_clusters(line: &str) -> Vec<CanvasPackedLineCluster> {
    line.graphemes(true)
        .map(|grapheme| CanvasPackedLineCluster {
            text: grapheme.to_string(),
            width: grapheme_width(grapheme),
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CanvasPackedStyledLineCluster {
    text: String,
    width: usize,
    style_id: u32,
    hyperlink_id: u32,
}

fn skip_escape_sequence_packed_clusters(clusters: &[CanvasPackedLineCluster], idx: usize) -> usize {
    let Some(next) = clusters
        .get(idx + 1)
        .and_then(|cluster| single_ascii_byte(&cluster.text))
    else {
        return idx + 1;
    };

    match next {
        b'(' | b')' | b'*' | b'+' => (idx + 3).min(clusters.len()),
        b'[' => {
            let mut j = idx + 2;
            while j < clusters.len() {
                if single_ascii_byte(&clusters[j].text)
                    .is_some_and(|byte| (0x40..=0x7e).contains(&byte))
                {
                    return j + 1;
                }
                j += 1;
            }
            clusters.len()
        }
        b']' | b'P' | b'_' | b'^' | b'X' => {
            let mut j = idx + 2;
            while j < clusters.len() {
                if single_ascii_byte(&clusters[j].text) == Some(0x07) {
                    return j + 1;
                }
                if single_ascii_byte(&clusters[j].text) == Some(0x1b)
                    && clusters
                        .get(j + 1)
                        .and_then(|cluster| single_ascii_byte(&cluster.text))
                        == Some(b'\\')
                {
                    return (j + 2).min(clusters.len());
                }
                j += 1;
            }
            clusters.len()
        }
        0x30..=0x7e => (idx + 2).min(clusters.len()),
        _ => idx + 1,
    }
}

/// A partial style that can be overlaid on an already-rendered [`CanvasCell`] without
/// touching the original text or style. Each `None` field means "keep the original value";
/// `Some(v)` means "override with `v`".
///
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
    character: Option<Character>,
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
    width: usize,
    cells: Vec<Vec<CanvasCell>>,
    overlays: Vec<Vec<Option<StyleOverlay>>>,
    no_select: Vec<Vec<bool>>,
    soft_wrap: Vec<usize>,
    cursor_declaration: Option<CursorDeclaration>,
    scroll_hint: Option<ScrollHint>,
    force_full_repaint: bool,
    damage_region: Option<DamageRegion>,
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
    fn union(self, other: Self) -> Self {
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

    fn intersects_row(self, row: usize) -> bool {
        self.height > 0 && row >= self.y && row < self.y.saturating_add(self.height)
    }
}

/// Public width marker used by opt-in packed canvas snapshots.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum CanvasPackedCellWidth {
    /// A normal single-column character or blank cell.
    #[default]
    Normal,
    /// The first column of a double-width grapheme.
    Wide,
    /// The trailing cell occupied by a double-width grapheme.
    WidthTail,
    /// A skipped placeholder where a wide grapheme would have crossed the right edge.
    SpacerHead,
}

impl From<CellWidth> for CanvasPackedCellWidth {
    fn from(value: CellWidth) -> Self {
        match value {
            CellWidth::Normal => Self::Normal,
            CellWidth::Wide => Self::Wide,
            CellWidth::WidthTail => Self::WidthTail,
            CellWidth::SpacerHead => Self::SpacerHead,
        }
    }
}

/// Packed retained-canvas cell using interned IDs instead of owned strings/styles.
///
/// This is an opt-in, Rust-native counterpart to CC Ink's packed `Screen` cell
/// words: character, resolved style, hyperlink, and width metadata are stable
/// integer IDs allocated by [`CanvasPackedCellPools`]. It is intended for
/// custom renderers, profilers, and benchmarks; iocraft's default canvas remains
/// a typed, readable data structure.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct CanvasPackedCell {
    /// Interned character/string ID. Blank cells use the same ID as a literal space.
    pub char_id: u32,
    /// Interned resolved style ID, including post-render overlay and background.
    pub style_id: u32,
    /// Interned OSC 8 hyperlink ID. `0` means no hyperlink.
    pub hyperlink_id: u32,
    /// Cell-width marker used for wide grapheme and spacer handling.
    pub width: CanvasPackedCellWidth,
}

impl CanvasPackedCell {
    /// Returns whether this packed cell is visually empty under the pool defaults.
    pub fn is_empty(self) -> bool {
        self.char_id == 0
            && self.style_id == 0
            && self.hyperlink_id == 0
            && self.width == CanvasPackedCellWidth::Normal
    }
}

/// Resolved borrowed view of a packed cell.
///
/// CC Ink exposes object-shaped `Cell` values from `cellAt(...)` and
/// `visibleCellAtIndex(...)` even though the backing screen is packed integer
/// arrays. This view provides the same inspection shape for opt-in iocraft
/// packed screens without unpacking the whole buffer or changing the default
/// typed [`Canvas`] model.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CanvasPackedCellView<'a> {
    /// Interned character ID from the packed cell.
    pub char_id: u32,
    /// Character/grapheme string resolved through [`CanvasPackedCellPools`].
    pub character: &'a str,
    /// Interned style ID from the packed cell.
    pub style_id: u32,
    /// Typed resolved style, if the ID exists in the compatible pools.
    pub style: Option<CanvasResolvedStyle>,
    /// Interned hyperlink ID from the packed cell. `0` means no hyperlink.
    pub hyperlink_id: u32,
    /// OSC 8 hyperlink target resolved through [`CanvasPackedCellPools`].
    pub hyperlink: Option<&'a str>,
    /// Cell-width marker used for wide grapheme and spacer handling.
    pub width: CanvasPackedCellWidth,
}

/// Intern pools shared across opt-in packed canvas snapshots.
///
/// CC Ink keeps `CharPool`, `StylePool`, and `HyperlinkPool` beside its packed
/// `Screen` so blits and diffs can compare small integers instead of per-cell
/// strings and style objects. This helper exposes the same optimization shape
/// without changing [`Canvas`]'s default representation or terminal writer.
#[derive(Clone, Debug)]
pub struct CanvasPackedCellPools {
    chars: Vec<String>,
    char_ids: HashMap<String, u32>,
    ascii_char_ids: [u32; 128],
    hyperlinks: Vec<String>,
    hyperlink_ids: HashMap<String, u32>,
    styles: Vec<CanvasResolvedStyle>,
    style_ids: HashMap<CanvasResolvedStyle, u32>,
}

impl Default for CanvasPackedCellPools {
    fn default() -> Self {
        let mut pools = Self {
            chars: vec![" ".to_string(), String::new()],
            char_ids: HashMap::from([(String::new(), 1)]),
            ascii_char_ids: [u32::MAX; 128],
            hyperlinks: vec![String::new()],
            hyperlink_ids: HashMap::new(),
            styles: vec![CanvasResolvedStyle::default()],
            style_ids: HashMap::from([(CanvasResolvedStyle::default(), 0)]),
        };
        pools.ascii_char_ids[b' ' as usize] = 0;
        pools
    }
}

impl CanvasPackedCellPools {
    /// Creates empty pools containing only CC Ink-compatible default entries:
    /// character `" "` at ID 0, spacer `""` at ID 1, no hyperlink at ID 0,
    /// and default style at ID 0.
    pub fn new() -> Self {
        Self::default()
    }

    /// Interns a character/grapheme string and returns its stable pool ID.
    pub fn intern_char(&mut self, value: &str) -> u32 {
        if value.len() == 1 {
            let code = value.as_bytes()[0];
            if code < 128 {
                let cached = self.ascii_char_ids[code as usize];
                if cached != u32::MAX {
                    return cached;
                }
                let id = self.chars.len() as u32;
                self.chars.push(value.to_string());
                self.ascii_char_ids[code as usize] = id;
                return id;
            }
        }

        if let Some(id) = self.char_ids.get(value) {
            return *id;
        }
        let id = self.chars.len() as u32;
        self.chars.push(value.to_string());
        self.char_ids.insert(value.to_string(), id);
        id
    }

    /// Returns an interned character by ID.
    pub fn character(&self, id: u32) -> Option<&str> {
        self.chars.get(id as usize).map(String::as_str)
    }

    /// Interns an optional OSC 8 hyperlink and returns its stable pool ID.
    pub fn intern_hyperlink(&mut self, value: Option<&str>) -> u32 {
        let Some(value) = value.filter(|value| !value.is_empty()) else {
            return 0;
        };
        if let Some(id) = self.hyperlink_ids.get(value) {
            return *id;
        }
        let id = self.hyperlinks.len() as u32;
        self.hyperlinks.push(value.to_string());
        self.hyperlink_ids.insert(value.to_string(), id);
        id
    }

    /// Returns an interned OSC 8 hyperlink by ID. ID 0 is `None`.
    pub fn hyperlink(&self, id: u32) -> Option<&str> {
        (id != 0)
            .then(|| self.hyperlinks.get(id as usize).map(String::as_str))
            .flatten()
    }

    /// Interns a resolved terminal style and returns its stable pool ID.
    pub fn intern_style(&mut self, style: CanvasResolvedStyle) -> u32 {
        if let Some(id) = self.style_ids.get(&style) {
            return *id;
        }
        let id = self.styles.len() as u32;
        self.styles.push(style);
        self.style_ids.insert(style, id);
        id
    }

    /// Returns an interned resolved terminal style by ID.
    pub fn style(&self, id: u32) -> Option<CanvasResolvedStyle> {
        self.styles.get(id as usize).copied()
    }

    /// Returns whether an interned style is visibly meaningful on a space cell.
    pub fn style_visible_on_space(&self, id: u32) -> bool {
        self.style(id)
            .map(CanvasResolvedStyle::is_visible_on_space)
            .unwrap_or(false)
    }

    /// Interns the result of applying a post-render overlay to an existing style ID.
    ///
    /// This is the typed packed-screen counterpart to CC Ink's `StylePool`
    /// overlay helpers. Unknown base IDs fall back to the default style, matching
    /// the forgiving lookup behavior of the JS pool's `get(id) ?? []` path.
    pub fn style_id_with_overlay(&mut self, base_id: u32, overlay: StyleOverlay) -> u32 {
        let base = self.style(base_id).unwrap_or_default();
        self.intern_style(base.with_overlay(overlay))
    }

    /// Interns an inverse-video variant of an existing style ID.
    pub fn style_id_with_inverse(&mut self, base_id: u32) -> u32 {
        self.style_id_with_overlay(base_id, StyleOverlay::inverse())
    }

    /// Interns a selection-background variant that preserves foreground styling.
    pub fn style_id_with_selection_background(&mut self, base_id: u32, color: Color) -> u32 {
        self.style_id_with_overlay(base_id, StyleOverlay::selection_background(color))
    }

    /// Interns a current-search-match variant with the CC Ink yellow/inverse marker.
    pub fn style_id_with_current_match(&mut self, base_id: u32, color: Color) -> u32 {
        self.style_id_with_overlay(base_id, StyleOverlay::current_match(color))
    }

    /// Resolves a packed cell into a borrowed object-shaped view.
    pub fn cell_view(&self, cell: CanvasPackedCell) -> CanvasPackedCellView<'_> {
        CanvasPackedCellView {
            char_id: cell.char_id,
            character: self.character(cell.char_id).unwrap_or(" "),
            style_id: cell.style_id,
            style: self.style(cell.style_id),
            hyperlink_id: cell.hyperlink_id,
            hyperlink: self.hyperlink(cell.hyperlink_id),
            width: cell.width,
        }
    }

    /// Returns whether a packed cell should be rendered by a sparse row writer.
    ///
    /// This mirrors CC Ink's `visibleCellAtIndex(...)` optimization: spacer
    /// cells and default blank spaces are skipped; foreground-only spaces are
    /// skipped once the same style is already the last rendered style on the
    /// row; spaces with hyperlinks or visible-on-space styles still render.
    pub fn cell_visible_for_sparse_render(
        &self,
        cell: CanvasPackedCell,
        last_rendered_style_id: Option<u32>,
    ) -> bool {
        if matches!(
            cell.width,
            CanvasPackedCellWidth::WidthTail | CanvasPackedCellWidth::SpacerHead
        ) || cell.char_id == 1
        {
            return false;
        }

        if cell.char_id == 0
            && cell.hyperlink_id == 0
            && !self.style_visible_on_space(cell.style_id)
            && (cell.style_id == 0 || last_rendered_style_id == Some(cell.style_id))
        {
            return false;
        }

        true
    }

    /// Interns one canvas cell plus any post-render overlay into a packed cell.
    pub fn intern_cell(
        &mut self,
        cell: &CanvasCell,
        overlay: Option<StyleOverlay>,
    ) -> CanvasPackedCell {
        let char_text = match cell.cell_width {
            CellWidth::WidthTail => "",
            CellWidth::SpacerHead => " ",
            CellWidth::Normal | CellWidth::Wide => cell.text().unwrap_or(" "),
        };
        let base_style = cell
            .character
            .as_ref()
            .map(|character| character.style)
            .unwrap_or_default();
        let base_background = cell.background_color;
        let resolved_style = match overlay {
            Some(overlay) => CanvasResolvedStyle {
                text: base_style.with_overlay(&overlay),
                background_color: overlay.background_color.unwrap_or(base_background),
            },
            None => CanvasResolvedStyle {
                text: base_style,
                background_color: base_background,
            },
        };

        CanvasPackedCell {
            char_id: self.intern_char(char_text),
            style_id: self.intern_style(resolved_style),
            hyperlink_id: self.intern_hyperlink(cell.hyperlink()),
            width: cell.cell_width.into(),
        }
    }

    /// Number of interned character strings.
    pub fn char_len(&self) -> usize {
        self.chars.len()
    }

    /// Number of interned hyperlinks, including the ID 0 empty entry.
    pub fn hyperlink_len(&self) -> usize {
        self.hyperlinks.len()
    }

    /// Number of interned resolved styles.
    pub fn style_len(&self) -> usize {
        self.styles.len()
    }

    /// Clears all pools back to their default entries.
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// Returns a new pool generation with char/hyperlink pools cleared and style IDs preserved.
    ///
    /// CC Ink keeps `StylePool` session-lived while `CharPool`/`HyperlinkPool`
    /// may be replaced between turns; old packed screens are then migrated with
    /// `migrateScreenPools(...)`. Because iocraft keeps these pools bundled for
    /// a Rust-friendly API, this helper creates that same generational shape:
    /// the returned pools have fresh transient char/link IDs but identical style
    /// IDs, so [`CanvasPackedScreen::migrate_transient_pools`] can re-intern a
    /// retained packed screen without invalidating style references.
    pub fn fork_with_transient_pools_cleared(&self) -> Self {
        let mut next = Self::default();
        next.styles = self.styles.clone();
        next.style_ids = self.style_ids.clone();
        next
    }

    /// Clears only char/hyperlink pools while preserving style IDs.
    ///
    /// Use this only after all screens that reference the old transient IDs have
    /// either been discarded or migrated with
    /// [`CanvasPackedScreen::migrate_transient_pools`]. For most retained
    /// double-buffering code, [`CanvasPackedCellPools::fork_with_transient_pools_cleared`]
    /// is safer because it leaves the old generation available during migration.
    pub fn clear_transient_pools(&mut self) {
        let styles = self.styles.clone();
        let style_ids = self.style_ids.clone();
        *self = Self::default();
        self.styles = styles;
        self.style_ids = style_ids;
    }
}

fn skip_escape_sequence_packed_styled_clusters(
    clusters: &[CanvasPackedStyledLineCluster],
    idx: usize,
) -> usize {
    let Some(next) = clusters
        .get(idx + 1)
        .and_then(|cluster| single_ascii_byte(&cluster.text))
    else {
        return idx + 1;
    };

    match next {
        b'(' | b')' | b'*' | b'+' => (idx + 3).min(clusters.len()),
        b'[' => {
            let mut j = idx + 2;
            while j < clusters.len() {
                if single_ascii_byte(&clusters[j].text)
                    .is_some_and(|byte| (0x40..=0x7e).contains(&byte))
                {
                    return j + 1;
                }
                j += 1;
            }
            clusters.len()
        }
        b']' | b'P' | b'_' | b'^' | b'X' => {
            let mut j = idx + 2;
            while j < clusters.len() {
                if single_ascii_byte(&clusters[j].text) == Some(0x07) {
                    return j + 1;
                }
                if single_ascii_byte(&clusters[j].text) == Some(0x1b)
                    && clusters
                        .get(j + 1)
                        .and_then(|cluster| single_ascii_byte(&cluster.text))
                        == Some(b'\\')
                {
                    return (j + 2).min(clusters.len());
                }
                j += 1;
            }
            clusters.len()
        }
        0x30..=0x7e => (idx + 2).min(clusters.len()),
        _ => idx + 1,
    }
}

/// Packed retained-canvas snapshot produced by [`Canvas::pack_with`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CanvasPackedScreen {
    /// Width in terminal cells.
    pub width: usize,
    /// Height in terminal rows.
    pub height: usize,
    /// Row-major packed cells using IDs from the pools passed to [`Canvas::pack_with`].
    pub cells: Vec<CanvasPackedCell>,
    /// Row-major no-select bitmap copied from the canvas.
    pub no_select: Vec<bool>,
    /// Per-row soft-wrap continuation/content-end metadata.
    pub soft_wrap: Vec<usize>,
    /// One-shot damage region captured from the canvas.
    pub damage_region: Option<DamageRegion>,
}

/// Per-cell change emitted by [`CanvasPackedScreen::diff`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CanvasPackedDiffChange {
    /// Column of the changed cell.
    pub x: usize,
    /// Row of the changed cell.
    pub y: usize,
    /// Previous packed cell, or `None` when the next screen grew into this position.
    pub removed: Option<CanvasPackedCell>,
    /// New packed cell, or `None` when the next screen shrank away from this position.
    pub added: Option<CanvasPackedCell>,
}

/// Clip bounds for [`CanvasPackedOutput`] operations.
///
/// Each bound is optional; `None` means unbounded on that side. Bounds are
/// intersected when clips are nested, mirroring CC Ink `Output.clip(...)`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CanvasPackedOutputClip {
    /// Inclusive left bound.
    pub x1: Option<usize>,
    /// Exclusive right bound.
    pub x2: Option<usize>,
    /// Inclusive top bound.
    pub y1: Option<usize>,
    /// Exclusive bottom bound.
    pub y2: Option<usize>,
}

/// Queued packed output operation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CanvasPackedOutputOperation {
    /// Queue a styled text write at a screen-space coordinate.
    Write {
        /// Start column.
        x: usize,
        /// Start row.
        y: usize,
        /// Pre-split logical lines. Each line contains one or more styled runs.
        lines: Vec<Vec<CanvasPackedLineOwnedRun>>,
        /// Optional soft-wrap flags parallel to `lines`.
        soft_wrap: Option<Vec<bool>>,
    },
    /// Push a nested clip rectangle.
    Clip(CanvasPackedOutputClip),
    /// Pop the current clip rectangle.
    Unclip,
    /// Copy a same-coordinate rectangle from another packed screen.
    Blit {
        /// Source screen snapshot.
        src: CanvasPackedScreen,
        /// Left column.
        x: usize,
        /// Top row.
        y: usize,
        /// Width in cells.
        width: usize,
        /// Height in rows.
        height: usize,
    },
    /// Mark a region dirty so stale previous-frame content is cleared by diffing.
    Clear {
        /// Region to mark damaged.
        region: DamageRegion,
        /// Whether this clear came from an absolute-positioned overlay.
        from_absolute: bool,
    },
    /// Mark a region as excluded from selection/search text.
    NoSelect {
        /// Region to mark as no-select.
        region: DamageRegion,
    },
    /// Shift full-width rows inside an inclusive row range.
    Shift {
        /// Top row, inclusive.
        top: usize,
        /// Bottom row, inclusive.
        bottom: usize,
        /// Positive values move rows up; negative values move rows down.
        delta: i32,
    },
}

/// Opt-in packed equivalent of CC Ink's `Output` operation queue.
///
/// The built-in iocraft renderer still writes directly to typed [`Canvas`].
/// Custom retained renderers can use `CanvasPackedOutput` when they want the
/// CC Ink ordering model without making packed screens the framework default:
/// clear operations first expand damage, blits honor active clips and skip rows
/// fully covered by absolute clears, writes use [`CanvasPackedLineCache`], shifts
/// mirror packed `shiftRows(...)`, and no-select metadata is replayed last so it
/// wins over blits and writes.
#[derive(Clone, Debug)]
pub struct CanvasPackedOutput {
    width: usize,
    height: usize,
    screen: CanvasPackedScreen,
    operations: Vec<CanvasPackedOutputOperation>,
    line_cache: CanvasPackedLineCache,
}

impl CanvasPackedScreen {
    /// Creates an empty packed screen using the default pool IDs.
    ///
    /// The returned screen assumes the standard [`CanvasPackedCellPools`]
    /// defaults: space char ID 0, spacer/empty char ID 1, no hyperlink ID 0,
    /// and default style ID 0. It is useful for custom retained renderers that
    /// want to build a packed buffer directly instead of first drawing a
    /// [`Canvas`] and calling [`Canvas::pack_with`].
    pub fn new(width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            cells: vec![Self::empty_cell(); width.saturating_mul(height)],
            no_select: vec![false; width.saturating_mul(height)],
            soft_wrap: vec![0; height],
            damage_region: None,
        }
    }

    /// Resets this packed screen for reuse, clearing active cells and metadata.
    ///
    /// This mirrors CC Ink's `resetScreen(...)` reuse path for retained double
    /// buffers: vectors may keep their allocation, dimensions are updated,
    /// cells/no-select/soft-wrap metadata are cleared for the active area, and
    /// damage is reset. It does not clear or mutate the external intern pools.
    pub fn reset(&mut self, width: usize, height: usize) {
        let size = width.saturating_mul(height);
        self.width = width;
        self.height = height;
        self.cells.resize(size, Self::empty_cell());
        self.cells.fill(Self::empty_cell());
        self.no_select.resize(size, false);
        self.no_select.fill(false);
        self.soft_wrap.resize(height, 0);
        self.soft_wrap.fill(0);
        self.damage_region = None;
    }

    fn empty_cell() -> CanvasPackedCell {
        CanvasPackedCell::default()
    }

    fn width_tail_cell() -> CanvasPackedCell {
        CanvasPackedCell {
            char_id: 1,
            style_id: 0,
            hyperlink_id: 0,
            width: CanvasPackedCellWidth::WidthTail,
        }
    }

    fn mark_damage_region(&mut self, region: DamageRegion) {
        if region.width == 0 || region.height == 0 {
            return;
        }
        self.damage_region = Some(
            self.damage_region
                .map_or(region, |damage| damage.union(region)),
        );
    }

    /// Returns the row-major index for a cell, if it is in bounds.
    pub fn index(&self, x: usize, y: usize) -> Option<usize> {
        (x < self.width && y < self.height).then_some(y * self.width + x)
    }

    /// Returns a packed cell by coordinates.
    pub fn cell(&self, x: usize, y: usize) -> Option<CanvasPackedCell> {
        self.index(x, y)
            .and_then(|index| self.cells.get(index).copied())
    }

    /// Returns the resolved character at a cell, or `None` when out of bounds.
    ///
    /// This mirrors CC Ink's `charInCellAt(...)`: spacer tails resolve to the
    /// pool's empty string and unwritten cells resolve to a literal space.
    pub fn char_in_cell<'a>(
        &self,
        pools: &'a CanvasPackedCellPools,
        x: usize,
        y: usize,
    ) -> Option<&'a str> {
        self.cell(x, y)
            .map(|cell| pools.character(cell.char_id).unwrap_or(" "))
    }

    /// Returns a resolved borrowed cell view by row-major index.
    pub fn cell_view_at_index<'a>(
        &self,
        pools: &'a CanvasPackedCellPools,
        index: usize,
    ) -> Option<CanvasPackedCellView<'a>> {
        self.cells
            .get(index)
            .copied()
            .map(|cell| pools.cell_view(cell))
    }

    /// Returns a resolved borrowed cell view by coordinates.
    pub fn cell_view<'a>(
        &self,
        pools: &'a CanvasPackedCellPools,
        x: usize,
        y: usize,
    ) -> Option<CanvasPackedCellView<'a>> {
        let index = self.index(x, y)?;
        self.cell_view_at_index(pools, index)
    }

    /// Returns whether a packed cell is visually empty by pool-default IDs.
    pub fn is_empty_cell(&self, x: usize, y: usize) -> bool {
        self.cell(x, y)
            .map(CanvasPackedCell::is_empty)
            .unwrap_or(true)
    }

    /// Returns whether a cell is marked no-select.
    pub fn is_no_select(&self, x: usize, y: usize) -> bool {
        self.index(x, y)
            .and_then(|index| self.no_select.get(index))
            .copied()
            .unwrap_or(false)
    }

    /// Returns the current packed-screen damage region.
    pub fn damage_region(&self) -> Option<DamageRegion> {
        self.damage_region
    }

    /// Adds a packed-screen damage region without changing cells.
    ///
    /// This is useful for custom output collectors that mirror CC Ink's
    /// `Output.clear(...)` pass: the next buffer may already be blank, while
    /// the diff still needs to scan the old bounds to clear stale terminal
    /// output from the previous frame.
    pub fn mark_damage(&mut self, region: DamageRegion) {
        self.mark_damage_region(region);
    }

    /// Clears one-shot packed-screen damage metadata.
    pub fn clear_damage(&mut self) {
        self.damage_region = None;
    }

    /// Returns the soft-wrap continuation/content-end marker for a row.
    pub fn soft_wrap_continuation(&self, row: usize) -> usize {
        self.soft_wrap.get(row).copied().unwrap_or(0)
    }

    /// Returns the sparse-render-visible cell at row-major `index`, if any.
    ///
    /// This is the packed-screen counterpart to CC Ink's
    /// `visibleCellAtIndex(...)`. It does not allocate or write terminal output;
    /// custom packed renderers can use it to skip spacer/default-space cells
    /// while preserving spaces whose style or hyperlink has visible semantics.
    pub fn visible_cell_at_index(
        &self,
        pools: &CanvasPackedCellPools,
        index: usize,
        last_rendered_style_id: Option<u32>,
    ) -> Option<CanvasPackedCell> {
        let cell = self.cells.get(index).copied()?;
        pools
            .cell_visible_for_sparse_render(cell, last_rendered_style_id)
            .then_some(cell)
    }

    /// Returns the sparse-render-visible packed cell by coordinates, if any.
    pub fn visible_cell(
        &self,
        pools: &CanvasPackedCellPools,
        x: usize,
        y: usize,
        last_rendered_style_id: Option<u32>,
    ) -> Option<CanvasPackedCell> {
        let index = self.index(x, y)?;
        self.visible_cell_at_index(pools, index, last_rendered_style_id)
    }

    /// Returns the sparse-render-visible resolved cell view at row-major `index`.
    pub fn visible_cell_view_at_index<'a>(
        &self,
        pools: &'a CanvasPackedCellPools,
        index: usize,
        last_rendered_style_id: Option<u32>,
    ) -> Option<CanvasPackedCellView<'a>> {
        self.visible_cell_at_index(pools, index, last_rendered_style_id)
            .map(|cell| pools.cell_view(cell))
    }

    /// Returns the sparse-render-visible resolved cell view by coordinates.
    pub fn visible_cell_view<'a>(
        &self,
        pools: &'a CanvasPackedCellPools,
        x: usize,
        y: usize,
        last_rendered_style_id: Option<u32>,
    ) -> Option<CanvasPackedCellView<'a>> {
        let index = self.index(x, y)?;
        self.visible_cell_view_at_index(pools, index, last_rendered_style_id)
    }

    /// Writes one packed cell, repairing wide-cell relationships and marking damage.
    ///
    /// This is the Rust-first counterpart to CC Ink's packed `setCellAt(...)`:
    /// overwriting a wide head clears its old tail, overwriting a wide tail
    /// clears the orphaned head, and writing a new wide cell creates the tail in
    /// the following column. The method expects all IDs in `cell` to come from
    /// the caller's compatible [`CanvasPackedCellPools`].
    pub fn set_cell(&mut self, x: usize, y: usize, cell: CanvasPackedCell) -> bool {
        let Some(index) = self.index(x, y) else {
            return false;
        };

        let previous_width = self.cells[index].width;
        if previous_width == CanvasPackedCellWidth::Wide
            && cell.width != CanvasPackedCellWidth::Wide
            && x + 1 < self.width
        {
            let tail_index = index + 1;
            if self.cells[tail_index].width == CanvasPackedCellWidth::WidthTail {
                self.cells[tail_index] = Self::empty_cell();
            }
        }

        let mut damage_min_x = x;
        if previous_width == CanvasPackedCellWidth::WidthTail
            && cell.width != CanvasPackedCellWidth::WidthTail
            && x > 0
        {
            let head_index = index - 1;
            if self.cells[head_index].width == CanvasPackedCellWidth::Wide {
                self.cells[head_index] = Self::empty_cell();
                damage_min_x = x - 1;
            }
        }

        self.cells[index] = cell;
        self.mark_damage_region(DamageRegion {
            x: damage_min_x,
            y,
            width: x.saturating_sub(damage_min_x) + 1,
            height: 1,
        });

        if cell.width == CanvasPackedCellWidth::Wide && x + 1 < self.width {
            let tail_index = index + 1;
            if self.cells[tail_index].width == CanvasPackedCellWidth::Wide && x + 2 < self.width {
                let orphan_tail = index + 2;
                if self.cells[orphan_tail].width == CanvasPackedCellWidth::WidthTail {
                    self.cells[orphan_tail] = Self::empty_cell();
                }
            }
            self.cells[tail_index] = Self::width_tail_cell();
            self.mark_damage_region(DamageRegion {
                x,
                y,
                width: 2,
                height: 1,
            });
        }

        true
    }

    /// Interns and writes one packed cell from typed text/style/link inputs.
    pub fn set_cell_text(
        &mut self,
        pools: &mut CanvasPackedCellPools,
        x: usize,
        y: usize,
        text: &str,
        style: CanvasResolvedStyle,
        hyperlink: Option<&str>,
        width: CanvasPackedCellWidth,
    ) -> bool {
        let char_text = match width {
            CanvasPackedCellWidth::WidthTail => "",
            CanvasPackedCellWidth::SpacerHead => " ",
            CanvasPackedCellWidth::Normal | CanvasPackedCellWidth::Wide => text,
        };
        let cell = CanvasPackedCell {
            char_id: pools.intern_char(char_text),
            style_id: pools.intern_style(style),
            hyperlink_id: pools.intern_hyperlink(hyperlink),
            width,
        };
        self.set_cell(x, y, cell)
    }

    /// Writes one logical line using cached grapheme widths and explicit style/link IDs.
    ///
    /// This mirrors CC Ink's `writeLineToScreen(...)` hot-loop shape for custom
    /// packed renderers: TABs expand to 8-column stops using default styling,
    /// C0 controls and raw terminal escape sequences are skipped, zero-width
    /// clusters do not mutate cells, and a wide grapheme at the right edge writes
    /// a `SpacerHead` placeholder instead of allowing terminal autowrap. The
    /// return value is the visual end column used by soft-wrap metadata.
    pub fn write_line_with_ids(
        &mut self,
        pools: &mut CanvasPackedCellPools,
        cache: &mut CanvasPackedLineCache,
        x: usize,
        y: usize,
        line: &str,
        style_id: u32,
        hyperlink_id: u32,
    ) -> usize {
        if y >= self.height {
            return x;
        }

        let clusters = cache.clusters(line);
        self.write_cached_clusters_with_ids(pools, clusters, x, y, style_id, hyperlink_id)
    }

    fn write_cached_clusters_with_ids(
        &mut self,
        pools: &mut CanvasPackedCellPools,
        clusters: &[CanvasPackedLineCluster],
        mut offset_x: usize,
        y: usize,
        style_id: u32,
        hyperlink_id: u32,
    ) -> usize {
        let mut cluster_idx = 0;
        while cluster_idx < clusters.len() {
            let cluster = &clusters[cluster_idx];
            if let Some(code) = single_ascii_byte(&cluster.text).filter(|code| *code <= 0x1f) {
                if code == b'\t' {
                    let spaces_to_next_stop = 8 - (offset_x % 8);
                    for _ in 0..spaces_to_next_stop {
                        if offset_x < self.width {
                            self.set_cell(
                                offset_x,
                                y,
                                CanvasPackedCell {
                                    char_id: 0,
                                    style_id: 0,
                                    hyperlink_id: 0,
                                    width: CanvasPackedCellWidth::Normal,
                                },
                            );
                        }
                        offset_x = offset_x.saturating_add(1);
                    }
                    cluster_idx += 1;
                    continue;
                }

                cluster_idx = if code == 0x1b {
                    skip_escape_sequence_packed_clusters(clusters, cluster_idx)
                } else {
                    cluster_idx + 1
                };
                continue;
            }

            let char_width = cluster.width;
            if char_width == 0 {
                cluster_idx += 1;
                continue;
            }

            let is_wide = char_width >= 2;
            if is_wide && offset_x.saturating_add(2) > self.width {
                if offset_x < self.width {
                    self.set_cell(
                        offset_x,
                        y,
                        CanvasPackedCell {
                            char_id: 0,
                            style_id: 0,
                            hyperlink_id: 0,
                            width: CanvasPackedCellWidth::SpacerHead,
                        },
                    );
                }
                offset_x = offset_x.saturating_add(1);
                cluster_idx += 1;
                continue;
            }

            if offset_x < self.width {
                self.set_cell(
                    offset_x,
                    y,
                    CanvasPackedCell {
                        char_id: pools.intern_char(&cluster.text),
                        style_id,
                        hyperlink_id,
                        width: if is_wide {
                            CanvasPackedCellWidth::Wide
                        } else {
                            CanvasPackedCellWidth::Normal
                        },
                    },
                );
            }
            offset_x = offset_x.saturating_add(if is_wide { 2 } else { 1 });
            cluster_idx += 1;
        }

        offset_x
    }

    fn write_cached_clusters_clipped_with_ids(
        &mut self,
        pools: &mut CanvasPackedCellPools,
        clusters: &[CanvasPackedLineCluster],
        mut offset_x: usize,
        y: usize,
        style_id: u32,
        hyperlink_id: u32,
        min_x: usize,
        max_x: usize,
    ) -> usize {
        if y >= self.height || min_x >= max_x {
            return offset_x;
        }

        let mut cluster_idx = 0;
        while cluster_idx < clusters.len() {
            let cluster = &clusters[cluster_idx];
            if let Some(code) = single_ascii_byte(&cluster.text).filter(|code| *code <= 0x1f) {
                if code == b'\t' {
                    let spaces_to_next_stop = 8 - (offset_x % 8);
                    for _ in 0..spaces_to_next_stop {
                        if offset_x >= min_x && offset_x < max_x && offset_x < self.width {
                            self.set_cell(
                                offset_x,
                                y,
                                CanvasPackedCell {
                                    char_id: 0,
                                    style_id: 0,
                                    hyperlink_id: 0,
                                    width: CanvasPackedCellWidth::Normal,
                                },
                            );
                        }
                        offset_x = offset_x.saturating_add(1);
                    }
                    cluster_idx += 1;
                    continue;
                }

                cluster_idx = if code == 0x1b {
                    skip_escape_sequence_packed_clusters(clusters, cluster_idx)
                } else {
                    cluster_idx + 1
                };
                continue;
            }

            let char_width = cluster.width;
            if char_width == 0 {
                cluster_idx += 1;
                continue;
            }

            let next_x = offset_x.saturating_add(char_width.max(1));
            if next_x <= min_x {
                offset_x = next_x;
                cluster_idx += 1;
                continue;
            }
            if offset_x >= max_x {
                break;
            }
            if offset_x < min_x {
                // Avoid rendering a partial grapheme clipped at the left edge.
                offset_x = next_x;
                cluster_idx += 1;
                continue;
            }

            let is_wide = char_width >= 2;
            if next_x > max_x {
                if is_wide && max_x >= self.width && offset_x < self.width {
                    self.set_cell(
                        offset_x,
                        y,
                        CanvasPackedCell {
                            char_id: 0,
                            style_id: 0,
                            hyperlink_id: 0,
                            width: CanvasPackedCellWidth::SpacerHead,
                        },
                    );
                    offset_x = offset_x.saturating_add(1);
                }
                break;
            }

            if is_wide && offset_x.saturating_add(2) > self.width {
                if offset_x < self.width {
                    self.set_cell(
                        offset_x,
                        y,
                        CanvasPackedCell {
                            char_id: 0,
                            style_id: 0,
                            hyperlink_id: 0,
                            width: CanvasPackedCellWidth::SpacerHead,
                        },
                    );
                }
                offset_x = offset_x.saturating_add(1);
                cluster_idx += 1;
                continue;
            }

            if offset_x < self.width {
                self.set_cell(
                    offset_x,
                    y,
                    CanvasPackedCell {
                        char_id: pools.intern_char(&cluster.text),
                        style_id,
                        hyperlink_id,
                        width: if is_wide {
                            CanvasPackedCellWidth::Wide
                        } else {
                            CanvasPackedCellWidth::Normal
                        },
                    },
                );
            }
            offset_x = next_x;
            cluster_idx += 1;
        }

        offset_x
    }

    /// Writes adjacent pre-interned styled runs with a shared line cache.
    ///
    /// This mirrors the CC Ink `styledCharsWithGraphemeClustering(...)` +
    /// `writeLineToScreen(...)` hot path after ANSI tokenization: style and
    /// hyperlink IDs are resolved once per run, grapheme widths are reused by
    /// [`CanvasPackedLineCache`], and the full line is reordered for terminals
    /// that need software bidi before the LTR cell-placement loop. The returned
    /// column is the visual end after all runs, including TAB expansion and
    /// skipped controls.
    pub fn write_line_runs_with_ids<'a, I>(
        &mut self,
        pools: &mut CanvasPackedCellPools,
        cache: &mut CanvasPackedLineCache,
        x: usize,
        y: usize,
        runs: I,
    ) -> usize
    where
        I: IntoIterator<Item = CanvasPackedLineRunIds<'a>>,
    {
        self.write_line_runs_with_ids_bidi_mode(
            pools,
            cache,
            x,
            y,
            runs,
            crate::bidi::needs_software_bidi(),
        )
    }

    fn write_line_runs_with_ids_bidi_mode<'a, I>(
        &mut self,
        pools: &mut CanvasPackedCellPools,
        cache: &mut CanvasPackedLineCache,
        x: usize,
        y: usize,
        runs: I,
        bidi_enabled: bool,
    ) -> usize
    where
        I: IntoIterator<Item = CanvasPackedLineRunIds<'a>>,
    {
        if y >= self.height {
            return x;
        }

        let runs = runs.into_iter().collect::<Vec<_>>();
        if !bidi_enabled {
            let mut offset_x = x;
            for run in runs {
                let clusters = cache.clusters(run.text);
                offset_x = self.write_cached_clusters_with_ids(
                    pools,
                    clusters,
                    offset_x,
                    y,
                    run.style_id,
                    run.hyperlink_id,
                );
            }
            return offset_x;
        }

        let mut graphemes = Vec::new();
        for run in runs {
            for cluster in cache.clusters(run.text) {
                graphemes.push(crate::bidi::BidiGrapheme {
                    text: cluster.text.clone(),
                    metadata: (cluster.width, run.style_id, run.hyperlink_id),
                });
            }
        }
        let reordered = crate::bidi::reorder_bidi_graphemes(graphemes, true)
            .into_iter()
            .map(|item| CanvasPackedStyledLineCluster {
                text: item.text,
                width: item.metadata.0,
                style_id: item.metadata.1,
                hyperlink_id: item.metadata.2,
            })
            .collect::<Vec<_>>();
        self.write_styled_clusters_with_ids(pools, &reordered, x, y)
    }

    fn write_styled_clusters_with_ids(
        &mut self,
        pools: &mut CanvasPackedCellPools,
        clusters: &[CanvasPackedStyledLineCluster],
        mut offset_x: usize,
        y: usize,
    ) -> usize {
        let mut cluster_idx = 0;
        while cluster_idx < clusters.len() {
            let cluster = &clusters[cluster_idx];
            if let Some(code) = single_ascii_byte(&cluster.text).filter(|code| *code <= 0x1f) {
                if code == b'\t' {
                    let spaces_to_next_stop = 8 - (offset_x % 8);
                    for _ in 0..spaces_to_next_stop {
                        if offset_x < self.width {
                            self.set_cell(
                                offset_x,
                                y,
                                CanvasPackedCell {
                                    char_id: 0,
                                    style_id: 0,
                                    hyperlink_id: 0,
                                    width: CanvasPackedCellWidth::Normal,
                                },
                            );
                        }
                        offset_x = offset_x.saturating_add(1);
                    }
                    cluster_idx += 1;
                    continue;
                }

                cluster_idx = if code == 0x1b {
                    skip_escape_sequence_packed_styled_clusters(clusters, cluster_idx)
                } else {
                    cluster_idx + 1
                };
                continue;
            }

            let char_width = cluster.width;
            if char_width == 0 {
                cluster_idx += 1;
                continue;
            }

            let is_wide = char_width >= 2;
            if is_wide && offset_x.saturating_add(2) > self.width {
                if offset_x < self.width {
                    self.set_cell(
                        offset_x,
                        y,
                        CanvasPackedCell {
                            char_id: 0,
                            style_id: 0,
                            hyperlink_id: 0,
                            width: CanvasPackedCellWidth::SpacerHead,
                        },
                    );
                }
                offset_x = offset_x.saturating_add(1);
                cluster_idx += 1;
                continue;
            }

            if offset_x < self.width {
                self.set_cell(
                    offset_x,
                    y,
                    CanvasPackedCell {
                        char_id: pools.intern_char(&cluster.text),
                        style_id: cluster.style_id,
                        hyperlink_id: cluster.hyperlink_id,
                        width: if is_wide {
                            CanvasPackedCellWidth::Wide
                        } else {
                            CanvasPackedCellWidth::Normal
                        },
                    },
                );
            }
            offset_x = offset_x.saturating_add(if is_wide { 2 } else { 1 });
            cluster_idx += 1;
        }

        offset_x
    }

    /// Interns typed style/link metadata once per run and writes adjacent runs.
    pub fn write_line_runs_with_cache<'a, I>(
        &mut self,
        pools: &mut CanvasPackedCellPools,
        cache: &mut CanvasPackedLineCache,
        x: usize,
        y: usize,
        runs: I,
    ) -> usize
    where
        I: IntoIterator<Item = CanvasPackedLineRun<'a>>,
    {
        let runs = runs
            .into_iter()
            .map(|run| CanvasPackedLineRunIds {
                text: run.text,
                style_id: pools.intern_style(run.style),
                hyperlink_id: pools.intern_hyperlink(run.hyperlink),
            })
            .collect::<Vec<_>>();
        self.write_line_runs_with_ids(pools, cache, x, y, runs)
    }

    /// Interns style/link inputs and writes one logical line with a line cache.
    pub fn write_line_with_cache(
        &mut self,
        pools: &mut CanvasPackedCellPools,
        cache: &mut CanvasPackedLineCache,
        x: usize,
        y: usize,
        line: &str,
        style: CanvasResolvedStyle,
        hyperlink: Option<&str>,
    ) -> usize {
        let style_id = pools.intern_style(style);
        let hyperlink_id = pools.intern_hyperlink(hyperlink);
        self.write_line_with_ids(pools, cache, x, y, line, style_id, hyperlink_id)
    }

    /// Re-interns char/hyperlink IDs from one transient pool generation into another.
    ///
    /// This is the opt-in Rust counterpart to CC Ink's `migrateScreenPools(...)`.
    /// It preserves packed cell contents, style IDs, width markers, damage,
    /// no-select metadata, and soft-wrap metadata while remapping only the
    /// transient character and hyperlink IDs. `new_pools` should normally be
    /// created with [`CanvasPackedCellPools::fork_with_transient_pools_cleared`]
    /// so style IDs remain valid across generations.
    pub fn migrate_transient_pools(
        &mut self,
        old_pools: &CanvasPackedCellPools,
        new_pools: &mut CanvasPackedCellPools,
    ) -> bool {
        let mut changed = false;
        for cell in &mut self.cells {
            let char_text = old_pools.character(cell.char_id).unwrap_or(" ");
            let new_char_id = new_pools.intern_char(char_text);
            if new_char_id != cell.char_id {
                cell.char_id = new_char_id;
                changed = true;
            }

            let hyperlink = old_pools.hyperlink(cell.hyperlink_id);
            let new_hyperlink_id = new_pools.intern_hyperlink(hyperlink);
            if new_hyperlink_id != cell.hyperlink_id {
                cell.hyperlink_id = new_hyperlink_id;
                changed = true;
            }
        }
        changed
    }

    /// Replaces the interned style ID of a non-spacer cell and marks it damaged.
    ///
    /// This is the packed-screen counterpart to CC Ink's `setCellStyleId(...)`:
    /// the character, width marker, and hyperlink ID are preserved, wide tails
    /// and spacer heads are skipped because styling the wide head visually covers
    /// the full grapheme, and the cell is added to the damage region so packed
    /// diffs see the post-render style change.
    pub fn set_cell_style_id(&mut self, x: usize, y: usize, style_id: u32) -> bool {
        let Some(index) = self.index(x, y) else {
            return false;
        };
        match self.cells[index].width {
            CanvasPackedCellWidth::WidthTail | CanvasPackedCellWidth::SpacerHead => return false,
            CanvasPackedCellWidth::Normal | CanvasPackedCellWidth::Wide => {}
        }
        self.cells[index].style_id = style_id;
        self.mark_damage_region(DamageRegion {
            x,
            y,
            width: 1,
            height: 1,
        });
        true
    }

    /// Interns and applies a resolved style to a non-spacer packed cell.
    pub fn set_cell_style(
        &mut self,
        pools: &mut CanvasPackedCellPools,
        x: usize,
        y: usize,
        style: CanvasResolvedStyle,
    ) -> bool {
        let style_id = pools.intern_style(style);
        self.set_cell_style_id(x, y, style_id)
    }

    /// Applies a post-render style overlay to one packed cell and marks it damaged.
    ///
    /// This is the packed-screen counterpart to [`Canvas::set_overlay`]: the
    /// overlay is composed into the cell's interned resolved style, wide tails
    /// are redirected to their leading wide cell, and spacer heads are ignored
    /// because they have no independently rendered glyph. It is intended for
    /// custom packed renderers that apply selection/search/debug overlays after
    /// drawing without unpacking back into [`Canvas`].
    pub fn apply_style_overlay(
        &mut self,
        pools: &mut CanvasPackedCellPools,
        x: usize,
        y: usize,
        overlay: StyleOverlay,
    ) -> bool {
        let Some(index) = self.index(x, y) else {
            return false;
        };
        let target_x = match self.cells[index].width {
            CanvasPackedCellWidth::WidthTail if x > 0 => {
                let previous = index - 1;
                if self.cells[previous].width == CanvasPackedCellWidth::Wide {
                    x - 1
                } else {
                    return false;
                }
            }
            CanvasPackedCellWidth::WidthTail | CanvasPackedCellWidth::SpacerHead => return false,
            CanvasPackedCellWidth::Normal | CanvasPackedCellWidth::Wide => x,
        };
        let Some(target_index) = self.index(target_x, y) else {
            return false;
        };
        let style_id = pools.style_id_with_overlay(self.cells[target_index].style_id, overlay);
        self.set_cell_style_id(target_x, y, style_id)
    }

    /// Applies a post-render style overlay to a rectangular packed-screen region.
    pub fn apply_style_overlay_region(
        &mut self,
        pools: &mut CanvasPackedCellPools,
        x: usize,
        y: usize,
        width: usize,
        height: usize,
        overlay: StyleOverlay,
    ) -> bool {
        let start_x = x.min(self.width);
        let start_y = y.min(self.height);
        let max_x = x.saturating_add(width).min(self.width);
        let max_y = y.saturating_add(height).min(self.height);
        let mut changed = false;
        for row in start_y..max_y {
            for col in start_x..max_x {
                changed |= self.apply_style_overlay(pools, col, row, overlay);
            }
        }
        changed
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

    fn cell_selection_text<'a>(
        &self,
        pools: &'a CanvasPackedCellPools,
        x: usize,
        y: usize,
    ) -> Option<&'a str> {
        self.cell(x, y)
            .and_then(|cell| pools.character(cell.char_id))
            .or(Some(" "))
    }

    /// Returns inclusive same-class word bounds for a packed-screen cell.
    ///
    /// This is the packed counterpart to CC Ink's double-click `wordBoundsAt`
    /// behavior used by `selectWordAt(...)`: if the target is a wide tail, the
    /// head cell is selected; no-select cells stop expansion; and word classes
    /// match terminal-like path/token classes.
    pub fn word_bounds_at(
        &self,
        pools: &CanvasPackedCellPools,
        col: usize,
        row: usize,
    ) -> Option<(usize, usize)> {
        if row >= self.height || self.width == 0 {
            return None;
        }

        let mut c = col;
        if c > 0
            && self
                .cell(c, row)
                .is_some_and(|cell| cell.width == CanvasPackedCellWidth::WidthTail)
        {
            c -= 1;
        }
        if c >= self.width || self.is_no_select(c, row) {
            return None;
        }
        let cls = Self::selection_char_class(self.cell_selection_text(pools, c, row)?);

        let mut lo = c;
        while lo > 0 {
            let prev = lo - 1;
            if self.is_no_select(prev, row) {
                break;
            }
            let Some(prev_cell) = self.cell(prev, row) else {
                break;
            };
            if prev_cell.width == CanvasPackedCellWidth::WidthTail {
                if prev == 0 || self.is_no_select(prev - 1, row) {
                    break;
                }
                let Some(head_text) = self.cell_selection_text(pools, prev - 1, row) else {
                    break;
                };
                if Self::selection_char_class(head_text) != cls {
                    break;
                }
                lo = prev - 1;
                continue;
            }
            let Some(prev_text) = self.cell_selection_text(pools, prev, row) else {
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
            if next_cell.width == CanvasPackedCellWidth::WidthTail {
                hi = next;
                continue;
            }
            let Some(next_text) = self.cell_selection_text(pools, next, row) else {
                break;
            };
            if Self::selection_char_class(next_text) != cls {
                break;
            }
            hi = next;
        }

        Some((lo, hi))
    }

    /// Returns the linear selection range for a double-click word selection.
    pub fn selection_range_for_word_at(
        &self,
        pools: &CanvasPackedCellPools,
        col: usize,
        row: usize,
    ) -> Option<SelectionRange> {
        let (lo, hi) = self.word_bounds_at(pools, col, row)?;
        Some(SelectionRange::new(
            SelectionPoint { col: lo, row },
            SelectionPoint { col: hi, row },
        ))
    }

    /// Returns the linear selection range for a triple-click row selection.
    pub fn selection_range_for_line(&self, row: usize) -> Option<SelectionRange> {
        if row >= self.height || self.width == 0 {
            return None;
        }
        Some(SelectionRange::new(
            SelectionPoint { col: 0, row },
            SelectionPoint {
                col: self.width - 1,
                row,
            },
        ))
    }

    fn extract_selected_row(
        &self,
        pools: &CanvasPackedCellPools,
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
                cell.width,
                CanvasPackedCellWidth::WidthTail | CanvasPackedCellWidth::SpacerHead
            ) {
                continue;
            }
            line.push_str(self.char_in_cell(pools, col, row).unwrap_or(" "));
        }
        if content_end > 0 {
            line
        } else {
            line.trim_end().to_string()
        }
    }

    /// Extracts text from a linear packed-screen selection range.
    ///
    /// This mirrors [`Canvas::selected_text`] and CC Ink's
    /// `getSelectedText(...)`: no-select cells are skipped, soft-wrap
    /// continuation rows are joined without a newline, and wide/spacer cells do
    /// not duplicate graphemes. It is useful for custom packed renderers that
    /// want selection/copy behavior without converting back to [`Canvas`].
    pub fn selected_text(&self, pools: &CanvasPackedCellPools, range: SelectionRange) -> String {
        let (start, end) = range.normalized();
        if self.width == 0 || self.height == 0 || start.row >= self.height {
            return String::new();
        }

        let mut lines = Vec::<String>::new();
        let last_row = end.row.min(self.height - 1);
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
                self.extract_selected_row(pools, row, col_start, col_end)
            };

            if self.soft_wrap_continuation(row) > 0 && !lines.is_empty() {
                lines.last_mut().unwrap().push_str(&line);
            } else {
                lines.push(line);
            }
        }

        lines.join("\n")
    }

    /// Applies a style overlay to every selectable packed cell in a linear range.
    ///
    /// This is the packed counterpart to [`Canvas::apply_selection_overlay`]: it
    /// skips no-select/spacer cells, composes the overlay into interned style IDs
    /// after rendering, and marks changed cells damaged so an otherwise-identical
    /// packed diff repaints the selection/search highlight.
    pub fn apply_selection_overlay(
        &mut self,
        pools: &mut CanvasPackedCellPools,
        range: SelectionRange,
        overlay: StyleOverlay,
    ) -> bool {
        let (start, end) = range.normalized();
        if self.width == 0 || self.height == 0 || start.row >= self.height {
            return false;
        }

        let mut changed = false;
        let last_row = end.row.min(self.height - 1);
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
                    cell.width,
                    CanvasPackedCellWidth::WidthTail | CanvasPackedCellWidth::SpacerHead
                ) {
                    continue;
                }
                changed |= self.apply_style_overlay(pools, col, row, overlay);
            }
        }
        changed
    }

    fn searchable_row_text_in_cols(
        &self,
        pools: &CanvasPackedCellPools,
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
                cell.width,
                CanvasPackedCellWidth::WidthTail | CanvasPackedCellWidth::SpacerHead
            ) {
                continue;
            }

            let lower = self
                .char_in_cell(pools, col, row)
                .unwrap_or(" ")
                .to_lowercase();
            let cell_idx = col_of_cell.len();
            byte_to_cell.extend(std::iter::repeat_n(cell_idx, lower.len()));
            text.push_str(&lower);
            col_of_cell.push(col);
        }
        (text, col_of_cell, byte_to_cell)
    }

    fn scan_text_positions_absolute(
        &self,
        pools: &CanvasPackedCellPools,
        query: &str,
        start_x: usize,
        start_y: usize,
        max_x: usize,
        max_y: usize,
        relative_to_region: bool,
    ) -> Vec<TextMatchPosition> {
        let query = query.to_lowercase();
        if query.is_empty() || self.width == 0 || self.height == 0 {
            return Vec::new();
        }

        let mut positions = Vec::new();
        for row in start_y.min(max_y)..max_y.min(self.height) {
            let (text, col_of_cell, byte_to_cell) =
                self.searchable_row_text_in_cols(pools, row, start_x, max_x);
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
                    .map(|cell| usize::from(cell.width == CanvasPackedCellWidth::Wide) + 1)
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

    /// Scans the packed screen for non-overlapping case-insensitive matches.
    ///
    /// This is the packed counterpart to [`Canvas::scan_text_positions`] and
    /// CC Ink's screen-space search: it searches visible rendered cells, skips
    /// no-select/spacer cells, and reports spans in terminal cells.
    pub fn scan_text_positions(
        &self,
        pools: &CanvasPackedCellPools,
        query: &str,
    ) -> Vec<TextMatchPosition> {
        self.scan_text_positions_absolute(pools, query, 0, 0, self.width, self.height, false)
    }

    /// Scans a packed-screen rectangular region and returns positions relative to it.
    pub fn scan_text_positions_region(
        &self,
        pools: &CanvasPackedCellPools,
        x: usize,
        y: usize,
        width: usize,
        height: usize,
        query: &str,
    ) -> Vec<TextMatchPosition> {
        if width == 0 || height == 0 || x >= self.width || y >= self.height {
            return Vec::new();
        }
        self.scan_text_positions_absolute(
            pools,
            query,
            x,
            y,
            x.saturating_add(width).min(self.width),
            y.saturating_add(height).min(self.height),
            true,
        )
    }

    fn apply_overlay_to_match(
        &mut self,
        pools: &mut CanvasPackedCellPools,
        position: TextMatchPosition,
        overlay: StyleOverlay,
    ) -> bool {
        if position.row >= self.height || position.len == 0 {
            return false;
        }
        let end = position.col.saturating_add(position.len).min(self.width);
        let mut changed = false;
        for col in position.col..end {
            if self.is_no_select(col, position.row) {
                continue;
            }
            let Some(cell) = self.cell(col, position.row) else {
                continue;
            };
            if matches!(
                cell.width,
                CanvasPackedCellWidth::WidthTail | CanvasPackedCellWidth::SpacerHead
            ) {
                continue;
            }
            changed |= self.apply_style_overlay(pools, col, position.row, overlay);
        }
        changed
    }

    /// Applies a search-highlight overlay to all visible packed-screen matches.
    ///
    /// Returns `true` if at least one cell was highlighted. Matches are
    /// non-overlapping and case-insensitive, and no-select cells are not search
    /// targets, matching CC Ink's `applySearchHighlight(...)`.
    pub fn apply_search_highlight(
        &mut self,
        pools: &mut CanvasPackedCellPools,
        query: &str,
        overlay: StyleOverlay,
    ) -> bool {
        let positions = self.scan_text_positions(pools, query);
        let mut applied = false;
        for position in positions {
            applied |= self.apply_overlay_to_match(pools, position, overlay);
        }
        applied
    }

    /// Applies an overlay to one pre-scanned packed-screen match plus row offset.
    pub fn apply_positioned_highlight(
        &mut self,
        pools: &mut CanvasPackedCellPools,
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
            pools,
            TextMatchPosition {
                row: row as usize,
                ..position
            },
            overlay,
        )
    }

    fn osc8_hyperlink_at<'a>(
        &self,
        pools: &'a CanvasPackedCellPools,
        col: usize,
        row: usize,
    ) -> Option<&'a str> {
        let cell = self.cell(col, row)?;
        if let Some(href) = pools.hyperlink(cell.hyperlink_id) {
            return Some(href);
        }
        if col > 0 && cell.width == CanvasPackedCellWidth::WidthTail {
            return self
                .cell(col - 1, row)
                .and_then(|head| pools.hyperlink(head.hyperlink_id));
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

    fn plain_url_char_at<'a>(
        &self,
        pools: &'a CanvasPackedCellPools,
        col: usize,
        row: usize,
    ) -> Option<&'a str> {
        let cell = self.cell(col, row)?;
        if cell.width != CanvasPackedCellWidth::Normal {
            return None;
        }
        let value = pools.character(cell.char_id)?;
        Self::is_plain_url_char(value).then_some(value)
    }

    /// Finds a plain-text URL at a packed-screen cell, respecting no-select boundaries.
    ///
    /// This mirrors CC Ink's fullscreen fallback used when mouse tracking
    /// intercepts native terminal URL detection. OSC 8 links are handled by
    /// [`CanvasPackedScreen::hyperlink_at`]; this method scans ASCII URL-like
    /// runs for `http://`, `https://`, or `file://` schemes and strips trailing
    /// sentence punctuation.
    pub fn plain_text_url_at(
        &self,
        pools: &CanvasPackedCellPools,
        col: usize,
        row: usize,
    ) -> Option<String> {
        if row >= self.height || self.width == 0 {
            return None;
        }
        let mut c = col;
        if c > 0
            && self
                .cell(c, row)
                .is_some_and(|cell| cell.width == CanvasPackedCellWidth::WidthTail)
        {
            c -= 1;
        }
        if c >= self.width
            || self.is_no_select(c, row)
            || self.plain_url_char_at(pools, c, row).is_none()
        {
            return None;
        }

        let mut lo = c;
        while lo > 0 {
            let prev = lo - 1;
            if self.is_no_select(prev, row) || self.plain_url_char_at(pools, prev, row).is_none() {
                break;
            }
            lo = prev;
        }

        let mut hi = c;
        while hi + 1 < self.width {
            let next = hi + 1;
            if self.is_no_select(next, row) || self.plain_url_char_at(pools, next, row).is_none() {
                break;
            }
            hi = next;
        }

        let mut token = String::new();
        for col in lo..=hi {
            token.push_str(self.plain_url_char_at(pools, col, row)?);
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

    /// Returns the hyperlink target at a packed-screen cell.
    ///
    /// OSC 8 hyperlinks are preferred, including when the queried cell is the
    /// tail of a wide linked grapheme. If no OSC 8 hyperlink is present, this
    /// falls back to [`CanvasPackedScreen::plain_text_url_at`], matching CC
    /// Ink's fullscreen click handling while staying a pure buffer lookup.
    pub fn hyperlink_at(
        &self,
        pools: &CanvasPackedCellPools,
        col: usize,
        row: usize,
    ) -> Option<String> {
        self.osc8_hyperlink_at(pools, col, row)
            .map(str::to_string)
            .or_else(|| self.plain_text_url_at(pools, col, row))
    }

    /// Returns a clone of `next` with repaint/debug overlay applied to changed packed cells.
    ///
    /// This mirrors [`Canvas::debug_repaint_overlay`] for opt-in packed screens:
    /// packed diffs, previous damage, and current damage are highlighted by
    /// composing `overlay` into style IDs in the returned clone. Overlayed cells
    /// are marked damaged so a custom packed terminal diff can repaint the
    /// visualization without requiring a full unpacked canvas round-trip.
    pub fn debug_repaint_overlay(
        previous: Option<&Self>,
        next: &Self,
        pools: &mut CanvasPackedCellPools,
        overlay: StyleOverlay,
    ) -> Self {
        let mut screen = next.clone();

        match previous {
            Some(previous) => {
                previous.diff_each(next, |change| {
                    screen.apply_style_overlay(pools, change.x, change.y, overlay);
                    false
                });
                if let Some(region) = previous.damage_region() {
                    screen.apply_style_overlay_region(
                        pools,
                        region.x,
                        region.y,
                        region.width,
                        region.height,
                        overlay,
                    );
                }
            }
            None => {
                let empty = CanvasPackedScreen::new(0, 0);
                empty.diff_each(next, |change| {
                    screen.apply_style_overlay(pools, change.x, change.y, overlay);
                    false
                });
            }
        }

        if let Some(region) = next.damage_region() {
            screen.apply_style_overlay_region(
                pools,
                region.x,
                region.y,
                region.width,
                region.height,
                overlay,
            );
        }

        screen
    }

    /// Marks a rectangular packed-screen region as no-select without terminal damage.
    ///
    /// Matching CC Ink's `markNoSelectRegion(...)`, this metadata affects
    /// selection/copy/highlight consumers only; it should not wake terminal
    /// writers by itself.
    pub fn mark_no_select_region(
        &mut self,
        x: usize,
        y: usize,
        width: usize,
        height: usize,
    ) -> bool {
        let start_x = x.min(self.width);
        let start_y = y.min(self.height);
        let max_x = x.saturating_add(width).min(self.width);
        let max_y = y.saturating_add(height).min(self.height);
        if start_x >= max_x || start_y >= max_y {
            return false;
        }
        for row in start_y..max_y {
            let row_start = row * self.width;
            self.no_select[(row_start + start_x)..(row_start + max_x)].fill(true);
        }
        true
    }

    /// Clears a rectangular region of packed cells and returns the damaged rectangle.
    ///
    /// This mirrors CC Ink's packed `clearRegion(...)` boundary behavior for
    /// wide cells: clearing a wide tail also clears its head, and clearing a
    /// wide head also clears the tail just outside the requested rectangle. The
    /// method is opt-in snapshot manipulation; it does not write terminal output.
    pub fn clear_region(
        &mut self,
        x: usize,
        y: usize,
        width: usize,
        height: usize,
    ) -> Option<DamageRegion> {
        let start_x = x.min(self.width);
        let start_y = y.min(self.height);
        let max_x = x.saturating_add(width).min(self.width);
        let max_y = y.saturating_add(height).min(self.height);
        if start_x >= max_x || start_y >= max_y {
            return None;
        }

        let mut damage_min_x = start_x;
        let mut damage_max_x = max_x;
        for row in start_y..max_y {
            let row_start = row * self.width;
            if start_x > 0 {
                let start_index = row_start + start_x;
                let previous_index = start_index - 1;
                if self.cells[start_index].width == CanvasPackedCellWidth::WidthTail
                    && self.cells[previous_index].width == CanvasPackedCellWidth::Wide
                {
                    self.cells[previous_index] = Self::empty_cell();
                    if let Some(marked) = self.no_select.get_mut(previous_index) {
                        *marked = false;
                    }
                    damage_min_x = damage_min_x.min(start_x - 1);
                }
            }

            if max_x < self.width {
                let last_index = row_start + max_x - 1;
                let next_index = last_index + 1;
                if self.cells[last_index].width == CanvasPackedCellWidth::Wide
                    && self.cells[next_index].width == CanvasPackedCellWidth::WidthTail
                {
                    self.cells[next_index] = Self::empty_cell();
                    if let Some(marked) = self.no_select.get_mut(next_index) {
                        *marked = false;
                    }
                    damage_max_x = damage_max_x.max(max_x + 1);
                }
            }

            for index in (row_start + start_x)..(row_start + max_x) {
                self.cells[index] = Self::empty_cell();
                if let Some(marked) = self.no_select.get_mut(index) {
                    *marked = false;
                }
            }
            if start_x == 0 && max_x == self.width {
                if let Some(soft_wrap) = self.soft_wrap.get_mut(row) {
                    *soft_wrap = 0;
                }
            }
        }

        let region = DamageRegion {
            x: damage_min_x,
            y: start_y,
            width: damage_max_x.saturating_sub(damage_min_x),
            height: max_y.saturating_sub(start_y),
        };
        self.mark_damage_region(region);
        Some(region)
    }

    /// Copies a rectangular same-coordinate region from another packed screen.
    ///
    /// The two screens must have been produced with compatible
    /// [`CanvasPackedCellPools`] so interned IDs have the same meaning. Like CC
    /// Ink's packed `blitRegion(...)`, this copies no-select metadata and the
    /// affected rows' soft-wrap provenance, marks the copied rectangle damaged,
    /// and repairs a wide-character tail just outside the right edge when the
    /// copied region ends on a wide head.
    pub fn blit_region_from(
        &mut self,
        src: &Self,
        x: usize,
        y: usize,
        width: usize,
        height: usize,
    ) -> Option<DamageRegion> {
        let start_x = x.min(self.width).min(src.width);
        let start_y = y.min(self.height).min(src.height);
        let max_x = x.saturating_add(width).min(self.width).min(src.width);
        let max_y = y.saturating_add(height).min(self.height).min(src.height);
        if start_x >= max_x || start_y >= max_y {
            return None;
        }

        for row in start_y..max_y {
            let src_start = row * src.width + start_x;
            let src_end = row * src.width + max_x;
            let dst_start = row * self.width + start_x;
            let dst_end = row * self.width + max_x;
            self.cells[dst_start..dst_end].copy_from_slice(&src.cells[src_start..src_end]);
            self.no_select[dst_start..dst_end].copy_from_slice(&src.no_select[src_start..src_end]);
            if let (Some(dst_soft_wrap), Some(src_soft_wrap)) =
                (self.soft_wrap.get_mut(row), src.soft_wrap.get(row))
            {
                *dst_soft_wrap = *src_soft_wrap;
            }
        }

        let mut damage_width = max_x.saturating_sub(start_x);
        if max_x < self.width && max_x <= src.width {
            let mut wrote_tail = false;
            for row in start_y..max_y {
                let src_last = row * src.width + max_x - 1;
                if src.cells[src_last].width == CanvasPackedCellWidth::Wide {
                    let dst_tail = row * self.width + max_x;
                    self.cells[dst_tail] = Self::width_tail_cell();
                    if let Some(marked) = self.no_select.get_mut(dst_tail) {
                        *marked = false;
                    }
                    wrote_tail = true;
                }
            }
            if wrote_tail {
                damage_width = damage_width.saturating_add(1);
            }
        }

        let region = DamageRegion {
            x: start_x,
            y: start_y,
            width: damage_width,
            height: max_y.saturating_sub(start_y),
        };
        self.mark_damage_region(region);
        Some(region)
    }

    /// Copies a rectangular region while skipping rows covered by absolute clears.
    ///
    /// This is the packed-screen counterpart to CC Ink `output.ts`'s
    /// absolute-clear blit guard and [`Canvas::blit_region_from_excluding_clears`].
    /// A retained sibling blit should not restore rows where a removed absolute
    /// overlay already queued a clear; only rows whose whole blit segment is
    /// covered by a clear are skipped, matching the official
    /// `startX >= clear.x && maxX <= clear.x + clear.width` rule.
    ///
    /// The clear itself is not applied here. Returned regions are the copied
    /// spans that were marked damaged by the underlying blits.
    pub fn blit_region_from_excluding_clears(
        &mut self,
        src: &Self,
        x: usize,
        y: usize,
        width: usize,
        height: usize,
        excluded_clears: &[DamageRegion],
    ) -> Vec<DamageRegion> {
        if excluded_clears.is_empty() {
            return self
                .blit_region_from(src, x, y, width, height)
                .into_iter()
                .collect();
        }
        if width == 0 || height == 0 || x >= self.width || x >= src.width {
            return Vec::new();
        }

        let start_x = x.min(self.width).min(src.width);
        let start_y = y.min(self.height).min(src.height);
        let max_x = x.saturating_add(width).min(self.width).min(src.width);
        let max_y = y.saturating_add(height).min(self.height).min(src.height);
        if start_x >= max_x || start_y >= max_y {
            return Vec::new();
        }

        let row_is_excluded = |row: usize| {
            excluded_clears.iter().any(|clear| {
                clear.height > 0
                    && row >= clear.y
                    && row < clear.y.saturating_add(clear.height)
                    && start_x >= clear.x
                    && max_x <= clear.x.saturating_add(clear.width)
            })
        };

        let mut regions = Vec::new();
        let mut span_start = start_y;
        for row in start_y..=max_y {
            if row == max_y || row_is_excluded(row) {
                if row > span_start {
                    if let Some(region) = self.blit_region_from(
                        src,
                        start_x,
                        span_start,
                        max_x - start_x,
                        row - span_start,
                    ) {
                        regions.push(region);
                    }
                }
                span_start = row.saturating_add(1);
            }
        }
        regions
    }

    /// Shifts full-width rows inside an inclusive row range.
    ///
    /// Positive `delta` shifts rows up, negative `delta` shifts rows down, and
    /// vacated rows are cleared. Cells, no-select metadata, and soft-wrap row
    /// markers move together. Matching CC Ink's packed `shiftRows(...)`, this
    /// does **not** mark damage; callers pair it with explicit edge repaint or
    /// terminal scroll-hint planning.
    pub fn shift_rows(&mut self, top: usize, bottom: usize, delta: i32) -> bool {
        if delta == 0 || top > bottom || bottom >= self.height || self.width == 0 {
            return false;
        }
        let row_count = bottom - top + 1;
        let abs_delta = delta.unsigned_abs() as usize;
        if abs_delta >= row_count {
            for row in top..=bottom {
                self.clear_packed_row(row);
            }
            return true;
        }

        if delta > 0 {
            for row in top..=(bottom - abs_delta) {
                self.copy_packed_row(row + abs_delta, row);
            }
            for row in (bottom - abs_delta + 1)..=bottom {
                self.clear_packed_row(row);
            }
        } else {
            for row in ((top + abs_delta)..=bottom).rev() {
                self.copy_packed_row(row - abs_delta, row);
            }
            for row in top..(top + abs_delta) {
                self.clear_packed_row(row);
            }
        }
        true
    }

    fn clear_packed_row(&mut self, row: usize) {
        let start = row * self.width;
        let end = start + self.width;
        self.cells[start..end].fill(Self::empty_cell());
        self.no_select[start..end].fill(false);
        if let Some(soft_wrap) = self.soft_wrap.get_mut(row) {
            *soft_wrap = 0;
        }
    }

    fn copy_packed_row(&mut self, src_row: usize, dst_row: usize) {
        if src_row == dst_row {
            return;
        }
        let src_start = src_row * self.width;
        let dst_start = dst_row * self.width;
        for offset in 0..self.width {
            self.cells[dst_start + offset] = self.cells[src_start + offset];
            self.no_select[dst_start + offset] = self.no_select[src_start + offset];
        }
        if let Some(src_soft_wrap) = self.soft_wrap.get(src_row).copied() {
            if let Some(dst_soft_wrap) = self.soft_wrap.get_mut(dst_row) {
                *dst_soft_wrap = src_soft_wrap;
            }
        }
    }

    fn add_diff_region(region: &mut Option<DamageRegion>, next: DamageRegion) {
        if next.width == 0 || next.height == 0 {
            return;
        }
        *region = Some(region.map_or(next, |region| region.union(next)));
    }

    fn row_damage_start(&self, y: usize) -> Option<usize> {
        let damage = self.damage_region?;
        if !damage.intersects_row(y) || damage.width == 0 {
            return None;
        }
        Some(damage.x.min(self.width))
    }

    fn cell_width_for_diff(&self, x: usize, y: usize) -> CanvasPackedCellWidth {
        self.cell(x, y)
            .map(|cell| cell.width)
            .unwrap_or(CanvasPackedCellWidth::Normal)
    }

    /// Returns the first column that must be repainted on row `y`.
    ///
    /// This is the packed-screen analogue of [`Canvas::row_change_start`] and
    /// CC Ink's damage-limited `diffEach(...)` scan. It includes previous/current
    /// damage starts, height shrink rows (which must clear stale terminal output
    /// even if the retained cells are blank), growth rows with non-empty content,
    /// and packed-cell differences. If the first changed cell is a wide tail, the
    /// start is expanded to the wide head so sparse row writers keep cursor
    /// accounting valid. The helper performs no terminal I/O.
    pub fn row_change_start(&self, next: &Self, y: usize) -> Option<usize> {
        let max_height = self.height.max(next.height);
        if y >= max_height {
            return None;
        }

        if self.width != next.width {
            return Some(0);
        }

        if y >= next.height && y < self.height {
            return Some(0);
        }

        let max_width = self.width.max(next.width);
        let damage_start = [self.row_damage_start(y), next.row_damage_start(y)]
            .into_iter()
            .flatten()
            .min();

        let diff_start = (0..max_width).find(|&x| {
            let removed = self.cell(x, y);
            let added = next.cell(x, y);
            match (removed, added) {
                (Some(previous), Some(next)) => previous != next,
                (Some(_), None) => true,
                (None, Some(next)) => !next.is_empty(),
                (None, None) => false,
            }
        });

        let mut start = match (damage_start, diff_start) {
            (Some(a), Some(b)) => a.min(b),
            (Some(a), None) => a,
            (None, Some(b)) => b,
            (None, None) => return None,
        };

        if start > 0
            && (self.cell_width_for_diff(start, y) == CanvasPackedCellWidth::WidthTail
                || next.cell_width_for_diff(start, y) == CanvasPackedCellWidth::WidthTail)
        {
            start -= 1;
        }

        Some(start.min(max_width))
    }

    fn diff_region(&self, next: &Self) -> Option<DamageRegion> {
        let mut region = None;
        if self.width == 0 && self.height == 0 {
            Self::add_diff_region(
                &mut region,
                DamageRegion {
                    x: 0,
                    y: 0,
                    width: next.width,
                    height: next.height,
                },
            );
        } else {
            if let Some(damage) = next.damage_region {
                Self::add_diff_region(&mut region, damage);
            }
            if let Some(damage) = self.damage_region {
                Self::add_diff_region(&mut region, damage);
            }
        }

        if self.height > next.height {
            Self::add_diff_region(
                &mut region,
                DamageRegion {
                    x: 0,
                    y: next.height,
                    width: self.width,
                    height: self.height - next.height,
                },
            );
        }
        if self.width > next.width {
            Self::add_diff_region(
                &mut region,
                DamageRegion {
                    x: next.width,
                    y: 0,
                    width: self.width - next.width,
                    height: self.height,
                },
            );
        }

        region
    }

    /// Returns all packed-cell differences needed to transform this screen into `next`.
    ///
    /// This mirrors CC Ink's packed `screen.diff(...)`: only the union of the
    /// previous/current damage regions plus shrink regions is scanned, additions
    /// for grown areas skip empty cells, and removals are emitted even for blank
    /// previous cells so terminal writers can clear stale output.
    pub fn diff(&self, next: &Self) -> Vec<CanvasPackedDiffChange> {
        let mut changes = Vec::new();
        self.diff_each(next, |change| {
            changes.push(change);
            false
        });
        changes
    }

    /// Calls `callback` for each packed-cell difference between this screen and `next`.
    ///
    /// The callback may return `true` to stop iteration early; this method
    /// returns whether such an early stop occurred. This is an optimization-only
    /// helper for custom retained renderers and never performs terminal I/O.
    pub fn diff_each<F>(&self, next: &Self, mut callback: F) -> bool
    where
        F: FnMut(CanvasPackedDiffChange) -> bool,
    {
        let Some(region) = self.diff_region(next) else {
            return false;
        };
        let max_height = self.height.max(next.height);
        let max_width = self.width.max(next.width);
        let start_y = region.y.min(max_height);
        let start_x = region.x.min(max_width);
        let end_y = region.y.saturating_add(region.height).min(max_height);
        let end_x = region.x.saturating_add(region.width).min(max_width);

        for y in start_y..end_y {
            for x in start_x..end_x {
                let removed = self.cell(x, y);
                let added = next.cell(x, y);
                let changed = match (removed, added) {
                    (Some(previous), Some(next)) => previous != next,
                    (Some(_), None) => true,
                    (None, Some(next)) => !next.is_empty(),
                    (None, None) => false,
                };
                if changed
                    && callback(CanvasPackedDiffChange {
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

    /// Writes one packed row as ANSI without a trailing newline.
    ///
    /// This is an opt-in packed-screen counterpart to CC Ink's
    /// `renderFrameSlice(...)` sparse row writer: spacer/default cells are
    /// skipped with cursor-forward movement, style transitions are cached via
    /// [`CanvasStyleTransitionCache`], OSC 8 hyperlinks are opened/closed around
    /// linked cells, and the row is reset/erased to EOL on exit. The caller must
    /// position the terminal cursor at `start_col` before writing this fragment.
    pub fn write_ansi_row_with_style_cache<W: Write>(
        &self,
        pools: &CanvasPackedCellPools,
        style_cache: &mut CanvasStyleTransitionCache,
        y: usize,
        start_col: usize,
        mut w: W,
    ) -> io::Result<()> {
        if y >= self.height || start_col >= self.width {
            return Ok(());
        }

        let mut current_style = CanvasResolvedStyle::default();
        let mut current_style_id = 0u32;
        let mut current_hyperlink: Option<&str> = None;
        let mut last_rendered_style_id = None;
        let mut cursor_x = start_col;
        for x in start_col..self.width {
            let Some(cell) = self.visible_cell(pools, x, y, last_rendered_style_id) else {
                continue;
            };
            let view = pools.cell_view(cell);
            let cell_width = if view.width == CanvasPackedCellWidth::Wide {
                2
            } else {
                1
            };
            if cell_width == 2 && x.saturating_add(2) > self.width {
                continue;
            }

            if x > cursor_x {
                write_cursor_forward(&mut w, x - cursor_x)?;
            }

            if view.hyperlink != current_hyperlink {
                if current_hyperlink.is_some() {
                    hyperlink_close(&mut w)?;
                }
                if let Some(href) = view.hyperlink {
                    hyperlink_open(&mut w, href)?;
                }
                current_hyperlink = view.hyperlink;
            }

            let target_style = view.style.unwrap_or_default();
            let transition = style_cache.transition(current_style, target_style);
            if !transition.is_empty() {
                w.write_all(transition.as_bytes())?;
                current_style = target_style;
                current_style_id = view.style_id;
            }

            if cell_width == 2
                && packed_grapheme_needs_width_compensation(view.character)
                && x + 1 < self.width
            {
                write!(
                    w,
                    "\x1b[{}G \x1b[{}G{}\x1b[{}G",
                    x + 2,
                    x + 1,
                    view.character,
                    x + cell_width + 1
                )?;
            } else {
                w.write_all(view.character.as_bytes())?;
            }
            cursor_x = x.saturating_add(cell_width);
            last_rendered_style_id = Some(view.style_id);
        }

        if current_hyperlink.is_some() {
            hyperlink_close(&mut w)?;
        }
        let reset = style_cache.transition(current_style, CanvasResolvedStyle::default());
        if !reset.is_empty() {
            w.write_all(reset.as_bytes())?;
        } else if current_style_id != 0 {
            sgr_reset(&mut w)?;
        }
        erase_to_eol(&mut w)?;
        sgr_reset(&mut w)?;
        Ok(())
    }

    /// Returns one packed row's ANSI representation without a trailing newline.
    pub fn ansi_row_with_style_cache(
        &self,
        pools: &CanvasPackedCellPools,
        style_cache: &mut CanvasStyleTransitionCache,
        y: usize,
        start_col: usize,
    ) -> io::Result<String> {
        let mut out = Vec::new();
        self.write_ansi_row_with_style_cache(pools, style_cache, y, start_col, &mut out)?;
        String::from_utf8(out).map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))
    }
}

fn write_cursor_forward(w: &mut impl Write, cols: usize) -> io::Result<()> {
    if cols == 0 {
        return Ok(());
    }
    write!(w, "\x1b[{cols}C")
}

fn packed_grapheme_needs_width_compensation(value: &str) -> bool {
    let Some(cp) = value.chars().next().map(|ch| ch as u32) else {
        return false;
    };
    (0x1fa70..=0x1faff).contains(&cp)
        || (0x1fb00..=0x1fbff).contains(&cp)
        || (value.len() >= 2 && value.contains('\u{fe0f}'))
}

impl CanvasPackedOutputClip {
    fn intersect(self, child: Self) -> Self {
        fn max_bound(a: Option<usize>, b: Option<usize>) -> Option<usize> {
            match (a, b) {
                (Some(a), Some(b)) => Some(a.max(b)),
                (Some(a), None) => Some(a),
                (None, Some(b)) => Some(b),
                (None, None) => None,
            }
        }
        fn min_bound(a: Option<usize>, b: Option<usize>) -> Option<usize> {
            match (a, b) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (Some(a), None) => Some(a),
                (None, Some(b)) => Some(b),
                (None, None) => None,
            }
        }

        Self {
            x1: max_bound(self.x1, child.x1),
            x2: min_bound(self.x2, child.x2),
            y1: max_bound(self.y1, child.y1),
            y2: min_bound(self.y2, child.y2),
        }
    }
}

impl CanvasPackedOutput {
    /// Creates an empty packed output queue and reusable screen.
    pub fn new(width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            screen: CanvasPackedScreen::new(width, height),
            operations: Vec::new(),
            line_cache: CanvasPackedLineCache::new(),
        }
    }

    /// Reuses this output queue for a new frame and clears queued operations.
    pub fn reset(&mut self, width: usize, height: usize) {
        self.width = width;
        self.height = height;
        self.operations.clear();
        self.screen.reset(width, height);
    }

    /// Returns the current target width in terminal cells.
    pub fn width(&self) -> usize {
        self.width
    }

    /// Returns the current target height in terminal rows.
    pub fn height(&self) -> usize {
        self.height
    }

    /// Returns the number of queued operations.
    pub fn operation_count(&self) -> usize {
        self.operations.len()
    }

    /// Returns the number of retained line-cache entries.
    pub fn line_cache_len(&self) -> usize {
        self.line_cache.len()
    }

    /// Clears the retained line cache.
    pub fn clear_line_cache(&mut self) {
        self.line_cache.clear();
    }

    /// Returns queued operations for inspection or custom planning.
    pub fn operations(&self) -> &[CanvasPackedOutputOperation] {
        &self.operations
    }

    /// Queues a single-style text write. Newlines create multiple logical rows.
    pub fn write(
        &mut self,
        x: usize,
        y: usize,
        text: impl Into<String>,
        style: CanvasResolvedStyle,
        hyperlink: Option<&str>,
        soft_wrap: Option<Vec<bool>>,
    ) {
        let hyperlink = hyperlink.map(str::to_string);
        self.write_runs(
            x,
            y,
            [CanvasPackedLineOwnedRun {
                text: text.into(),
                style,
                hyperlink,
            }],
            soft_wrap,
        );
    }

    /// Queues styled text runs. Newlines inside run text create multiple rows.
    pub fn write_runs<I>(&mut self, x: usize, y: usize, runs: I, soft_wrap: Option<Vec<bool>>)
    where
        I: IntoIterator<Item = CanvasPackedLineOwnedRun>,
    {
        let lines = split_packed_output_runs(runs);
        if lines.is_empty() {
            return;
        }
        self.operations.push(CanvasPackedOutputOperation::Write {
            x,
            y,
            lines,
            soft_wrap,
        });
    }

    /// Pushes a nested clip rectangle.
    pub fn clip(&mut self, clip: CanvasPackedOutputClip) {
        self.operations
            .push(CanvasPackedOutputOperation::Clip(clip));
    }

    /// Pops the current clip rectangle.
    pub fn unclip(&mut self) {
        self.operations.push(CanvasPackedOutputOperation::Unclip);
    }

    /// Queues a same-coordinate blit from a packed source screen.
    pub fn blit(
        &mut self,
        src: CanvasPackedScreen,
        x: usize,
        y: usize,
        width: usize,
        height: usize,
    ) {
        self.operations.push(CanvasPackedOutputOperation::Blit {
            src,
            x,
            y,
            width,
            height,
        });
    }

    /// Queues a damage-only clear region.
    pub fn clear(&mut self, region: DamageRegion, from_absolute: bool) {
        self.operations.push(CanvasPackedOutputOperation::Clear {
            region,
            from_absolute,
        });
    }

    /// Queues a no-select metadata region, replayed after writes and blits.
    pub fn no_select(&mut self, region: DamageRegion) {
        self.operations
            .push(CanvasPackedOutputOperation::NoSelect { region });
    }

    /// Queues a full-width packed row shift.
    pub fn shift(&mut self, top: usize, bottom: usize, delta: i32) {
        self.operations
            .push(CanvasPackedOutputOperation::Shift { top, bottom, delta });
    }

    /// Applies queued operations to the reusable packed screen and returns it.
    pub fn get(&mut self, pools: &mut CanvasPackedCellPools) -> &CanvasPackedScreen {
        self.screen.reset(self.width, self.height);
        let operations = self.operations.clone();

        let mut absolute_clears = Vec::new();
        for operation in &operations {
            let CanvasPackedOutputOperation::Clear {
                region,
                from_absolute,
            } = operation
            else {
                continue;
            };
            let Some(region) = clip_damage_region(*region, self.width, self.height) else {
                continue;
            };
            self.screen.mark_damage(region);
            if *from_absolute {
                absolute_clears.push(region);
            }
        }

        let mut clips = Vec::<CanvasPackedOutputClip>::new();
        for operation in &operations {
            match operation {
                CanvasPackedOutputOperation::Clear { .. } => {}
                CanvasPackedOutputOperation::Clip(clip) => {
                    let next = clips
                        .last()
                        .copied()
                        .map_or(*clip, |parent| parent.intersect(*clip));
                    clips.push(next);
                }
                CanvasPackedOutputOperation::Unclip => {
                    clips.pop();
                }
                CanvasPackedOutputOperation::Blit {
                    src,
                    x,
                    y,
                    width,
                    height,
                } => {
                    let Some((start_x, start_y, copy_width, copy_height)) = clip_packed_region(
                        *x,
                        *y,
                        *width,
                        *height,
                        self.width,
                        self.height,
                        src.width,
                        src.height,
                        clips.last().copied(),
                    ) else {
                        continue;
                    };
                    self.screen.blit_region_from_excluding_clears(
                        src,
                        start_x,
                        start_y,
                        copy_width,
                        copy_height,
                        &absolute_clears,
                    );
                }
                CanvasPackedOutputOperation::Shift { top, bottom, delta } => {
                    self.screen.shift_rows(*top, *bottom, *delta);
                }
                CanvasPackedOutputOperation::Write {
                    x,
                    y,
                    lines,
                    soft_wrap,
                } => {
                    self.apply_write_operation(
                        pools,
                        *x,
                        *y,
                        lines,
                        soft_wrap.as_deref(),
                        clips.last().copied(),
                    );
                }
                CanvasPackedOutputOperation::NoSelect { .. } => {}
            }
        }

        for operation in &operations {
            if let CanvasPackedOutputOperation::NoSelect { region } = operation {
                self.screen
                    .mark_no_select_region(region.x, region.y, region.width, region.height);
            }
        }

        &self.screen
    }

    fn apply_write_operation(
        &mut self,
        pools: &mut CanvasPackedCellPools,
        x: usize,
        y: usize,
        lines: &[Vec<CanvasPackedLineOwnedRun>],
        soft_wrap: Option<&[bool]>,
        clip: Option<CanvasPackedOutputClip>,
    ) {
        let min_x = clip.and_then(|clip| clip.x1).unwrap_or(0).min(self.width);
        let max_x = clip
            .and_then(|clip| clip.x2)
            .unwrap_or(self.width)
            .min(self.width);
        let min_y = clip.and_then(|clip| clip.y1).unwrap_or(0).min(self.height);
        let max_y = clip
            .and_then(|clip| clip.y2)
            .unwrap_or(self.height)
            .min(self.height);
        if min_x >= max_x || min_y >= max_y || y >= self.height || lines.is_empty() {
            return;
        }

        let from = min_y.saturating_sub(y).min(lines.len());
        let to = lines.len().min(max_y.saturating_sub(y));
        if from >= to {
            return;
        }

        let mut prev_content_end = 0usize;
        if let Some(soft_wrap) = soft_wrap {
            if from > 0 && soft_wrap.get(from).copied() == Some(true) {
                prev_content_end =
                    measure_packed_output_line_end(&mut self.line_cache, x, &lines[from - 1]);
            }
        }

        for (offset_y, line) in lines[from..to].iter().enumerate() {
            let line_idx = from + offset_y;
            let line_y = y + line_idx;
            let content_end = self.write_output_line(pools, x, line_y, line, min_x, max_x);
            if let Some(soft_wrap) = soft_wrap {
                if let Some(slot) = self.screen.soft_wrap.get_mut(line_y) {
                    *slot = if soft_wrap.get(line_idx).copied() == Some(true) {
                        prev_content_end.min(self.width)
                    } else {
                        0
                    };
                }
                prev_content_end = content_end;
            }
        }
    }

    fn write_output_line(
        &mut self,
        pools: &mut CanvasPackedCellPools,
        x: usize,
        y: usize,
        line: &[CanvasPackedLineOwnedRun],
        min_x: usize,
        max_x: usize,
    ) -> usize {
        let mut offset_x = x;
        for run in line {
            let style_id = pools.intern_style(run.style);
            let hyperlink_id = pools.intern_hyperlink(run.hyperlink.as_deref());
            let clusters = self.line_cache.clusters(&run.text);
            offset_x = self.screen.write_cached_clusters_clipped_with_ids(
                pools,
                clusters,
                offset_x,
                y,
                style_id,
                hyperlink_id,
                min_x,
                max_x,
            );
        }
        offset_x
    }
}

fn split_packed_output_runs<I>(runs: I) -> Vec<Vec<CanvasPackedLineOwnedRun>>
where
    I: IntoIterator<Item = CanvasPackedLineOwnedRun>,
{
    let mut lines: Vec<Vec<CanvasPackedLineOwnedRun>> = vec![Vec::new()];
    for run in runs {
        let parts = run.text.split('\n').collect::<Vec<_>>();
        for (idx, part) in parts.iter().enumerate() {
            if idx > 0 {
                lines.push(Vec::new());
            }
            if !part.is_empty() {
                lines.last_mut().unwrap().push(CanvasPackedLineOwnedRun {
                    text: (*part).to_string(),
                    style: run.style,
                    hyperlink: run.hyperlink.clone(),
                });
            }
        }
    }
    lines
}

fn measure_packed_output_line_end(
    cache: &mut CanvasPackedLineCache,
    x: usize,
    line: &[CanvasPackedLineOwnedRun],
) -> usize {
    let mut offset_x = x;
    for run in line {
        let clusters = cache.clusters(&run.text);
        offset_x = measure_packed_clusters_end(clusters, offset_x);
    }
    offset_x
}

fn measure_packed_clusters_end(clusters: &[CanvasPackedLineCluster], mut offset_x: usize) -> usize {
    let mut idx = 0;
    while idx < clusters.len() {
        let cluster = &clusters[idx];
        if let Some(code) = single_ascii_byte(&cluster.text).filter(|code| *code <= 0x1f) {
            if code == b'\t' {
                offset_x = offset_x.saturating_add(8 - (offset_x % 8));
                idx += 1;
            } else if code == 0x1b {
                idx = skip_escape_sequence_packed_clusters(clusters, idx);
            } else {
                idx += 1;
            }
            continue;
        }
        offset_x = offset_x.saturating_add(cluster.width);
        idx += 1;
    }
    offset_x
}

fn clip_damage_region(region: DamageRegion, width: usize, height: usize) -> Option<DamageRegion> {
    let start_x = region.x.min(width);
    let start_y = region.y.min(height);
    let max_x = region.x.saturating_add(region.width).min(width);
    let max_y = region.y.saturating_add(region.height).min(height);
    (start_x < max_x && start_y < max_y).then_some(DamageRegion {
        x: start_x,
        y: start_y,
        width: max_x - start_x,
        height: max_y - start_y,
    })
}

fn clip_packed_region(
    x: usize,
    y: usize,
    width: usize,
    height: usize,
    dst_width: usize,
    dst_height: usize,
    src_width: usize,
    src_height: usize,
    clip: Option<CanvasPackedOutputClip>,
) -> Option<(usize, usize, usize, usize)> {
    let start_x = x
        .max(clip.and_then(|clip| clip.x1).unwrap_or(0))
        .min(dst_width)
        .min(src_width);
    let start_y = y
        .max(clip.and_then(|clip| clip.y1).unwrap_or(0))
        .min(dst_height)
        .min(src_height);
    let max_x = x
        .saturating_add(width)
        .min(clip.and_then(|clip| clip.x2).unwrap_or(usize::MAX))
        .min(dst_width)
        .min(src_width);
    let max_y = y
        .saturating_add(height)
        .min(clip.and_then(|clip| clip.y2).unwrap_or(usize::MAX))
        .min(dst_height)
        .min(src_height);
    (start_x < max_x && start_y < max_y).then_some((
        start_x,
        start_y,
        max_x - start_x,
        max_y - start_y,
    ))
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

    fn normalized(self) -> (SelectionPoint, SelectionPoint) {
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
struct SelectionSpan {
    lo: SelectionPoint,
    hi: SelectionPoint,
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
    anchor_span: Option<SelectionSpan>,
    virtual_anchor_row: Option<isize>,
    virtual_focus_row: Option<isize>,
    scrolled_off_above: Vec<String>,
    scrolled_off_below: Vec<String>,
    scrolled_off_above_soft_wrap: Vec<bool>,
    scrolled_off_below_soft_wrap: Vec<bool>,
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
    selection: SelectionState,
    click_tracker: SelectionClickTracker,
    last_hover: Option<SelectionPoint>,
    last_drag_scroll_dir: Option<SelectionDragScrollDirection>,
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

    fn word_bounds_at(&self, col: usize, row: usize) -> Option<(usize, usize)> {
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

    fn extract_selected_row(&self, row: usize, col_start: usize, col_end: usize) -> String {
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
    pub fn diff_each<F>(&self, next: &Self, mut callback: F) -> bool
    where
        F: FnMut(CanvasDiffChangeRef<'_>) -> bool,
    {
        let max_height = self.height().max(next.height());
        let max_width = self.width.max(next.width);

        for y in 0..max_height {
            let row_start = if y < self.height() && y < next.height() && self.width == next.width {
                self.row_change_start(next, y).unwrap_or(max_width)
            } else {
                0
            };
            if row_start >= max_width {
                continue;
            }

            for x in row_start..max_width {
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

    fn clear_text(&mut self, x: usize, y: usize, w: usize, h: usize) {
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

    fn set_background_color(&mut self, x: usize, y: usize, w: usize, h: usize, color: Color) {
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

    fn set_text_row_str_clipped(
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

    fn row(&self, y: usize) -> &[CanvasCell] {
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
    fn overlay_row(&self, y: usize) -> &[Option<StyleOverlay>] {
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

    /// Writes a single row.
    ///
    /// In ANSI mode the caller must ensure that SGR state is reset (e.g. via
    /// `CSI 0 m`) before invoking this method; the function does not emit a
    /// leading reset of its own. It always leaves SGR state reset on return,
    /// so consecutive calls (or any subsequent writer use) start from a clean
    /// state.
    fn write_row_impl<W: Write>(
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

/// Represents a writeable region of a [`Canvas`]. All coordinates provided to functions of this
/// type are relative to the region's top-left corner.
pub struct CanvasSubviewMut<'a> {
    x: isize,
    y: isize,
    clip_x: isize,
    clip_y: isize,
    clip_width: usize,
    clip_height: usize,
    canvas: &'a mut Canvas,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prelude::*;
    use crossterm::{csi, style::Colored};

    #[test]
    fn test_render_metadata_does_not_affect_canvas_equality() {
        let mut a = Canvas::new(10, 3);
        let b = Canvas::new(10, 3);
        a.set_scroll_hint(ScrollHint {
            top: 0,
            bottom: 2,
            delta: 1,
        });
        a.force_full_repaint();
        a.mark_damage(DamageRegion {
            x: 2,
            y: 1,
            width: 3,
            height: 1,
        });

        assert!(
            a == b,
            "render metadata is handled explicitly by the render loop"
        );
        assert_eq!(
            a.scroll_hint(),
            Some(ScrollHint {
                top: 0,
                bottom: 2,
                delta: 1,
            })
        );
        assert!(a.should_force_full_repaint());
        assert_eq!(
            a.damage_region(),
            Some(DamageRegion {
                x: 2,
                y: 1,
                width: 3,
                height: 1,
            })
        );
    }

    #[test]
    fn test_damage_regions_union_and_track_rows() {
        let mut canvas = Canvas::new(10, 5);
        canvas.mark_damage(DamageRegion {
            x: 2,
            y: 1,
            width: 3,
            height: 2,
        });
        canvas.mark_damage(DamageRegion {
            x: 1,
            y: 3,
            width: 4,
            height: 1,
        });

        assert_eq!(
            canvas.damage_region(),
            Some(DamageRegion {
                x: 1,
                y: 1,
                width: 4,
                height: 3,
            })
        );
        assert!(!canvas.row_is_damaged(0));
        assert!(canvas.row_is_damaged(1));
        assert!(canvas.row_is_damaged(3));
        assert!(!canvas.row_is_damaged(4));

        canvas.clear_damage();
        assert_eq!(canvas.damage_region(), None);
    }

    #[test]
    fn test_damage_regions_clip_to_canvas_bounds() {
        let mut canvas = Canvas::new(10, 5);
        canvas.mark_damage(DamageRegion {
            x: 8,
            y: 3,
            width: 10,
            height: 10,
        });
        assert_eq!(
            canvas.damage_region(),
            Some(DamageRegion {
                x: 8,
                y: 3,
                width: 2,
                height: 2,
            })
        );

        canvas.mark_damage(DamageRegion {
            x: 10,
            y: 1,
            width: 2,
            height: 1,
        });
        assert_eq!(
            canvas.damage_region(),
            Some(DamageRegion {
                x: 8,
                y: 3,
                width: 2,
                height: 2,
            }),
            "fully off-canvas damage should be ignored"
        );
    }

    #[test]
    fn test_canvas_diff_each_reports_cells_overlays_growth_and_shrink() {
        let mut prev = Canvas::new(4, 2);
        prev.subview_mut(0, 0, 0, 0, 4, 2)
            .set_text(0, 0, "ab", CanvasTextStyle::default());
        prev.subview_mut(0, 0, 0, 0, 4, 2)
            .set_text(0, 1, "zz", CanvasTextStyle::default());

        let mut next = Canvas::new(5, 1);
        next.subview_mut(0, 0, 0, 0, 5, 1)
            .set_text(0, 0, "ac", CanvasTextStyle::default());
        next.set_overlay(2, 0, StyleOverlay::inverse());
        next.subview_mut(0, 0, 0, 0, 5, 1)
            .set_text(4, 0, "d", CanvasTextStyle::default());

        let changes = prev.diff(&next);
        let coords = changes
            .iter()
            .map(|change| (change.x, change.y))
            .collect::<Vec<_>>();
        assert_eq!(
            coords,
            vec![(1, 0), (2, 0), (4, 0), (0, 1), (1, 1), (2, 1), (3, 1)]
        );
        assert_eq!(
            changes[0]
                .removed
                .as_ref()
                .and_then(|cell| cell.cell.text()),
            Some("b")
        );
        assert_eq!(
            changes[0].added.as_ref().and_then(|cell| cell.cell.text()),
            Some("c")
        );
        assert_eq!(
            changes[1].removed.as_ref().and_then(|cell| cell.overlay),
            None
        );
        assert_eq!(
            changes[1].added.as_ref().and_then(|cell| cell.overlay),
            Some(StyleOverlay::inverse())
        );
        assert!(changes[2].removed.is_none());
        assert_eq!(
            changes[2].added.as_ref().and_then(|cell| cell.cell.text()),
            Some("d")
        );
        assert_eq!(
            changes[3]
                .removed
                .as_ref()
                .and_then(|cell| cell.cell.text()),
            Some("z")
        );
        assert!(changes[3].added.is_none());

        let mut first = None;
        let stopped = prev.diff_each(&next, |change| {
            first = Some((change.x, change.y));
            true
        });
        assert!(stopped);
        assert_eq!(first, Some((1, 0)));
    }

    #[test]
    fn test_canvas_packed_screen_interns_cells_styles_links_and_metadata() {
        let mut canvas = Canvas::new(5, 2);
        let style = CanvasTextStyle {
            color: Some(Color::Red),
            weight: Weight::Bold,
            ..Default::default()
        };
        canvas.subview_mut(0, 0, 0, 0, 5, 2).set_text_with_link(
            0,
            0,
            "A好",
            style,
            Some("https://example.com"),
        );
        canvas.set_overlay(0, 0, StyleOverlay::selection_background(Color::Blue));
        canvas.mark_no_select_region(0, 0, 1, 1);
        canvas.mark_soft_wrap_continuation(1, 2);
        canvas.mark_damage(DamageRegion {
            x: 0,
            y: 0,
            width: 2,
            height: 1,
        });

        let mut pools = CanvasPackedCellPools::new();
        let packed = canvas.pack_with(&mut pools);

        assert_eq!(packed.width, 5);
        assert_eq!(packed.height, 2);
        assert_eq!(packed.cells.len(), 10);
        assert_eq!(
            pools.character(packed.cell(0, 0).unwrap().char_id),
            Some("A")
        );
        assert_eq!(
            pools.character(packed.cell(1, 0).unwrap().char_id),
            Some("好")
        );
        assert_eq!(
            pools.character(packed.cell(2, 0).unwrap().char_id),
            Some("")
        );
        assert_eq!(
            packed.cell(1, 0).unwrap().width,
            CanvasPackedCellWidth::Wide
        );
        assert_eq!(
            packed.cell(2, 0).unwrap().width,
            CanvasPackedCellWidth::WidthTail
        );
        assert_eq!(
            pools.hyperlink(packed.cell(0, 0).unwrap().hyperlink_id),
            Some("https://example.com")
        );
        assert_eq!(
            pools.style(packed.cell(0, 0).unwrap().style_id),
            Some(CanvasResolvedStyle {
                text: style.with_overlay(&StyleOverlay::selection_background(Color::Blue)),
                background_color: Some(Color::Blue),
            })
        );
        assert!(packed.is_no_select(0, 0));
        assert_eq!(packed.soft_wrap_continuation(1), 2);
        assert!(packed.damage_region.is_some());
        assert!(packed.is_empty_cell(4, 1));

        let second = canvas.pack_with(&mut pools);
        assert_eq!(
            packed, second,
            "stable pools make snapshots directly comparable"
        );
    }

    #[test]
    fn test_canvas_packed_screen_diff_uses_damage_and_shrink_regions_like_cc_screen() {
        let mut prev = Canvas::new(4, 2);
        {
            let mut view = prev.subview_mut(0, 0, 0, 0, 4, 2);
            view.set_text(0, 0, "ab", CanvasTextStyle::default());
            view.set_text(0, 1, "zzzz", CanvasTextStyle::default());
        }
        prev.clear_damage();

        let mut next = prev.clone();
        next.subview_mut(0, 0, 0, 0, 4, 2)
            .set_text(1, 0, "c", CanvasTextStyle::default());
        next.mark_damage(DamageRegion {
            x: 1,
            y: 0,
            width: 1,
            height: 1,
        });

        let mut pools = CanvasPackedCellPools::new();
        let prev = prev.pack_with(&mut pools);
        let next = next.pack_with(&mut pools);
        let changes = prev.diff(&next);
        assert_eq!(changes.len(), 1);
        assert_eq!((changes[0].x, changes[0].y), (1, 0));
        assert_eq!(
            pools.character(changes[0].removed.unwrap().char_id),
            Some("b")
        );
        assert_eq!(
            pools.character(changes[0].added.unwrap().char_id),
            Some("c")
        );

        let mut first = None;
        let stopped = prev.diff_each(&next, |change| {
            first = Some((change.x, change.y));
            true
        });
        assert!(stopped);
        assert_eq!(first, Some((1, 0)));

        let shrunk = Canvas::new(4, 1).pack_with(&mut pools);
        let shrink_changes = prev.diff(&shrunk);
        assert_eq!(shrink_changes.len(), 4);
        assert_eq!(
            shrink_changes
                .iter()
                .map(|change| (change.x, change.y, change.added.is_none()))
                .collect::<Vec<_>>(),
            vec![(0, 1, true), (1, 1, true), (2, 1, true), (3, 1, true)]
        );
    }

    #[test]
    fn test_canvas_packed_screen_write_line_cache_matches_cc_output_write_line() {
        let mut pools = CanvasPackedCellPools::new();
        let mut cache = CanvasPackedLineCache::with_max_entries(2);
        let mut style_text = CanvasTextStyle::default();
        style_text.color = Some(Color::Green);
        let style = CanvasResolvedStyle {
            text: style_text,
            background_color: Some(Color::Blue),
        };

        let mut packed = CanvasPackedScreen::new(12, 2);
        let end = packed.write_line_with_cache(
            &mut pools,
            &mut cache,
            1,
            0,
            "A\tB\u{0007}\x1b[31mC",
            style,
            Some("https://example.com"),
        );
        assert_eq!(end, 10);
        assert_eq!(cache.len(), 1);
        assert_eq!(packed.char_in_cell(&pools, 1, 0), Some("A"));
        assert_eq!(packed.char_in_cell(&pools, 8, 0), Some("B"));
        assert_eq!(packed.char_in_cell(&pools, 9, 0), Some("C"));
        assert_eq!(packed.cell_view(&pools, 1, 0).unwrap().style, Some(style));
        assert_eq!(
            packed.cell_view(&pools, 1, 0).unwrap().hyperlink,
            Some("https://example.com")
        );
        assert_eq!(
            packed.cell_view(&pools, 2, 0).unwrap().style,
            Some(CanvasResolvedStyle::default()),
            "TAB expansion writes default-styled spaces like CC Ink output.ts"
        );
        assert_eq!(packed.cell_view(&pools, 2, 0).unwrap().hyperlink, None);

        let mut edge = CanvasPackedScreen::new(4, 1);
        let edge_end = edge.write_line_with_cache(&mut pools, &mut cache, 3, 0, "好", style, None);
        assert_eq!(edge_end, 4);
        assert_eq!(edge.char_in_cell(&pools, 3, 0), Some(" "));
        assert_eq!(
            edge.cell(3, 0).unwrap().width,
            CanvasPackedCellWidth::SpacerHead,
            "wide grapheme at the right edge becomes a SpacerHead placeholder"
        );

        let same_end = packed.write_line_with_cache(
            &mut pools,
            &mut cache,
            1,
            1,
            "A\tB\u{0007}\x1b[31mC",
            style,
            Some("https://example.com"),
        );
        assert_eq!(same_end, 10);
        assert_eq!(cache.len(), 2, "line cache reuses retained cluster entries");
    }

    #[test]
    fn test_canvas_packed_screen_write_line_runs_cache_styles_once_per_run() {
        let mut pools = CanvasPackedCellPools::new();
        let mut cache = CanvasPackedLineCache::new();
        let mut link_style = CanvasTextStyle::default();
        link_style.color = Some(Color::Cyan);
        let linked = CanvasResolvedStyle {
            text: link_style,
            background_color: Some(Color::DarkBlue),
        };
        let plain = CanvasResolvedStyle::default();

        let mut packed = CanvasPackedScreen::new(12, 1);
        let end = packed.write_line_runs_with_cache(
            &mut pools,
            &mut cache,
            0,
            0,
            [
                CanvasPackedLineRun {
                    text: "A\t",
                    style: linked,
                    hyperlink: Some("https://example.com"),
                },
                CanvasPackedLineRun {
                    text: "B好",
                    style: plain,
                    hyperlink: None,
                },
            ],
        );

        assert_eq!(end, 11);
        assert_eq!(cache.len(), 2);
        assert_eq!(packed.char_in_cell(&pools, 0, 0), Some("A"));
        assert_eq!(packed.char_in_cell(&pools, 8, 0), Some("B"));
        assert_eq!(packed.char_in_cell(&pools, 9, 0), Some("好"));
        assert_eq!(
            packed.cell(10, 0).unwrap().width,
            CanvasPackedCellWidth::WidthTail
        );
        assert_eq!(packed.cell_view(&pools, 0, 0).unwrap().style, Some(linked));
        assert_eq!(
            packed.cell_view(&pools, 0, 0).unwrap().hyperlink,
            Some("https://example.com")
        );
        assert_eq!(
            packed.cell_view(&pools, 1, 0).unwrap().style,
            Some(CanvasResolvedStyle::default()),
            "TAB expansion writes default spaces instead of inheriting the styled run"
        );
        assert_eq!(packed.cell_view(&pools, 8, 0).unwrap().style, Some(plain));
        assert_eq!(packed.cell_view(&pools, 8, 0).unwrap().hyperlink, None);

        let style_id = pools.intern_style(linked);
        let hyperlink_id = pools.intern_hyperlink(Some("https://example.com"));
        let mut ids = CanvasPackedScreen::new(4, 1);
        let ids_end = ids.write_line_runs_with_ids(
            &mut pools,
            &mut cache,
            0,
            0,
            [CanvasPackedLineRunIds {
                text: "CD",
                style_id,
                hyperlink_id,
            }],
        );
        assert_eq!(ids_end, 2);
        assert_eq!(ids.cell_view(&pools, 0, 0).unwrap().style, Some(linked));
        assert_eq!(
            ids.cell_view(&pools, 1, 0).unwrap().hyperlink,
            Some("https://example.com")
        );
    }

    #[test]
    fn test_canvas_packed_screen_write_line_runs_reorders_bidi_with_metadata_like_cc_output() {
        let mut pools = CanvasPackedCellPools::new();
        let mut cache = CanvasPackedLineCache::new();
        let mut rtl_style_text = CanvasTextStyle::default();
        rtl_style_text.color = Some(Color::Yellow);
        let rtl_style = CanvasResolvedStyle {
            text: rtl_style_text,
            background_color: None,
        };
        let rtl_style_id = pools.intern_style(rtl_style);
        let ltr_style_id = pools.intern_style(CanvasResolvedStyle::default());
        let link_id = pools.intern_hyperlink(Some("https://rtl.example"));
        let mut packed = CanvasPackedScreen::new(8, 1);

        let end = packed.write_line_runs_with_ids_bidi_mode(
            &mut pools,
            &mut cache,
            0,
            0,
            [
                CanvasPackedLineRunIds {
                    text: "אבג",
                    style_id: rtl_style_id,
                    hyperlink_id: link_id,
                },
                CanvasPackedLineRunIds {
                    text: "abc",
                    style_id: ltr_style_id,
                    hyperlink_id: 0,
                },
            ],
            true,
        );

        assert_eq!(end, 6);
        let rendered = (0..6)
            .filter_map(|x| packed.char_in_cell(&pools, x, 0))
            .collect::<String>();
        assert_eq!(rendered, "abcגבא");
        assert_eq!(
            packed.cell_view(&pools, 0, 0).unwrap().style_id,
            ltr_style_id
        );
        assert_eq!(
            packed.cell_view(&pools, 3, 0).unwrap().style_id,
            rtl_style_id
        );
        assert_eq!(
            packed.cell_view(&pools, 3, 0).unwrap().hyperlink,
            Some("https://rtl.example"),
            "bidi reorder preserves per-grapheme style/link metadata"
        );
    }

    #[test]
    fn test_canvas_packed_screen_ansi_row_writer_matches_cc_sparse_row_shape() {
        let mut pools = CanvasPackedCellPools::new();
        let mut style_cache = CanvasStyleTransitionCache::new();
        let mut screen = CanvasPackedScreen::new(6, 1);
        let mut linked_text = CanvasTextStyle::default();
        linked_text.color = Some(Color::Green);
        let linked_style = CanvasResolvedStyle {
            text: linked_text,
            background_color: None,
        };
        screen.set_cell_text(
            &mut pools,
            2,
            0,
            "A",
            linked_style,
            Some("https://example.com"),
            CanvasPackedCellWidth::Normal,
        );
        screen.set_cell_text(
            &mut pools,
            4,
            0,
            "B",
            CanvasResolvedStyle::default(),
            None,
            CanvasPackedCellWidth::Normal,
        );

        let row = screen
            .ansi_row_with_style_cache(&pools, &mut style_cache, 0, 0)
            .unwrap();
        assert!(
            row.starts_with("\x1b[2C"),
            "leading empty cells are skipped with cursor-forward movement: {row:?}"
        );
        assert!(row.contains("https://example.com"));
        assert!(row.contains("A"));
        assert!(row.contains("\x1b[1C"), "gap before B is sparse: {row:?}");
        assert!(row.contains("B"));
        assert!(
            row.contains("\x1b]8;;\x1b\\"),
            "OSC 8 link is closed: {row:?}"
        );
        assert!(
            row.contains("\x1b[K"),
            "row writer clears through EOL: {row:?}"
        );
        assert!(
            style_cache.len() >= 2,
            "style transitions are cached for packed sparse row writers"
        );
    }

    #[test]
    fn test_canvas_packed_output_queue_matches_cc_output_get_ordering() {
        let mut pools = CanvasPackedCellPools::new();
        let mut cache = CanvasPackedLineCache::new();
        let style = CanvasResolvedStyle::default();

        let mut src = CanvasPackedScreen::new(8, 3);
        src.write_line_with_cache(&mut pools, &mut cache, 0, 0, "OLD0", style, None);
        src.write_line_with_cache(&mut pools, &mut cache, 0, 1, "OLD1", style, None);

        let mut output = CanvasPackedOutput::new(8, 3);
        output.blit(src, 0, 0, 4, 2);
        output.clear(
            DamageRegion {
                x: 0,
                y: 0,
                width: 4,
                height: 1,
            },
            true,
        );
        output.clip(CanvasPackedOutputClip {
            x1: Some(2),
            x2: Some(5),
            y1: Some(0),
            y2: Some(3),
        });
        output.write(0, 2, "ABCDE", style, None, None);
        output.unclip();
        output.no_select(DamageRegion {
            x: 1,
            y: 1,
            width: 2,
            height: 1,
        });

        let screen = output.get(&mut pools);
        assert_eq!(screen.char_in_cell(&pools, 0, 0), Some(" "));
        assert_eq!(screen.char_in_cell(&pools, 0, 1), Some("O"));
        assert_eq!(screen.char_in_cell(&pools, 3, 1), Some("1"));
        assert_eq!(screen.char_in_cell(&pools, 0, 2), Some(" "));
        assert_eq!(screen.char_in_cell(&pools, 1, 2), Some(" "));
        assert_eq!(screen.char_in_cell(&pools, 2, 2), Some("C"));
        assert_eq!(screen.char_in_cell(&pools, 4, 2), Some("E"));
        assert!(screen.is_no_select(1, 1));
        assert!(screen.is_no_select(2, 1));
        assert!(
            screen
                .damage_region()
                .is_some_and(|damage| damage.y == 0 && damage.height >= 3),
            "clear, blit, and clipped write regions are all represented in damage"
        );
        assert_eq!(output.line_cache_len(), 1);
    }

    #[test]
    fn test_canvas_packed_output_soft_wrap_survives_vertical_clip_like_cc_output() {
        let mut pools = CanvasPackedCellPools::new();
        let style = CanvasResolvedStyle::default();
        let mut output = CanvasPackedOutput::new(8, 2);
        output.clip(CanvasPackedOutputClip {
            x1: None,
            x2: None,
            y1: Some(1),
            y2: Some(2),
        });
        output.write(0, 0, "prev\ncont", style, None, Some(vec![false, true]));

        let screen = output.get(&mut pools);
        assert_eq!(screen.char_in_cell(&pools, 0, 0), Some(" "));
        assert_eq!(screen.char_in_cell(&pools, 0, 1), Some("c"));
        assert_eq!(
            screen.soft_wrap_continuation(1),
            4,
            "the clipped previous row's content end is retained for soft-wrap copy"
        );
    }

    #[test]
    fn test_canvas_packed_style_overlay_helpers_match_cc_style_pool_overlays() {
        let mut pools = CanvasPackedCellPools::new();
        let base_style = CanvasResolvedStyle {
            text: CanvasTextStyle {
                color: Some(Color::Green),
                weight: Weight::Light,
                italic: true,
                invert: true,
                ..Default::default()
            },
            background_color: Some(Color::Red),
        };
        let base_id = pools.intern_style(base_style);

        let inverse_id = pools.style_id_with_inverse(base_id);
        assert_eq!(
            inverse_id, base_id,
            "already-inverted styles intern back to the base ID"
        );

        let selection_id = pools.style_id_with_selection_background(base_id, Color::Blue);
        let selection = pools.style(selection_id).unwrap();
        assert_eq!(selection.text.color, Some(Color::Green));
        assert_eq!(selection.text.weight, Weight::Light);
        assert!(selection.text.italic);
        assert!(
            !selection.text.invert,
            "selection background disables inverse like CC Ink"
        );
        assert_eq!(selection.background_color, Some(Color::Blue));
        assert!(selection.is_visible_on_space());

        let current_id = pools.style_id_with_current_match(base_id, Color::Yellow);
        let current = pools.style(current_id).unwrap();
        assert_eq!(current.text.color, Some(Color::Yellow));
        assert_eq!(
            current.background_color, None,
            "current match strips existing background"
        );
        assert_eq!(current.text.weight, Weight::Bold);
        assert!(current.text.underline);
        assert!(current.text.invert);
        assert!(current.text.italic, "unowned style fields are preserved");

        let fallback_id = pools.style_id_with_overlay(u32::MAX, StyleOverlay::inverse());
        assert_eq!(
            pools.style(fallback_id).unwrap(),
            CanvasResolvedStyle::default().with_overlay(StyleOverlay::inverse())
        );
    }

    #[test]
    fn test_canvas_packed_screen_row_change_start_matches_cc_damage_scan() {
        let mut pools = CanvasPackedCellPools::new();
        let mut prev = CanvasPackedScreen::new(6, 2);
        prev.set_cell_text(
            &mut pools,
            1,
            0,
            "好",
            CanvasResolvedStyle::default(),
            None,
            CanvasPackedCellWidth::Wide,
        );
        prev.set_cell_text(
            &mut pools,
            0,
            1,
            "z",
            CanvasResolvedStyle::default(),
            None,
            CanvasPackedCellWidth::Normal,
        );
        prev.clear_damage();

        let mut next = prev.clone();
        let tail_index = next.index(2, 0).unwrap();
        next.cells[tail_index] = CanvasPackedCell::default();
        assert_eq!(
            prev.row_change_start(&next, 0),
            Some(1),
            "tail-only differences repaint from the wide head"
        );

        let mut damaged = prev.clone();
        damaged.mark_damage(DamageRegion {
            x: 4,
            y: 0,
            width: 1,
            height: 1,
        });
        assert_eq!(prev.row_change_start(&damaged, 0), Some(4));
        damaged.clear_damage();
        assert_eq!(damaged.damage_region(), None);
        assert_eq!(prev.row_change_start(&damaged, 0), None);

        let shrunk = CanvasPackedScreen::new(6, 1);
        assert_eq!(prev.row_change_start(&shrunk, 1), Some(0));

        let empty_growth = CanvasPackedScreen::new(6, 3);
        assert_eq!(shrunk.row_change_start(&empty_growth, 2), None);

        let mut non_empty_growth = empty_growth.clone();
        non_empty_growth.set_cell_text(
            &mut pools,
            3,
            2,
            "g",
            CanvasResolvedStyle::default(),
            None,
            CanvasPackedCellWidth::Normal,
        );
        non_empty_growth.clear_damage();
        assert_eq!(shrunk.row_change_start(&non_empty_growth, 2), Some(3));

        let resized = CanvasPackedScreen::new(5, 2);
        assert_eq!(prev.row_change_start(&resized, 0), Some(0));
    }

    #[test]
    fn test_canvas_packed_screen_clear_blit_and_shift_match_cc_screen_helpers() {
        let mut pools = CanvasPackedCellPools::new();

        let mut source_canvas = Canvas::new(4, 3);
        {
            let mut view = source_canvas.subview_mut(0, 0, 0, 0, 4, 3);
            view.set_text(0, 0, "好x", CanvasTextStyle::default());
            view.set_text(0, 1, "b", CanvasTextStyle::default());
            view.set_text(0, 2, "c", CanvasTextStyle::default());
        }
        source_canvas.mark_no_select_region(0, 1, 1, 1);
        source_canvas.mark_soft_wrap_continuation(1, 3);
        source_canvas.clear_damage();

        let source = source_canvas.pack_with(&mut pools);
        let mut packed = Canvas::new(4, 3).pack_with(&mut pools);
        let blit_region = packed
            .blit_region_from(&source, 0, 0, 1, 1)
            .expect("wide-head blit should damage copied head and repaired tail");
        assert_eq!(
            blit_region,
            DamageRegion {
                x: 0,
                y: 0,
                width: 2,
                height: 1,
            }
        );
        assert_eq!(
            pools.character(packed.cell(0, 0).unwrap().char_id),
            Some("好")
        );
        assert_eq!(
            packed.cell(0, 0).unwrap().width,
            CanvasPackedCellWidth::Wide
        );
        assert_eq!(
            packed.cell(1, 0).unwrap().width,
            CanvasPackedCellWidth::WidthTail
        );

        let clear_region = packed
            .clear_region(1, 0, 1, 1)
            .expect("clearing a wide tail repairs the wide head");
        assert_eq!(
            clear_region,
            DamageRegion {
                x: 0,
                y: 0,
                width: 2,
                height: 1,
            }
        );
        assert!(packed.is_empty_cell(0, 0));
        assert!(packed.is_empty_cell(1, 0));

        let mut scrolling = source.clone();
        assert!(scrolling.shift_rows(0, 2, 1));
        assert_eq!(
            pools.character(scrolling.cell(0, 0).unwrap().char_id),
            Some("b")
        );
        assert_eq!(
            pools.character(scrolling.cell(0, 1).unwrap().char_id),
            Some("c")
        );
        assert!(scrolling.is_empty_cell(0, 2));
        assert!(scrolling.is_no_select(0, 0));
        assert_eq!(scrolling.soft_wrap_continuation(0), 3);
        assert_eq!(
            scrolling.damage_region, source.damage_region,
            "packed shiftRows is damage-neutral"
        );
    }

    #[test]
    fn test_canvas_packed_screen_blit_excluding_clears_matches_cc_output_guard() {
        let mut pools = CanvasPackedCellPools::new();
        let mut source_canvas = Canvas::new(6, 4);
        {
            let mut view = source_canvas.subview_mut(0, 0, 0, 0, 6, 4);
            view.set_text(0, 0, "aaaaaa", CanvasTextStyle::default());
            view.set_text(0, 1, "bbbbbb", CanvasTextStyle::default());
            view.set_text(0, 2, "cccccc", CanvasTextStyle::default());
            view.set_text(0, 3, "dddddd", CanvasTextStyle::default());
        }
        source_canvas.mark_no_select_region(1, 1, 1, 1);
        source_canvas.mark_no_select_region(1, 2, 2, 1);
        source_canvas.mark_soft_wrap_continuation(2, 6);
        source_canvas.clear_damage();
        let source = source_canvas.pack_with(&mut pools);

        let mut packed = CanvasPackedScreen::new(6, 4);
        packed.set_cell_text(
            &mut pools,
            1,
            0,
            "0",
            CanvasResolvedStyle::default(),
            None,
            CanvasPackedCellWidth::Normal,
        );
        packed.set_cell_text(
            &mut pools,
            1,
            1,
            "1",
            CanvasResolvedStyle::default(),
            None,
            CanvasPackedCellWidth::Normal,
        );
        packed.set_cell_text(
            &mut pools,
            1,
            2,
            "2",
            CanvasResolvedStyle::default(),
            None,
            CanvasPackedCellWidth::Normal,
        );
        packed.set_cell_text(
            &mut pools,
            1,
            3,
            "3",
            CanvasResolvedStyle::default(),
            None,
            CanvasPackedCellWidth::Normal,
        );
        packed.damage_region = None;

        let absolute_clear = DamageRegion {
            x: 1,
            y: 1,
            width: 4,
            height: 2,
        };
        let copied =
            packed.blit_region_from_excluding_clears(&source, 1, 0, 4, 4, &[absolute_clear]);
        assert_eq!(
            copied,
            vec![
                DamageRegion {
                    x: 1,
                    y: 0,
                    width: 4,
                    height: 1,
                },
                DamageRegion {
                    x: 1,
                    y: 3,
                    width: 4,
                    height: 1,
                },
            ]
        );
        assert_eq!(
            pools.character(packed.cell(1, 0).unwrap().char_id),
            Some("a")
        );
        assert_eq!(
            pools.character(packed.cell(1, 1).unwrap().char_id),
            Some("1")
        );
        assert_eq!(
            pools.character(packed.cell(1, 2).unwrap().char_id),
            Some("2")
        );
        assert_eq!(
            pools.character(packed.cell(1, 3).unwrap().char_id),
            Some("d")
        );
        assert!(
            !packed.is_no_select(1, 1),
            "excluded row keeps destination metadata"
        );
        assert_eq!(
            packed.soft_wrap_continuation(2),
            0,
            "excluded row keeps destination soft-wrap"
        );

        let partial_clear = DamageRegion {
            x: 2,
            y: 2,
            width: 1,
            height: 1,
        };
        let copied_partial =
            packed.blit_region_from_excluding_clears(&source, 1, 2, 4, 1, &[partial_clear]);
        assert_eq!(
            copied_partial,
            vec![DamageRegion {
                x: 1,
                y: 2,
                width: 4,
                height: 1,
            }]
        );
        assert_eq!(
            pools.character(packed.cell(1, 2).unwrap().char_id),
            Some("c")
        );
        assert!(packed.is_no_select(1, 2));
        assert_eq!(packed.soft_wrap_continuation(2), 6);
    }

    #[test]
    fn test_canvas_packed_screen_cell_views_match_cc_cell_at_helpers() {
        let mut pools = CanvasPackedCellPools::new();
        let style = CanvasResolvedStyle {
            text: CanvasTextStyle {
                color: Some(Color::Green),
                weight: Weight::Bold,
                ..Default::default()
            },
            background_color: Some(Color::Blue),
        };
        let mut packed = CanvasPackedScreen::new(4, 1);
        assert!(packed.set_cell_text(
            &mut pools,
            0,
            0,
            "A",
            style,
            Some("https://example.com"),
            CanvasPackedCellWidth::Normal,
        ));
        assert!(packed.set_cell_text(
            &mut pools,
            1,
            0,
            "好",
            CanvasResolvedStyle::default(),
            None,
            CanvasPackedCellWidth::Wide,
        ));

        let view = packed.cell_view(&pools, 0, 0).expect("cell in bounds");
        assert_eq!(view.character, "A");
        assert_eq!(view.style, Some(style));
        assert_eq!(view.hyperlink, Some("https://example.com"));
        assert_eq!(view.width, CanvasPackedCellWidth::Normal);
        assert_eq!(packed.char_in_cell(&pools, 0, 0), Some("A"));
        assert_eq!(packed.char_in_cell(&pools, 2, 0), Some(""));
        assert_eq!(packed.char_in_cell(&pools, 99, 0), None);

        let tail = packed
            .cell_view_at_index(&pools, 2)
            .expect("wide tail in bounds");
        assert_eq!(tail.character, "");
        assert_eq!(tail.width, CanvasPackedCellWidth::WidthTail);
        assert_eq!(
            packed
                .visible_cell_view(&pools, 1, 0, None)
                .unwrap()
                .character,
            "好"
        );
        assert_eq!(packed.visible_cell_view(&pools, 2, 0, None), None);
    }

    #[test]
    fn test_canvas_packed_screen_visible_cell_helper_matches_cc_visible_cell_at_index() {
        let mut pools = CanvasPackedCellPools::new();
        let mut packed = CanvasPackedScreen::new(6, 1);
        assert_eq!(packed.visible_cell(&pools, 0, 0, None), None);

        let fg_only = CanvasResolvedStyle {
            text: CanvasTextStyle {
                color: Some(Color::Green),
                ..Default::default()
            },
            background_color: None,
        };
        let fg_id = pools.intern_style(fg_only);
        assert!(!fg_only.is_visible_on_space());
        assert!(!pools.style_visible_on_space(fg_id));
        assert!(packed.set_cell_text(
            &mut pools,
            0,
            0,
            " ",
            fg_only,
            None,
            CanvasPackedCellWidth::Normal,
        ));
        assert!(packed.visible_cell(&pools, 0, 0, None).is_some());
        assert_eq!(packed.visible_cell(&pools, 0, 0, Some(fg_id)), None);
        assert!(packed.visible_cell(&pools, 0, 0, Some(0)).is_some());

        let visible_space_style = CanvasResolvedStyle {
            text: CanvasTextStyle {
                underline: true,
                ..Default::default()
            },
            background_color: Some(Color::Blue),
        };
        let visible_id = pools.intern_style(visible_space_style);
        assert!(visible_space_style.is_visible_on_space());
        assert!(pools.style_visible_on_space(visible_id));
        assert!(packed.set_cell_text(
            &mut pools,
            1,
            0,
            " ",
            visible_space_style,
            None,
            CanvasPackedCellWidth::Normal,
        ));
        assert!(packed
            .visible_cell(&pools, 1, 0, Some(visible_id))
            .is_some());

        assert!(packed.set_cell_text(
            &mut pools,
            2,
            0,
            " ",
            CanvasResolvedStyle::default(),
            Some("https://example.com"),
            CanvasPackedCellWidth::Normal,
        ));
        assert!(packed.visible_cell(&pools, 2, 0, Some(0)).is_some());

        assert!(packed.set_cell_text(
            &mut pools,
            3,
            0,
            "x",
            CanvasResolvedStyle::default(),
            None,
            CanvasPackedCellWidth::Normal,
        ));
        assert!(packed.visible_cell_at_index(&pools, 3, None).is_some());

        assert!(packed.set_cell_text(
            &mut pools,
            4,
            0,
            "好",
            CanvasResolvedStyle::default(),
            None,
            CanvasPackedCellWidth::Wide,
        ));
        assert!(packed.visible_cell(&pools, 4, 0, None).is_some());
        assert_eq!(packed.visible_cell(&pools, 5, 0, None), None);
    }

    #[test]
    fn test_canvas_packed_screen_reset_reuses_and_clears_like_cc_screen() {
        let mut pools = CanvasPackedCellPools::new();
        let mut packed = CanvasPackedScreen::new(2, 2);
        packed.set_cell_text(
            &mut pools,
            0,
            0,
            "x",
            CanvasResolvedStyle::default(),
            Some("https://example.com"),
            CanvasPackedCellWidth::Normal,
        );
        packed.mark_no_select_region(0, 0, 2, 2);
        packed.soft_wrap[1] = 2;
        assert!(packed.damage_region.is_some());

        packed.reset(3, 1);
        assert_eq!((packed.width, packed.height), (3, 1));
        assert_eq!(packed.cells.len(), 3);
        assert_eq!(packed.no_select.len(), 3);
        assert_eq!(packed.soft_wrap, vec![0]);
        assert_eq!(packed.damage_region, None);
        assert!(packed.cells.iter().all(|cell| cell.is_empty()));
        assert!(packed.no_select.iter().all(|marked| !marked));
    }

    #[test]
    fn test_canvas_packed_screen_migrate_transient_pools_matches_cc_screen_pool_reset() {
        let mut pools = CanvasPackedCellPools::new();
        pools.intern_char("unused-before-reset");
        pools.intern_hyperlink(Some("https://unused.example"));
        let style = CanvasResolvedStyle {
            text: CanvasTextStyle {
                color: Some(Color::Yellow),
                weight: Weight::Bold,
                ..Default::default()
            },
            background_color: Some(Color::Blue),
        };
        let style_id = pools.intern_style(style);

        let mut packed = CanvasPackedScreen::new(2, 1);
        assert!(packed.set_cell_text(
            &mut pools,
            0,
            0,
            "好",
            style,
            Some("https://example.com"),
            CanvasPackedCellWidth::Normal,
        ));
        let old_cell = packed.cell(0, 0).unwrap();
        assert_eq!(old_cell.style_id, style_id);
        assert!(
            old_cell.char_id > 2,
            "old transient char pool has unused IDs"
        );
        assert!(
            old_cell.hyperlink_id > 1,
            "old transient hyperlink pool has unused IDs"
        );

        let old_pools = pools.clone();
        let mut next_pools = old_pools.fork_with_transient_pools_cleared();
        assert_eq!(next_pools.char_len(), 2);
        assert_eq!(next_pools.hyperlink_len(), 1);
        assert_eq!(next_pools.style(style_id), Some(style));
        packed.damage_region = None;

        assert!(packed.migrate_transient_pools(&old_pools, &mut next_pools));
        let migrated = packed.cell(0, 0).unwrap();
        assert_ne!(migrated.char_id, old_cell.char_id);
        assert_ne!(migrated.hyperlink_id, old_cell.hyperlink_id);
        assert_eq!(next_pools.character(migrated.char_id), Some("好"));
        assert_eq!(
            next_pools.hyperlink(migrated.hyperlink_id),
            Some("https://example.com")
        );
        assert_eq!(
            migrated.style_id, style_id,
            "style IDs remain session-lived"
        );
        assert_eq!(next_pools.style(migrated.style_id), Some(style));
        assert_eq!(
            packed.damage_region, None,
            "pool migration is output-neutral"
        );
    }

    #[test]
    fn test_canvas_packed_screen_set_cell_repairs_wide_relationships_like_cc_screen() {
        let mut pools = CanvasPackedCellPools::new();
        let mut packed = CanvasPackedScreen::new(5, 1);
        let default_style = CanvasResolvedStyle::default();

        assert!(packed.set_cell_text(
            &mut pools,
            1,
            0,
            "好",
            default_style,
            None,
            CanvasPackedCellWidth::Wide,
        ));
        assert_eq!(
            pools.character(packed.cell(1, 0).unwrap().char_id),
            Some("好")
        );
        assert_eq!(
            packed.cell(1, 0).unwrap().width,
            CanvasPackedCellWidth::Wide
        );
        assert_eq!(
            packed.cell(2, 0).unwrap().width,
            CanvasPackedCellWidth::WidthTail
        );
        assert_eq!(
            packed.damage_region,
            Some(DamageRegion {
                x: 1,
                y: 0,
                width: 2,
                height: 1,
            })
        );

        packed.damage_region = None;
        assert!(packed.set_cell_text(
            &mut pools,
            2,
            0,
            "x",
            default_style,
            None,
            CanvasPackedCellWidth::Normal,
        ));
        assert!(
            packed.is_empty_cell(1, 0),
            "overwriting a tail clears its wide head"
        );
        assert_eq!(
            pools.character(packed.cell(2, 0).unwrap().char_id),
            Some("x")
        );
        assert_eq!(
            packed.damage_region,
            Some(DamageRegion {
                x: 1,
                y: 0,
                width: 2,
                height: 1,
            })
        );

        packed.damage_region = None;
        assert!(packed.set_cell_text(
            &mut pools,
            2,
            0,
            "界",
            default_style,
            None,
            CanvasPackedCellWidth::Wide,
        ));
        assert_eq!(
            packed.cell(2, 0).unwrap().width,
            CanvasPackedCellWidth::Wide
        );
        assert_eq!(
            packed.cell(3, 0).unwrap().width,
            CanvasPackedCellWidth::WidthTail
        );
        packed.damage_region = None;
        assert!(packed.set_cell_text(
            &mut pools,
            2,
            0,
            "n",
            default_style,
            None,
            CanvasPackedCellWidth::Normal,
        ));
        assert!(
            packed.is_empty_cell(3, 0),
            "overwriting a wide head clears its tail"
        );
    }

    #[test]
    fn test_canvas_packed_screen_style_and_no_select_metadata_match_cc_helpers() {
        let mut canvas = Canvas::new(4, 1);
        canvas
            .subview_mut(0, 0, 0, 0, 4, 1)
            .set_text(0, 0, "A好", CanvasTextStyle::default());
        canvas.clear_damage();

        let mut pools = CanvasPackedCellPools::new();
        let mut packed = canvas.pack_with(&mut pools);
        let mut highlighted_text = CanvasTextStyle::default();
        highlighted_text.color = Some(Color::Yellow);
        highlighted_text.weight = Weight::Bold;
        let highlighted = CanvasResolvedStyle {
            text: highlighted_text,
            background_color: Some(Color::Blue),
        };

        assert!(packed.set_cell_style(&mut pools, 0, 0, highlighted));
        let highlighted_id = packed.cell(0, 0).unwrap().style_id;
        assert_eq!(pools.style(highlighted_id), Some(highlighted));
        assert_eq!(
            packed.damage_region,
            Some(DamageRegion {
                x: 0,
                y: 0,
                width: 1,
                height: 1,
            })
        );

        let tail_before = packed.cell(2, 0).unwrap();
        assert!(!packed.set_cell_style_id(2, 0, highlighted_id));
        assert_eq!(packed.cell(2, 0).unwrap(), tail_before);

        let damage_before_no_select = packed.damage_region;
        assert!(packed.mark_no_select_region(1, 0, 10, 1));
        assert!(packed.is_no_select(1, 0));
        assert!(packed.is_no_select(3, 0));
        assert_eq!(
            packed.damage_region, damage_before_no_select,
            "noSelect metadata does not affect terminal damage"
        );
    }

    #[test]
    fn test_canvas_packed_screen_style_overlay_marks_damage_and_wide_head() {
        let mut pools = CanvasPackedCellPools::new();
        let mut line_cache = CanvasPackedLineCache::new();
        let mut packed = CanvasPackedScreen::new(4, 1);
        packed.write_line_with_cache(
            &mut pools,
            &mut line_cache,
            0,
            0,
            "好",
            CanvasResolvedStyle::default(),
            None,
        );
        packed.clear_damage();

        assert!(packed.apply_style_overlay(&mut pools, 1, 0, StyleOverlay::inverse()));
        let head_style = pools
            .style(packed.cell(0, 0).unwrap().style_id)
            .expect("overlay style interned");
        assert!(head_style.text.invert);
        assert_eq!(
            packed.cell(1, 0).unwrap().width,
            CanvasPackedCellWidth::WidthTail
        );
        assert_eq!(
            packed.damage_region(),
            Some(DamageRegion {
                x: 0,
                y: 0,
                width: 1,
                height: 1,
            })
        );

        assert!(packed.apply_style_overlay_region(
            &mut pools,
            2,
            0,
            2,
            1,
            StyleOverlay::selection_background(Color::Blue),
        ));
        let blank_style = pools
            .style(packed.cell(2, 0).unwrap().style_id)
            .expect("blank overlay style interned");
        assert_eq!(blank_style.background_color, Some(Color::Blue));
        assert!(
            packed.visible_cell(&pools, 2, 0, None).is_some(),
            "background overlay on a packed blank space must be sparse-render-visible"
        );
    }

    #[test]
    fn test_canvas_packed_screen_word_bounds_match_cc_double_click_shape() {
        let mut pools = CanvasPackedCellPools::new();
        let mut line_cache = CanvasPackedLineCache::new();
        let style = CanvasResolvedStyle::default();
        let mut packed = CanvasPackedScreen::new(20, 1);
        packed.write_line_with_cache(
            &mut pools,
            &mut line_cache,
            0,
            0,
            "run /usr/bin/bash!",
            style,
            None,
        );

        assert_eq!(
            packed.word_bounds_at(&pools, 6, 0),
            Some((4, 16)),
            "path punctuation should stay in the same word class like CC Ink selection"
        );
        assert_eq!(packed.word_bounds_at(&pools, 17, 0), Some((17, 17)));
    }

    #[test]
    fn test_canvas_packed_screen_word_bounds_steps_from_wide_tail_and_no_selects() {
        let mut pools = CanvasPackedCellPools::new();
        let mut line_cache = CanvasPackedLineCache::new();
        let style = CanvasResolvedStyle::default();
        let mut packed = CanvasPackedScreen::new(8, 1);
        packed.write_line_with_cache(&mut pools, &mut line_cache, 0, 0, "中a x", style, None);

        assert_eq!(
            packed.word_bounds_at(&pools, 1, 0),
            Some((0, 2)),
            "double-clicking a wide tail should select from the head cell"
        );
        packed.mark_no_select_region(0, 0, 1, 1);
        assert_eq!(packed.word_bounds_at(&pools, 0, 0), None);
        assert_eq!(
            packed.word_bounds_at(&pools, 1, 0),
            None,
            "wide-tail fallback should still respect noSelect on the head"
        );
    }

    #[test]
    fn test_canvas_packed_screen_selection_range_for_word_and_line() {
        let mut pools = CanvasPackedCellPools::new();
        let mut line_cache = CanvasPackedLineCache::new();
        let style = CanvasResolvedStyle::default();
        let mut packed = CanvasPackedScreen::new(8, 2);
        packed.write_line_with_cache(&mut pools, &mut line_cache, 0, 0, "one two", style, None);
        packed.write_line_with_cache(&mut pools, &mut line_cache, 0, 1, "row", style, None);

        let word = packed
            .selection_range_for_word_at(&pools, 5, 0)
            .expect("word selection range");
        assert_eq!(packed.selected_text(&pools, word), "two");

        let line = packed
            .selection_range_for_line(1)
            .expect("line selection range");
        assert_eq!(packed.selected_text(&pools, line), "row");
        assert_eq!(packed.selection_range_for_line(2), None);
    }

    #[test]
    fn test_selection_state_packed_multi_click_and_extend_match_screen() {
        let mut pools = CanvasPackedCellPools::new();
        let mut line_cache = CanvasPackedLineCache::new();
        let style = CanvasResolvedStyle::default();
        let mut packed = CanvasPackedScreen::new(16, 1);
        packed.write_line_with_cache(
            &mut pools,
            &mut line_cache,
            0,
            0,
            "one two three",
            style,
            None,
        );

        let mut selection = SelectionState::new();
        selection.start_multi_click_packed(&packed, &pools, 4, 0, SelectionClickCount::Double);
        assert_eq!(selection.selected_text_packed(&packed, &pools), "two");

        selection.extend_span_selection_packed(&packed, &pools, 10, 0);
        assert_eq!(selection.selected_text_packed(&packed, &pools), "two three");
    }

    #[test]
    fn test_selection_state_packed_line_overlay_and_capture_rows() {
        let mut pools = CanvasPackedCellPools::new();
        let mut line_cache = CanvasPackedLineCache::new();
        let style = CanvasResolvedStyle::default();
        let mut packed = CanvasPackedScreen::new(8, 3);
        packed.write_line_with_cache(&mut pools, &mut line_cache, 0, 0, "row0", style, None);
        packed.write_line_with_cache(&mut pools, &mut line_cache, 0, 1, "row1", style, None);
        packed.write_line_with_cache(&mut pools, &mut line_cache, 0, 2, "row2", style, None);

        let mut selection = SelectionState::new();
        assert!(selection.select_line_at_packed(&packed, 1));
        assert_eq!(selection.selected_text_packed(&packed, &pools), "row1");

        let mut overlayed = packed.clone();
        assert!(selection.apply_overlay_packed(
            &mut overlayed,
            &mut pools,
            StyleOverlay::selection_background(Color::Blue),
        ));
        assert_eq!(
            pools
                .style(overlayed.cell(0, 1).unwrap().style_id)
                .unwrap_or_default()
                .background_color,
            Some(Color::Blue)
        );

        selection.move_focus(3, 2);
        selection.capture_scrolled_rows_packed(&packed, &pools, 1, 1, SelectionCaptureSide::Above);
        selection.shift_rows(-2, 0, 2, packed.width);
        assert_eq!(
            selection.selected_text_packed(&packed, &pools),
            "row1\nrow0",
            "packed capture should preserve copied text for the off-screen debt"
        );
    }

    #[test]
    fn test_selection_controller_packed_double_click_drag_copy_and_take() {
        let mut pools = CanvasPackedCellPools::new();
        let mut line_cache = CanvasPackedLineCache::new();
        let style = CanvasResolvedStyle::default();
        let mut packed = CanvasPackedScreen::new(16, 1);
        packed.write_line_with_cache(
            &mut pools,
            &mut line_cache,
            0,
            0,
            "one two three",
            style,
            None,
        );
        let mut controller = SelectionController::new();

        controller.handle_left_press_packed(&packed, &pools, 4, 0, 1_000, false);
        let outcome = controller.handle_left_press_packed(&packed, &pools, 4, 0, 1_200, false);
        assert_eq!(outcome.kind, SelectionMousePressKind::Double);
        assert!(outcome.cancel_pending_hyperlink);
        assert_eq!(controller.selected_text_packed(&packed, &pools), "two");

        controller.handle_drag_packed(&packed, &pools, 10, 0);
        assert_eq!(
            controller.selected_text_packed(&packed, &pools),
            "two three"
        );
        assert!(controller.handle_release());
        assert_eq!(
            controller
                .copy_on_select_text_packed(&packed, &pools)
                .as_deref(),
            Some("two three")
        );
        assert_eq!(controller.copy_on_select_text_packed(&packed, &pools), None);
        assert_eq!(
            controller.take_selected_text_packed(&packed, &pools),
            "two three"
        );
        assert!(!controller.has_selection());
    }

    #[test]
    fn test_selection_controller_packed_release_hyperlink_and_scroll_jump() {
        let mut pools = CanvasPackedCellPools::new();
        let mut line_cache = CanvasPackedLineCache::new();
        let style = CanvasResolvedStyle::default();
        let mut linked = CanvasPackedScreen::new(8, 1);
        linked.write_line_with_cache(
            &mut pools,
            &mut line_cache,
            0,
            0,
            "中x",
            style,
            Some("https://linked.example"),
        );
        let mut controller = SelectionController::new();
        controller.handle_left_press_packed(&linked, &pools, 1, 0, 1_000, false);
        let release = controller.handle_release_at_packed(&linked, &pools, 1, 0, false);
        assert_eq!(release.click, Some(SelectionPoint { col: 1, row: 0 }));
        assert_eq!(
            release.hyperlink.as_deref(),
            Some("https://linked.example"),
            "packed release should resolve wide-tail OSC 8 hyperlinks"
        );

        let mut packed = CanvasPackedScreen::new(6, 3);
        packed.write_line_with_cache(&mut pools, &mut line_cache, 0, 0, "row0", style, None);
        packed.write_line_with_cache(&mut pools, &mut line_cache, 0, 1, "row1", style, None);
        packed.write_line_with_cache(&mut pools, &mut line_cache, 0, 2, "row2", style, None);
        let mut controller = SelectionController::new();
        controller.handle_left_press_packed(&packed, &pools, 0, 0, 2_000, false);
        controller.handle_drag_packed(&packed, &pools, 3, 2);
        controller.handle_release();

        let outcome = controller.translate_for_scroll_jump_packed(&packed, &pools, 1, 0, 2);
        packed.shift_rows(0, 2, 1);
        assert_eq!(
            outcome,
            SelectionScrollOutcome {
                translated: true,
                cleared: false,
            }
        );
        assert_eq!(
            controller.selected_text_packed(&packed, &pools),
            "row0\nrow1\nrow2"
        );
    }

    #[test]
    fn test_selection_controller_packed_follow_scroll_preserves_copied_text() {
        let mut pools = CanvasPackedCellPools::new();
        let mut line_cache = CanvasPackedLineCache::new();
        let style = CanvasResolvedStyle::default();
        let mut packed = CanvasPackedScreen::new(6, 3);
        packed.write_line_with_cache(&mut pools, &mut line_cache, 0, 0, "row0", style, None);
        packed.write_line_with_cache(&mut pools, &mut line_cache, 0, 1, "row1", style, None);
        packed.write_line_with_cache(&mut pools, &mut line_cache, 0, 2, "row2", style, None);
        let mut controller = SelectionController::new();
        controller.handle_left_press_packed(&packed, &pools, 0, 0, 1_000, false);
        controller.handle_drag_packed(&packed, &pools, 3, 2);
        controller.handle_release();

        let outcome = controller.translate_for_follow_scroll_packed(&packed, &pools, 1, 0, 2);
        packed.shift_rows(0, 2, 1);

        assert!(outcome.translated);
        assert!(!outcome.cleared);
        assert_eq!(
            controller.selected_text_packed(&packed, &pools),
            "row0\nrow1\nrow2"
        );
    }

    #[test]
    fn test_selection_controller_packed_drag_autoscroll_captures_and_shifts_anchor() {
        let mut pools = CanvasPackedCellPools::new();
        let mut line_cache = CanvasPackedLineCache::new();
        let style = CanvasResolvedStyle::default();
        let mut packed = CanvasPackedScreen::new(6, 4);
        packed.write_line_with_cache(&mut pools, &mut line_cache, 0, 1, "row1", style, None);
        packed.write_line_with_cache(&mut pools, &mut line_cache, 0, 2, "row2", style, None);
        let mut controller = SelectionController::new();
        controller.handle_left_press_packed(&packed, &pools, 0, 1, 1_000, false);
        controller.handle_drag_packed(&packed, &pools, 3, 4);

        assert_eq!(
            controller.drag_scroll_direction(1, 3),
            Some(SelectionDragScrollDirection::Down)
        );
        let outcome = controller.translate_for_drag_autoscroll_packed(
            &packed,
            &pools,
            SelectionDragScrollDirection::Down,
            1,
            1,
            3,
        );

        assert!(outcome.translated);
        assert_eq!(
            controller.selection().anchor(),
            Some(SelectionPoint { col: 0, row: 1 })
        );
        assert_eq!(controller.selection.virtual_anchor_row, Some(0));
        assert_eq!(
            controller.selection.scrolled_off_above,
            vec!["row1".to_string()]
        );
    }

    #[test]
    fn test_canvas_packed_screen_selection_text_matches_cc_get_selected_text_shape() {
        let mut pools = CanvasPackedCellPools::new();
        let mut line_cache = CanvasPackedLineCache::new();
        let style = CanvasResolvedStyle::default();
        let mut packed = CanvasPackedScreen::new(8, 2);
        packed.write_line_with_cache(&mut pools, &mut line_cache, 0, 0, "A好 ", style, None);
        packed.write_line_with_cache(&mut pools, &mut line_cache, 0, 1, "world", style, None);
        packed.soft_wrap[1] = 4;
        packed.mark_no_select_region(1, 1, 1, 1);

        assert_eq!(
            packed.selected_text(
                &pools,
                SelectionRange::new(
                    SelectionPoint { col: 0, row: 0 },
                    SelectionPoint { col: 4, row: 1 },
                ),
            ),
            "A好 wrld",
            "packed selected text should skip wide tails/noSelect and join soft-wrap continuations"
        );
    }

    #[test]
    fn test_canvas_packed_screen_apply_selection_overlay_skips_no_select_and_marks_damage() {
        let mut pools = CanvasPackedCellPools::new();
        let mut line_cache = CanvasPackedLineCache::new();
        let style = CanvasResolvedStyle::default();
        let mut packed = CanvasPackedScreen::new(6, 1);
        packed.write_line_with_cache(&mut pools, &mut line_cache, 0, 0, "abcdef", style, None);
        packed.clear_damage();
        packed.mark_no_select_region(2, 0, 1, 1);

        assert!(packed.apply_selection_overlay(
            &mut pools,
            SelectionRange::new(
                SelectionPoint { col: 1, row: 0 },
                SelectionPoint { col: 3, row: 0 },
            ),
            StyleOverlay::selection_background(Color::Blue),
        ));
        let is_blue = |screen: &CanvasPackedScreen, pools: &CanvasPackedCellPools, x| {
            pools
                .style(screen.cell(x, 0).unwrap().style_id)
                .unwrap_or_default()
                .background_color
                == Some(Color::Blue)
        };

        assert!(is_blue(&packed, &pools, 1));
        assert!(
            !is_blue(&packed, &pools, 2),
            "noSelect cells are not highlighted"
        );
        assert!(is_blue(&packed, &pools, 3));
        assert_eq!(
            packed.damage_region(),
            Some(DamageRegion {
                x: 1,
                y: 0,
                width: 3,
                height: 1,
            })
        );
    }

    #[test]
    fn test_canvas_packed_screen_scan_text_positions_is_case_insensitive_and_wide_aware() {
        let mut pools = CanvasPackedCellPools::new();
        let mut line_cache = CanvasPackedLineCache::new();
        let style = CanvasResolvedStyle::default();
        let mut packed = CanvasPackedScreen::new(12, 1);
        packed.write_line_with_cache(&mut pools, &mut line_cache, 0, 0, "ab中c AB", style, None);

        assert_eq!(
            packed.scan_text_positions(&pools, "中c"),
            vec![TextMatchPosition {
                row: 0,
                col: 2,
                len: 3,
            }],
            "packed match spans should be terminal-cell based and include wide tails"
        );
        assert_eq!(
            packed.scan_text_positions(&pools, "ab"),
            vec![
                TextMatchPosition {
                    row: 0,
                    col: 0,
                    len: 2,
                },
                TextMatchPosition {
                    row: 0,
                    col: 6,
                    len: 2,
                },
            ],
            "packed search should be case-insensitive and non-overlapping"
        );
    }

    #[test]
    fn test_canvas_packed_screen_scan_text_positions_region_returns_relative_positions() {
        let mut pools = CanvasPackedCellPools::new();
        let mut line_cache = CanvasPackedLineCache::new();
        let style = CanvasResolvedStyle::default();
        let mut packed = CanvasPackedScreen::new(16, 3);
        packed.write_line_with_cache(
            &mut pools,
            &mut line_cache,
            2,
            1,
            "xx lazy 中c",
            style,
            None,
        );
        packed.mark_no_select_region(5, 1, 4, 1);

        assert_eq!(
            packed.scan_text_positions_region(&pools, 5, 1, 10, 1, "lazy"),
            Vec::<TextMatchPosition>::new(),
            "packed region scanning should respect noSelect metadata"
        );
        assert_eq!(
            packed.scan_text_positions_region(&pools, 8, 1, 8, 1, "中c"),
            vec![TextMatchPosition {
                row: 0,
                col: 2,
                len: 3,
            }],
            "packed region positions should be relative to the scanned region"
        );
    }

    #[test]
    fn test_canvas_packed_screen_apply_search_highlight_skips_no_select_and_marks_damage() {
        let mut pools = CanvasPackedCellPools::new();
        let mut line_cache = CanvasPackedLineCache::new();
        let style = CanvasResolvedStyle::default();
        let mut packed = CanvasPackedScreen::new(12, 1);
        packed.write_line_with_cache(&mut pools, &mut line_cache, 0, 0, "foo foo", style, None);
        packed.clear_damage();
        packed.mark_no_select_region(0, 0, 3, 1);

        assert!(packed.apply_search_highlight(&mut pools, "foo", StyleOverlay::inverse()));
        let is_inverted = |screen: &CanvasPackedScreen, pools: &CanvasPackedCellPools, x| {
            pools
                .style(screen.cell(x, 0).unwrap().style_id)
                .unwrap_or_default()
                .text
                .invert
        };

        for col in 0..3 {
            assert!(!is_inverted(&packed, &pools, col));
        }
        for col in 4..7 {
            assert!(is_inverted(&packed, &pools, col));
        }
        assert_eq!(
            packed.damage_region(),
            Some(DamageRegion {
                x: 4,
                y: 0,
                width: 3,
                height: 1,
            })
        );
    }

    #[test]
    fn test_canvas_packed_screen_apply_positioned_highlight_translates_row_offset() {
        let mut pools = CanvasPackedCellPools::new();
        let mut line_cache = CanvasPackedLineCache::new();
        let style = CanvasResolvedStyle::default();
        let mut packed = CanvasPackedScreen::new(8, 2);
        packed.write_line_with_cache(&mut pools, &mut line_cache, 0, 1, "target", style, None);
        let positions = vec![TextMatchPosition {
            row: 0,
            col: 0,
            len: 6,
        }];

        assert!(!packed.apply_positioned_highlight(
            &mut pools,
            &positions,
            -1,
            0,
            StyleOverlay::inverse(),
        ));
        assert!(packed.apply_positioned_highlight(
            &mut pools,
            &positions,
            1,
            0,
            StyleOverlay::inverse(),
        ));
        let is_inverted = |screen: &CanvasPackedScreen, pools: &CanvasPackedCellPools, x| {
            pools
                .style(screen.cell(x, 1).unwrap().style_id)
                .unwrap_or_default()
                .text
                .invert
        };
        assert!(is_inverted(&packed, &pools, 0));
        assert!(is_inverted(&packed, &pools, 5));
    }

    #[test]
    fn test_canvas_packed_screen_hyperlink_at_prefers_osc8_and_wide_tail() {
        let mut pools = CanvasPackedCellPools::new();
        let mut line_cache = CanvasPackedLineCache::new();
        let style = CanvasResolvedStyle::default();
        let mut packed = CanvasPackedScreen::new(20, 1);
        packed.write_line_with_cache(
            &mut pools,
            &mut line_cache,
            0,
            0,
            "中x",
            style,
            Some("https://linked.example"),
        );

        assert_eq!(
            packed.hyperlink_at(&pools, 0, 0).as_deref(),
            Some("https://linked.example")
        );
        assert_eq!(
            packed.hyperlink_at(&pools, 1, 0).as_deref(),
            Some("https://linked.example"),
            "packed wide-character tail should resolve the head cell's OSC 8 link"
        );
    }

    #[test]
    fn test_canvas_packed_screen_plain_text_url_at_trims_sentence_punctuation() {
        let mut pools = CanvasPackedCellPools::new();
        let mut line_cache = CanvasPackedLineCache::new();
        let style = CanvasResolvedStyle::default();
        let mut packed = CanvasPackedScreen::new(40, 1);
        packed.write_line_with_cache(
            &mut pools,
            &mut line_cache,
            0,
            0,
            "see https://example.com/foo).",
            style,
            None,
        );

        assert_eq!(
            packed.hyperlink_at(&pools, 8, 0).as_deref(),
            Some("https://example.com/foo")
        );
        assert_eq!(packed.hyperlink_at(&pools, 29, 0), None);
    }

    #[test]
    fn test_canvas_packed_screen_plain_text_url_at_chooses_scheme_under_click() {
        let mut pools = CanvasPackedCellPools::new();
        let mut line_cache = CanvasPackedLineCache::new();
        let style = CanvasResolvedStyle::default();
        let mut packed = CanvasPackedScreen::new(50, 1);
        packed.write_line_with_cache(
            &mut pools,
            &mut line_cache,
            0,
            0,
            "https://a.com,https://b.com",
            style,
            None,
        );

        assert_eq!(
            packed.hyperlink_at(&pools, 8, 0).as_deref(),
            Some("https://a.com")
        );
        assert_eq!(
            packed.hyperlink_at(&pools, 20, 0).as_deref(),
            Some("https://b.com")
        );
    }

    #[test]
    fn test_canvas_packed_screen_plain_text_url_at_respects_no_select_boundaries() {
        let mut pools = CanvasPackedCellPools::new();
        let mut line_cache = CanvasPackedLineCache::new();
        let style = CanvasResolvedStyle::default();
        let mut packed = CanvasPackedScreen::new(30, 1);
        packed.write_line_with_cache(
            &mut pools,
            &mut line_cache,
            0,
            0,
            "https://example.com",
            style,
            None,
        );
        packed.mark_no_select_region(0, 0, 5, 1);

        assert_eq!(packed.hyperlink_at(&pools, 2, 0), None);
        assert_eq!(packed.hyperlink_at(&pools, 8, 0), None);
    }

    #[test]
    fn test_canvas_packed_screen_debug_repaint_overlay_marks_changed_and_damaged_cells() {
        let mut pools = CanvasPackedCellPools::new();
        let mut line_cache = CanvasPackedLineCache::new();
        let style = CanvasResolvedStyle::default();
        let mut prev = CanvasPackedScreen::new(5, 1);
        prev.write_line_with_cache(&mut pools, &mut line_cache, 0, 0, "abcde", style, None);
        prev.clear_damage();
        prev.mark_damage(DamageRegion {
            x: 4,
            y: 0,
            width: 1,
            height: 1,
        });

        let mut next = CanvasPackedScreen::new(5, 1);
        next.write_line_with_cache(&mut pools, &mut line_cache, 0, 0, "abXde", style, None);
        next.clear_damage();
        next.mark_damage(DamageRegion {
            x: 2,
            y: 0,
            width: 1,
            height: 1,
        });

        let overlayed = CanvasPackedScreen::debug_repaint_overlay(
            Some(&prev),
            &next,
            &mut pools,
            StyleOverlay::inverse(),
        );
        let cell_inverted = |screen: &CanvasPackedScreen, pools: &CanvasPackedCellPools, x| {
            pools
                .style(screen.cell(x, 0).unwrap().style_id)
                .unwrap_or_default()
                .text
                .invert
        };

        assert!(!cell_inverted(&overlayed, &pools, 1));
        assert!(cell_inverted(&overlayed, &pools, 2));
        assert!(!cell_inverted(&overlayed, &pools, 3));
        assert!(cell_inverted(&overlayed, &pools, 4));
        assert_eq!(
            overlayed.damage_region(),
            Some(DamageRegion {
                x: 2,
                y: 0,
                width: 3,
                height: 1,
            })
        );
    }

    #[test]
    fn test_canvas_diff_ignores_damage_only_metadata_like_cc_screen_diff_each() {
        let prev = Canvas::new(3, 1);
        let mut next = prev.clone();
        next.mark_damage(DamageRegion {
            x: 1,
            y: 0,
            width: 1,
            height: 1,
        });

        assert!(prev.diff(&next).is_empty());
    }

    #[test]
    fn test_debug_repaint_overlay_marks_changed_and_damaged_cells() {
        let mut prev = Canvas::new(5, 1);
        let mut next = Canvas::new(5, 1);
        let style = CanvasTextStyle::default();
        prev.subview_mut(0, 0, 0, 0, 5, 1)
            .set_text(0, 0, "abc", style);
        next.subview_mut(0, 0, 0, 0, 5, 1)
            .set_text(0, 0, "axc", style);
        prev.mark_damage(DamageRegion {
            x: 3,
            y: 0,
            width: 1,
            height: 1,
        });
        next.mark_damage(DamageRegion {
            x: 4,
            y: 0,
            width: 1,
            height: 1,
        });

        let overlay = Canvas::debug_repaint_overlay(Some(&prev), &next, StyleOverlay::inverse());
        assert!(!overlay.resolved_text_style(0, 0).unwrap().invert);
        assert!(overlay.resolved_text_style(1, 0).unwrap().invert);
        assert!(!overlay.resolved_text_style(2, 0).unwrap().invert);
        assert!(overlay.resolved_text_style(3, 0).unwrap_or_default().invert);
        assert!(overlay.resolved_text_style(4, 0).unwrap_or_default().invert);
    }

    #[test]
    fn test_debug_repaint_overlay_marks_full_repaint() {
        let mut next = Canvas::new(3, 1);
        next.subview_mut(0, 0, 0, 0, 3, 1)
            .set_text(0, 0, "abc", CanvasTextStyle::default());
        next.force_full_repaint();

        let overlay = Canvas::debug_repaint_overlay(None, &next, StyleOverlay::inverse());
        for x in 0..3 {
            assert!(overlay.resolved_text_style(x, 0).unwrap().invert);
        }
    }

    #[test]
    fn test_shift_rows_moves_cells_and_clears_vacated_rows() {
        let mut canvas = Canvas::new(8, 4);
        for (y, label) in ["zero", "one", "two", "three"].iter().enumerate() {
            canvas.subview_mut(0, 0, 0, 0, 8, 4).set_text(
                0,
                y as isize,
                label,
                CanvasTextStyle::default(),
            );
        }
        canvas.set_scroll_hint(ScrollHint {
            top: 1,
            bottom: 3,
            delta: 1,
        });
        canvas.mark_damage(DamageRegion {
            x: 0,
            y: 1,
            width: 8,
            height: 1,
        });

        canvas.shift_rows(1, 3, 1);

        assert_eq!(canvas.get_text(0, 0, 8, 1), "zero");
        assert_eq!(canvas.get_text(0, 1, 8, 1), "two");
        assert_eq!(canvas.get_text(0, 2, 8, 1), "three");
        assert_eq!(canvas.get_text(0, 3, 8, 1), "");
        assert_eq!(canvas.scroll_hint(), None);
        assert_eq!(
            canvas.damage_region(),
            Some(DamageRegion {
                x: 0,
                y: 1,
                width: 8,
                height: 1,
            }),
            "shift_rows mirrors CC Ink and must not update damage metadata"
        );
    }

    #[test]
    fn test_canvas_background_color() {
        let mut canvas = Canvas::new(6, 3);
        assert_eq!(canvas.width(), 6);
        assert_eq!(canvas.height(), 3);

        canvas
            .subview_mut(2, 0, 2, 0, 3, 2)
            .set_background_color(0, 0, 5, 5, Color::Red);

        let mut actual = Vec::new();
        canvas.write_ansi(&mut actual).unwrap();

        let mut expected = Vec::new();
        // row 0
        write!(expected, csi!("0m")).unwrap();
        write!(expected, "  ").unwrap();
        write!(expected, csi!("{}m"), Colored::BackgroundColor(Color::Red)).unwrap();
        write!(expected, "   ").unwrap();
        write!(expected, csi!("0m")).unwrap();
        write!(expected, csi!("K")).unwrap();
        write!(expected, csi!("0m")).unwrap();
        write!(expected, "\r\n").unwrap();
        // row 1
        write!(expected, "  ").unwrap();
        write!(expected, csi!("{}m"), Colored::BackgroundColor(Color::Red)).unwrap();
        write!(expected, "   ").unwrap();
        write!(expected, csi!("0m")).unwrap();
        write!(expected, csi!("K")).unwrap();
        write!(expected, csi!("0m")).unwrap();
        write!(expected, "\r\n").unwrap();
        // row 2
        write!(expected, csi!("K")).unwrap();
        write!(expected, csi!("0m")).unwrap();
        write!(expected, "\r\n").unwrap();

        assert_eq!(actual, expected);
    }

    #[test]
    fn test_canvas_full_background_color() {
        let mut canvas = Canvas::new(6, 3);
        assert_eq!(canvas.width(), 6);
        assert_eq!(canvas.height(), 3);

        canvas
            .subview_mut(0, 0, 0, 0, 6, 6)
            .set_background_color(0, 0, 6, 6, Color::Red);

        let mut actual = Vec::new();
        canvas.write_ansi(&mut actual).unwrap();

        // the important thing here is that the background color is reset before each line is
        // cleared and before each newline
        // see: https://github.com/ccbrown/iocraft/issues/142

        let mut expected = Vec::new();

        // line 1: character is written before the erase, so all 6 cells
        // are emitted with the background, then reset + CSI K clears any
        // leftover content past the last column.
        write!(expected, csi!("0m")).unwrap();
        write!(expected, csi!("{}m"), Colored::BackgroundColor(Color::Red)).unwrap();
        write!(expected, "      ").unwrap();
        write!(expected, csi!("0m")).unwrap();
        write!(expected, csi!("K")).unwrap();
        write!(expected, csi!("0m")).unwrap();
        write!(expected, "\r\n").unwrap();

        // line 2
        write!(expected, csi!("{}m"), Colored::BackgroundColor(Color::Red)).unwrap();
        write!(expected, "      ").unwrap();
        write!(expected, csi!("0m")).unwrap();
        write!(expected, csi!("K")).unwrap();
        write!(expected, csi!("0m")).unwrap();
        write!(expected, "\r\n").unwrap();

        // line 3
        write!(expected, csi!("{}m"), Colored::BackgroundColor(Color::Red)).unwrap();
        write!(expected, "      ").unwrap();
        write!(expected, csi!("0m")).unwrap();
        write!(expected, csi!("K")).unwrap();
        write!(expected, csi!("0m")).unwrap();
        write!(expected, "\r\n").unwrap();

        assert_eq!(actual, expected);
    }

    #[test]
    fn test_canvas_style_transition_cache_matches_row_writer_sgr_order() {
        let default = CanvasResolvedStyle::default();
        let highlighted = CanvasResolvedStyle {
            text: CanvasTextStyle {
                weight: Weight::Bold,
                underline: true,
                invert: true,
                ..Default::default()
            },
            background_color: Some(Color::Blue),
        };

        let mut expected = Vec::new();
        write!(expected, csi!("{}m"), Attribute::Bold.sgr()).unwrap();
        write!(expected, csi!("{}m"), Attribute::Underlined.sgr()).unwrap();
        write!(expected, csi!("{}m"), Attribute::Reverse.sgr()).unwrap();
        write!(expected, csi!("{}m"), Colored::BackgroundColor(Color::Blue)).unwrap();
        let expected = String::from_utf8(expected).unwrap();

        assert_eq!(canvas_style_transition_to_ansi(default, default), "");
        assert_eq!(
            canvas_style_transition_to_ansi(default, highlighted),
            expected
        );
        assert_eq!(
            canvas_style_transition_to_ansi(highlighted, default),
            csi!("0m")
        );

        let mut cache = CanvasStyleTransitionCache::new();
        assert!(cache.is_empty());
        assert_eq!(cache.transition(default, highlighted), expected);
        assert_eq!(cache.len(), 1);
        assert_eq!(cache.transition(default, highlighted), expected);
        assert_eq!(cache.len(), 1, "repeated transition should hit the cache");
        assert_eq!(cache.transition(highlighted, default), csi!("0m"));
        assert_eq!(cache.len(), 2);
        cache.clear();
        assert!(cache.is_empty());
    }

    #[test]
    fn test_canvas_ansi_row_cache_reuses_and_invalidates_serialized_rows() {
        let mut canvas = Canvas::new(8, 1);
        canvas.subview_mut(0, 0, 0, 0, 8, 1).set_text(
            0,
            0,
            "hello",
            CanvasTextStyle {
                color: Some(Color::Green),
                ..Default::default()
            },
        );

        let mut expected = Vec::new();
        canvas
            .write_ansi_row_without_newline(0, &mut expected)
            .unwrap();

        let mut cache = CanvasAnsiRowCache::new();
        let mut first = Vec::new();
        cache.write_row(&canvas, 0, &mut first).unwrap();
        assert_eq!(first, expected);
        assert_eq!(cache.len(), 1);

        let mut second = Vec::new();
        cache.write_row(&canvas, 0, &mut second).unwrap();
        assert_eq!(second, expected);
        assert_eq!(cache.len(), 1, "unchanged row should hit the cache");

        canvas.set_overlay(1, 0, StyleOverlay::selection_background(Color::Blue));
        let mut overlay_expected = Vec::new();
        canvas
            .write_ansi_row_without_newline(0, &mut overlay_expected)
            .unwrap();
        let mut overlay_actual = Vec::new();
        cache.write_row(&canvas, 0, &mut overlay_actual).unwrap();
        assert_eq!(overlay_actual, overlay_expected);
        assert_ne!(
            overlay_actual, expected,
            "overlay changes must invalidate the cache"
        );
        assert_eq!(cache.len(), 1);

        let mut suffix = Vec::new();
        cache
            .write_row_from_col(&canvas, 0, 2, &mut suffix)
            .unwrap();
        assert_eq!(cache.len(), 2, "start_col is part of the cache key");
        assert!(!suffix.is_empty());

        cache.clear();
        assert!(cache.is_empty());
    }

    #[test]
    fn test_canvas_text_styles() {
        let mut canvas = Canvas::new(100, 1);
        assert_eq!(canvas.width(), 100);
        assert_eq!(canvas.height(), 1);

        canvas
            .subview_mut(0, 0, 0, 0, 1, 1)
            .set_text(0, 0, ".", CanvasTextStyle::default());
        canvas.subview_mut(1, 0, 1, 0, 1, 1).set_text(
            0,
            0,
            ".",
            CanvasTextStyle {
                color: Some(Color::Red),
                weight: Weight::Bold,
                underline: true,
                ..Default::default()
            },
        );
        canvas.subview_mut(2, 0, 2, 0, 1, 1).set_text(
            0,
            0,
            ".",
            CanvasTextStyle {
                color: Some(Color::Red),
                weight: Weight::Bold,
                italic: true,
                ..Default::default()
            },
        );
        canvas.subview_mut(3, 0, 3, 0, 1, 1).set_text(
            0,
            0,
            ".",
            CanvasTextStyle {
                color: Some(Color::Red),
                weight: Weight::Bold,
                ..Default::default()
            },
        );
        canvas.subview_mut(4, 0, 4, 0, 1, 1).set_text(
            0,
            0,
            ".",
            CanvasTextStyle {
                color: Some(Color::Red),
                weight: Weight::Light,
                ..Default::default()
            },
        );
        canvas.subview_mut(5, 0, 5, 0, 1, 1).set_text(
            0,
            0,
            ".",
            CanvasTextStyle {
                color: Some(Color::Red),
                ..Default::default()
            },
        );
        canvas.subview_mut(6, 0, 6, 0, 1, 1).set_text(
            0,
            0,
            ".",
            CanvasTextStyle {
                color: Some(Color::Green),
                ..Default::default()
            },
        );
        canvas.subview_mut(7, 0, 7, 0, 1, 1).set_text(
            0,
            0,
            ".",
            CanvasTextStyle {
                color: Some(Color::Green),
                invert: true,
                ..Default::default()
            },
        );
        canvas.subview_mut(8, 0, 8, 0, 1, 1).set_text(
            0,
            0,
            ".",
            CanvasTextStyle {
                color: Some(Color::Green),
                ..Default::default()
            },
        );
        canvas.subview_mut(9, 0, 9, 0, 1, 1).set_text(
            0,
            0,
            ".",
            CanvasTextStyle {
                color: Some(Color::Green),
                strikethrough: true,
                ..Default::default()
            },
        );
        canvas.subview_mut(10, 0, 10, 0, 1, 1).set_text(
            0,
            0,
            ".",
            CanvasTextStyle {
                color: Some(Color::Green),
                overline: true,
                ..Default::default()
            },
        );

        let mut actual = Vec::new();
        canvas.write_ansi(&mut actual).unwrap();

        let mut expected = Vec::new();
        write!(expected, csi!("0m")).unwrap();
        write!(expected, ".").unwrap();

        write!(expected, csi!("{}m"), Colored::ForegroundColor(Color::Red)).unwrap();
        write!(expected, csi!("{}m"), Attribute::Bold.sgr()).unwrap();
        write!(expected, csi!("{}m"), Attribute::Underlined.sgr()).unwrap();
        write!(expected, ".").unwrap();

        write!(expected, csi!("0m")).unwrap();
        write!(expected, csi!("{}m"), Colored::ForegroundColor(Color::Red)).unwrap();
        write!(expected, csi!("{}m"), Attribute::Bold.sgr()).unwrap();
        write!(expected, csi!("{}m"), Attribute::Italic.sgr()).unwrap();
        write!(expected, ".").unwrap();

        write!(expected, csi!("0m")).unwrap();
        write!(expected, csi!("{}m"), Colored::ForegroundColor(Color::Red)).unwrap();
        write!(expected, csi!("{}m"), Attribute::Bold.sgr()).unwrap();
        write!(expected, ".").unwrap();

        write!(expected, csi!("{}m"), Attribute::Dim.sgr()).unwrap();
        write!(expected, ".").unwrap();

        write!(expected, csi!("0m")).unwrap();
        write!(expected, csi!("{}m"), Colored::ForegroundColor(Color::Red)).unwrap();
        write!(expected, ".").unwrap();

        write!(
            expected,
            csi!("{}m"),
            Colored::ForegroundColor(Color::Green)
        )
        .unwrap();
        write!(expected, ".").unwrap();

        write!(expected, csi!("{}m"), Attribute::Reverse.sgr()).unwrap();
        write!(expected, ".").unwrap();

        write!(expected, csi!("0m")).unwrap();
        write!(
            expected,
            csi!("{}m"),
            Colored::ForegroundColor(Color::Green)
        )
        .unwrap();
        write!(expected, ".").unwrap();

        write!(expected, csi!("{}m"), Attribute::CrossedOut.sgr()).unwrap();
        write!(expected, ".").unwrap();

        write!(expected, csi!("0m")).unwrap();
        write!(
            expected,
            csi!("{}m"),
            Colored::ForegroundColor(Color::Green)
        )
        .unwrap();
        write!(expected, csi!("{}m"), Attribute::OverLined.sgr()).unwrap();
        write!(expected, ".").unwrap();

        write!(expected, csi!("0m")).unwrap();
        write!(expected, csi!("K")).unwrap();
        write!(expected, csi!("0m")).unwrap();
        write!(expected, "\r\n").unwrap();

        assert_eq!(actual, expected);
    }

    #[test]
    fn test_canvas_ansi_underline_color_matches_cc_ink_sgr_parser() {
        let mut canvas = Canvas::new(4, 1);
        canvas.subview_mut(0, 0, 0, 0, 4, 1).set_text(
            0,
            0,
            "u",
            CanvasTextStyle {
                underline: true,
                underline_color: Some(Color::Rgb { r: 1, g: 2, b: 3 }),
                ..Default::default()
            },
        );

        let mut actual = Vec::new();
        canvas.write_ansi(&mut actual).unwrap();
        let actual = String::from_utf8(actual).unwrap();
        assert!(
            actual.contains(&format!(
                "\x1b[{}m",
                Colored::UnderlineColor(Color::Rgb { r: 1, g: 2, b: 3 })
            )),
            "underline color should emit SGR 58 truecolor: {actual:?}"
        );
    }

    #[test]
    fn test_canvas_ansi_blink_and_hidden_match_cc_ink_sgr_parser() {
        for (style, attr) in [
            (
                CanvasTextStyle {
                    blink: true,
                    ..Default::default()
                },
                Attribute::SlowBlink,
            ),
            (
                CanvasTextStyle {
                    hidden: true,
                    ..Default::default()
                },
                Attribute::Hidden,
            ),
        ] {
            let mut canvas = Canvas::new(4, 1);
            canvas
                .subview_mut(0, 0, 0, 0, 4, 1)
                .set_text(0, 0, "x", style);

            let mut actual = Vec::new();
            canvas.write_ansi(&mut actual).unwrap();
            let actual = String::from_utf8(actual).unwrap();
            assert!(
                actual.contains(&format!("\x1b[{}m", attr.sgr())),
                "style {style:?} should emit {:?}: {actual:?}",
                attr.sgr()
            );
        }
    }

    #[test]
    fn test_canvas_ansi_underline_variants_match_cc_ink_sgr_parser() {
        for (style, attr) in [
            (UnderlineStyle::Single, Attribute::Underlined),
            (UnderlineStyle::Double, Attribute::DoubleUnderlined),
            (UnderlineStyle::Curly, Attribute::Undercurled),
            (UnderlineStyle::Dotted, Attribute::Underdotted),
            (UnderlineStyle::Dashed, Attribute::Underdashed),
        ] {
            let mut canvas = Canvas::new(4, 1);
            canvas.subview_mut(0, 0, 0, 0, 4, 1).set_text(
                0,
                0,
                "u",
                CanvasTextStyle {
                    underline: true,
                    underline_style: style,
                    ..Default::default()
                },
            );

            let mut actual = Vec::new();
            canvas.write_ansi(&mut actual).unwrap();
            let actual = String::from_utf8(actual).unwrap();
            assert!(
                actual.contains(&format!("\x1b[{}m", attr.sgr())),
                "underline style {style:?} should emit {:?}: {actual:?}",
                attr.sgr()
            );
        }
    }

    #[test]
    fn test_style_overlay_helpers_match_selection_and_search_semantics() {
        let selection = StyleOverlay::selection_background(Color::Blue);
        assert_eq!(selection.background_color, Some(Some(Color::Blue)));
        assert_eq!(
            selection.color, None,
            "selection keeps the existing foreground"
        );
        assert_eq!(selection.invert, Some(false));

        let inverse = StyleOverlay::inverse();
        assert_eq!(inverse.invert, Some(true));

        let current = StyleOverlay::current_match(Color::Yellow);
        assert_eq!(current.color, Some(Some(Color::Yellow)));
        assert_eq!(current.background_color, Some(None));
        assert_eq!(current.weight, Some(Weight::Bold));
        assert_eq!(current.underline, Some(true));
        assert_eq!(current.invert, Some(true));
    }

    #[test]
    fn test_canvas_text_clipping() {
        let mut canvas = Canvas::new(10, 5);
        assert_eq!(canvas.width(), 10);
        assert_eq!(canvas.height(), 5);

        canvas.subview_mut(2, 2, 2, 2, 4, 2).set_text(
            -2,
            -1,
            "line 1\nline 2\nline 3\nline 4",
            CanvasTextStyle::default(),
        );

        let actual = canvas.to_string();
        assert_eq!(actual, "\n\n  ne 2\n  ne 3\n\n");
    }

    #[test]
    fn test_canvas_text_clearing() {
        let mut canvas = Canvas::new(10, 1);
        canvas
            .subview_mut(0, 0, 0, 0, 10, 1)
            .set_text(0, 0, "hello!", CanvasTextStyle::default());
        assert_eq!(canvas.to_string(), "hello!\n");

        canvas.subview_mut(0, 0, 0, 0, 10, 1).clear_text(0, 0, 3, 1);
        assert_eq!(canvas.to_string(), "   lo!\n");
    }

    #[test]
    fn test_clear_text_clears_wide_character_relationships() {
        let mut canvas = Canvas::new(10, 1);
        {
            let mut sv = canvas.subview_mut(0, 0, 0, 0, 10, 1);
            sv.set_text(0, 0, "中", CanvasTextStyle::default());
            sv.clear_text(1, 0, 1, 1);
        }
        assert_eq!(canvas.cells[0][0].cell_width, CellWidth::Normal);
        assert_eq!(canvas.cells[0][1].cell_width, CellWidth::Normal);
        assert_eq!(canvas.get_text(0, 0, 10, 1), "");
    }

    #[test]
    fn test_write_ansi_without_final_newline() {
        let mut canvas = Canvas::new(10, 3);

        canvas
            .subview_mut(0, 0, 0, 0, 10, 3)
            .set_text(0, 0, "hello!", CanvasTextStyle::default());

        let mut actual = Vec::new();
        canvas
            .write_ansi_without_final_newline(&mut actual)
            .unwrap();

        let mut expected = Vec::new();
        // row 0
        write!(expected, csi!("0m")).unwrap();
        write!(expected, "hello!").unwrap();
        write!(expected, csi!("K")).unwrap();
        write!(expected, csi!("0m")).unwrap();
        write!(expected, "\r\n").unwrap();
        // row 1
        write!(expected, csi!("K")).unwrap();
        write!(expected, csi!("0m")).unwrap();
        write!(expected, "\r\n").unwrap();
        // row 2 (final, no newline)
        write!(expected, csi!("K")).unwrap();
        write!(expected, csi!("0m")).unwrap();

        assert_eq!(actual, expected);
    }

    #[test]
    fn test_ansi_erase_for_full_rows() {
        let mut canvas = Canvas::new(10, 1);

        canvas.subview_mut(0, 0, 0, 0, 10, 1).set_text(
            0,
            0,
            "1234512345",
            CanvasTextStyle::default(),
        );

        let mut actual = Vec::new();
        canvas.write_ansi(&mut actual).unwrap();

        let mut expected = Vec::new();
        write!(expected, csi!("0m")).unwrap();
        write!(expected, "1234512345").unwrap();
        write!(expected, csi!("K")).unwrap();
        write!(expected, csi!("0m")).unwrap();
        write!(expected, "\r\n").unwrap();

        assert_eq!(actual, expected);
    }

    #[test]
    fn test_cell_read() {
        let mut canvas = Canvas::new(10, 3);
        canvas
            .subview_mut(0, 0, 0, 0, 10, 3)
            .set_text(0, 0, "hello", CanvasTextStyle::default());
        assert_eq!(canvas.cell(0, 0).and_then(|c| c.text()), Some("h"));
        assert_eq!(canvas.cell(4, 0).and_then(|c| c.text()), Some("o"));
        assert_eq!(canvas.cell(5, 0).and_then(|c| c.text()), None);
        assert_eq!(canvas.cell(99, 99), None);
    }

    #[test]
    fn test_get_text_single_row() {
        let mut canvas = Canvas::new(10, 3);
        {
            let mut sv = canvas.subview_mut(0, 0, 0, 0, 10, 3);
            sv.set_text(0, 0, "hello", CanvasTextStyle::default());
            sv.set_text(2, 1, "ab", CanvasTextStyle::default());
        }
        assert_eq!(canvas.get_text(0, 0, 10, 1), "hello");
        assert_eq!(canvas.get_text(0, 1, 10, 1), "  ab");
        assert_eq!(canvas.get_text(0, 2, 10, 1), "");
    }

    #[test]
    fn test_get_text_multi_row() {
        let mut canvas = Canvas::new(10, 3);
        {
            let mut sv = canvas.subview_mut(0, 0, 0, 0, 10, 3);
            sv.set_text(0, 0, "line one", CanvasTextStyle::default());
            sv.set_text(0, 1, "line two", CanvasTextStyle::default());
        }
        assert_eq!(
            canvas.get_text(0, 0, 10, 3),
            "line one
line two
"
        );
    }

    #[test]
    fn test_get_text_partial_row() {
        let mut canvas = Canvas::new(10, 1);
        canvas
            .subview_mut(0, 0, 0, 0, 10, 1)
            .set_text(0, 0, "abcdef", CanvasTextStyle::default());
        assert_eq!(canvas.get_text(2, 0, 3, 1), "cde");
    }

    #[test]
    fn test_cell_text_style() {
        let mut canvas = Canvas::new(10, 1);
        let style = CanvasTextStyle {
            weight: Weight::Bold,
            invert: true,
            ..Default::default()
        };
        canvas
            .subview_mut(0, 0, 0, 0, 10, 1)
            .set_text(0, 0, "hi", style);
        let cell = canvas.cell(0, 0).unwrap();
        let ts = cell.text_style().unwrap();
        assert_eq!(ts.weight, Weight::Bold);
        assert!(ts.invert);
        // Empty cell returns None.
        assert!(canvas.cell(5, 0).unwrap().text_style().is_none());
    }

    #[test]
    fn test_cell_is_empty() {
        let mut canvas = Canvas::new(5, 1);
        assert!(canvas.cell(0, 0).unwrap().is_empty());
        canvas
            .subview_mut(0, 0, 0, 0, 5, 1)
            .set_text(0, 0, "a", CanvasTextStyle::default());
        assert!(!canvas.cell(0, 0).unwrap().is_empty());
    }

    #[test]
    fn test_cell_is_blank_matches_click_metadata_semantics() {
        let mut canvas = Canvas::new(5, 1);
        assert!(canvas.cell_is_blank(0, 0));
        assert!(canvas.cell_is_blank(99, 0));

        canvas
            .subview_mut(0, 0, 0, 0, 5, 1)
            .set_text(0, 0, "a ", CanvasTextStyle::default());
        assert!(!canvas.cell_is_blank(0, 0));
        assert!(
            canvas.cell_is_blank(1, 0),
            "plain default spaces are screen-buffer blank like CC Ink"
        );

        canvas.set_background_color(2, 0, 1, 1, Color::Blue);
        assert!(!canvas.cell_is_blank(2, 0));
        canvas.set_overlay(3, 0, StyleOverlay::inverse());
        assert!(!canvas.cell_is_blank(3, 0));
    }

    #[test]
    fn test_subview_cell_relative_coords() {
        let mut canvas = Canvas::new(10, 5);
        // Subview at offset (2, 1) with clip matching subview area
        let mut sv = canvas.subview_mut(2, 1, 2, 1, 6, 3);
        sv.set_text(0, 0, "abc", CanvasTextStyle::default());
        // Read back via subview using relative coordinates
        assert_eq!(sv.cell(0, 0).and_then(|c| c.text()), Some("a"));
        assert_eq!(sv.cell(2, 0).and_then(|c| c.text()), Some("c"));
        // Out of clip bounds → None
        assert_eq!(sv.cell(-1, 0), None);
        assert_eq!(sv.cell(6, 0), None);
        assert_eq!(sv.cell(0, -1), None);
        assert_eq!(sv.cell(0, 3), None);
    }

    #[test]
    fn test_overlay_merges_invert_into_resolved_style() {
        let mut canvas = Canvas::new(5, 1);
        let style = CanvasTextStyle {
            color: Some(Color::Red),
            weight: Weight::Bold,
            ..Default::default()
        };
        canvas
            .subview_mut(0, 0, 0, 0, 5, 1)
            .set_text(0, 0, "hello", style);
        canvas.set_overlay(
            1,
            0,
            StyleOverlay {
                invert: Some(true),
                ..Default::default()
            },
        );
        let resolved = canvas.resolved_text_style(1, 0).unwrap();
        assert!(resolved.invert);
        assert_eq!(resolved.color, Some(Color::Red));
        assert_eq!(resolved.weight, Weight::Bold);
        // Cell without overlay keeps original style.
        let original = canvas.resolved_text_style(0, 0).unwrap();
        assert!(!original.invert);
    }

    #[test]
    fn test_overlay_on_empty_cell_emits_sgr_reverse() {
        let mut canvas = Canvas::new(3, 1);
        canvas.set_overlay(
            1,
            0,
            StyleOverlay {
                invert: Some(true),
                ..Default::default()
            },
        );
        let mut buf = Vec::new();
        canvas.write_ansi(&mut buf).unwrap();
        let output = String::from_utf8_lossy(&buf);
        assert!(
            output.contains(&Attribute::Reverse.sgr().to_string()),
            "expected SGR Reverse in output: {output:?}"
        );
    }

    #[test]
    fn test_overlay_rect_applies_to_all_cells_in_range() {
        let mut canvas = Canvas::new(5, 2);
        canvas
            .subview_mut(0, 0, 0, 0, 5, 2)
            .set_text(0, 0, "abcde", CanvasTextStyle::default());
        canvas
            .subview_mut(0, 0, 0, 0, 5, 2)
            .set_text(0, 1, "fghij", CanvasTextStyle::default());
        canvas.set_overlay_rect(
            1,
            0,
            3,
            2,
            StyleOverlay {
                invert: Some(true),
                ..Default::default()
            },
        );
        for y in 0..2 {
            for x in 0..5 {
                let resolved = canvas.resolved_text_style(x, y);
                let expected_invert = (1..4).contains(&x);
                assert_eq!(
                    resolved.map(|s| s.invert).unwrap_or(false),
                    expected_invert,
                    "cell ({x},{y}) invert mismatch"
                );
            }
        }
    }

    #[test]
    fn test_clear_overlay_removes_inversion() {
        let mut canvas = Canvas::new(3, 1);
        canvas
            .subview_mut(0, 0, 0, 0, 3, 1)
            .set_text(0, 0, "abc", CanvasTextStyle::default());
        canvas.set_overlay(
            1,
            0,
            StyleOverlay {
                invert: Some(true),
                ..Default::default()
            },
        );
        assert!(canvas.resolved_text_style(1, 0).unwrap().invert);
        canvas.clear_overlay(1, 0);
        assert!(!canvas.resolved_text_style(1, 0).unwrap().invert);
    }

    #[test]
    fn test_clear_overlays_resets_all() {
        let mut canvas = Canvas::new(3, 1);
        canvas
            .subview_mut(0, 0, 0, 0, 3, 1)
            .set_text(0, 0, "abc", CanvasTextStyle::default());
        canvas.set_overlay_rect(
            0,
            0,
            3,
            1,
            StyleOverlay {
                invert: Some(true),
                ..Default::default()
            },
        );
        canvas.clear_overlays();
        for x in 0..3 {
            assert!(!canvas.resolved_text_style(x, 0).unwrap().invert);
        }
    }

    #[test]
    fn test_overlay_background_color_override() {
        let mut canvas = Canvas::new(3, 1);
        canvas
            .subview_mut(0, 0, 0, 0, 3, 1)
            .set_background_color(0, 0, 3, 1, Color::Red);
        canvas.set_overlay(
            1,
            0,
            StyleOverlay {
                background_color: Some(Some(Color::Blue)),
                ..Default::default()
            },
        );
        let mut buf = Vec::new();
        canvas.write_ansi(&mut buf).unwrap();
        let output = String::from_utf8_lossy(&buf);
        // Cell 0 should have Red background, cell 1 should have Blue (overridden by overlay).
        assert!(output.contains(&format!("{}", Colored::BackgroundColor(Color::Blue))));
    }

    #[test]
    fn test_overlay_composes_post_render_layers_like_ink() {
        let mut canvas = Canvas::new(5, 1);
        canvas
            .subview_mut(0, 0, 0, 0, 5, 1)
            .set_text(0, 0, "hello", CanvasTextStyle::default());

        canvas.set_overlay(1, 0, StyleOverlay::selection_background(Color::Blue));
        canvas.set_overlay(1, 0, StyleOverlay::inverse());
        let selected_match = canvas.overlays[0][1].unwrap();
        assert_eq!(selected_match.background_color, Some(Some(Color::Blue)));
        assert_eq!(selected_match.invert, Some(true));

        canvas.set_overlay(1, 0, StyleOverlay::current_match(Color::Yellow));
        let current = canvas.overlays[0][1].unwrap();
        assert_eq!(current.color, Some(Some(Color::Yellow)));
        assert_eq!(current.background_color, Some(None));
        assert_eq!(current.invert, Some(true));
        assert_eq!(current.weight, Some(Weight::Bold));
        assert_eq!(current.underline, Some(true));
    }

    #[test]
    fn test_subview_set_overlay_respects_clip() {
        let mut canvas = Canvas::new(10, 5);
        canvas.subview_mut(0, 0, 0, 0, 10, 5).set_text(
            0,
            0,
            "0123456789",
            CanvasTextStyle::default(),
        );
        // Subview at (2,1) with clip width 4. Overlay at relative (0,0) = absolute (2,1).
        {
            let mut sv = canvas.subview_mut(2, 1, 2, 1, 4, 3);
            sv.set_overlay(
                0,
                0,
                StyleOverlay {
                    invert: Some(true),
                    ..Default::default()
                },
            );
            // Out-of-clip: relative (-1, 0) should be silently ignored.
            sv.set_overlay(
                -1,
                0,
                StyleOverlay {
                    invert: Some(true),
                    ..Default::default()
                },
            );
        }
        // Absolute (2,1) should have overlay; absolute (1,1) should not.
        assert!(
            canvas
                .overlays
                .get(1)
                .and_then(|r| r.get(2))
                .and_then(|o| o.as_ref())
                .is_some(),
            "overlay at abs (2,1) should exist"
        );
        assert!(
            canvas
                .overlays
                .get(1)
                .and_then(|r| r.get(1))
                .and_then(|o| o.as_ref())
                .is_none(),
            "overlay at abs (1,1) should NOT exist (out of clip)"
        );
    }

    #[test]
    fn test_declare_cursor_bounds_and_subview_translation() {
        let cd = |x, y, v| CursorDeclaration { x, y, visible: v };
        let mut canvas = Canvas::new(10, 5);
        assert_eq!(canvas.cursor_declaration(), None);

        // Out-of-bounds declarations are ignored.
        canvas.declare_cursor(10, 0, false);
        canvas.declare_cursor(0, 5, false);
        assert_eq!(canvas.cursor_declaration(), None);

        canvas.declare_cursor(3, 2, false);
        assert_eq!(canvas.cursor_declaration(), Some(cd(3, 2, false)));

        // Last writer wins.
        canvas.declare_cursor(1, 1, true);
        assert_eq!(canvas.cursor_declaration(), Some(cd(1, 1, true)));

        // Subview translates relative coordinates and respects clipping.
        {
            let mut sv = canvas.subview_mut(2, 1, 2, 1, 4, 3);
            sv.declare_cursor(1, 1, false); // absolute (3, 2)
        }
        assert_eq!(canvas.cursor_declaration(), Some(cd(3, 2, false)));
        {
            let mut sv = canvas.subview_mut(2, 1, 2, 1, 4, 3);
            sv.declare_cursor(-1, 0, false); // outside clip — ignored
        }
        assert_eq!(canvas.cursor_declaration(), Some(cd(3, 2, false)));
    }

    #[test]
    fn test_clear_region_clears_cells_overlays_and_marks_damage() {
        let mut canvas = Canvas::new(6, 2);
        canvas
            .subview_mut(0, 0, 0, 0, 6, 2)
            .set_text(0, 0, "abcdef", CanvasTextStyle::default());
        canvas.mark_no_select_region(1, 0, 2, 1);
        canvas.mark_soft_wrap_continuation(1, 5);
        canvas.set_overlay(
            2,
            0,
            StyleOverlay {
                invert: Some(true),
                ..Default::default()
            },
        );

        canvas.clear_region(1, 0, 3, 1);

        assert_eq!(canvas.get_text(0, 0, 6, 2), "a   ef\n");
        assert!(!canvas.resolved_text_style(2, 0).unwrap_or_default().invert);
        assert!(
            canvas.is_no_select(1, 0),
            "clearRegion should not erase selection-only noSelect metadata"
        );
        assert_eq!(
            canvas.soft_wrap_continuation(1),
            5,
            "clearRegion should not erase per-row softWrap metadata"
        );
        assert_eq!(
            canvas.damage_region(),
            Some(DamageRegion {
                x: 1,
                y: 0,
                width: 3,
                height: 1,
            })
        );
    }

    #[test]
    fn test_clear_region_repairs_wide_boundaries() {
        let mut canvas = Canvas::new(6, 1);
        canvas
            .subview_mut(0, 0, 0, 0, 6, 1)
            .set_text(1, 0, "中x中", CanvasTextStyle::default());

        canvas.clear_region(2, 0, 3, 1);

        assert_eq!(canvas.cells[0][1].cell_width, CellWidth::Normal);
        assert_eq!(canvas.cells[0][5].cell_width, CellWidth::Normal);
        assert_eq!(
            canvas.damage_region(),
            Some(DamageRegion {
                x: 1,
                y: 0,
                width: 5,
                height: 1,
            })
        );
    }

    #[test]
    fn test_copy_region_preserves_cells_metadata_and_clears_snapshot_damage() {
        let mut src = Canvas::new(6, 2);
        src.subview_mut(0, 0, 0, 0, 6, 2).set_text_with_link(
            1,
            0,
            "abc",
            CanvasTextStyle {
                color: Some(Color::Red),
                ..Default::default()
            },
            Some("https://example.com"),
        );
        src.set_overlay(2, 0, StyleOverlay::selection_background(Color::Blue));
        src.mark_no_select_region(3, 0, 1, 1);

        let copy = src.copy_region(1, 0, 3, 1);
        assert_eq!(copy.width(), 3);
        assert_eq!(copy.height(), 1);
        assert_eq!(copy.to_string(), "abc\n");
        assert_eq!(
            copy.hyperlink_at(0, 0).as_deref(),
            Some("https://example.com")
        );
        let mut ansi = Vec::new();
        copy.write_ansi(&mut ansi).unwrap();
        let ansi = String::from_utf8_lossy(&ansi);
        assert!(ansi.contains(&format!("{}", Colored::BackgroundColor(Color::Blue))));
        assert!(copy.is_no_select(2, 0));
        assert_eq!(copy.damage_region(), None);
    }

    #[test]
    fn test_blit_region_copies_cells_metadata_and_marks_damage() {
        let mut src = Canvas::new(6, 2);
        src.subview_mut(0, 0, 0, 0, 6, 2)
            .set_text(0, 0, "abcdef", CanvasTextStyle::default());
        src.subview_mut(0, 0, 0, 0, 6, 2)
            .set_text(0, 1, "ghijkl", CanvasTextStyle::default());
        src.mark_no_select_region(2, 0, 2, 2);
        src.mark_soft_wrap_continuation(1, 5);
        src.set_overlay(
            3,
            0,
            StyleOverlay {
                invert: Some(true),
                ..Default::default()
            },
        );

        let mut dst = Canvas::new(6, 2);
        dst.blit_region_from(&src, 1, 0, 4, 2);

        assert_eq!(dst.get_text(0, 0, 6, 2), " bcde\n hijk");
        assert!(dst.is_no_select(2, 0));
        assert!(dst.is_no_select(3, 1));
        assert_eq!(dst.soft_wrap_continuation(1), 5);
        assert!(dst.resolved_text_style(3, 0).unwrap().invert);
        assert_eq!(
            dst.damage_region(),
            Some(DamageRegion {
                x: 1,
                y: 0,
                width: 4,
                height: 2,
            })
        );
    }

    #[test]
    fn test_blit_region_excluding_clears_skips_absolute_clear_rows_like_cc_output() {
        let mut src = Canvas::new(6, 4);
        for (row, text) in ["aaaaaa", "bbbbbb", "cccccc", "dddddd"]
            .into_iter()
            .enumerate()
        {
            src.subview_mut(0, 0, 0, 0, 6, 4).set_text(
                0,
                row as isize,
                text,
                CanvasTextStyle::default(),
            );
        }
        src.mark_no_select_region(1, 1, 1, 1);
        src.mark_no_select_region(1, 2, 2, 1);

        let mut dst = Canvas::new(6, 4);
        dst.subview_mut(0, 0, 0, 0, 6, 4)
            .set_text(0, 2, "stale", CanvasTextStyle::default());
        let absolute_clear = DamageRegion {
            x: 0,
            y: 2,
            width: 6,
            height: 1,
        };
        dst.clear_region(
            absolute_clear.x,
            absolute_clear.y,
            absolute_clear.width,
            absolute_clear.height,
        );

        dst.blit_region_from_excluding_clears(&src, 0, 0, 6, 4, &[absolute_clear]);

        assert_eq!(dst.get_text(0, 0, 6, 4), "aaaaaa\nbbbbbb\n\ndddddd");
        assert!(
            !dst.is_no_select(1, 2),
            "skipped rows must not restore noSelect metadata from prevScreen"
        );
        assert!(
            dst.is_no_select(1, 1),
            "non-excluded rows should still copy noSelect metadata normally"
        );
    }

    #[test]
    fn test_blit_region_excluding_clears_keeps_partially_covered_rows() {
        let mut src = Canvas::new(6, 2);
        src.subview_mut(0, 0, 0, 0, 6, 2)
            .set_text(1, 1, "bcde", CanvasTextStyle::default());
        let mut dst = Canvas::new(6, 2);
        let partial_clear = DamageRegion {
            x: 2,
            y: 1,
            width: 2,
            height: 1,
        };

        dst.blit_region_from_excluding_clears(&src, 1, 1, 4, 1, &[partial_clear]);

        assert_eq!(dst.get_text(0, 1, 6, 1), " bcde");
    }

    #[test]
    fn test_blit_region_repairs_wide_tail_outside_region() {
        let mut src = Canvas::new(4, 1);
        src.subview_mut(0, 0, 0, 0, 4, 1)
            .set_text(1, 0, "中", CanvasTextStyle::default());
        let mut dst = Canvas::new(4, 1);

        dst.blit_region_from(&src, 1, 0, 1, 1);

        assert_eq!(dst.cells[0][1].cell_width, CellWidth::Wide);
        assert_eq!(dst.cells[0][2].cell_width, CellWidth::WidthTail);
        assert_eq!(
            dst.damage_region(),
            Some(DamageRegion {
                x: 1,
                y: 0,
                width: 2,
                height: 1,
            })
        );
    }

    #[test]
    fn test_subview_blit_region_copies_metadata_with_offset() {
        let mut src = Canvas::new(6, 2);
        src.subview_mut(0, 0, 0, 0, 6, 2)
            .set_text(0, 0, "abcdef", CanvasTextStyle::default());
        src.subview_mut(0, 0, 0, 0, 6, 2)
            .set_text(0, 1, "ghijkl", CanvasTextStyle::default());
        src.mark_no_select_region(2, 0, 2, 2);
        src.mark_soft_wrap_continuation(1, 5);
        src.set_overlay(3, 0, StyleOverlay::inverse());

        let mut dst = Canvas::new(10, 4);
        dst.subview_mut(2, 1, 0, 0, 10, 4)
            .blit_region_from(&src, 0, 0, 1, 0, 4, 2);

        assert_eq!(dst.get_text(0, 0, 10, 4), "\n  bcde\n  hijk\n");
        assert!(dst.is_no_select(3, 1));
        assert!(dst.is_no_select(4, 2));
        assert_eq!(dst.soft_wrap_continuation(2), 6);
        assert!(dst.resolved_text_style(4, 1).unwrap().invert);
        assert_eq!(
            dst.damage_region(),
            Some(DamageRegion {
                x: 2,
                y: 1,
                width: 4,
                height: 2,
            })
        );
    }

    #[test]
    fn test_subview_clear_region_clears_overlay_and_marks_damage() {
        let mut canvas = Canvas::new(10, 3);
        canvas.subview_mut(0, 0, 0, 0, 10, 3).set_text(
            0,
            1,
            "abcdefghij",
            CanvasTextStyle::default(),
        );
        canvas.mark_no_select_region(4, 1, 1, 1);
        canvas.mark_soft_wrap_continuation(1, 8);
        canvas.set_overlay(4, 1, StyleOverlay::inverse());

        canvas
            .subview_mut(2, 1, 2, 1, 5, 1)
            .clear_region(1, 0, 3, 1);

        assert_eq!(canvas.get_text(0, 1, 10, 1), "abc   ghij");
        assert!(
            canvas.resolved_text_style(4, 1).is_none(),
            "clear_region should remove text and overlay style metadata"
        );
        assert!(
            canvas.is_no_select(4, 1),
            "clear_region mirrors CC Ink and leaves noSelect metadata intact"
        );
        assert_eq!(canvas.soft_wrap_continuation(1), 8);
        assert_eq!(
            canvas.damage_region(),
            Some(DamageRegion {
                x: 3,
                y: 1,
                width: 3,
                height: 1,
            })
        );
    }

    #[test]
    fn test_no_select_region_is_metadata_only_and_clipped() {
        let mut canvas = Canvas::new(4, 2);
        let clean = Canvas::new(4, 2);

        canvas.mark_no_select_region(1, 0, 10, 10);

        assert!(!canvas.is_no_select(0, 0));
        assert!(canvas.is_no_select(1, 0));
        assert!(canvas.is_no_select(3, 1));
        assert!(!canvas.is_no_select(4, 1));
        assert!(
            canvas == clean,
            "noSelect metadata must not affect terminal-output equality"
        );
        assert_eq!(canvas.to_string(), clean.to_string());
    }

    #[test]
    fn test_no_select_subview_clipping_and_shift_rows() {
        let mut canvas = Canvas::new(6, 4);
        {
            let mut sv = canvas.subview_mut(2, 1, 2, 1, 3, 2);
            sv.mark_no_select_region(-1, 0, 3, 2);
        }

        assert!(!canvas.is_no_select(1, 1));
        assert!(canvas.is_no_select(2, 1));
        assert!(canvas.is_no_select(3, 2));
        assert!(!canvas.is_no_select(4, 1));

        canvas.shift_rows(0, 3, 1);
        assert!(
            canvas.is_no_select(2, 0),
            "noSelect marks should move with cells during scroll blits"
        );
        assert!(!canvas.is_no_select(2, 3));
    }

    #[test]
    fn test_soft_wrap_selection_joins_rows_and_clamps_padding() {
        let mut canvas = Canvas::new(8, 2);
        canvas
            .subview_mut(0, 0, 0, 0, 8, 2)
            .set_text(0, 0, "hello", CanvasTextStyle::default());
        canvas
            .subview_mut(0, 0, 0, 0, 8, 2)
            .set_text(0, 1, "world", CanvasTextStyle::default());
        canvas.mark_soft_wrap_continuation(1, 5);

        assert_eq!(
            canvas.selected_text(SelectionRange::new(
                SelectionPoint { col: 0, row: 0 },
                SelectionPoint { col: 7, row: 1 },
            )),
            "helloworld",
            "soft-wrap continuations should copy as one logical line without unwritten padding"
        );
    }

    #[test]
    fn test_soft_wrap_metadata_shifts_with_rows() {
        let mut canvas = Canvas::new(4, 3);
        canvas.mark_soft_wrap_continuation(1, 3);
        canvas.shift_rows(0, 2, 1);
        assert_eq!(canvas.soft_wrap_continuation(0), 3);
        assert_eq!(canvas.soft_wrap_continuation(2), 0);
    }

    #[test]
    fn test_selection_state_click_without_drag_does_not_select() {
        let mut selection = SelectionState::new();
        selection.start(1, 0);
        selection.update(1, 0);
        assert!(!selection.has_selection());
        selection.update(2, 0);
        assert!(selection.has_selection());
        selection.finish();
        assert!(!selection.is_dragging());
    }

    #[test]
    fn test_selection_click_tracker_matches_cc_ink_thresholds() {
        let mut tracker = SelectionClickTracker::new();

        assert_eq!(
            tracker.record_press(10, 5, 1_000),
            SelectionMousePressKind::Single
        );
        assert_eq!(
            tracker.record_press(11, 6, 1_499),
            SelectionMousePressKind::Double
        );
        assert_eq!(
            tracker.record_press(11, 6, 1_998),
            SelectionMousePressKind::Triple
        );
        assert_eq!(
            tracker.record_press(11, 6, 2_100),
            SelectionMousePressKind::Triple,
            "quadruple and later clicks are capped to line-selection semantics"
        );
    }

    #[test]
    fn test_selection_click_tracker_resets_on_timeout_or_distance() {
        let mut tracker = SelectionClickTracker::new();

        assert_eq!(
            tracker.record_press(2, 2, 100),
            SelectionMousePressKind::Single
        );
        assert_eq!(
            tracker.record_press(2, 2, 600),
            SelectionMousePressKind::Single,
            "CC Ink uses a strict < 500ms threshold"
        );
        assert_eq!(
            tracker.record_press(2, 2, 700),
            SelectionMousePressKind::Double
        );
        assert_eq!(
            tracker.record_press(4, 2, 800),
            SelectionMousePressKind::Single,
            "movement beyond one cell breaks the multi-click chain"
        );
        tracker.reset();
        assert_eq!(
            tracker.record_press(4, 2, 801),
            SelectionMousePressKind::Single
        );
    }

    #[test]
    fn test_selection_controller_single_press_drag_release_and_take_text() {
        let mut canvas = Canvas::new(8, 1);
        canvas
            .subview_mut(0, 0, 0, 0, 8, 1)
            .set_text(0, 0, "abcdef", CanvasTextStyle::default());
        let mut controller = SelectionController::new();

        let outcome = controller.handle_left_press(&canvas, 1, 0, 1_000, true);
        assert_eq!(outcome.kind, SelectionMousePressKind::Single);
        assert!(!outcome.finished_previous_drag);
        assert!(!outcome.cancel_pending_hyperlink);
        assert!(controller.selection().last_press_had_alt());
        assert!(
            !controller.has_selection(),
            "bare press is not yet a selection"
        );

        controller.handle_drag(&canvas, 3, 0);
        assert!(controller.has_selection());
        assert_eq!(controller.selected_text(&canvas), "bcd");
        assert!(controller.handle_release());
        assert!(!controller.selection().is_dragging());

        assert_eq!(controller.take_selected_text(&canvas), "bcd");
        assert!(!controller.has_selection());
    }

    #[test]
    fn test_selection_controller_copy_on_select_text_fires_once_after_release() {
        let mut canvas = Canvas::new(8, 1);
        canvas
            .subview_mut(0, 0, 0, 0, 8, 1)
            .set_text(0, 0, "abcdef", CanvasTextStyle::default());
        let mut controller = SelectionController::new();

        controller.handle_left_press(&canvas, 1, 0, 1_000, false);
        controller.handle_drag(&canvas, 3, 0);
        assert!(!controller.copy_on_select_would_mutate());
        assert_eq!(controller.copy_on_select_text(&canvas), None);
        controller.handle_release();

        assert!(controller.copy_on_select_would_mutate());
        assert_eq!(
            controller.copy_on_select_text(&canvas).as_deref(),
            Some("bcd")
        );
        assert!(!controller.copy_on_select_would_mutate());
        assert_eq!(controller.copy_on_select_text(&canvas), None);
        assert!(
            controller.has_selection(),
            "copy-on-select should not clear highlight"
        );
    }

    #[test]
    fn test_selection_controller_copy_on_select_text_resets_for_new_drag() {
        let mut canvas = Canvas::new(8, 1);
        canvas
            .subview_mut(0, 0, 0, 0, 8, 1)
            .set_text(0, 0, "abcdef", CanvasTextStyle::default());
        let mut controller = SelectionController::new();

        controller.handle_left_press(&canvas, 1, 0, 1_000, false);
        controller.handle_drag(&canvas, 3, 0);
        controller.handle_release();
        assert_eq!(
            controller.copy_on_select_text(&canvas).as_deref(),
            Some("bcd")
        );

        controller.handle_left_press(&canvas, 2, 0, 2_000, false);
        controller.handle_drag(&canvas, 4, 0);
        controller.handle_release();
        assert_eq!(
            controller.copy_on_select_text(&canvas).as_deref(),
            Some("cde")
        );
    }

    #[test]
    fn test_selection_controller_copy_on_select_text_noops_for_click_without_drag() {
        let canvas = Canvas::new(8, 1);
        let mut controller = SelectionController::new();

        controller.handle_left_press(&canvas, 1, 0, 1_000, false);
        controller.handle_release();

        assert_eq!(controller.copy_on_select_text(&canvas), None);
    }

    #[test]
    fn test_selection_controller_copy_on_select_skips_whitespace_only_once() {
        let mut canvas = Canvas::new(8, 1);
        canvas
            .subview_mut(0, 0, 0, 0, 8, 1)
            .set_text(0, 0, "a   b", CanvasTextStyle::default());
        let mut controller = SelectionController::new();

        controller.handle_left_press(&canvas, 1, 0, 1_000, false);
        controller.handle_drag(&canvas, 3, 0);
        controller.handle_release();

        assert!(controller.has_selection());
        assert_eq!(controller.copy_on_select_text(&canvas), None);
        assert_eq!(
            controller.copy_on_select_text(&canvas),
            None,
            "whitespace-only selection should settle the copy-on-select guard"
        );

        controller.handle_left_press(&canvas, 0, 0, 2_000, false);
        controller.handle_drag(&canvas, 4, 0);
        controller.handle_release();
        assert_eq!(
            controller.copy_on_select_text(&canvas).as_deref(),
            Some("a   b")
        );
    }

    #[test]
    fn test_selection_controller_double_click_drag_extends_by_word() {
        let mut canvas = Canvas::new(16, 1);
        canvas.subview_mut(0, 0, 0, 0, 16, 1).set_text(
            0,
            0,
            "one two three",
            CanvasTextStyle::default(),
        );
        let mut controller = SelectionController::new();

        controller.handle_left_press(&canvas, 4, 0, 1_000, false);
        let outcome = controller.handle_left_press(&canvas, 4, 0, 1_200, false);
        assert_eq!(outcome.kind, SelectionMousePressKind::Double);
        assert!(outcome.cancel_pending_hyperlink);
        assert_eq!(controller.selected_text(&canvas), "two");

        controller.handle_drag(&canvas, 10, 0);
        assert_eq!(controller.selected_text(&canvas), "two three");
    }

    #[test]
    fn test_selection_controller_non_left_press_resets_click_chain() {
        let mut canvas = Canvas::new(8, 1);
        canvas
            .subview_mut(0, 0, 0, 0, 8, 1)
            .set_text(0, 0, "abcdef", CanvasTextStyle::default());
        let mut controller = SelectionController::new();

        assert_eq!(
            controller
                .handle_left_press(&canvas, 1, 0, 1_000, false)
                .kind,
            SelectionMousePressKind::Single
        );
        controller.handle_non_left_press();
        assert_eq!(
            controller
                .handle_left_press(&canvas, 1, 0, 1_100, false)
                .kind,
            SelectionMousePressKind::Single
        );
    }

    #[test]
    fn test_selection_controller_no_button_motion_finishes_drag_and_dedupes_hover() {
        let mut canvas = Canvas::new(8, 1);
        canvas
            .subview_mut(0, 0, 0, 0, 8, 1)
            .set_text(0, 0, "abcdef", CanvasTextStyle::default());
        let mut controller = SelectionController::new();

        controller.handle_left_press(&canvas, 1, 0, 1_000, false);
        controller.handle_drag(&canvas, 3, 0);
        assert!(controller.no_button_motion_would_change(3, 0));
        let first = controller.handle_no_button_motion(3, 0);
        assert!(first.finished_drag);
        assert_eq!(first.hover, Some(SelectionPoint { col: 3, row: 0 }));
        assert!(!controller.selection().is_dragging());

        assert!(!controller.no_button_motion_would_change(3, 0));
        let repeat = controller.handle_no_button_motion(3, 0);
        assert!(!repeat.finished_drag);
        assert_eq!(repeat.hover, None);
        assert!(controller.no_button_motion_would_change(4, 0));
        let moved = controller.handle_no_button_motion(4, 0);
        assert_eq!(moved.hover, Some(SelectionPoint { col: 4, row: 0 }));
    }

    #[test]
    fn test_selection_controller_focus_loss_finishes_drag() {
        let mut canvas = Canvas::new(8, 1);
        canvas
            .subview_mut(0, 0, 0, 0, 8, 1)
            .set_text(0, 0, "abcdef", CanvasTextStyle::default());
        let mut controller = SelectionController::new();

        controller.handle_left_press(&canvas, 1, 0, 1_000, false);
        assert!(controller.handle_focus_lost());
        assert!(!controller.selection().is_dragging());
        assert!(!controller.handle_focus_lost());
    }

    #[test]
    fn test_selection_controller_finish_drag_resets_autoscroll_direction() {
        let mut canvas = Canvas::new(6, 4);
        canvas
            .subview_mut(0, 0, 0, 0, 6, 4)
            .set_text(0, 1, "row1", CanvasTextStyle::default());
        let mut controller = SelectionController::new();
        controller.selection_mut().start(0, 1);
        controller.selection_mut().update(3, 4);
        assert_eq!(
            controller.drag_scroll_direction(1, 3),
            Some(SelectionDragScrollDirection::Down)
        );
        controller.translate_for_drag_autoscroll(
            &canvas,
            SelectionDragScrollDirection::Down,
            1,
            1,
            3,
        );

        assert!(controller.handle_release());

        assert_eq!(controller.last_drag_scroll_dir, None);
    }

    #[test]
    fn test_selection_controller_drag_scroll_direction_requires_anchor_in_viewport() {
        let mut canvas = Canvas::new(6, 4);
        canvas
            .subview_mut(0, 0, 0, 0, 6, 4)
            .set_text(0, 0, "head", CanvasTextStyle::default());
        let mut controller = SelectionController::new();
        controller.selection_mut().start(0, 0);
        controller.selection_mut().update(0, 4);

        assert_eq!(controller.drag_scroll_direction(1, 3), None);
    }

    #[test]
    fn test_selection_controller_drag_autoscroll_down_captures_above_and_shifts_anchor() {
        let mut canvas = Canvas::new(6, 4);
        canvas
            .subview_mut(0, 0, 0, 0, 6, 4)
            .set_text(0, 1, "row1", CanvasTextStyle::default());
        canvas
            .subview_mut(0, 0, 0, 0, 6, 4)
            .set_text(0, 2, "row2", CanvasTextStyle::default());
        let mut controller = SelectionController::new();
        controller.selection_mut().start(0, 1);
        controller.selection_mut().update(3, 4);

        assert_eq!(
            controller.drag_scroll_direction(1, 3),
            Some(SelectionDragScrollDirection::Down)
        );
        let outcome = controller.translate_for_drag_autoscroll(
            &canvas,
            SelectionDragScrollDirection::Down,
            1,
            1,
            3,
        );

        assert!(outcome.translated);
        assert_eq!(
            controller.selection().anchor(),
            Some(SelectionPoint { col: 0, row: 1 })
        );
        assert_eq!(controller.selection.virtual_anchor_row, Some(0));
        assert_eq!(
            controller.selection.scrolled_off_above,
            vec!["row1".to_string()]
        );
    }

    #[test]
    fn test_selection_controller_drag_autoscroll_up_captures_below_and_shifts_anchor() {
        let mut canvas = Canvas::new(6, 4);
        canvas
            .subview_mut(0, 0, 0, 0, 6, 4)
            .set_text(0, 2, "row2", CanvasTextStyle::default());
        canvas
            .subview_mut(0, 0, 0, 0, 6, 4)
            .set_text(0, 3, "row3", CanvasTextStyle::default());
        let mut controller = SelectionController::new();
        controller.selection_mut().start(0, 3);
        controller.selection_mut().update(3, 0);

        assert_eq!(
            controller.drag_scroll_direction(1, 3),
            Some(SelectionDragScrollDirection::Up)
        );
        controller.translate_for_drag_autoscroll(
            &canvas,
            SelectionDragScrollDirection::Up,
            1,
            1,
            3,
        );

        assert_eq!(
            controller.selection().anchor(),
            Some(SelectionPoint { col: 5, row: 3 })
        );
        assert_eq!(controller.selection.virtual_anchor_row, Some(4));
        assert_eq!(
            controller.selection.scrolled_off_below,
            vec!["r".to_string()],
            "anchor-side column constraint is applied before capture then reset for future rows"
        );
    }

    #[test]
    fn test_selection_controller_drag_scroll_blocked_reversal_clears_captures() {
        let mut canvas = Canvas::new(6, 4);
        canvas
            .subview_mut(0, 0, 0, 0, 6, 4)
            .set_text(0, 1, "row1", CanvasTextStyle::default());
        let mut controller = SelectionController::new();
        controller.selection_mut().start(0, 1);
        controller.selection_mut().update(3, 4);
        assert_eq!(
            controller.drag_scroll_direction(1, 3),
            Some(SelectionDragScrollDirection::Down)
        );
        controller.translate_for_drag_autoscroll(
            &canvas,
            SelectionDragScrollDirection::Down,
            1,
            1,
            3,
        );

        controller.selection_mut().update(3, 0);
        assert_eq!(controller.drag_scroll_direction(1, 3), None);
        assert!(controller.selection().captured_rows_empty());
        assert_eq!(controller.last_drag_scroll_dir, None);
    }

    #[test]
    fn test_selection_controller_follow_scroll_drag_shifts_anchor_only() {
        let mut canvas = Canvas::new(6, 3);
        canvas
            .subview_mut(0, 0, 0, 0, 6, 3)
            .set_text(0, 0, "row0", CanvasTextStyle::default());
        canvas
            .subview_mut(0, 0, 0, 0, 6, 3)
            .set_text(0, 1, "row1", CanvasTextStyle::default());
        canvas
            .subview_mut(0, 0, 0, 0, 6, 3)
            .set_text(0, 2, "row2", CanvasTextStyle::default());
        let mut controller = SelectionController::new();
        controller.selection_mut().start(0, 0);
        controller.selection_mut().update(3, 2);

        let outcome = controller.translate_for_follow_scroll(&canvas, 1, 0, 2);

        assert_eq!(
            outcome,
            SelectionScrollOutcome {
                translated: true,
                cleared: false,
            }
        );
        assert_eq!(
            controller.selection().anchor(),
            Some(SelectionPoint { col: 0, row: 0 })
        );
        assert_eq!(
            controller.selection().focus(),
            Some(SelectionPoint { col: 3, row: 2 })
        );
        assert_eq!(controller.selection.virtual_anchor_row, Some(-1));
        assert_eq!(
            controller.selection.scrolled_off_above,
            vec!["row0".to_string()]
        );
    }

    #[test]
    fn test_selection_controller_follow_scroll_released_shifts_both_and_preserves_text() {
        let mut canvas = Canvas::new(6, 3);
        canvas
            .subview_mut(0, 0, 0, 0, 6, 3)
            .set_text(0, 0, "row0", CanvasTextStyle::default());
        canvas
            .subview_mut(0, 0, 0, 0, 6, 3)
            .set_text(0, 1, "row1", CanvasTextStyle::default());
        canvas
            .subview_mut(0, 0, 0, 0, 6, 3)
            .set_text(0, 2, "row2", CanvasTextStyle::default());
        let mut controller = SelectionController::new();
        controller.selection_mut().start(0, 0);
        controller.selection_mut().update(3, 2);
        controller.selection_mut().finish();

        let outcome = controller.translate_for_follow_scroll(&canvas, 1, 0, 2);
        canvas.shift_rows(0, 2, 1);

        assert!(outcome.translated);
        assert!(!outcome.cleared);
        assert_eq!(controller.selected_text(&canvas), "row0\nrow1\nrow2");
    }

    #[test]
    fn test_selection_controller_follow_scroll_clears_when_selection_leaves_top() {
        let mut canvas = Canvas::new(6, 3);
        canvas
            .subview_mut(0, 0, 0, 0, 6, 3)
            .set_text(0, 0, "row0", CanvasTextStyle::default());
        canvas
            .subview_mut(0, 0, 0, 0, 6, 3)
            .set_text(0, 1, "row1", CanvasTextStyle::default());
        let mut controller = SelectionController::new();
        controller.selection_mut().start(0, 0);
        controller.selection_mut().update(3, 1);
        controller.selection_mut().finish();

        let outcome = controller.translate_for_follow_scroll(&canvas, 2, 0, 2);

        assert_eq!(
            outcome,
            SelectionScrollOutcome {
                translated: true,
                cleared: true,
            }
        );
        assert!(!controller.has_selection());
        assert!(controller.selection().captured_rows_empty());
    }

    #[test]
    fn test_selection_controller_follow_scroll_ignores_static_focus_endpoint() {
        let mut canvas = Canvas::new(6, 4);
        canvas
            .subview_mut(0, 0, 0, 0, 6, 4)
            .set_text(0, 1, "row1", CanvasTextStyle::default());
        canvas
            .subview_mut(0, 0, 0, 0, 6, 4)
            .set_text(0, 3, "foot", CanvasTextStyle::default());
        let mut controller = SelectionController::new();
        controller.selection_mut().start(0, 1);
        controller.selection_mut().update(3, 3);
        controller.selection_mut().finish();

        let outcome = controller.translate_for_follow_scroll(&canvas, 1, 1, 2);

        assert_eq!(outcome, SelectionScrollOutcome::default());
        assert_eq!(
            controller.selection().anchor(),
            Some(SelectionPoint { col: 0, row: 1 })
        );
        assert_eq!(
            controller.selection().focus(),
            Some(SelectionPoint { col: 3, row: 3 })
        );
    }

    #[test]
    fn test_selection_controller_scroll_jump_down_preserves_copied_text() {
        let mut canvas = Canvas::new(6, 3);
        canvas
            .subview_mut(0, 0, 0, 0, 6, 3)
            .set_text(0, 0, "row0", CanvasTextStyle::default());
        canvas
            .subview_mut(0, 0, 0, 0, 6, 3)
            .set_text(0, 1, "row1", CanvasTextStyle::default());
        canvas
            .subview_mut(0, 0, 0, 0, 6, 3)
            .set_text(0, 2, "row2", CanvasTextStyle::default());
        let mut controller = SelectionController::new();
        controller.selection_mut().start(0, 0);
        controller.selection_mut().update(3, 2);

        let outcome = controller.translate_for_scroll_jump(&canvas, 1, 0, 2);
        canvas.shift_rows(0, 2, 1);

        assert_eq!(
            outcome,
            SelectionScrollOutcome {
                translated: true,
                cleared: false,
            }
        );
        assert_eq!(controller.selected_text(&canvas), "row0\nrow1\nrow2");
    }

    #[test]
    fn test_selection_controller_scroll_jump_up_preserves_copied_text() {
        let mut canvas = Canvas::new(6, 3);
        canvas
            .subview_mut(0, 0, 0, 0, 6, 3)
            .set_text(0, 0, "row0", CanvasTextStyle::default());
        canvas
            .subview_mut(0, 0, 0, 0, 6, 3)
            .set_text(0, 1, "row1", CanvasTextStyle::default());
        canvas
            .subview_mut(0, 0, 0, 0, 6, 3)
            .set_text(0, 2, "row2", CanvasTextStyle::default());
        let mut controller = SelectionController::new();
        controller.selection_mut().start(0, 0);
        controller.selection_mut().update(3, 2);

        let outcome = controller.translate_for_scroll_jump(&canvas, -1, 0, 2);
        canvas.shift_rows(0, 2, -1);

        assert!(outcome.translated);
        assert!(!outcome.cleared);
        assert_eq!(controller.selected_text(&canvas), "row0\nrow1\nrow2");
    }

    #[test]
    fn test_selection_controller_scroll_jump_ignores_static_endpoint() {
        let mut canvas = Canvas::new(6, 4);
        canvas
            .subview_mut(0, 0, 0, 0, 6, 4)
            .set_text(0, 1, "row1", CanvasTextStyle::default());
        canvas
            .subview_mut(0, 0, 0, 0, 6, 4)
            .set_text(0, 3, "foot", CanvasTextStyle::default());
        let mut controller = SelectionController::new();
        controller.selection_mut().start(0, 1);
        controller.selection_mut().update(3, 3);

        let outcome = controller.translate_for_scroll_jump(&canvas, 1, 1, 2);

        assert_eq!(outcome, SelectionScrollOutcome::default());
        assert_eq!(
            controller.selection().anchor(),
            Some(SelectionPoint { col: 0, row: 1 })
        );
        assert_eq!(
            controller.selection().focus(),
            Some(SelectionPoint { col: 3, row: 3 })
        );
    }

    #[test]
    fn test_selection_controller_new_press_finishes_lost_release() {
        let mut canvas = Canvas::new(8, 1);
        canvas
            .subview_mut(0, 0, 0, 0, 8, 1)
            .set_text(0, 0, "abcdef", CanvasTextStyle::default());
        let mut controller = SelectionController::new();

        controller.handle_left_press(&canvas, 1, 0, 1_000, false);
        controller.handle_drag(&canvas, 3, 0);
        let outcome = controller.handle_left_press(&canvas, 5, 0, 1_100, false);
        assert!(outcome.finished_previous_drag);
        assert_eq!(outcome.kind, SelectionMousePressKind::Single);
        assert_eq!(
            controller.selection().anchor(),
            Some(SelectionPoint { col: 5, row: 0 })
        );
        assert!(!controller.has_selection());
    }

    #[test]
    fn test_selection_controller_release_classifies_single_click_and_link() {
        let mut canvas = Canvas::new(16, 1);
        canvas.subview_mut(0, 0, 0, 0, 16, 1).set_text_with_link(
            0,
            0,
            "link",
            CanvasTextStyle::default(),
            Some("https://example.com"),
        );
        let mut controller = SelectionController::new();

        controller.handle_left_press(&canvas, 1, 0, 1_000, false);
        let outcome = controller.handle_release_at(&canvas, 1, 0, false);

        assert!(outcome.was_dragging);
        assert_eq!(outcome.click, Some(SelectionPoint { col: 1, row: 0 }));
        assert_eq!(outcome.hyperlink.as_deref(), Some("https://example.com"));
        assert!(!controller.has_selection());
    }

    #[test]
    fn test_selection_controller_release_suppresses_link_when_click_consumed_or_selected() {
        let mut canvas = Canvas::new(16, 1);
        canvas.subview_mut(0, 0, 0, 0, 16, 1).set_text_with_link(
            0,
            0,
            "linktext",
            CanvasTextStyle::default(),
            Some("https://example.com"),
        );
        let mut controller = SelectionController::new();

        controller.handle_left_press(&canvas, 1, 0, 1_000, false);
        let consumed = controller.handle_release_at(&canvas, 1, 0, true);
        assert_eq!(consumed.click, Some(SelectionPoint { col: 1, row: 0 }));
        assert_eq!(consumed.hyperlink, None);

        controller.handle_left_press(&canvas, 1, 0, 2_000, false);
        controller.handle_drag(&canvas, 3, 0);
        let selected = controller.handle_release_at(&canvas, 3, 0, false);
        assert_eq!(selected.click, None);
        assert_eq!(selected.hyperlink, None);
        assert!(controller.has_selection());
    }

    #[test]
    fn test_selection_state_tracks_alt_press_marker() {
        let mut selection = SelectionState::new();
        selection.start_with_alt(1, 0, true);
        assert!(selection.last_press_had_alt());
        selection.set_last_press_had_alt(false);
        assert!(!selection.last_press_had_alt());
        selection.clear();
        assert!(!selection.last_press_had_alt());
    }

    #[test]
    fn test_selection_state_multi_click_falls_back_to_anchor_on_no_select() {
        let mut canvas = Canvas::new(8, 1);
        canvas
            .subview_mut(0, 0, 0, 0, 8, 1)
            .set_text(0, 0, " gutter", CanvasTextStyle::default());
        canvas.mark_no_select_region(0, 0, 3, 1);

        let mut selection = SelectionState::new();
        selection.start_multi_click(&canvas, 1, 0, SelectionClickCount::Double);

        assert!(selection.has_selection());
        assert_eq!(selection.anchor(), Some(SelectionPoint { col: 1, row: 0 }));
        assert_eq!(selection.focus(), Some(SelectionPoint { col: 1, row: 0 }));
        assert!(selection.anchor_span.is_none());
    }

    #[test]
    fn test_selection_state_multi_click_selects_word_or_line() {
        let mut canvas = Canvas::new(12, 2);
        canvas
            .subview_mut(0, 0, 0, 0, 12, 2)
            .set_text(0, 0, "one two", CanvasTextStyle::default());
        canvas
            .subview_mut(0, 0, 0, 0, 12, 2)
            .set_text(0, 1, "three", CanvasTextStyle::default());

        let mut selection = SelectionState::new();
        selection.start_multi_click(&canvas, 5, 0, SelectionClickCount::Double);
        assert_eq!(selection.selected_text(&canvas), "two");

        selection.start_multi_click(&canvas, 0, 1, SelectionClickCount::Triple);
        assert_eq!(selection.selected_text(&canvas), "three");
    }

    #[test]
    fn test_selection_state_captures_scrolled_soft_wrap_rows() {
        let mut canvas = Canvas::new(6, 2);
        canvas
            .subview_mut(0, 0, 0, 0, 6, 2)
            .set_text(0, 0, "hello", CanvasTextStyle::default());
        canvas
            .subview_mut(0, 0, 0, 0, 6, 2)
            .set_text(0, 1, "world", CanvasTextStyle::default());
        canvas.mark_soft_wrap_continuation(1, 6);

        let mut selection = SelectionState::new();
        selection.start(0, 0);
        selection.update(5, 1);
        selection.capture_scrolled_rows(&canvas, 0, 0, SelectionCaptureSide::Above);
        canvas.shift_rows(0, 1, 1);
        selection.shift_rows(-1, 0, 1, canvas.width());

        assert_eq!(
            selection.selected_text(&canvas),
            "hello world",
            "captured rows and shifted soft-wrap metadata should copy as one logical line"
        );
    }

    #[test]
    fn test_selection_state_capture_resets_anchor_span_cols_above() {
        let mut canvas = Canvas::new(10, 2);
        canvas
            .subview_mut(0, 0, 0, 0, 10, 2)
            .set_text(0, 0, "one two", CanvasTextStyle::default());
        canvas
            .subview_mut(0, 0, 0, 0, 10, 2)
            .set_text(0, 1, "three", CanvasTextStyle::default());

        let mut selection = SelectionState::new();
        assert!(selection.select_word_at(&canvas, 4, 0));
        selection.extend_span_selection(&canvas, 2, 1);
        selection.capture_scrolled_rows(&canvas, 0, 0, SelectionCaptureSide::Above);

        let span = selection
            .anchor_span
            .expect("word span should remain active");
        assert_eq!(selection.anchor().unwrap().col, 0);
        assert_eq!(span.lo.col, 0);
        assert_eq!(span.hi.col, canvas.width() - 1);
    }

    #[test]
    fn test_selection_state_capture_resets_anchor_span_cols_below() {
        let mut canvas = Canvas::new(10, 2);
        canvas
            .subview_mut(0, 0, 0, 0, 10, 2)
            .set_text(0, 0, "one", CanvasTextStyle::default());
        canvas.subview_mut(0, 0, 0, 0, 10, 2).set_text(
            0,
            1,
            "two three",
            CanvasTextStyle::default(),
        );

        let mut selection = SelectionState::new();
        assert!(selection.select_word_at(&canvas, 4, 1));
        selection.extend_span_selection(&canvas, 1, 0);
        selection.capture_scrolled_rows(&canvas, 1, 1, SelectionCaptureSide::Below);

        let span = selection
            .anchor_span
            .expect("word span should remain active");
        assert_eq!(selection.anchor().unwrap().col, canvas.width() - 1);
        assert_eq!(span.lo.col, 0);
        assert_eq!(span.hi.col, canvas.width() - 1);
    }

    #[test]
    fn test_selection_state_select_word_matches_terminal_classes() {
        let mut canvas = Canvas::new(24, 1);
        canvas.subview_mut(0, 0, 0, 0, 24, 1).set_text(
            0,
            0,
            "run /usr/bin/bash ok",
            CanvasTextStyle::default(),
        );

        let mut selection = SelectionState::new();
        assert!(selection.select_word_at(&canvas, 5, 0));
        assert_eq!(selection.selected_text(&canvas), "/usr/bin/bash");
    }

    #[test]
    fn test_selection_state_select_word_steps_from_wide_tail() {
        let mut canvas = Canvas::new(6, 1);
        canvas
            .subview_mut(0, 0, 0, 0, 6, 1)
            .set_text(0, 0, "中a!", CanvasTextStyle::default());

        let mut selection = SelectionState::new();
        assert!(selection.select_word_at(&canvas, 1, 0));
        assert_eq!(selection.selected_text(&canvas), "中a");
    }

    #[test]
    fn test_selection_state_select_line_uses_no_select_copy_filter() {
        let mut canvas = Canvas::new(10, 1);
        canvas.subview_mut(0, 0, 0, 0, 10, 1).set_text(
            0,
            0,
            " 42 code",
            CanvasTextStyle::default(),
        );
        canvas.mark_no_select_region(0, 0, 4, 1);

        let mut selection = SelectionState::new();
        assert!(selection.select_line_at(&canvas, 0));
        assert_eq!(selection.selected_text(&canvas), "code");
    }

    #[test]
    fn test_selection_state_extends_word_span_forward_and_backward() {
        let mut canvas = Canvas::new(16, 1);
        canvas.subview_mut(0, 0, 0, 0, 16, 1).set_text(
            0,
            0,
            "one two three",
            CanvasTextStyle::default(),
        );

        let mut selection = SelectionState::new();
        assert!(selection.select_word_at(&canvas, 4, 0));
        selection.extend_span_selection(&canvas, 10, 0);
        assert_eq!(selection.selected_text(&canvas), "two three");

        assert!(selection.select_word_at(&canvas, 4, 0));
        selection.extend_span_selection(&canvas, 1, 0);
        assert_eq!(selection.selected_text(&canvas), "one two");
    }

    #[test]
    fn test_selection_state_keyboard_focus_moves_wrap_and_clamp() {
        let mut selection = SelectionState::new();
        selection.start(1, 1);
        selection.update(0, 1);

        assert!(selection.move_focus_by(SelectionFocusMove::Left, 4, 3));
        assert_eq!(selection.focus(), Some(SelectionPoint { col: 3, row: 0 }));
        assert!(selection.move_focus_by(SelectionFocusMove::Right, 4, 3));
        assert_eq!(selection.focus(), Some(SelectionPoint { col: 0, row: 1 }));
        assert!(selection.move_focus_by(SelectionFocusMove::LineEnd, 4, 3));
        assert_eq!(selection.focus(), Some(SelectionPoint { col: 3, row: 1 }));
        assert!(selection.move_focus_by(SelectionFocusMove::Down, 4, 3));
        assert_eq!(selection.focus(), Some(SelectionPoint { col: 3, row: 2 }));
        assert!(!selection.move_focus_by(SelectionFocusMove::Down, 4, 3));
        assert!(selection.move_focus_by(SelectionFocusMove::Up, 4, 3));
        assert_eq!(selection.focus(), Some(SelectionPoint { col: 3, row: 1 }));
        assert!(selection.move_focus_by(SelectionFocusMove::LineStart, 4, 3));
        assert_eq!(selection.focus(), Some(SelectionPoint { col: 0, row: 1 }));
    }

    #[test]
    fn test_selection_state_keyboard_focus_drops_word_span() {
        let mut canvas = Canvas::new(12, 1);
        canvas
            .subview_mut(0, 0, 0, 0, 12, 1)
            .set_text(0, 0, "one two", CanvasTextStyle::default());

        let mut selection = SelectionState::new();
        assert!(selection.select_word_at(&canvas, 4, 0));
        assert!(selection.anchor_span.is_some());
        assert!(selection.move_focus_by(
            SelectionFocusMove::Right,
            canvas.width(),
            canvas.height()
        ));
        assert!(selection.anchor_span.is_none());
    }

    #[test]
    fn test_selection_state_shift_rows_uses_virtual_rows_and_trims_capture_debt() {
        let mut canvas = Canvas::new(6, 3);
        canvas
            .subview_mut(0, 0, 0, 0, 6, 3)
            .set_text(0, 0, "row0", CanvasTextStyle::default());
        canvas
            .subview_mut(0, 0, 0, 0, 6, 3)
            .set_text(0, 1, "row1", CanvasTextStyle::default());
        canvas
            .subview_mut(0, 0, 0, 0, 6, 3)
            .set_text(0, 2, "row2", CanvasTextStyle::default());

        let mut selection = SelectionState::new();
        selection.start(0, 0);
        selection.update(3, 2);
        selection.capture_scrolled_rows(&canvas, 0, 0, SelectionCaptureSide::Above);
        selection.shift_rows(-1, 0, 2, canvas.width());
        assert_eq!(selection.virtual_anchor_row, Some(-1));
        assert_eq!(selection.scrolled_off_above.len(), 1);

        selection.shift_rows(1, 0, 2, canvas.width());
        assert_eq!(selection.virtual_anchor_row, None);
        assert!(
            selection.scrolled_off_above.is_empty(),
            "reverse scroll should drop rows whose virtual debt returned on-screen"
        );
    }

    #[test]
    fn test_selection_state_shift_anchor_and_follow_track_virtual_rows() {
        let mut selection = SelectionState::new();
        selection.start(1, 1);
        selection.update(2, 2);

        selection.shift_anchor(-2, 0, 2);
        assert_eq!(selection.anchor(), Some(SelectionPoint { col: 1, row: 0 }));
        assert_eq!(selection.focus(), Some(SelectionPoint { col: 2, row: 2 }));
        assert_eq!(selection.virtual_anchor_row, Some(-1));

        assert!(!selection.shift_for_follow(1, 0, 2));
        assert_eq!(selection.virtual_anchor_row, None);
        assert_eq!(selection.anchor(), Some(SelectionPoint { col: 1, row: 0 }));
        assert_eq!(selection.focus(), Some(SelectionPoint { col: 2, row: 2 }));
    }

    #[test]
    fn test_selected_text_respects_no_select_and_wide_tails() {
        let mut canvas = Canvas::new(12, 2);
        canvas.subview_mut(0, 0, 0, 0, 12, 2).set_text(
            0,
            0,
            " 42 +中x  ",
            CanvasTextStyle::default(),
        );
        canvas.mark_no_select_region(0, 0, 5, 1);

        let selected = canvas.selected_text(SelectionRange::new(
            SelectionPoint { col: 0, row: 0 },
            SelectionPoint { col: 9, row: 0 },
        ));

        assert_eq!(
            selected, "中x",
            "selection copy should skip noSelect gutter cells, wide-char tails, and trailing blanks"
        );
    }

    #[test]
    fn test_apply_selection_overlay_skips_no_select_and_marks_damage() {
        let mut canvas = Canvas::new(6, 1);
        canvas
            .subview_mut(0, 0, 0, 0, 6, 1)
            .set_text(0, 0, "abcdef", CanvasTextStyle::default());
        canvas.mark_no_select_region(0, 0, 2, 1);

        let applied = canvas.apply_selection_overlay(
            SelectionRange::new(
                SelectionPoint { col: 0, row: 0 },
                SelectionPoint { col: 3, row: 0 },
            ),
            StyleOverlay {
                invert: Some(true),
                ..Default::default()
            },
        );

        assert!(applied);

        assert!(!canvas.resolved_text_style(0, 0).unwrap().invert);
        assert!(!canvas.resolved_text_style(1, 0).unwrap().invert);
        assert!(canvas.resolved_text_style(2, 0).unwrap().invert);
        assert!(canvas.resolved_text_style(3, 0).unwrap().invert);
        assert_eq!(
            canvas.damage_region(),
            Some(DamageRegion {
                x: 2,
                y: 0,
                width: 2,
                height: 1,
            }),
            "selection overlay should damage only selectable overlaid cells"
        );
    }

    #[test]
    fn test_selection_state_contains_and_apply_overlay() {
        let mut canvas = Canvas::new(6, 1);
        canvas
            .subview_mut(0, 0, 0, 0, 6, 1)
            .set_text(0, 0, "abcdef", CanvasTextStyle::default());
        let mut selection = SelectionState::new();
        selection.start(1, 0);
        selection.update(3, 0);

        assert!(!selection.is_cell_selected(0, 0));
        assert!(selection.is_cell_selected(2, 0));
        assert!(selection.apply_overlay(
            &mut canvas,
            StyleOverlay {
                invert: Some(true),
                ..Default::default()
            },
        ));
        assert!(!canvas.resolved_text_style(0, 0).unwrap().invert);
        assert!(canvas.resolved_text_style(1, 0).unwrap().invert);
        assert!(canvas.resolved_text_style(3, 0).unwrap().invert);
        assert_eq!(
            canvas.damage_region(),
            Some(DamageRegion {
                x: 1,
                y: 0,
                width: 3,
                height: 1,
            })
        );
    }

    #[test]
    fn test_scan_text_positions_is_case_insensitive_and_wide_aware() {
        let mut canvas = Canvas::new(12, 1);
        canvas.subview_mut(0, 0, 0, 0, 12, 1).set_text(
            0,
            0,
            "ab中c AB",
            CanvasTextStyle::default(),
        );

        assert_eq!(
            canvas.scan_text_positions("中c"),
            vec![TextMatchPosition {
                row: 0,
                col: 2,
                len: 3,
            }],
            "match spans should be measured in terminal cells, including wide tails"
        );
        assert_eq!(
            canvas.scan_text_positions("ab"),
            vec![
                TextMatchPosition {
                    row: 0,
                    col: 0,
                    len: 2,
                },
                TextMatchPosition {
                    row: 0,
                    col: 6,
                    len: 2,
                },
            ],
            "search should be case-insensitive and non-overlapping"
        );
    }

    #[test]
    fn test_scan_text_positions_region_returns_subtree_relative_positions() {
        let mut canvas = Canvas::new(16, 3);
        canvas.subview_mut(0, 0, 0, 0, 16, 3).set_text(
            2,
            1,
            "xx lazy 中c",
            CanvasTextStyle::default(),
        );
        canvas.mark_no_select_region(5, 1, 4, 1);

        assert_eq!(
            canvas.scan_text_positions_region(5, 1, 10, 1, "lazy"),
            Vec::<TextMatchPosition>::new(),
            "region scanning should respect noSelect metadata"
        );
        assert_eq!(
            canvas.scan_text_positions_region(8, 1, 8, 1, "中c"),
            vec![TextMatchPosition {
                row: 0,
                col: 2,
                len: 3,
            }],
            "positions should be relative to the scanned region and wide-aware"
        );
    }

    #[test]
    fn test_apply_search_highlight_skips_no_select_and_marks_damage() {
        let mut canvas = Canvas::new(12, 1);
        canvas
            .subview_mut(0, 0, 0, 0, 12, 1)
            .set_text(0, 0, "foo foo", CanvasTextStyle::default());
        canvas.mark_no_select_region(0, 0, 3, 1);

        let applied = canvas.apply_search_highlight(
            "foo",
            StyleOverlay {
                invert: Some(true),
                ..Default::default()
            },
        );

        assert!(applied);
        for col in 0..3 {
            assert!(!canvas.resolved_text_style(col, 0).unwrap().invert);
        }
        for col in 4..7 {
            assert!(canvas.resolved_text_style(col, 0).unwrap().invert);
        }
        assert_eq!(
            canvas.damage_region(),
            Some(DamageRegion {
                x: 4,
                y: 0,
                width: 3,
                height: 1,
            }),
            "search highlight should damage only highlighted selectable cells"
        );
    }

    #[test]
    fn test_apply_positioned_highlight_translates_and_clips_row_offset() {
        let mut canvas = Canvas::new(8, 2);
        canvas
            .subview_mut(0, 0, 0, 0, 8, 2)
            .set_text(0, 1, "target", CanvasTextStyle::default());
        let positions = vec![TextMatchPosition {
            row: 0,
            col: 0,
            len: 6,
        }];

        assert!(!canvas.apply_positioned_highlight(
            &positions,
            -1,
            0,
            StyleOverlay {
                invert: Some(true),
                ..Default::default()
            },
        ));
        assert!(canvas.apply_positioned_highlight(
            &positions,
            1,
            0,
            StyleOverlay {
                invert: Some(true),
                ..Default::default()
            },
        ));
        assert!(canvas.resolved_text_style(0, 1).unwrap().invert);
        assert!(canvas.resolved_text_style(5, 1).unwrap().invert);
    }

    // ----- P1-5: wide character / grapheme cluster tests -----

    #[test]
    fn test_grapheme_width() {
        assert_eq!(grapheme_width("a"), 1);
        assert_eq!(grapheme_width("中"), 2);
        assert_eq!(grapheme_width("☀\u{fe0f}"), 2); // sun + VS16
        assert_eq!(grapheme_width("1\u{fe0f}"), 1); // incomplete keycap + VS16
        assert_eq!(grapheme_width("1\u{fe0f}\u{20e3}"), 2); // complete keycap
        assert_eq!(grapheme_width("e\u{0301}"), 1); // e + combining acute accent
        assert_eq!(grapheme_width("\u{0301}"), 0); // combining acute accent
        assert_eq!(grapheme_width("\u{200d}"), 0); // ZWJ
        assert_eq!(grapheme_width("\u{0600}"), 0); // Arabic number sign formatting control
    }

    #[test]
    fn test_string_display_width() {
        assert_eq!(string_display_width("hello"), 5);
        assert_eq!(string_display_width("中文"), 4);
        assert_eq!(string_display_width("☀\u{fe0f}☀\u{fe0f}"), 4);
        assert_eq!(string_display_width("ab中c"), 5);
        assert_eq!(string_display_width("a\tb"), 9);
        assert_eq!(string_display_width_from_col("\tb", 2), 7);
        assert_eq!(string_display_width("a\x08b\x07c"), 3);
        assert_eq!(string_display_width("\x1b[31mred\x1b[0m"), 3);
        assert_eq!(string_display_width("a\x1b]0;title\x07b"), 2);
        assert_eq!(string_display_width("1\u{fe0f}"), 1);
        assert_eq!(string_display_width("a\u{7f}b\u{80}c"), 3);
        assert_eq!(string_display_width("a\u{0600}b\u{06dd}c"), 3);
    }

    #[test]
    fn test_expand_tabs_matches_cc_ink_tabstops() {
        assert_eq!(expand_tabs("plain"), "plain");
        assert_eq!(expand_tabs("a\tb"), "a       b");
        assert_eq!(expand_tabs("中\tb"), "中      b");
        assert_eq!(expand_tabs("a\n\tb"), "a\n        b");
        assert_eq!(
            expand_tabs("\x1b[31ma\tb\x1b[0m"),
            "\x1b[31ma       b\x1b[0m"
        );
        assert_eq!(expand_tabs_with_interval("a\tb", 4), "a   b");
    }

    #[test]
    fn test_line_width_and_widest_line_match_cc_ink_helpers() {
        assert_eq!(line_width("hello"), 5);
        assert_eq!(line_width("a\tb"), 9);
        assert_eq!(line_width("\x1b[31mred\x1b[0m"), 3);
        assert_eq!(widest_line(""), 0);
        assert_eq!(widest_line("abc\n中中\n"), 4);
        assert_eq!(widest_line("short\nlonger"), 6);
    }

    #[test]
    fn test_measure_text_matches_cc_ink_measure_text_semantics() {
        assert_eq!(
            measure_text("", Some(10)),
            TextMeasurement {
                width: 0,
                height: 0
            }
        );
        assert_eq!(
            measure_text("abc\n", None),
            TextMeasurement {
                width: 3,
                height: 2,
            }
        );
        assert_eq!(
            measure_text("abcdef", Some(4)),
            TextMeasurement {
                width: 6,
                height: 2,
            }
        );
        assert_eq!(
            measure_text("中中a", Some(3)),
            TextMeasurement {
                width: 5,
                height: 2,
            }
        );
        assert_eq!(
            measure_text("a\tb\x1b[31mred\x1b[0m", Some(5)),
            TextMeasurement {
                width: 12,
                height: 3,
            }
        );
        assert_eq!(
            measure_text("abcdef", Some(0)),
            TextMeasurement {
                width: 6,
                height: 1,
            }
        );
    }

    #[test]
    fn test_canvas_text_expands_tabs_to_terminal_tab_stops() {
        let mut canvas = Canvas::new(10, 1);
        canvas
            .subview_mut(0, 0, 0, 0, 10, 1)
            .set_text(0, 0, "a\tb", CanvasTextStyle::default());
        assert_eq!(canvas.to_string(), "a       b\n");
        assert_eq!(canvas.cell(1, 0).unwrap().text(), Some(" "));
        assert_eq!(canvas.cell(8, 0).unwrap().text(), Some("b"));

        let mut canvas = Canvas::new(10, 1);
        canvas
            .subview_mut(0, 0, 0, 0, 10, 1)
            .set_text(2, 0, "\tb", CanvasTextStyle::default());
        assert_eq!(canvas.to_string(), "        b\n");
        assert_eq!(canvas.cell(2, 0).unwrap().text(), Some(" "));
        assert_eq!(canvas.cell(8, 0).unwrap().text(), Some("b"));
    }

    #[test]
    fn test_canvas_text_preserves_composed_graphemes_and_skips_zero_width_controls() {
        let mut canvas = Canvas::new(20, 1);
        canvas.subview_mut(0, 0, 0, 0, 20, 1).set_text(
            0,
            0,
            "\u{0301}e\u{0301}a\u{7f}b\u{80}c\u{0600}",
            CanvasTextStyle::default(),
        );

        assert_eq!(canvas.get_text(0, 0, 20, 1), "e\u{0301}abc");
        assert_eq!(canvas.ansi_row_rendered_width(0), 4);
    }

    #[test]
    fn test_canvas_text_skips_c0_controls_like_cc_ink_output() {
        let mut canvas = Canvas::new(20, 1);
        canvas.subview_mut(0, 0, 0, 0, 20, 1).set_text(
            0,
            0,
            "a\x08b\x07c\rd\n",
            CanvasTextStyle::default(),
        );

        assert_eq!(canvas.get_text(0, 0, 20, 1), "abcd");
        assert_eq!(canvas.ansi_row_rendered_width(0), 4);
    }

    #[test]
    fn test_canvas_text_skips_escape_sequences_like_cc_ink_output() {
        let mut canvas = Canvas::new(20, 1);
        canvas.subview_mut(0, 0, 0, 0, 20, 1).set_text(
            0,
            0,
            "a\x1b[31mb\x1b[0mc\x1b]0;title\x07d\x1bPpayload\x1b\\e\x1b(0f",
            CanvasTextStyle::default(),
        );

        assert_eq!(canvas.get_text(0, 0, 20, 1), "abcdef");
        assert_eq!(canvas.ansi_row_rendered_width(0), 6);
    }

    #[test]
    fn test_cjk_get_text_no_phantom_space() {
        let mut canvas = Canvas::new(20, 1);
        canvas
            .subview_mut(0, 0, 0, 0, 20, 1)
            .set_text(0, 0, "中文abc", CanvasTextStyle::default());
        assert_eq!(canvas.get_text(0, 0, 20, 1), "中文abc");
    }

    #[test]
    fn test_wide_cell_width_marks() {
        let mut canvas = Canvas::new(10, 1);
        canvas
            .subview_mut(0, 0, 0, 0, 10, 1)
            .set_text(0, 0, "中a", CanvasTextStyle::default());
        assert_eq!(canvas.cells[0][0].cell_width, CellWidth::Wide);
        assert_eq!(canvas.cells[0][1].cell_width, CellWidth::WidthTail);
        assert_eq!(canvas.cells[0][2].cell_width, CellWidth::Normal);
    }

    #[test]
    fn test_wide_character_at_right_edge_is_clipped() {
        let mut canvas = Canvas::new(10, 1);
        canvas
            .subview_mut(0, 0, 0, 0, 10, 1)
            .set_text(9, 0, "中", CanvasTextStyle::default());

        assert_eq!(canvas.get_text(0, 0, 10, 1), "");
        assert_eq!(canvas.ansi_row_rendered_width(0), 0);
        assert_eq!(canvas.cells[0][9].cell_width, CellWidth::SpacerHead);

        let mut actual = Vec::new();
        canvas
            .write_ansi_row_without_newline(0, &mut actual)
            .unwrap();
        let rendered = String::from_utf8_lossy(&actual);
        assert!(
            !rendered.contains('中'),
            "wide glyph crossing the right margin must not be written: {rendered:?}"
        );
    }

    #[test]
    fn test_wide_character_at_right_edge_marks_spacer_head_and_clears_existing_cell() {
        let mut canvas = Canvas::new(10, 1);
        {
            let mut sv = canvas.subview_mut(0, 0, 0, 0, 10, 1);
            sv.set_text(9, 0, "x", CanvasTextStyle::default());
            sv.set_text(9, 0, "中", CanvasTextStyle::default());
        }

        assert_eq!(canvas.get_text(0, 0, 10, 1), "");
        assert_eq!(canvas.cells[0][9].cell_width, CellWidth::SpacerHead);
        assert!(canvas.cells[0][9].character.is_none());
    }

    #[test]
    fn test_overwriting_spacer_head_restores_normal_cell() {
        let mut canvas = Canvas::new(10, 1);
        {
            let mut sv = canvas.subview_mut(0, 0, 0, 0, 10, 1);
            sv.set_text(9, 0, "中", CanvasTextStyle::default());
            sv.set_text(9, 0, "x", CanvasTextStyle::default());
        }

        assert_eq!(canvas.get_text(0, 0, 10, 1), "         x");
        assert_eq!(canvas.cells[0][9].cell_width, CellWidth::Normal);
    }

    #[test]
    fn test_overwriting_wide_character_clears_stale_tail() {
        let mut canvas = Canvas::new(10, 1);
        {
            let mut sv = canvas.subview_mut(0, 0, 0, 0, 10, 1);
            sv.set_text(0, 0, "中", CanvasTextStyle::default());
            sv.set_text(0, 0, "ab", CanvasTextStyle::default());
        }
        assert_eq!(canvas.cells[0][0].cell_width, CellWidth::Normal);
        assert_eq!(canvas.cells[0][1].cell_width, CellWidth::Normal);
        assert_eq!(canvas.get_text(0, 0, 10, 1), "ab");
    }

    #[test]
    fn test_writing_into_width_tail_clears_leading_wide_cell() {
        let mut canvas = Canvas::new(10, 1);
        {
            let mut sv = canvas.subview_mut(0, 0, 0, 0, 10, 1);
            sv.set_text(0, 0, "中", CanvasTextStyle::default());
            sv.set_text(1, 0, "a", CanvasTextStyle::default());
        }
        assert_eq!(canvas.cells[0][0].cell_width, CellWidth::Normal);
        assert_eq!(canvas.cells[0][1].cell_width, CellWidth::Normal);
        assert_eq!(canvas.get_text(0, 0, 10, 1), " a");
    }

    #[test]
    fn test_overlay_auto_expand_to_wide_tail() {
        let mut canvas = Canvas::new(10, 1);
        canvas
            .subview_mut(0, 0, 0, 0, 10, 1)
            .set_text(0, 0, "中ab", CanvasTextStyle::default());
        // Overlay on the Wide cell auto-extends to the Tail.
        canvas.set_overlay(
            0,
            0,
            StyleOverlay {
                invert: Some(true),
                ..Default::default()
            },
        );
        assert!(canvas.overlays[0][0].is_some());
        assert!(
            canvas.overlays[0][1].is_some(),
            "tail must be overlayed too"
        );
        assert!(canvas.overlays[0][2].is_none());
    }

    #[test]
    fn test_overlay_on_width_tail_expands_to_wide() {
        let mut canvas = Canvas::new(10, 1);
        canvas
            .subview_mut(0, 0, 0, 0, 10, 1)
            .set_text(0, 0, "中ab", CanvasTextStyle::default());
        // Overlay on the Tail cell auto-extends to the Wide cell.
        canvas.set_overlay(
            1,
            0,
            StyleOverlay {
                invert: Some(true),
                ..Default::default()
            },
        );
        assert!(
            canvas.overlays[0][0].is_some(),
            "wide must be overlayed too"
        );
        assert!(canvas.overlays[0][1].is_some());
    }

    #[test]
    fn test_hyperlink_osc8_output() {
        let mut canvas = Canvas::new(20, 1);
        canvas.subview_mut(0, 0, 0, 0, 20, 1).set_text_with_link(
            0,
            0,
            "click me",
            CanvasTextStyle::default(),
            Some("https://example.com"),
        );
        let mut buf = Vec::new();
        canvas.write_ansi(&mut buf).unwrap();
        let output = String::from_utf8_lossy(&buf);
        // OSC 8 open sequence. CC Ink adds a deterministic id= param so
        // wrapped lines of the same URL are grouped by terminals.
        assert!(
            output.contains("\x1b]8;id=ags5vy;https://example.com\x1b\\"),
            "expected OSC 8 open with grouped link id: {output:?}"
        );
        // OSC 8 close sequence
        assert!(
            output.contains("\x1b]8;;\x1b\\"),
            "expected OSC 8 close: {output:?}"
        );
        assert!(output.contains("click me"), "text content: {output:?}");
    }

    #[test]
    fn test_hyperlink_not_emitted_without_href() {
        let mut canvas = Canvas::new(10, 1);
        canvas
            .subview_mut(0, 0, 0, 0, 10, 1)
            .set_text(0, 0, "plain", CanvasTextStyle::default());
        let mut buf = Vec::new();
        canvas.write_ansi(&mut buf).unwrap();
        let output = String::from_utf8_lossy(&buf);
        assert!(
            !output.contains("\x1b]8;"),
            "no OSC 8 for plain text: {output:?}"
        );
    }

    #[test]
    fn test_hyperlink_href_filters_terminal_control_chars() {
        let mut canvas = Canvas::new(10, 1);
        canvas.subview_mut(0, 0, 0, 0, 10, 1).set_text_with_link(
            0,
            0,
            "x",
            CanvasTextStyle::default(),
            Some("https://safe.example/\x1b]0;owned\x07"),
        );
        let mut buf = Vec::new();
        canvas.write_ansi(&mut buf).unwrap();
        let output = String::from_utf8_lossy(&buf);
        assert!(output.contains("https://safe.example/]0;owned"));
        assert!(
            !output.contains("\x1b]0;owned"),
            "href must not inject OSC title sequence: {output:?}"
        );
    }

    #[test]
    fn test_hyperlink_adjacent_links() {
        let mut canvas = Canvas::new(20, 1);
        canvas.subview_mut(0, 0, 0, 0, 10, 1).set_text_with_link(
            0,
            0,
            "aaa",
            CanvasTextStyle::default(),
            Some("https://a.com"),
        );
        canvas.subview_mut(3, 0, 3, 0, 10, 1).set_text_with_link(
            0,
            0,
            "bbb",
            CanvasTextStyle::default(),
            Some("https://b.com"),
        );
        let mut buf = Vec::new();
        canvas.write_ansi(&mut buf).unwrap();
        let output = String::from_utf8_lossy(&buf);
        // Both links present
        assert!(output.contains("https://a.com"), "link a: {output:?}");
        assert!(output.contains("https://b.com"), "link b: {output:?}");
    }

    #[test]
    fn test_hyperlink_at_prefers_osc8_and_wide_tail() {
        let mut canvas = Canvas::new(20, 1);
        canvas.subview_mut(0, 0, 0, 0, 20, 1).set_text_with_link(
            0,
            0,
            "中x",
            CanvasTextStyle::default(),
            Some("https://linked.example"),
        );

        assert_eq!(
            canvas.hyperlink_at(0, 0).as_deref(),
            Some("https://linked.example")
        );
        assert_eq!(
            canvas.hyperlink_at(1, 0).as_deref(),
            Some("https://linked.example"),
            "wide-character tail should resolve the head cell's OSC 8 link"
        );
    }

    #[test]
    fn test_plain_text_url_at_trims_sentence_punctuation() {
        let mut canvas = Canvas::new(40, 1);
        canvas.subview_mut(0, 0, 0, 0, 40, 1).set_text(
            0,
            0,
            "see https://example.com/foo).",
            CanvasTextStyle::default(),
        );

        assert_eq!(
            canvas.hyperlink_at(8, 0).as_deref(),
            Some("https://example.com/foo")
        );
        assert_eq!(canvas.hyperlink_at(29, 0), None);
    }

    #[test]
    fn test_plain_text_url_at_chooses_scheme_under_click() {
        let mut canvas = Canvas::new(50, 1);
        canvas.subview_mut(0, 0, 0, 0, 50, 1).set_text(
            0,
            0,
            "https://a.com,https://b.com",
            CanvasTextStyle::default(),
        );

        assert_eq!(canvas.hyperlink_at(8, 0).as_deref(), Some("https://a.com"));
        assert_eq!(canvas.hyperlink_at(20, 0).as_deref(), Some("https://b.com"));
    }

    #[test]
    fn test_plain_text_url_at_respects_no_select_boundaries() {
        let mut canvas = Canvas::new(30, 1);
        canvas.subview_mut(0, 0, 0, 0, 30, 1).set_text(
            0,
            0,
            "https://example.com",
            CanvasTextStyle::default(),
        );
        canvas.mark_no_select_region(0, 0, 5, 1);

        assert_eq!(canvas.hyperlink_at(2, 0), None);
        assert_eq!(canvas.hyperlink_at(8, 0), None);
    }

    #[test]
    fn test_subview_get_text_relative_coords() {
        let mut canvas = Canvas::new(10, 5);
        let mut sv = canvas.subview_mut(2, 1, 2, 1, 6, 3);
        sv.set_text(0, 0, "hello", CanvasTextStyle::default());
        sv.set_text(0, 1, "world", CanvasTextStyle::default());
        // Read back relative to subview origin
        assert_eq!(sv.get_text(0, 0, 6, 1), "hello");
        assert_eq!(sv.get_text(0, 0, 6, 2), "hello\nworld");
    }

    #[test]
    fn test_row_eq_same_content() {
        let mut a = Canvas::new(10, 2);
        let mut b = Canvas::new(10, 2);
        a.subview_mut(0, 0, 0, 0, 10, 2)
            .set_text(0, 0, "hello", CanvasTextStyle::default());
        b.subview_mut(0, 0, 0, 0, 10, 2)
            .set_text(0, 0, "hello", CanvasTextStyle::default());

        assert!(a.row_eq(&b, 0));
        assert!(a.row_eq(&b, 1));
    }

    #[test]
    fn test_row_eq_different_content() {
        let mut a = Canvas::new(10, 1);
        let mut b = Canvas::new(10, 1);
        a.subview_mut(0, 0, 0, 0, 10, 1)
            .set_text(0, 0, "hello", CanvasTextStyle::default());
        b.subview_mut(0, 0, 0, 0, 10, 1)
            .set_text(0, 0, "world", CanvasTextStyle::default());

        assert!(!a.row_eq(&b, 0));
    }

    #[test]
    fn test_row_eq_different_widths() {
        let mut a = Canvas::new(10, 1);
        let mut b = Canvas::new(20, 1);
        a.subview_mut(0, 0, 0, 0, 10, 1)
            .set_text(0, 0, "hello", CanvasTextStyle::default());
        b.subview_mut(0, 0, 0, 0, 20, 1)
            .set_text(0, 0, "hello", CanvasTextStyle::default());

        assert!(!a.row_eq(&b, 0));
    }

    #[test]
    fn test_row_eq_out_of_bounds() {
        let a = Canvas::new(10, 1);
        let b = Canvas::new(10, 2);

        // row 1 is out of bounds for a, but exists (empty) in b
        assert!(a.row_eq(&b, 1));
    }

    /// Regression guard for the row-level diff renderer: a row whose cells are
    /// identical but whose overlay differs (e.g. the cursor moved) must NOT be
    /// considered equal, otherwise the renderer would skip redrawing it and
    /// leave a stale inverted cell on screen.
    #[test]
    fn test_row_eq_detects_overlay_change() {
        let mut a = Canvas::new(10, 1);
        let mut b = Canvas::new(10, 1);
        let style = CanvasTextStyle::default();
        a.subview_mut(0, 0, 0, 0, 10, 1).set_text(0, 0, "hi", style);
        b.subview_mut(0, 0, 0, 0, 10, 1).set_text(0, 0, "hi", style);
        assert!(a.row_eq(&b, 0), "identical rows should be equal");

        // Apply a cursor-style overlay to one canvas only.
        b.set_overlay(
            0,
            0,
            StyleOverlay {
                invert: Some(true),
                ..Default::default()
            },
        );
        assert!(
            !a.row_eq(&b, 0),
            "overlay-only difference must invalidate row equality"
        );

        // And once both have the same overlay, they're equal again.
        a.set_overlay(
            0,
            0,
            StyleOverlay {
                invert: Some(true),
                ..Default::default()
            },
        );
        assert!(a.row_eq(&b, 0));
    }

    #[test]
    fn test_row_change_start_tracks_damage_overlay_and_wide_tail() {
        let style = CanvasTextStyle::default();
        let mut prev = Canvas::new(12, 1);
        let mut next = Canvas::new(12, 1);
        prev.subview_mut(0, 0, 0, 0, 12, 1)
            .set_text(0, 0, "prefix-old", style);
        next.subview_mut(0, 0, 0, 0, 12, 1)
            .set_text(0, 0, "prefix-new", style);
        assert_eq!(prev.row_change_start(&next, 0), Some(7));

        let mut damaged = prev.clone();
        damaged.mark_damage(DamageRegion {
            x: 3,
            y: 0,
            width: 2,
            height: 1,
        });
        assert_eq!(prev.row_change_start(&damaged, 0), Some(3));

        let mut wide_prev = Canvas::new(6, 1);
        let mut wide_next = Canvas::new(6, 1);
        wide_prev
            .subview_mut(0, 0, 0, 0, 6, 1)
            .set_text(0, 0, "a中", style);
        wide_next
            .subview_mut(0, 0, 0, 0, 6, 1)
            .set_text(0, 0, "a中", style);
        wide_next.set_overlay(2, 0, StyleOverlay::inverse());
        assert_eq!(wide_prev.row_change_start(&wide_next, 0), Some(1));
    }

    #[test]
    fn test_row_trimming_keeps_visible_sgr_spaces_like_cc_ink_screen() {
        let style = CanvasTextStyle {
            strikethrough: true,
            ..Default::default()
        };
        let mut canvas = Canvas::new(6, 1);
        canvas
            .subview_mut(0, 0, 0, 0, 6, 1)
            .set_text(0, 0, "x ", style);
        assert_eq!(
            canvas.ansi_row_rendered_width(0),
            2,
            "strikethrough is visible on trailing spaces and must not be trimmed"
        );

        let overline_style = CanvasTextStyle {
            overline: true,
            ..Default::default()
        };
        let mut overline = Canvas::new(6, 1);
        overline
            .subview_mut(0, 0, 0, 0, 6, 1)
            .set_text(0, 0, "x ", overline_style);
        assert_eq!(
            overline.ansi_row_rendered_width(0),
            2,
            "overline is visible on trailing spaces and must not be trimmed"
        );

        let mut overlay = Canvas::new(6, 1);
        overlay.set_overlay(
            3,
            0,
            StyleOverlay {
                strikethrough: Some(true),
                ..Default::default()
            },
        );
        overlay.set_overlay(
            4,
            0,
            StyleOverlay {
                overline: Some(true),
                ..Default::default()
            },
        );
        assert_eq!(
            overlay.ansi_row_rendered_width(0),
            5,
            "strikethrough/overline overlays on empty cells should keep those cells render-significant"
        );
    }

    #[test]
    fn test_write_ansi_row_from_col_without_newline_skips_unchanged_prefix() {
        let style = CanvasTextStyle::default();
        let mut canvas = Canvas::new(12, 1);
        canvas
            .subview_mut(0, 0, 0, 0, 12, 1)
            .set_text(0, 0, "prefix-new", style);

        let mut actual = Vec::new();
        canvas
            .write_ansi_row_from_col_without_newline(0, 7, &mut actual)
            .unwrap();
        let actual = String::from_utf8(actual).unwrap();
        assert!(
            actual.starts_with("new"),
            "partial row writer should start at the requested column: {actual:?}"
        );
        assert!(
            !actual.contains("prefix"),
            "partial row writer should not repaint unchanged prefixes: {actual:?}"
        );
        assert!(actual.contains("\x1b[K"));
    }

    #[test]
    fn test_write_ansi_row_without_newline() {
        let mut canvas = Canvas::new(10, 2);
        canvas
            .subview_mut(0, 0, 0, 0, 10, 2)
            .set_text(0, 0, "hello", CanvasTextStyle::default());
        canvas
            .subview_mut(0, 0, 0, 0, 10, 2)
            .set_text(0, 1, "world", CanvasTextStyle::default());

        // Each row renders without a leading reset (caller's contract is to
        // provide clean SGR state) but always leaves SGR state reset on return.
        let mut row0 = Vec::new();
        canvas.write_ansi_row_without_newline(0, &mut row0).unwrap();

        let mut expected0 = Vec::new();
        write!(expected0, "hello").unwrap();
        write!(expected0, csi!("K")).unwrap();
        write!(expected0, csi!("0m")).unwrap();
        assert_eq!(row0, expected0);

        let mut row1 = Vec::new();
        canvas.write_ansi_row_without_newline(1, &mut row1).unwrap();

        let mut expected1 = Vec::new();
        write!(expected1, "world").unwrap();
        write!(expected1, csi!("K")).unwrap();
        write!(expected1, csi!("0m")).unwrap();
        assert_eq!(row1, expected1);
    }

    #[test]
    fn test_ansi_row_compensates_new_emoji_width_like_cc_ink() {
        let style = CanvasTextStyle::default();
        let mut canvas = Canvas::new(6, 1);
        canvas
            .subview_mut(0, 0, 0, 0, 6, 1)
            .set_text(0, 0, "\u{1fa77}", style);

        let mut actual = Vec::new();
        canvas
            .write_ansi_row_without_newline(0, &mut actual)
            .unwrap();
        let actual = String::from_utf8(actual).unwrap();
        assert!(
            actual.contains("\x1b[2G \x1b[1G\u{1fa77}\x1b[3G"),
            "new emoji should be prefilled and cursor-corrected: {actual:?}"
        );
    }

    #[test]
    fn test_ansi_row_compensates_vs16_emoji_width_like_cc_ink() {
        let style = CanvasTextStyle::default();
        let mut canvas = Canvas::new(6, 1);
        canvas
            .subview_mut(0, 0, 0, 0, 6, 1)
            .set_text(0, 0, "☀\u{fe0f}", style);

        let mut actual = Vec::new();
        canvas
            .write_ansi_row_without_newline(0, &mut actual)
            .unwrap();
        let actual = String::from_utf8(actual).unwrap();
        assert!(
            actual.contains("\x1b[2G \x1b[1G☀\u{fe0f}\x1b[3G"),
            "VS16 emoji should be prefilled and cursor-corrected: {actual:?}"
        );
    }

    #[test]
    fn test_ansi_row_rendered_width_tracks_sparse_row_output() {
        let style = CanvasTextStyle::default();
        let mut canvas = Canvas::new(10, 3);
        canvas
            .subview_mut(0, 0, 0, 0, 10, 3)
            .set_text(0, 0, "hello", style);
        canvas
            .subview_mut(0, 0, 0, 0, 10, 3)
            .set_text(3, 1, "x", style);
        canvas
            .subview_mut(0, 0, 0, 0, 10, 3)
            .set_text(8, 2, "中", style);

        assert_eq!(canvas.ansi_row_rendered_width(0), 5);
        // Interior empty cells are rendered as spaces to reach the non-empty cell.
        assert_eq!(canvas.ansi_row_rendered_width(1), 4);
        // Wide characters advance two terminal columns and reach the right margin here.
        assert_eq!(canvas.ansi_row_rendered_width(2), 10);
    }

    #[test]
    fn test_ansi_row_trims_trailing_invisible_spaces() {
        let style = CanvasTextStyle::default();
        let mut canvas = Canvas::new(10, 2);
        canvas
            .subview_mut(0, 0, 0, 0, 10, 2)
            .set_text(0, 0, "hello     ", style);
        canvas
            .subview_mut(0, 0, 0, 0, 10, 2)
            .set_text(0, 1, "   x", style);

        assert_eq!(canvas.ansi_row_rendered_width(0), 5);
        assert_eq!(canvas.ansi_row_rendered_width(1), 4);

        let mut row0 = Vec::new();
        canvas.write_ansi_row_without_newline(0, &mut row0).unwrap();
        let mut expected0 = Vec::new();
        write!(expected0, "hello").unwrap();
        write!(expected0, csi!("K")).unwrap();
        write!(expected0, csi!("0m")).unwrap();
        assert_eq!(row0, expected0);
    }

    #[test]
    fn test_row_eq_ignores_trailing_invisible_spaces() {
        let style = CanvasTextStyle::default();
        let mut a = Canvas::new(10, 1);
        let mut b = Canvas::new(10, 1);
        a.subview_mut(0, 0, 0, 0, 10, 1)
            .set_text(0, 0, "hello", style);
        b.subview_mut(0, 0, 0, 0, 10, 1)
            .set_text(0, 0, "hello     ", style);

        assert!(
            a.row_eq(&b, 0),
            "trailing invisible padding should not force a row rewrite"
        );
    }
}
