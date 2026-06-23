use crate::{
    canvas::UnderlineStyle,
    components::text::{Text, TextAlign, TextDecoration, TextDrawer, TextWrap},
    segmented_string::SegmentedString,
    strip_ansi::strip_ansi,
    CanvasTextStyle, Color, Component, ComponentDrawer, ComponentUpdater, Hooks, Props, Weight,
};

/// A section of text in a [`MixedText`] component.
#[non_exhaustive]
#[derive(Default, Clone)]
pub struct MixedTextContent {
    /// The text to display.
    pub text: String,

    /// The color to make the text.
    pub color: Option<Color>,

    /// The background color to paint behind this segment's rendered text cells.
    pub background_color: Option<Color>,

    /// The weight of the text.
    pub weight: Weight,

    /// The text decoration.
    pub decoration: TextDecoration,

    /// Whether to italicize the text.
    pub italic: bool,

    /// Whether to strike through the text.
    pub strikethrough: bool,

    /// Whether to draw an overline above the text.
    pub overline: bool,

    /// Whether to invert the text's foreground and background colors.
    pub invert: bool,

    /// Optional OSC 8 hyperlink target for this segment.
    pub href: Option<String>,
}

impl MixedTextContent {
    /// Creates a new [`MixedTextContent`] with the given text.
    pub fn new<S: ToString>(text: S) -> Self {
        Self {
            text: text.to_string(),
            ..Default::default()
        }
    }

    /// Returns a new [`MixedTextContent`] with the given color.
    pub fn color(mut self, color: Color) -> Self {
        self.color = Some(color);
        self
    }

    /// Returns a new [`MixedTextContent`] with the given background color.
    pub fn background_color(mut self, color: Color) -> Self {
        self.background_color = Some(color);
        self
    }

    /// Returns a new [`MixedTextContent`] with the given weight.
    pub fn weight(mut self, weight: Weight) -> Self {
        self.weight = weight;
        self
    }

    /// Returns a new [`MixedTextContent`] with the given text decoration.
    pub fn decoration(mut self, decoration: TextDecoration) -> Self {
        self.decoration = decoration;
        self
    }

    /// Returns a new [`MixedTextContent`] with italic text.
    pub fn italic(mut self) -> Self {
        self.italic = true;
        self
    }

    /// Returns a new [`MixedTextContent`] with strikethrough text.
    pub fn strikethrough(mut self) -> Self {
        self.strikethrough = true;
        self
    }

    /// Returns a new [`MixedTextContent`] with overlined text.
    pub fn overline(mut self) -> Self {
        self.overline = true;
        self
    }

    /// Returns a new [`MixedTextContent`] with inverted foreground and background colors.
    pub fn invert(mut self) -> Self {
        self.invert = true;
        self
    }

    /// Returns a new [`MixedTextContent`] with an OSC 8 hyperlink target.
    pub fn href(mut self, href: impl ToString) -> Self {
        self.href = Some(href.to_string());
        self
    }
}

/// The props which can be passed to the [`MixedText`] component.
#[non_exhaustive]
#[derive(Default, Props)]
pub struct MixedTextProps {
    /// The contents of the text.
    pub contents: Vec<MixedTextContent>,

    /// The text wrapping behavior.
    pub wrap: TextWrap,

    /// The text alignment.
    pub align: TextAlign,
}

