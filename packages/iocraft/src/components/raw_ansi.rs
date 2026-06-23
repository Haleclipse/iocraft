use crate::{
    canvas::{string_display_width_from_col, UnderlineStyle},
    CanvasTextStyle, Color, Component, ComponentDrawer, ComponentUpdater, Hooks, Props, Weight,
};
use std::collections::HashMap;
use taffy::{geometry::Size, style::Dimension};

/// Default maximum number of ANSI lines retained by [`RawAnsiLineCache`].
///
/// Mirrors CC Ink's `Output.charCache` cap: once the cache grows beyond this
/// many distinct lines it is cleared so long-running transcripts do not retain
/// unbounded parse state.
pub const RAW_ANSI_LINE_CACHE_MAX_ENTRIES: usize = 16_384;

/// Props for [`RawAnsi`].
#[non_exhaustive]
#[derive(Default, Props)]
pub struct RawAnsiProps {
    /// Pre-rendered ANSI lines. Each item is treated as one terminal row.
    pub lines: Vec<String>,

    /// Fixed terminal-column width the producer wrapped to.
    pub width: usize,
}

/// Renders pre-wrapped ANSI text directly into the screen buffer.
///
/// This is the iocraft counterpart to the CC Ink fork's `<RawAnsi>` component:
/// it avoids rebuilding a large styled text tree when an external producer has
/// already emitted terminal-ready ANSI. SGR styling and OSC 8 hyperlink metadata
/// are parsed into retained [`Canvas`](crate::Canvas) cells, so selection,
/// search, copy, and hyperlink hit-testing continue to work.
#[derive(Default)]
pub struct RawAnsi {
    lines: Vec<String>,
    width: usize,
    line_cache: RawAnsiLineCache,
}

impl Component for RawAnsi {
    type Props<'a> = RawAnsiProps;

    fn new(_props: &Self::Props<'_>) -> Self {
        Self::default()
    }

    fn update(
        &mut self,
        props: &mut Self::Props<'_>,
        _hooks: Hooks,
        updater: &mut ComponentUpdater,
    ) {
        self.lines = props.lines.clone();
        self.width = props.width;
        updater.set_layout_style_if_changed(taffy::style::Style {
            size: Size {
                width: Dimension::length(self.width as f32),
                height: Dimension::length(self.lines.len() as f32),
            },
            ..Default::default()
        });
    }

    fn draw(&mut self, drawer: &mut ComponentDrawer<'_>) {
        if drawer.zero_height_sibling_shares_y() {
            return;
        }
        let mut canvas = drawer.canvas();
        for (row, line) in self.lines.iter().enumerate() {
            let runs = self.line_cache.parse_line(line).to_vec();
            let mut col = 0isize;
            for run in runs {
                if run.text.is_empty() {
                    continue;
                }
                let width = string_display_width_from_col(&run.text, col.max(0) as usize);
                if let Some(bg) = run.background_color {
                    canvas.set_background_color(col, row as isize, width, 1, bg);
                }
                canvas.set_text_with_link(
                    col,
                    row as isize,
                    &run.text,
                    run.style,
                    run.hyperlink.as_deref(),
                );
                col += width as isize;
            }
        }
    }
}

/// A parsed ANSI text run with resolved canvas style metadata.
///
/// Returned by [`RawAnsiLineCache::parse_line`]. Runs are split whenever the
/// active SGR style, background color, or OSC 8 hyperlink changes. They are
/// already reordered for terminals that need software bidi, matching the
/// [`RawAnsi`] component's draw path.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq)]
pub struct AnsiRun {
    /// Plain text for this run, without ANSI control sequences.
    pub text: String,
    /// Resolved text style for the run.
    pub style: CanvasTextStyle,
    /// Optional background fill to apply behind the run.
    pub background_color: Option<Color>,
    /// Optional OSC 8 hyperlink carried by the run.
    pub hyperlink: Option<String>,
}

