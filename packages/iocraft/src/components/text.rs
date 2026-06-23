use crate::{
    canvas::UnderlineStyle, render::MeasureFunc, segmented_string::SegmentedString,
    strip_ansi::strip_ansi, CanvasTextStyle, Color, Component, ComponentDrawer, ComponentUpdater,
    Hooks, Props, Weight,
};
use taffy::{AvailableSpace, Size};

/// The text wrapping behavior of a [`Text`] component.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum TextWrap {
    /// Text is wrapped at appropriate characters to minimize overflow. This is the default.
    #[default]
    Wrap,
    /// Text is wrapped and each visual line is trimmed, matching CC Ink's `wrap-trim` mode.
    WrapTrim,
    /// Text is not wrapped, and may overflow the bounds of the component.
    NoWrap,
    /// CC Ink legacy `end` wrap value. In the current CC Ink fork this is a
    /// no-op in `wrapText(...)`, so it behaves like [`Self::NoWrap`].
    End,
    /// CC Ink legacy `middle` wrap value. In the current CC Ink fork this is a
    /// no-op in `wrapText(...)`, so it behaves like [`Self::NoWrap`].
    Middle,
    /// Text is truncated at the end with an ellipsis, matching CC Ink's `truncate` alias.
    Truncate,
    /// Text is truncated at the end with an ellipsis.
    TruncateEnd,
    /// Text is truncated in the middle with an ellipsis.
    TruncateMiddle,
    /// Text is truncated at the start with an ellipsis.
    TruncateStart,
}

/// The text alignment of a [`Text`] component.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum TextAlign {
    /// Text is aligned to the left. This is the default.
    #[default]
    Left,
    /// Text is aligned to the right.
    Right,
    /// Text is aligned to the center.
    Center,
}

/// The text decoration of a [`Text`] component.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum TextDecoration {
    /// No text decoration. This is the default.
    #[default]
    None,
    /// The text is underlined.
    Underline,
}

/// The props which can be passed to the [`Text`] component.
#[non_exhaustive]
#[derive(Default, Props)]
pub struct TextProps {
    /// The color to make the text.
    pub color: Option<Color>,

    /// The background color to paint behind the rendered text cells.
    pub background_color: Option<Color>,

    /// The content of the text.
    pub content: String,

    /// The weight of the text.
    pub weight: Weight,

    /// CC Ink-style alias for [`Weight::Bold`]. If both `bold` and `dim` are
    /// set dynamically, `dim` wins to match the CC Ink ANSI span wrapper.
    pub bold: bool,

    /// CC Ink-style alias for [`Weight::Light`] / SGR dim. This takes
    /// precedence over [`Self::bold`] and [`Self::weight`].
    pub dim: bool,

    /// The text wrapping behavior.
    pub wrap: TextWrap,

    /// The text alignment.
    pub align: TextAlign,

    /// The text decoration.
    pub decoration: TextDecoration,

    /// CC Ink-style alias for [`TextDecoration::Underline`].
    pub underline: bool,

    /// Whether to italicize the text.
    pub italic: bool,

    /// Whether to strike through the text.
    pub strikethrough: bool,

    /// Whether to draw an overline above the text.
    pub overline: bool,

    /// Whether to invert the text's foreground and background colors.
    pub invert: bool,

    /// CC Ink-style alias for [`Self::invert`].
    pub inverse: bool,

    /// If set, renders the text as an OSC 8 hyperlink. Terminals with support
    /// (kitty, iTerm2, WezTerm, Windows Terminal, etc.) allow clicking or
    /// Cmd/Ctrl-clicking to open the URL. Unsupported terminals display the
    /// text normally.
    pub href: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TruncatePosition {
    Start,
    Middle,
    End,
}

struct WrappedTextLine {
    text: String,
    soft_continuation: bool,
    content_end: usize,
}

/// `Text` is a component that renders a text string.
///
/// # Example
///
/// ```
/// # use iocraft::prelude::*;
/// # fn my_element() -> impl Into<AnyElement<'static>> {
/// element! {
///     Text(content: "Hello!")
/// }
/// # }
/// ```
#[derive(Default)]
pub struct Text {
    style: CanvasTextStyle,
    background_color: Option<Color>,
    content: String,
    wrap: TextWrap,
    align: TextAlign,
    hyperlink: Option<String>,
}

impl Text {
    pub(crate) fn measure_func(content: String, text_wrap: TextWrap) -> MeasureFunc {
        Box::new(move |known_size, available_space, _| {
            let content = Text::wrap(&content, text_wrap, known_size.width, available_space.width);
            let measured = crate::canvas::measure_text(&content, None);
            Size {
                width: measured.width as _,
                height: measured.height as _,
            }
        })
    }

