use std::{
    collections::HashMap,
    env,
    sync::{Mutex, Once, OnceLock},
};

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthChar;

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
pub(crate) fn grapheme_width(grapheme: &str) -> usize {
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

pub(crate) fn single_ascii_byte(grapheme: &str) -> Option<u8> {
    let bytes = grapheme.as_bytes();
    (bytes.len() == 1 && bytes[0].is_ascii()).then_some(bytes[0])
}

pub(crate) fn skip_escape_sequence_graphemes(graphemes: &[&str], idx: usize) -> usize {
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