/// Opt-in cache for parsed RawAnsi lines.
///
/// CC Ink keeps `Output.charCache` across frames so unchanged lines avoid
/// repeated ANSI tokenization, grapheme clustering, style interning, hyperlink
/// extraction, and bidi reordering. `RawAnsiLineCache` is the Rust-first helper
/// for the same optimization: callers keep explicit ownership of the cache,
/// feed pre-wrapped lines through [`Self::parse_line`], and receive reusable
/// [`AnsiRun`] slices. The cache is mode-neutral and does not render or mutate a
/// terminal.
#[derive(Clone, Debug)]
pub struct RawAnsiLineCache {
    lines: HashMap<String, Vec<AnsiRun>>,
    scratch: Vec<AnsiRun>,
    max_entries: usize,
}

impl Default for RawAnsiLineCache {
    fn default() -> Self {
        Self::new()
    }
}

impl RawAnsiLineCache {
    /// Creates an empty cache using [`RAW_ANSI_LINE_CACHE_MAX_ENTRIES`].
    pub fn new() -> Self {
        Self::with_max_entries(RAW_ANSI_LINE_CACHE_MAX_ENTRIES)
    }

    /// Creates an empty cache with a custom maximum entry count.
    ///
    /// `max_entries == 0` disables retention while keeping parsing behavior
    /// available through the same API.
    pub fn with_max_entries(max_entries: usize) -> Self {
        Self {
            lines: HashMap::new(),
            scratch: Vec::new(),
            max_entries,
        }
    }

    /// Parses `line` and returns cached ANSI runs for it.
    ///
    /// The input is treated as one terminal row; parsing stops at carriage
    /// return or newline just like [`RawAnsi`] does for each `lines` entry.
    pub fn parse_line(&mut self, line: &str) -> &[AnsiRun] {
        if self.max_entries == 0 {
            self.lines.clear();
            self.scratch = reorder_bidi_ansi_runs_for_terminal(parse_ansi_line(line));
            return &self.scratch;
        }

        if !self.lines.contains_key(line) {
            if self.lines.len() >= self.max_entries {
                self.lines.clear();
            }
            let runs = reorder_bidi_ansi_runs_for_terminal(parse_ansi_line(line));
            self.lines.insert(line.to_string(), runs);
        }

        self.lines.get(line).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Number of cached line entries.
    pub fn len(&self) -> usize {
        self.lines.len()
    }

    /// Whether the cache is empty.
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// Clears all cached line entries.
    pub fn clear(&mut self) {
        self.lines.clear();
        self.scratch.clear();
    }

    /// Maximum number of entries retained before clearing.
    pub fn max_entries(&self) -> usize {
        self.max_entries
    }
}

#[derive(Clone, Debug, PartialEq)]
struct AnsiRunMetadata {
    style: CanvasTextStyle,
    background_color: Option<Color>,
    hyperlink: Option<String>,
}

fn reorder_bidi_ansi_runs(runs: Vec<AnsiRun>, enabled: bool) -> Vec<AnsiRun> {
    if runs.is_empty() {
        return runs;
    }

    let mut graphemes = Vec::new();
    for run in runs {
        let metadata = AnsiRunMetadata {
            style: run.style,
            background_color: run.background_color,
            hyperlink: run.hyperlink,
        };
        for grapheme in
            unicode_segmentation::UnicodeSegmentation::graphemes(run.text.as_str(), true)
        {
            graphemes.push(crate::bidi::BidiGrapheme {
                text: grapheme.to_string(),
                metadata: metadata.clone(),
            });
        }
    }

    let reordered = crate::bidi::reorder_bidi_graphemes(graphemes, enabled);
    let mut ret: Vec<AnsiRun> = Vec::new();
    for grapheme in reordered {
        if let Some(last) = ret.last_mut().filter(|last| {
            last.style == grapheme.metadata.style
                && last.background_color == grapheme.metadata.background_color
                && last.hyperlink == grapheme.metadata.hyperlink
        }) {
            last.text.push_str(&grapheme.text);
        } else {
            ret.push(AnsiRun {
                text: grapheme.text,
                style: grapheme.metadata.style,
                background_color: grapheme.metadata.background_color,
                hyperlink: grapheme.metadata.hyperlink,
            });
        }
    }
    ret
}

fn reorder_bidi_ansi_runs_for_terminal(runs: Vec<AnsiRun>) -> Vec<AnsiRun> {
    if crate::bidi::needs_software_bidi() {
        reorder_bidi_ansi_runs(runs, true)
    } else {
        runs
    }
}

#[derive(Clone, Debug, PartialEq)]
struct AnsiState {
    style: CanvasTextStyle,
    background_color: Option<Color>,
    hyperlink: Option<String>,
    bold: bool,
    dim: bool,
}

impl Default for AnsiState {
    fn default() -> Self {
        Self {
            style: CanvasTextStyle::default(),
            background_color: None,
            hyperlink: None,
            bold: false,
            dim: false,
        }
    }
}

impl AnsiState {
    fn reset_sgr(&mut self) {
        self.style = CanvasTextStyle::default();
        self.background_color = None;
        self.bold = false;
        self.dim = false;
    }