    fn line_is_soft_continuation(
        content: &str,
        line: &crate::segmented_string::SegmentedStringLine<'_>,
    ) -> bool {
        let Some(first_segment) = line.segments.first() else {
            return false;
        };
        first_segment.index == 0
            && first_segment.offset > 0
            && !content[..first_segment.offset].ends_with('\n')
    }

    fn truncate_position(text_wrap: TextWrap) -> Option<TruncatePosition> {
        match text_wrap {
            TextWrap::Truncate | TextWrap::TruncateEnd => Some(TruncatePosition::End),
            TextWrap::TruncateMiddle => Some(TruncatePosition::Middle),
            TextWrap::TruncateStart => Some(TruncatePosition::Start),
            _ => None,
        }
    }

    fn slice_display_columns(text: &str, start: usize, end: usize) -> String {
        if start >= end {
            return String::new();
        }

        let mut ret = String::new();
        let mut col = 0;
        for grapheme in unicode_segmentation::UnicodeSegmentation::graphemes(text, true) {
            let width = crate::canvas::string_display_width_from_col(grapheme, col);
            let next = col + width;
            if next <= start {
                col = next;
                continue;
            }
            if col >= end {
                break;
            }
            // Match CC Ink's sliceFit behavior: a wide grapheme that straddles
            // the boundary is omitted rather than allowed to overflow the target
            // column range.
            if col >= start && next <= end {
                ret.push_str(grapheme);
            }
            col = next;
        }
        ret
    }

    fn truncate_line(text: &str, columns: usize, position: TruncatePosition) -> String {
        const ELLIPSIS: &str = "…";

        if columns < 1 {
            return String::new();
        }
        if columns == 1 {
            return ELLIPSIS.to_string();
        }

        let width = crate::canvas::string_display_width(text);
        if width <= columns {
            return text.to_string();
        }

        match position {
            TruncatePosition::Start => {
                format!(
                    "{ELLIPSIS}{}",
                    Self::slice_display_columns(text, width - columns + 1, width)
                )
            }
            TruncatePosition::Middle => {
                let prefix_columns = columns / 2;
                let suffix_columns = columns - prefix_columns - 1;
                format!(
                    "{}{}{}",
                    Self::slice_display_columns(text, 0, prefix_columns),
                    ELLIPSIS,
                    Self::slice_display_columns(text, width - suffix_columns, width)
                )
            }
            TruncatePosition::End => {
                format!(
                    "{}{ELLIPSIS}",
                    Self::slice_display_columns(text, 0, columns - 1)
                )
            }
        }
    }

    fn wrap_lines(content: &str, text_wrap: TextWrap, width: usize) -> Vec<WrappedTextLine> {
        if let Some(position) = Self::truncate_position(text_wrap) {
            return content
                .split('\n')
                .map(|line| {
                    let text = Self::truncate_line(line, width, position);
                    let content_end = crate::canvas::string_display_width(&text);
                    WrappedTextLine {
                        text,
                        soft_continuation: false,
                        content_end,
                    }
                })
                .collect();
        }

        match text_wrap {
            TextWrap::NoWrap | TextWrap::End | TextWrap::Middle => content
                .split('\n')
                .map(|line| WrappedTextLine {
                    text: line.to_string(),
                    soft_continuation: false,
                    content_end: crate::canvas::string_display_width(line),
                })
                .collect(),
            TextWrap::Wrap | TextWrap::WrapTrim => {
                let trimmed_content;
                let source = if text_wrap == TextWrap::WrapTrim {
                    trimmed_content = content
                        .split('\n')
                        .map(str::trim)
                        .collect::<Vec<_>>()
                        .join("\n");
                    trimmed_content.as_str()
                } else {
                    content
                };
                let segmented: SegmentedString = source.into();
                let lines = segmented.wrap(width);
                let soft_continuations = lines
                    .iter()
                    .map(|line| Self::line_is_soft_continuation(source, line))
                    .collect::<Vec<_>>();
                let line_count = lines.len();
                lines
                    .into_iter()
                    .enumerate()
                    .map(|(index, line)| {
                        let preserve_content_end =
                            index + 1 < line_count && soft_continuations[index + 1];
                        let (text, rendered_width) = if text_wrap == TextWrap::WrapTrim {
                            let text = line.to_string().trim().to_string();
                            let width = crate::canvas::string_display_width(&text);
                            (text, width)
                        } else {
                            let mut trimmed = line.clone();
                            trimmed.trim_end();
                            (trimmed.to_string(), trimmed.width)
                        };
                        let content_end = if preserve_content_end {
                            line.width
                        } else {
                            rendered_width
                        };
                        WrappedTextLine {
                            text,
                            soft_continuation: soft_continuations[index],
                            content_end,
                        }
                    })
                    .collect()
            }
            TextWrap::Truncate
            | TextWrap::TruncateEnd
            | TextWrap::TruncateMiddle
            | TextWrap::TruncateStart => unreachable!("handled above"),
        }
    }