/// `MixedText` is a component that renders a text string containing a mix of styles.
///
/// If you want to render a text string with a single style, use the [`Text`] component instead.
///
/// # Example
///
/// ```
/// # use iocraft::prelude::*;
/// # fn my_element() -> impl Into<AnyElement<'static>> {
/// element! {
///     View(
///         border_style: BorderStyle::Round,
///         border_color: Color::Blue,
///         width: 30,
///     ) {
///         MixedText(align: TextAlign::Center, contents: vec![
///             MixedTextContent::new("Hello, world!").color(Color::Red).weight(Weight::Bold),
///             MixedTextContent::new(" Lorem ipsum odor amet, consectetuer adipiscing elit.").color(Color::Green),
///         ])
///     }
/// }
/// # }
/// ```
#[derive(Default)]
pub struct MixedText {
    contents: Vec<MixedTextContent>,
    wrap: TextWrap,
    align: TextAlign,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RenderedMixedTextSegment {
    text: String,
    index: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RenderedMixedTextLine {
    segments: Vec<RenderedMixedTextSegment>,
    width: usize,
    soft_continuation: bool,
    content_end: usize,
}

impl MixedText {
    fn line_is_soft_continuation(
        contents: &[MixedTextContent],
        line: &crate::segmented_string::SegmentedStringLine<'_>,
    ) -> bool {
        let Some(first_segment) = line.segments.first() else {
            return false;
        };
        if first_segment.offset > 0 {
            return !contents[first_segment.index].text[..first_segment.offset].ends_with('\n');
        }
        for index in (0..first_segment.index).rev() {
            let text = &contents[index].text;
            if text.is_empty() {
                continue;
            }
            return !text.ends_with('\n');
        }
        false
    }

    fn push_rendered_segment(
        segments: &mut Vec<RenderedMixedTextSegment>,
        index: usize,
        text: impl AsRef<str>,
    ) {
        let text = text.as_ref();
        if text.is_empty() {
            return;
        }
        if let Some(last) = segments.last_mut().filter(|last| last.index == index) {
            last.text.push_str(text);
        } else {
            segments.push(RenderedMixedTextSegment {
                text: text.to_string(),
                index,
            });
        }
    }

    fn rendered_width(segments: &[RenderedMixedTextSegment]) -> usize {
        let mut col = 0;
        for segment in segments {
            col += crate::canvas::string_display_width_from_col(&segment.text, col);
        }
        col
    }

    fn trim_start_segments(segments: &mut Vec<RenderedMixedTextSegment>) {
        while let Some(first) = segments.first_mut() {
            let trimmed = first.text.trim_start();
            if trimmed.is_empty() {
                segments.remove(0);
            } else {
                if trimmed.len() != first.text.len() {
                    first.text = trimmed.to_string();
                }
                break;
            }
        }
    }

    fn trim_end_segments(segments: &mut Vec<RenderedMixedTextSegment>) {
        while let Some(last) = segments.last_mut() {
            let trimmed = last.text.trim_end();
            if trimmed.is_empty() {
                segments.pop();
            } else {
                if trimmed.len() != last.text.len() {
                    last.text = trimmed.to_string();
                }
                break;
            }
        }
    }

    fn segment_index_at_column(segments: &[RenderedMixedTextSegment], column: usize) -> usize {
        let mut current = 0;
        let mut fallback = 0;
        for segment in segments {
            fallback = segment.index;
            let next =
                current + crate::canvas::string_display_width_from_col(&segment.text, current);
            if column < next {
                return segment.index;
            }
            current = next;
        }
        fallback
    }

    fn slice_segments_by_columns(
        segments: &[RenderedMixedTextSegment],
        start: usize,
        end: usize,
    ) -> Vec<RenderedMixedTextSegment> {
        if start >= end {
            return Vec::new();
        }

        let mut ret = Vec::new();
        let mut col = 0;
        for segment in segments {
            for grapheme in
                unicode_segmentation::UnicodeSegmentation::graphemes(segment.text.as_str(), true)
            {
                let width = crate::canvas::string_display_width_from_col(grapheme, col);
                let next = col + width;
                if next <= start {
                    col = next;
                    continue;
                }
                if col >= end {
                    break;
                }
                // Keep only complete graphemes so wide CJK/emoji never overflow
                // the target range, matching CC Ink's sliceFit retry behavior.
                if col >= start && next <= end {
                    Self::push_rendered_segment(&mut ret, segment.index, grapheme);
                }
                col = next;
            }
            if col >= end {
                break;
            }
        }
        ret
    }

    fn truncate_segments(
        segments: &[RenderedMixedTextSegment],
        columns: usize,
        wrap: TextWrap,
    ) -> Vec<RenderedMixedTextSegment> {
        const ELLIPSIS: &str = "…";
        let width = Self::rendered_width(segments);
        if width <= columns {
            return segments.to_vec();
        }
        if columns < 1 {
            return Vec::new();
        }
        if columns == 1 {
            let index = Self::segment_index_at_column(segments, 0);
            return vec![RenderedMixedTextSegment {
                text: ELLIPSIS.to_string(),
                index,
            }];
        }

        let mut ret = Vec::new();
        match wrap {
            TextWrap::TruncateStart => {
                let suffix_columns = columns - 1;
                let start = width - suffix_columns;
                let index = Self::segment_index_at_column(segments, start);
                Self::push_rendered_segment(&mut ret, index, ELLIPSIS);
                ret.extend(Self::slice_segments_by_columns(segments, start, width));
            }
            TextWrap::TruncateMiddle => {
                let prefix_columns = columns / 2;
                let suffix_columns = columns - prefix_columns - 1;
                ret.extend(Self::slice_segments_by_columns(segments, 0, prefix_columns));
                let index = Self::segment_index_at_column(segments, prefix_columns);
                Self::push_rendered_segment(&mut ret, index, ELLIPSIS);
                ret.extend(Self::slice_segments_by_columns(
                    segments,
                    width - suffix_columns,
                    width,
                ));
            }
            TextWrap::Truncate | TextWrap::TruncateEnd => {
                let prefix_columns = columns - 1;
                ret.extend(Self::slice_segments_by_columns(segments, 0, prefix_columns));
                let index = Self::segment_index_at_column(segments, prefix_columns);
                Self::push_rendered_segment(&mut ret, index, ELLIPSIS);
            }
            _ => ret.extend_from_slice(segments),
        }
        ret
    }

    fn style_for_content(content: &MixedTextContent) -> CanvasTextStyle {
        CanvasTextStyle {
            color: content.color,
            weight: content.weight,
            underline: content.decoration == TextDecoration::Underline,
            underline_style: UnderlineStyle::Single,
            underline_color: None,
            italic: content.italic,
            blink: false,
            hidden: false,
            strikethrough: content.strikethrough,
            overline: content.overline,
            invert: content.invert,
        }
    }

    fn reorder_bidi_segments(
        segments: Vec<RenderedMixedTextSegment>,
        enabled: bool,
    ) -> Vec<RenderedMixedTextSegment> {
        if segments.is_empty() {
            return segments;
        }

        let mut graphemes = Vec::new();
        for segment in segments {
            for grapheme in
                unicode_segmentation::UnicodeSegmentation::graphemes(segment.text.as_str(), true)
            {
                graphemes.push(crate::bidi::BidiGrapheme {
                    text: grapheme.to_string(),
                    metadata: segment.index,
                });
            }
        }

        let reordered = crate::bidi::reorder_bidi_graphemes(graphemes, enabled);
        let mut ret = Vec::new();
        for grapheme in reordered {
            Self::push_rendered_segment(&mut ret, grapheme.metadata, grapheme.text);
        }
        ret
    }

    fn reorder_bidi_segments_for_terminal(
        segments: Vec<RenderedMixedTextSegment>,
    ) -> Vec<RenderedMixedTextSegment> {
        if crate::bidi::needs_software_bidi() {
            Self::reorder_bidi_segments(segments, true)
        } else {
            segments
        }
    }
}

impl Component for MixedText {
    type Props<'a> = MixedTextProps;

    fn new(_props: &Self::Props<'_>) -> Self {
        Self::default()
    }

    fn update(
        &mut self,
        props: &mut Self::Props<'_>,
        _hooks: Hooks,
        updater: &mut ComponentUpdater,
    ) {
        for content in props.contents.iter_mut() {
            content.text = strip_ansi(&content.text).into_owned();
        }
        let plaintext = props
            .contents
            .iter()
            .map(|content| content.text.as_str())
            .collect::<Vec<_>>()
            .join("");
        self.contents = props.contents.clone();
        self.wrap = props.wrap;
        self.align = props.align;
        updater.set_measure_func(Text::measure_func(plaintext, props.wrap));
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
        let segmented_string: SegmentedString = self
            .contents
            .iter()
            .map(|content| content.text.as_str())
            .collect();
        let lines = segmented_string.wrap(match self.wrap {
            TextWrap::Wrap | TextWrap::WrapTrim => width as usize,
            TextWrap::NoWrap
            | TextWrap::End
            | TextWrap::Middle
            | TextWrap::Truncate
            | TextWrap::TruncateEnd
            | TextWrap::TruncateMiddle
            | TextWrap::TruncateStart => usize::MAX,
        });

        let soft_continuations = lines
            .iter()
            .map(|line| Self::line_is_soft_continuation(&self.contents, line))
            .collect::<Vec<_>>();
        let source_line_count = lines.len();
        let mut rendered_lines = lines
            .into_iter()
            .enumerate()
            .map(|(index, line)| {
                let original_width = line.width;
                let mut segments = line
                    .segments
                    .into_iter()
                    .map(|segment| RenderedMixedTextSegment {
                        text: segment.text.to_string(),
                        index: segment.index,
                    })
                    .collect::<Vec<_>>();

                match self.wrap {
                    TextWrap::Wrap => Self::trim_end_segments(&mut segments),
                    TextWrap::WrapTrim => {
                        Self::trim_start_segments(&mut segments);
                        Self::trim_end_segments(&mut segments);
                    }
                    TextWrap::Truncate
                    | TextWrap::TruncateEnd
                    | TextWrap::TruncateMiddle
                    | TextWrap::TruncateStart => {
                        segments = Self::truncate_segments(&segments, width as usize, self.wrap);
                    }
                    TextWrap::NoWrap | TextWrap::End | TextWrap::Middle => {}
                }

                segments = Self::reorder_bidi_segments_for_terminal(segments);
                let rendered_width = Self::rendered_width(&segments);
                let content_end = if self.wrap == TextWrap::Wrap
                    && index + 1 < source_line_count
                    && soft_continuations[index + 1]
                {
                    original_width
                } else {
                    rendered_width
                };
                RenderedMixedTextLine {
                    segments,
                    width: rendered_width,
                    soft_continuation: soft_continuations[index],
                    content_end,
                }
            })
            .collect::<Vec<_>>();
        if self.wrap == TextWrap::WrapTrim {
            rendered_lines.retain(|line| line.width > 0);
        }
        let line_count = rendered_lines.len();
        let paddings = rendered_lines
            .iter()
            .map(|line| Text::alignment_padding(line.width, self.align, width as _))
            .collect::<Vec<_>>();
        let x_offset = paddings.iter().copied().min().unwrap_or(0);

        let mut drawer = TextDrawer::new(drawer, x_offset, self.align != TextAlign::Left);
        for (line_index, (line, padding)) in rendered_lines.into_iter().zip(paddings).enumerate() {
            if line.soft_continuation {
                drawer.mark_current_line_soft_wrap();
            }

            let additional_padding = padding - x_offset;
            if additional_padding > 0 {
                drawer.append_lines(
                    [format!("{:width$}", "", width = additional_padding as usize).as_str()],
                    CanvasTextStyle::default(),
                );
            }
            for segment in line.segments {
                let content = &self.contents[segment.index];
                drawer.append_lines_with_background_and_link(
                    [segment.text.as_str()],
                    Self::style_for_content(content),
                    content.background_color,
                    content.href.as_deref(),
                );
            }
            drawer.set_prev_line_content_end((padding + line.content_end as isize).max(0) as usize);
            if line_index + 1 < line_count {
                drawer.finish_line();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prelude::*;
    use futures::StreamExt;

    #[test]
    fn test_mixed_text() {
        assert_eq!(element!(MixedText).to_string(), "");

        assert_eq!(
            element! {
                View(width: 14) {
                    MixedText(contents: vec![
                        MixedTextContent::new("this is ").color(Color::Red).weight(Weight::Bold).italic(),
                        MixedTextContent::new("a wrapping test").decoration(TextDecoration::Underline),
                    ])
                }
            }
            .to_string(),
            "this is a\nwrapping test\n"
        );

        assert_eq!(
            element! {
                View(width: 8) {
                    MixedText(contents: vec![
                        MixedTextContent::new("a"),
                        MixedTextContent::new("\tb"),
                    ])
                }
            }
            .to_string(),
            "a\nb\n"
        );
    }

    #[component]
    fn MixedTextSoftWrapApp(hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        system.exit();
        element! {
            View(width: 6) {
                MixedText(contents: vec![
                    MixedTextContent::new("hello ").color(Color::Red),
                    MixedTextContent::new("world").weight(Weight::Bold),
                ])
            }
        }
    }

    #[test]
    fn test_mixed_text_marks_soft_wrap_for_selection_copy() {
        let canvases: Vec<_> = smol::block_on(
            element!(MixedTextSoftWrapApp)
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
            "hello world"
        );
    }

    #[test]
    fn test_mixed_text_invert() {
        let canvas = element! {
            MixedText(contents: vec![
                MixedTextContent::new("foo").invert(),
            ])
        }
        .render(None);
        assert!(canvas.cell(0, 0).unwrap().text_style().unwrap().invert);
    }

    #[test]
    fn test_mixed_text_strikethrough() {
        let canvas = element! {
            MixedText(contents: vec![
                MixedTextContent::new("foo").strikethrough(),
            ])
        }
        .render(None);
        assert!(
            canvas
                .cell(0, 0)
                .unwrap()
                .text_style()
                .unwrap()
                .strikethrough
        );
    }

    #[test]
    fn test_mixed_text_overline() {
        let canvas = element! {
            MixedText(contents: vec![
                MixedTextContent::new("foo").overline(),
            ])
        }
        .render(None);
        assert!(canvas.cell(0, 0).unwrap().text_style().unwrap().overline);
    }

    #[test]
    fn test_mixed_text_background_color() {
        let canvas = element! {
            MixedText(contents: vec![
                MixedTextContent::new("foo").background_color(Color::Blue),
            ])
        }
        .render(None);
        assert_eq!(
            canvas.cell(0, 0).unwrap().background_color,
            Some(Color::Blue)
        );
        assert_eq!(
            canvas.cell(2, 0).unwrap().background_color,
            Some(Color::Blue)
        );
    }

    #[test]
    fn test_mixed_text_truncate_preserves_segment_styles() {
        let canvas = element! {
            View(width: 5) {
                MixedText(wrap: TextWrap::TruncateMiddle, contents: vec![
                    MixedTextContent::new("abc").color(Color::Red),
                    MixedTextContent::new("def").color(Color::Green),
                ])
            }
        }
        .render(None);

        assert_eq!(canvas.to_string(), "ab…ef\n");
        assert_eq!(
            canvas.resolved_text_style(0, 0).unwrap().color,
            Some(Color::Red)
        );
        assert_eq!(
            canvas.resolved_text_style(4, 0).unwrap().color,
            Some(Color::Green)
        );
    }

    #[test]
    fn test_mixed_text_legacy_end_middle_wrap_modes_are_noops_like_cc_ink() {
        assert_eq!(
            element! {
                View(width: 3) {
                    MixedText(wrap: TextWrap::End, contents: vec![
                        MixedTextContent::new("abcdef"),
                    ])
                }
            }
            .to_string(),
            "abc\n"
        );
        assert_eq!(
            element! {
                View(width: 3) {
                    MixedText(wrap: TextWrap::Middle, contents: vec![
                        MixedTextContent::new("abcdef"),
                    ])
                }
            }
            .to_string(),
            "abc\n"
        );
    }

    #[test]
    fn test_mixed_text_bidi_reorder_preserves_segment_metadata() {
        let reordered = MixedText::reorder_bidi_segments(
            vec![
                RenderedMixedTextSegment {
                    text: "אבג".to_string(),
                    index: 0,
                },
                RenderedMixedTextSegment {
                    text: "abc".to_string(),
                    index: 1,
                },
            ],
            true,
        );

        assert_eq!(
            reordered
                .iter()
                .map(|segment| segment.text.as_str())
                .collect::<String>(),
            "abcגבא"
        );
        assert_eq!(
            reordered
                .iter()
                .map(|segment| segment.index)
                .collect::<Vec<_>>(),
            vec![1, 0]
        );
    }

    #[test]
    fn test_mixed_text_wrap_trim() {
        assert_eq!(
            element! {
                View(width: 4) {
                    MixedText(wrap: TextWrap::WrapTrim, contents: vec![
                        MixedTextContent::new("  abc ").color(Color::Red),
                        MixedTextContent::new(" def").color(Color::Green),
                    ])
                }
            }
            .to_string(),
            "abc\ndef\n"
        );
    }

    #[test]
    fn test_mixed_text_wrap_clamps_to_visible_canvas_width_like_cc_ink() {
        assert_eq!(
            element! {
                View(width: 5, height: 2) {
                    View(width: 10) {
                        MixedText(contents: vec![
                            MixedTextContent::new("abcdef").color(Color::Red),
                        ])
                    }
                }
            }
            .render(Some(5))
            .to_string(),
            "abcde\nf\n"
        );
    }

    #[test]
    fn test_mixed_text_strips_ansi() {
        assert_eq!(
            element! {
                View(width: 14) {
                    MixedText(contents: vec![
                        MixedTextContent::new("\x1b[31mthis is \x1b[0m"),
                        MixedTextContent::new("\x1b[1ma wrapping test\x1b[0m"),
                    ])
                }
            }
            .to_string(),
            "this is a\nwrapping test\n"
        );
    }
}