    fn update_intensity(&mut self) {
        self.style.weight = if self.dim {
            Weight::Light
        } else if self.bold {
            Weight::Bold
        } else {
            Weight::Normal
        };
    }
}

fn basic_color(code: i32, bright: bool) -> Option<Color> {
    match (code, bright) {
        (0, false) => Some(Color::Black),
        (1, false) => Some(Color::DarkRed),
        (2, false) => Some(Color::DarkGreen),
        (3, false) => Some(Color::DarkYellow),
        (4, false) => Some(Color::DarkBlue),
        (5, false) => Some(Color::DarkMagenta),
        (6, false) => Some(Color::DarkCyan),
        (7, false) => Some(Color::Grey),
        (0, true) => Some(Color::DarkGrey),
        (1, true) => Some(Color::Red),
        (2, true) => Some(Color::Green),
        (3, true) => Some(Color::Yellow),
        (4, true) => Some(Color::Blue),
        (5, true) => Some(Color::Magenta),
        (6, true) => Some(Color::Cyan),
        (7, true) => Some(Color::White),
        _ => None,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SgrParam {
    value: Option<i32>,
    subparams: Vec<i32>,
    colon: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ExtendedColor {
    Indexed(u8),
    Rgb { r: u8, g: u8, b: u8 },
}

impl From<ExtendedColor> for Color {
    fn from(value: ExtendedColor) -> Self {
        match value {
            ExtendedColor::Indexed(value) => Color::AnsiValue(value),
            ExtendedColor::Rgb { r, g, b } => Color::Rgb { r, g, b },
        }
    }
}

fn parse_sgr_params(params: &str) -> Vec<SgrParam> {
    if params.is_empty() {
        return vec![SgrParam {
            value: Some(0),
            subparams: Vec::new(),
            colon: false,
        }];
    }

    let mut result = Vec::new();
    let mut value = None;
    let mut subparams = Vec::new();
    let mut colon = false;
    let mut number = String::new();
    let mut in_subparams = false;

    for ch in params.chars().map(Some).chain(std::iter::once(None)) {
        match ch {
            Some(';') | None => {
                let parsed = if number.is_empty() {
                    None
                } else {
                    number.parse::<i32>().ok()
                };
                if in_subparams {
                    if let Some(parsed) = parsed {
                        subparams.push(parsed);
                    }
                } else {
                    value = parsed;
                }
                result.push(SgrParam {
                    value,
                    subparams: std::mem::take(&mut subparams),
                    colon,
                });
                value = None;
                colon = false;
                number.clear();
                in_subparams = false;
            }
            Some(':') => {
                let parsed = if number.is_empty() {
                    None
                } else {
                    number.parse::<i32>().ok()
                };
                if in_subparams {
                    if let Some(parsed) = parsed {
                        subparams.push(parsed);
                    }
                } else {
                    value = parsed;
                    colon = true;
                    in_subparams = true;
                }
                number.clear();
            }
            Some(ch) if ch.is_ascii_digit() => number.push(ch),
            Some(_) => {}
        }
    }

    result
}

fn parse_extended_color(params: &[SgrParam], idx: usize) -> Option<(ExtendedColor, usize)> {
    let param = params.get(idx)?;
    if param.colon {
        match param.subparams.as_slice() {
            [5, value, ..] => u8::try_from(*value)
                .ok()
                .map(|value| (ExtendedColor::Indexed(value), 1)),
            [2, _color_space, r, g, b, ..] => Some((
                ExtendedColor::Rgb {
                    r: u8::try_from(*r).ok()?,
                    g: u8::try_from(*g).ok()?,
                    b: u8::try_from(*b).ok()?,
                },
                1,
            )),
            [2, r, g, b] => Some((
                ExtendedColor::Rgb {
                    r: u8::try_from(*r).ok()?,
                    g: u8::try_from(*g).ok()?,
                    b: u8::try_from(*b).ok()?,
                },
                1,
            )),
            _ => None,
        }
    } else {
        match params.get(idx + 1).and_then(|param| param.value) {
            Some(5) => {
                let value = params.get(idx + 2).and_then(|param| param.value)?;
                u8::try_from(value)
                    .ok()
                    .map(|value| (ExtendedColor::Indexed(value), 3))
            }
            Some(2) => {
                let r = params.get(idx + 2).and_then(|param| param.value)?;
                let g = params.get(idx + 3).and_then(|param| param.value)?;
                let b = params.get(idx + 4).and_then(|param| param.value)?;
                Some((
                    ExtendedColor::Rgb {
                        r: u8::try_from(r).ok()?,
                        g: u8::try_from(g).ok()?,
                        b: u8::try_from(b).ok()?,
                    },
                    5,
                ))
            }
            _ => None,
        }
    }
}

fn apply_sgr(state: &mut AnsiState, params: &[SgrParam]) {
    let mut idx = 0;
    while idx < params.len() {
        let param = &params[idx];
        let code = param.value.unwrap_or(0);
        match code {
            0 => state.reset_sgr(),
            1 => {
                state.bold = true;
                state.update_intensity();
            }
            2 => {
                state.dim = true;
                state.update_intensity();
            }
            3 => state.style.italic = true,
            4 => {
                if param.colon {
                    match param.subparams.first().copied().unwrap_or(1) {
                        0 => state.style.underline = false,
                        2 => {
                            state.style.underline = true;
                            state.style.underline_style = UnderlineStyle::Double;
                        }
                        3 => {
                            state.style.underline = true;
                            state.style.underline_style = UnderlineStyle::Curly;
                        }
                        4 => {
                            state.style.underline = true;
                            state.style.underline_style = UnderlineStyle::Dotted;
                        }
                        5 => {
                            state.style.underline = true;
                            state.style.underline_style = UnderlineStyle::Dashed;
                        }
                        _ => {
                            state.style.underline = true;
                            state.style.underline_style = UnderlineStyle::Single;
                        }
                    }
                } else {
                    state.style.underline = true;
                    state.style.underline_style = UnderlineStyle::Single;
                }
            }
            5 | 6 => state.style.blink = true,
            7 => state.style.invert = true,
            8 => state.style.hidden = true,
            9 => state.style.strikethrough = true,
            21 => {
                state.style.underline = true;
                state.style.underline_style = UnderlineStyle::Double;
            }
            22 => {
                state.bold = false;
                state.dim = false;
                state.update_intensity();
            }
            23 => state.style.italic = false,
            24 => state.style.underline = false,
            25 => state.style.blink = false,
            27 => state.style.invert = false,
            28 => state.style.hidden = false,
            29 => state.style.strikethrough = false,
            30..=37 => state.style.color = basic_color(code - 30, false),
            39 => state.style.color = None,
            40..=47 => state.background_color = basic_color(code - 40, false),
            49 => state.background_color = None,
            53 => state.style.overline = true,
            55 => state.style.overline = false,
            90..=97 => state.style.color = basic_color(code - 90, true),
            100..=107 => state.background_color = basic_color(code - 100, true),
            38 => {
                if let Some((color, consumed)) = parse_extended_color(params, idx) {
                    state.style.color = Some(color.into());
                    idx += consumed;
                    continue;
                }
            }
            48 => {
                if let Some((color, consumed)) = parse_extended_color(params, idx) {
                    state.background_color = Some(color.into());
                    idx += consumed;
                    continue;
                }
            }
            58 => {
                if let Some((color, consumed)) = parse_extended_color(params, idx) {
                    state.style.underline_color = Some(color.into());
                    idx += consumed;
                    continue;
                }
            }
            59 => state.style.underline_color = None,
            _ => {}
        }
        idx += 1;
    }
}

fn parse_osc8_hyperlink(payload: &str) -> Option<Option<String>> {
    let rest = payload.strip_prefix("8;")?;
    let (_params, uri) = rest.split_once(';')?;
    if uri.is_empty() {
        Some(None)
    } else {
        Some(Some(uri.to_string()))
    }
}

fn string_sequence_end(input: &str, body_start: usize) -> Option<(usize, usize)> {
    let bel_end = input[body_start..].find('\x07').map(|rel| body_start + rel);
    let st_end = input[body_start..]
        .find("\x1b\\")
        .map(|rel| body_start + rel);
    match (bel_end, st_end) {
        (Some(bel), Some(st)) if bel < st => Some((bel, 1)),
        (Some(bel), None) => Some((bel, 1)),
        (_, Some(st)) => Some((st, 2)),
        (None, None) => None,
    }
}

fn flush_run(runs: &mut Vec<AnsiRun>, buf: &mut String, state: &AnsiState) {
    if buf.is_empty() {
        return;
    }
    runs.push(AnsiRun {
        text: std::mem::take(buf),
        style: state.style,
        background_color: state.background_color,
        hyperlink: state.hyperlink.clone(),
    });
}

fn parse_ansi_impl(input: &str, stop_at_newline: bool) -> Vec<AnsiRun> {
    let mut runs = Vec::new();
    let mut state = AnsiState::default();
    let mut text = String::new();
    let mut i = 0;

    while i < input.len() {
        let rest = &input[i..];
        if rest.starts_with("\x1b[") {
            let Some(final_rel) = rest[2..].find(|ch: char| ('@'..='~').contains(&ch)) else {
                break;
            };
            let final_idx = i + 2 + final_rel;
            let final_char = input[final_idx..].chars().next().unwrap_or('m');
            if final_char == 'm' {
                flush_run(&mut runs, &mut text, &state);
                let params = parse_sgr_params(&input[i + 2..final_idx]);
                apply_sgr(&mut state, &params);
            }
            i = final_idx + final_char.len_utf8();
            continue;
        }

        if rest.starts_with("\x1b]") {
            let body_start = i + 2;
            let Some((end, terminator_len)) = string_sequence_end(input, body_start) else {
                break;
            };
            flush_run(&mut runs, &mut text, &state);
            if let Some(link) = parse_osc8_hyperlink(&input[body_start..end]) {
                state.hyperlink = link;
            }
            i = end + terminator_len;
            continue;
        }

        if rest.starts_with('\x1b')
            && matches!(rest.as_bytes().get(1), Some(b'P' | b'_' | b'^' | b'X'))
        {
            let body_start = i + 2;
            let Some((end, terminator_len)) = string_sequence_end(input, body_start) else {
                break;
            };
            flush_run(&mut runs, &mut text, &state);
            i = end + terminator_len;
            continue;
        }

        let Some(ch) = rest.chars().next() else {
            break;
        };
        if stop_at_newline && (ch == '\r' || ch == '\n') {
            break;
        }
        if ch == '\x1b' {
            i += ch.len_utf8();
            if let Some(next) = input[i..].chars().next() {
                i += next.len_utf8();
                if matches!(next, '(' | ')' | '*' | '+') {
                    if let Some(designator) = input[i..].chars().next() {
                        i += designator.len_utf8();
                    }
                }
            }
            continue;
        }
        text.push(ch);
        i += ch.len_utf8();
    }

    flush_run(&mut runs, &mut text, &state);
    runs
}

pub(crate) fn parse_ansi(input: &str) -> Vec<AnsiRun> {
    parse_ansi_impl(input, false)
}

pub(crate) fn parse_ansi_line(line: &str) -> Vec<AnsiRun> {
    parse_ansi_impl(line, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prelude::*;

    #[test]
    fn test_raw_ansi_line_cache_reuses_and_caps_like_cc_output_char_cache() {
        let mut cache = RawAnsiLineCache::with_max_entries(2);
        let first = cache.parse_line("\x1b[31mred\x1b[0m").to_vec();
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].text, "red");
        assert_eq!(first[0].style.color, Some(Color::DarkRed));
        assert_eq!(cache.len(), 1);

        let second = cache.parse_line("\x1b[31mred\x1b[0m").to_vec();
        assert_eq!(second, first);
        assert_eq!(cache.len(), 1, "unchanged line should hit the cache");

        cache.parse_line("plain");
        assert_eq!(cache.len(), 2);
        cache.parse_line("third");
        assert_eq!(
            cache.len(),
            1,
            "cache should clear when the entry cap is reached"
        );

        let mut no_retention = RawAnsiLineCache::with_max_entries(0);
        assert_eq!(no_retention.parse_line("a")[0].text, "a");
        assert_eq!(no_retention.len(), 0);
        assert_eq!(no_retention.parse_line("b")[0].text, "b");
        assert_eq!(no_retention.len(), 0);
    }

    #[test]
    fn test_raw_ansi_parses_sgr_styles_and_fixed_layout() {
        let mut raw = element!(RawAnsi(
            lines: vec!["\x1b[31;1mred\x1b[0m plain".to_string()],
            width: 10usize,
        ));
        let canvas = raw.render(None);

        assert_eq!(canvas.width(), 10);
        assert_eq!(canvas.height(), 1);
        assert_eq!(canvas.to_string(), "red plain\n");
        let red = canvas.resolved_text_style(0, 0).unwrap();
        assert_eq!(red.color, Some(Color::DarkRed));
        assert_eq!(red.weight, Weight::Bold);
        assert_eq!(canvas.resolved_text_style(4, 0).unwrap().color, None);

        let mut raw = element!(RawAnsi(
            lines: vec!["\x1b[9mstrike\x1b[29m plain".to_string()],
            width: 16usize,
        ));
        let canvas = raw.render(None);
        assert!(canvas.resolved_text_style(0, 0).unwrap().strikethrough);
        assert!(!canvas.resolved_text_style(7, 0).unwrap().strikethrough);

        let mut raw = element!(RawAnsi(
            lines: vec!["\x1b[4:3mcurly\x1b[24m \x1b[21mdouble".to_string()],
            width: 16usize,
        ));
        let canvas = raw.render(None);
        let curly = canvas.resolved_text_style(0, 0).unwrap();
        assert!(curly.underline);
        assert_eq!(curly.underline_style, UnderlineStyle::Curly);
        let double = canvas.resolved_text_style(7, 0).unwrap();
        assert!(double.underline);
        assert_eq!(double.underline_style, UnderlineStyle::Double);

        let mut raw = element!(RawAnsi(
            lines: vec!["\x1b[4;58:2::1:2:3mcolor\x1b[59m plain".to_string()],
            width: 20usize,
        ));
        let canvas = raw.render(None);
        assert_eq!(
            canvas.resolved_text_style(0, 0).unwrap().underline_color,
            Some(Color::Rgb { r: 1, g: 2, b: 3 }),
            "SGR 58 underline color should be preserved in RawAnsi"
        );
        assert_eq!(
            canvas.resolved_text_style(6, 0).unwrap().underline_color,
            None,
            "SGR 59 should reset underline color"
        );

        let mut raw = element!(RawAnsi(
            lines: vec!["\x1b[5mblink\x1b[25m \x1b[8mhide\x1b[28m plain".to_string()],
            width: 20usize,
        ));
        let canvas = raw.render(None);
        assert!(
            canvas.resolved_text_style(0, 0).unwrap().blink,
            "SGR 5/6 blink should be preserved in RawAnsi"
        );
        assert!(
            !canvas.resolved_text_style(6, 0).unwrap().blink,
            "SGR 25 should clear blink"
        );
        assert!(
            canvas.resolved_text_style(7, 0).unwrap().hidden,
            "SGR 8 hidden/conceal should be preserved in RawAnsi"
        );
        assert!(
            !canvas.resolved_text_style(12, 0).unwrap().hidden,
            "SGR 28 should clear hidden/conceal"
        );

        let mut raw = element!(RawAnsi(
            lines: vec!["\x1b[53mover\x1b[55m plain".to_string()],
            width: 16usize,
        ));
        let canvas = raw.render(None);
        assert!(
            canvas.resolved_text_style(0, 0).unwrap().overline,
            "SGR 53 overline should be preserved in RawAnsi"
        );
        assert!(
            !canvas.resolved_text_style(5, 0).unwrap().overline,
            "SGR 55 should clear overline"
        );
    }

    #[test]
    fn test_raw_ansi_parses_background_and_osc8_links() {
        let mut raw = element!(RawAnsi(
            lines: vec![
                "\x1b[48;5;4mblue\x1b[0m \x1b]8;;https://example.com\x07link\x1b]8;;\x07".to_string()
            ],
            width: 16usize,
        ));
        let canvas = raw.render(None);

        assert_eq!(canvas.to_string(), "blue link\n");
        let mut ansi = Vec::new();
        canvas.write_ansi(&mut ansi).unwrap();
        let ansi = String::from_utf8(ansi).unwrap();
        assert!(
            ansi.contains("\x1b[48;5;4m"),
            "background should survive: {ansi:?}"
        );
        assert_eq!(
            canvas.hyperlink_at(6, 0).as_deref(),
            Some("https://example.com")
        );
    }

    #[test]
    fn test_raw_ansi_sgr_reset_preserves_active_osc8_link() {
        let mut raw = element!(RawAnsi(
            lines: vec!["\x1b]8;;https://example.com\x07\x1b[31mred\x1b[0m plain\x1b]8;;\x07".to_string()],
            width: 10usize,
        ));
        let canvas = raw.render(None);

        assert_eq!(canvas.to_string(), "red plain\n");
        assert_eq!(
            canvas.resolved_text_style(0, 0).unwrap().color,
            Some(Color::DarkRed)
        );
        assert_eq!(canvas.resolved_text_style(4, 0).unwrap().color, None);
        assert_eq!(
            canvas.hyperlink_at(0, 0).as_deref(),
            Some("https://example.com")
        );
        assert_eq!(
            canvas.hyperlink_at(4, 0).as_deref(),
            Some("https://example.com")
        );
    }

    #[test]
    fn test_raw_ansi_colon_sgr_extended_color_and_subparams() {
        let mut raw = element!(RawAnsi(
            lines: vec!["\x1b[38:2::1:2:3mfg \x1b[48:2:0:4:5:6mbg \x1b[31mred \x1b[4:0mstill-red \x1b[58;2;1;2;3mplain".to_string()],
            width: 32usize,
        ));
        let canvas = raw.render(None);

        assert_eq!(
            canvas.resolved_text_style(0, 0).unwrap().color,
            Some(Color::Rgb { r: 1, g: 2, b: 3 })
        );
        assert_eq!(
            canvas.cell(3, 0).unwrap().background_color,
            Some(Color::Rgb { r: 4, g: 5, b: 6 })
        );
        assert_eq!(
            canvas.resolved_text_style(7, 0).unwrap().color,
            Some(Color::DarkRed)
        );
        assert!(!canvas.resolved_text_style(11, 0).unwrap().underline);
        assert_eq!(
            canvas.resolved_text_style(11, 0).unwrap().color,
            Some(Color::DarkRed)
        );
        assert_eq!(
            canvas.resolved_text_style(21, 0).unwrap().weight,
            Weight::Normal
        );
    }

    #[test]
    fn test_raw_ansi_bidi_reorder_preserves_run_styles() {
        let mut runs = parse_ansi_line("\x1b[31mאבג\x1b[0mabc");
        runs = reorder_bidi_ansi_runs(runs, true);

        assert_eq!(
            runs.iter().map(|run| run.text.as_str()).collect::<String>(),
            "abcגבא"
        );
        assert_eq!(runs[0].style.color, None);
        assert_eq!(runs[1].style.color, Some(Color::DarkRed));
    }

    #[test]
    fn test_raw_ansi_skips_non_sgr_escape_and_string_sequences() {
        let mut raw = element!(RawAnsi(
            lines: vec![
                "A\x08B\x1bPignored\x1b\\C\x1b_ignored\x07D\x1b(0E\x1b[2KF".to_string()
            ],
            width: 6usize,
        ));
        let canvas = raw.render(None);

        assert_eq!(canvas.to_string(), "ABCDEF\n");
    }
}