    fn wrap_to_string(s: &str, text_wrap: TextWrap, width: usize) -> String {
        Self::wrap_lines(s, text_wrap, width)
            .into_iter()
            .map(|line| line.text)
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn wrap(
        content: &str,
        text_wrap: TextWrap,
        known_width: Option<f32>,
        available_width: AvailableSpace,
    ) -> String {
        match text_wrap {
            TextWrap::Wrap
            | TextWrap::WrapTrim
            | TextWrap::Truncate
            | TextWrap::TruncateEnd
            | TextWrap::TruncateMiddle
            | TextWrap::TruncateStart => match known_width {
                Some(w) => Self::wrap_to_string(content, text_wrap, w as usize),
                None => match available_width {
                    AvailableSpace::Definite(w) => {
                        Self::wrap_to_string(content, text_wrap, w as usize)
                    }
                    AvailableSpace::MaxContent => content.to_string(),
                    AvailableSpace::MinContent => Self::wrap_to_string(content, text_wrap, 1),
                },
            },
            TextWrap::NoWrap | TextWrap::End | TextWrap::Middle => content.to_string(),
        }
    }

    pub(crate) fn alignment_padding(line_width: usize, align: TextAlign, width: usize) -> isize {
        match align {
            TextAlign::Left => 0,
            TextAlign::Right => width as isize - line_width as isize,
            TextAlign::Center => width as isize / 2 - line_width as isize / 2,
        }
    }
}

/// Wraps or truncates text using the same modes as the [`Text`] component.
///
/// This is the Rust counterpart to CC Ink's exported `wrapText(...)` helper.
/// `TextWrap::Wrap` and `TextWrap::WrapTrim` hard-wrap using terminal display
/// width, while the truncate modes return a single ellipsized line per input
/// line. `TextWrap::NoWrap`, `TextWrap::End`, and `TextWrap::Middle` return
/// the input unchanged, matching the current CC Ink `wrapText(...)` helper.
pub fn wrap_text(text: &str, max_width: usize, wrap: TextWrap) -> String {
    if matches!(wrap, TextWrap::NoWrap | TextWrap::End | TextWrap::Middle) {
        return text.to_string();
    }
    Text::wrap_to_string(text, wrap, max_width)
}

pub(crate) struct TextDrawer<'a, 'b> {
    x_offset: isize,
    x: isize,
    y: isize,
    drawer: &'a mut ComponentDrawer<'b>,
    line_encountered_non_whitespace: bool,
    skip_leading_whitespace: bool,
    prev_line_content_end: usize,
}

impl<'a, 'b> TextDrawer<'a, 'b> {
    pub fn new(
        drawer: &'a mut ComponentDrawer<'b>,
        x_offset: isize,
        skip_leading_whitespace: bool,
    ) -> Self {
        TextDrawer {
            x_offset,
            x: x_offset,
            y: 0,
            drawer,
            line_encountered_non_whitespace: false,
            skip_leading_whitespace,
            prev_line_content_end: 0,
        }
    }

    pub fn append_lines<'c>(
        &mut self,
        lines: impl IntoIterator<Item = &'c str>,
        style: CanvasTextStyle,
    ) {
        self.append_lines_with_link(lines, style, None);
    }

    pub fn append_lines_with_link<'c>(
        &mut self,
        lines: impl IntoIterator<Item = &'c str>,
        style: CanvasTextStyle,
        hyperlink: Option<&str>,
    ) {
        self.append_lines_with_background_and_link(lines, style, None, hyperlink);
    }

    pub(crate) fn append_lines_with_background_and_link<'c>(
        &mut self,
        lines: impl IntoIterator<Item = &'c str>,
        style: CanvasTextStyle,
        background_color: Option<Color>,
        hyperlink: Option<&str>,
    ) {
        self.append_lines_with_soft_wrap_with_link(
            lines.into_iter().map(|line| (line, false)),
            style,
            background_color,
            hyperlink,
        );
    }

    pub(crate) fn mark_current_line_soft_wrap(&mut self) {
        self.drawer
            .canvas()
            .mark_soft_wrap_continuation(self.y, self.prev_line_content_end);
    }

    pub(crate) fn finish_line(&mut self) {
        self.y += 1;
        self.x = self.x_offset;
        self.line_encountered_non_whitespace = false;
    }

    pub(crate) fn set_prev_line_content_end(&mut self, content_end: usize) {
        self.prev_line_content_end = content_end;
    }

    fn append_lines_with_soft_wrap_with_link<'c>(
        &mut self,
        lines: impl IntoIterator<Item = (&'c str, bool)>,
        style: CanvasTextStyle,
        background_color: Option<Color>,
        hyperlink: Option<&str>,
    ) {
        let mut lines = lines.into_iter().peekable();
        while let Some((mut line, soft_continuation)) = lines.next() {
            if soft_continuation {
                self.drawer
                    .canvas()
                    .mark_soft_wrap_continuation(self.y, self.prev_line_content_end);
            }
            if self.skip_leading_whitespace && !self.line_encountered_non_whitespace {
                let to_skip = line
                    .chars()
                    .position(|c| !c.is_whitespace())
                    .unwrap_or(line.len());
                let (whitespace, remaining) = line.split_at(to_skip);
                self.x += crate::canvas::string_display_width_from_col(
                    whitespace,
                    self.x.max(0) as usize,
                ) as isize;
                line = remaining;
                if !line.is_empty() {
                    self.line_encountered_non_whitespace = true;
                }
            }
            let visual_line = crate::bidi::reorder_bidi_text_for_terminal(line);
            let line = visual_line.as_ref();
            let line_width =
                crate::canvas::string_display_width_from_col(line, self.x.max(0) as usize);
            if let Some(color) = background_color {
                self.drawer
                    .canvas()
                    .set_background_color(self.x, self.y, line_width, 1, color);
            }
            self.drawer
                .canvas()
                .set_text_with_link(self.x, self.y, line, style, hyperlink);
            let line_end = (self.x + line_width as isize).max(0);
            self.prev_line_content_end = line_end as usize;
            if lines.peek().is_some() {
                self.finish_line();
            } else {
                self.x += line_width as isize;
            }
        }
    }
}

impl Component for Text {
    type Props<'a> = TextProps;

    fn new(_props: &Self::Props<'_>) -> Self {
        Self::default()
    }

    fn update(
        &mut self,
        props: &mut Self::Props<'_>,
        _hooks: Hooks,
        updater: &mut ComponentUpdater,
    ) {
        self.style = CanvasTextStyle {
            color: props.color,
            weight: if props.dim {
                Weight::Light
            } else if props.bold {
                Weight::Bold
            } else {
                props.weight
            },
            underline: props.underline || props.decoration == TextDecoration::Underline,
            underline_style: UnderlineStyle::Single,
            underline_color: None,
            italic: props.italic,
            blink: false,
            hidden: false,
            strikethrough: props.strikethrough,
            overline: props.overline,
            invert: props.invert || props.inverse,
        };
        self.background_color = props.background_color;
        self.hyperlink = props.href.clone();
        self.content = strip_ansi(&props.content).into_owned();
        self.wrap = props.wrap;
        self.align = props.align;
        updater.set_measure_func(Self::measure_func(self.content.clone(), props.wrap));
    }

    fn draw(&mut self, drawer: &mut ComponentDrawer<'_>) {
        if drawer.zero_height_sibling_shares_y() {
            return;
        }
        let layout_width = drawer.layout().size.width;
        let width = if matches!(
            self.wrap,
            TextWrap::NoWrap | TextWrap::End | TextWrap::Middle
        ) {
            layout_width
        } else {
            layout_width.min(drawer.remaining_canvas_size().width as f32)
        };
        let wrapped_lines = Self::wrap_lines(&self.content, self.wrap, width as usize);
        let paddings = wrapped_lines
            .iter()
            .map(|line| {
                Self::alignment_padding(
                    crate::canvas::string_display_width(&line.text),
                    self.align,
                    width as _,
                )
            })
            .collect::<Vec<_>>();
        let x_offset = paddings.iter().copied().min().unwrap_or(0);
        let line_count = wrapped_lines.len();
        let mut drawer = TextDrawer::new(drawer, x_offset, self.align != TextAlign::Left);
        for (index, line) in wrapped_lines.into_iter().enumerate() {
            if line.soft_continuation {
                drawer.mark_current_line_soft_wrap();
            }
            let padding = paddings[index];
            let additional_padding = padding - x_offset;
            if additional_padding > 0 {
                drawer.append_lines(
                    [format!("{:width$}", "", width = additional_padding as usize).as_str()],
                    CanvasTextStyle::default(),
                );
            }
            drawer.append_lines_with_background_and_link(
                [line.text.as_str()],
                self.style,
                self.background_color,
                self.hyperlink.as_deref(),
            );
            drawer.set_prev_line_content_end((padding + line.content_end as isize).max(0) as usize);
            if index + 1 < line_count {
                drawer.finish_line();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::prelude::*;
    use crossterm::{csi, style::Attribute};
    use futures::StreamExt;
    use std::io::Write;

    #[test]
    fn test_wrap_text_helper_matches_text_modes() {
        assert_eq!(wrap_text("abcdef", 5, TextWrap::TruncateEnd), "abcd…");
        assert_eq!(wrap_text("abcdef", 5, TextWrap::TruncateStart), "…cdef");
        assert_eq!(wrap_text("abcdef", 5, TextWrap::TruncateMiddle), "ab…ef");
        assert_eq!(wrap_text("  abc def", 4, TextWrap::WrapTrim), "abc\ndef");
        assert_eq!(wrap_text("abcdef", 3, TextWrap::NoWrap), "abcdef");
        assert_eq!(wrap_text("abcdef", 3, TextWrap::End), "abcdef");
        assert_eq!(wrap_text("abcdef", 3, TextWrap::Middle), "abcdef");
    }

    #[test]
    fn test_text() {
        assert_eq!(element!(Text).to_string(), "");

        assert_eq!(element!(Text(content: "foo")).to_string(), "foo\n");

        assert_eq!(
            element!(Text(content: "foo\nbar")).to_string(),
            "foo\nbar\n"
        );

        assert_eq!(element!(Text(content: "foo\n")).to_string(), "foo\n\n");
        assert_eq!(
            element! {
                View(flex_direction: FlexDirection::Column) {
                    Text(content: "A")
                    Text(content: "")
                    Text(content: "B")
                }
            }
            .to_string(),
            "A\nB\n"
        );
        assert_eq!(
            element!(Text(content: "foo\n", wrap: TextWrap::NoWrap)).to_string(),
            "foo\n\n"
        );

        assert_eq!(element!(Text(content: "😀")).to_string(), "😀\n");

        assert_eq!(
            element! {
                View(width: 14) {
                    Text(content: "this is a wrapping test")
                }
            }
            .to_string(),
            "this is a\nwrapping test\n"
        );

        assert_eq!(
            element! {
                View(width: 2) {
                    Text(content: "☀️x")
                }
            }
            .to_string(),
            "☀️\nx\n"
        );

        assert_eq!(
            element! {
                View(width: 5, height: 2) {
                    View(width: 10) {
                        Text(content: "abcdef")
                    }
                }
            }
            .render(Some(5))
            .to_string(),
            "abcde\nf\n"
        );

        assert_eq!(
            element! {
                View(width: 15) {
                    Text(content: "this is an alignment test", align: TextAlign::Right)
                }
            }
            .to_string(),
            "     this is an\n alignment test\n"
        );

        assert_eq!(
            element! {
                View(width: 15) {
                    Text(content: "this is an alignment test", align: TextAlign::Center)
                }
            }
            .to_string(),
            "  this is an\nalignment test\n"
        );

        {
            let canvas = element!(Text(content: "bold", bold: true)).render(None);
            assert_eq!(
                canvas.resolved_text_style(0, 0).unwrap().weight,
                Weight::Bold
            );
        }

        {
            let canvas = element!(Text(content: "dim", bold: true, dim: true)).render(None);
            assert_eq!(
                canvas.resolved_text_style(0, 0).unwrap().weight,
                Weight::Light
            );
        }

        {
            let canvas = element!(Text(content: "under", underline: true)).render(None);
            assert!(canvas.resolved_text_style(0, 0).unwrap().underline);
        }

        {
            let canvas = element!(Text(content: "strike", strikethrough: true)).render(None);
            assert!(canvas.resolved_text_style(0, 0).unwrap().strikethrough);
        }

        {
            let canvas = element!(Text(content: "over", overline: true)).render(None);
            assert!(canvas.resolved_text_style(0, 0).unwrap().overline);
        }

        {
            let canvas = element!(Text(content: "inverse", inverse: true)).render(None);
            assert!(canvas.resolved_text_style(0, 0).unwrap().invert);
        }

        {
            let canvas =
                element!(Text(content: "bg", background_color: Some(Color::Blue))).render(None);
            assert_eq!(
                canvas.cell(0, 0).unwrap().background_color,
                Some(Color::Blue)
            );
            assert_eq!(
                canvas.cell(1, 0).unwrap().background_color,
                Some(Color::Blue)
            );
        }

        assert_eq!(
            element! {
                View(width: 5) {
                    Text(content: "abcdef", wrap: TextWrap::Truncate)
                }
            }
            .to_string(),
            "abcd…\n"
        );
        assert_eq!(
            element! {
                View(width: 5) {
                    Text(content: "abcdef", wrap: TextWrap::TruncateStart)
                }
            }
            .to_string(),
            "…cdef\n"
        );
        assert_eq!(
            element! {
                View(width: 5) {
                    Text(content: "abcdef", wrap: TextWrap::TruncateMiddle)
                }
            }
            .to_string(),
            "ab…ef\n"
        );
        assert_eq!(
            element! {
                View(width: 4) {
                    Text(content: "界abc", wrap: TextWrap::TruncateEnd)
                }
            }
            .to_string(),
            "界a…\n"
        );
        assert_eq!(
            element! {
                View(width: 8) {
                    Text(content: "a\tb")
                }
            }
            .to_string(),
            "a\nb\n"
        );
        assert_eq!(
            element! {
                View(width: 4) {
                    Text(content: "  abc def", wrap: TextWrap::WrapTrim)
                }
            }
            .to_string(),
            "abc\ndef\n"
        );

        // Make sure that when the text is not left-aligned, leading whitespace is not underlined.
        {
            let canvas = element! {
                View(width: 16) {
                    Text(content: "this is an alignment test", align: TextAlign::Center, decoration: TextDecoration::Underline)
                }
            }
            .render(None);
            let mut actual = Vec::new();
            canvas.write_ansi(&mut actual).unwrap();

            let mut expected = Vec::new();
            // row 0
            write!(expected, csi!("0m")).unwrap();
            write!(expected, "   ").unwrap();
            write!(expected, csi!("{}m"), Attribute::Underlined.sgr()).unwrap();
            write!(expected, "this is an").unwrap();
            // Full SGR reset before CSI K so underline doesn't bleed on kitty.
            write!(expected, csi!("0m")).unwrap();
            write!(expected, csi!("K")).unwrap();
            write!(expected, csi!("0m")).unwrap();
            write!(expected, "\r\n").unwrap();
            // row 1
            write!(expected, " ").unwrap();
            write!(expected, csi!("{}m"), Attribute::Underlined.sgr()).unwrap();
            write!(expected, "alignment test").unwrap();
            write!(expected, csi!("0m")).unwrap();
            write!(expected, csi!("K")).unwrap();
            write!(expected, csi!("0m")).unwrap();
            write!(expected, "\r\n").unwrap();

            assert_eq!(actual, expected);
        }
    }

    #[component]
    fn WrappedTextSoftWrapApp(hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        system.exit();
        element! {
            View(width: 5) {
                Text(content: "hello world")
            }
        }
    }

    #[test]
    fn test_text_marks_soft_wrap_for_selection_copy() {
        let canvases: Vec<_> = smol::block_on(
            element!(WrappedTextSoftWrapApp)
                .mock_terminal_render_loop(MockTerminalConfig::default())
                .collect(),
        );
        assert_eq!(canvases.len(), 1);
        assert_eq!(canvases[0].to_string(), "hello\nworld\n");
        assert_eq!(canvases[0].soft_wrap_continuation(1), 5);
        assert_eq!(
            canvases[0].selected_text(SelectionRange::new(
                SelectionPoint { col: 0, row: 0 },
                SelectionPoint { col: 4, row: 1 },
            )),
            "helloworld"
        );
    }

    #[component]
    fn WrappedTextSoftWrapWithSeparatorApp(hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        system.exit();
        element! {
            View(width: 6) {
                Text(content: "hello world")
            }
        }
    }

    #[test]
    fn test_text_soft_wrap_selection_preserves_word_separator_when_rendered() {
        let canvases: Vec<_> = smol::block_on(
            element!(WrappedTextSoftWrapWithSeparatorApp)
                .mock_terminal_render_loop(MockTerminalConfig::default())
                .collect(),
        );
        assert_eq!(canvases.len(), 1);
        assert_eq!(canvases[0].to_string(), "hello\nworld\n");
        assert_eq!(canvases[0].soft_wrap_continuation(1), 6);
        assert_eq!(
            canvases[0].selected_text(SelectionRange::new(
                SelectionPoint { col: 0, row: 0 },
                SelectionPoint { col: 5, row: 1 },
            )),
            "hello world",
            "softWrap content-end should preserve an actual rendered word-separator space while terminal output stays trimmed"
        );
    }

    #[test]
    fn test_text_strips_ansi() {
        assert_eq!(
            element!(Text(content: "\x1b[31mhello\x1b[0m")).to_string(),
            "hello\n"
        );

        assert_eq!(
            element! {
                View(width: 10) {
                    Text(content: "\x1b[1mthis is\x1b[0m a wrap test")
                }
            }
            .to_string(),
            "this is a\nwrap test\n"
        );

        assert_eq!(
            element!(Text(content: "no ansi here")).to_string(),
            "no ansi here\n"
        );
    }

    #[test]
    fn test_text_invert() {
        let canvas = element!(Text(content: "foo", invert: true)).render(None);
        assert!(canvas.cell(0, 0).unwrap().text_style().unwrap().invert);
    }

    #[test]
    fn test_alignment_no_wrap_overflow() {
        assert_eq!(
            element! {
                View(
                    flex_direction: FlexDirection::Column,
                    width: 9,
                ) {
                    Text(
                        content: "123456789abcdef",
                        align: TextAlign::Left,
                        wrap: TextWrap::NoWrap
                    )
                }
            }
            .to_string(),
            "123456789\n"
        );

        assert_eq!(
            element! {
                View(
                    flex_direction: FlexDirection::Column,
                    width: 9,
                ) {
                    Text(
                        content: "123456789abcdef",
                        align: TextAlign::Center,
                        wrap: TextWrap::NoWrap
                    )
                }
            }
            .to_string(),
            "456789abc\n"
        );

        assert_eq!(
            element! {
                View(
                    flex_direction: FlexDirection::Column,
                    width: 9,
                ) {
                    Text(
                        content: "123456789abcdef\n1",
                        align: TextAlign::Center,
                        wrap: TextWrap::NoWrap
                    )
                }
            }
            .to_string(),
            "456789abc\n    1\n"
        );

        // If we expand the outer view, we should be able to see some of the overflowing text.
        assert_eq!(
            element! {
                View(width: 20, padding_left: 2) {
                    View(
                        flex_direction: FlexDirection::Column,
                        width: 9,
                    ) {
                        Text(
                            content: "123456789abcdef",
                            align: TextAlign::Center,
                            wrap: TextWrap::NoWrap
                        )
                    }
                }
            }
            .to_string(),
            "23456789abcdef\n"
        );

        assert_eq!(
            element! {
                View(
                    flex_direction: FlexDirection::Column,
                    width: 9,
                ) {
                    Text(
                        content: "123456789abcdef",
                        align: TextAlign::Right,
                        wrap: TextWrap::NoWrap
                    )
                }
            }
            .to_string(),
            "789abcdef\n"
        );
    }
}
