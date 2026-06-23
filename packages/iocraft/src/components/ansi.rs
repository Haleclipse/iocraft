use crate::{
    components::{
        raw_ansi::parse_ansi, MixedText, MixedTextContent, TextAlign, TextDecoration, TextWrap,
    },
    element, AnyElement, Color, Props, Weight,
};

/// Props for [`Ansi`].
#[non_exhaustive]
#[derive(Default, Props)]
pub struct AnsiProps {
    /// ANSI-formatted text to parse and render.
    pub content: String,

    /// Force dim styling for all parsed text, matching CC Ink's `dimColor` prop.
    pub dim_color: bool,

    /// Default text color inherited by ANSI spans without an explicit SGR color.
    pub color: Option<Color>,

    /// The text wrapping behavior.
    pub wrap: TextWrap,

    /// The text alignment.
    pub align: TextAlign,
}

/// Parses ANSI escape sequences and renders them as styled text segments.
///
/// This is the iocraft counterpart to CC Ink's `<Ansi>` helper. It is useful
/// when an external producer emits ANSI-styled strings but the UI should still
/// participate in normal layout, wrapping, selection, search, and OSC 8
/// hyperlink metadata.
#[crate::component]
pub fn Ansi(props: &AnsiProps) -> impl Into<AnyElement<'static>> {
    let contents = parse_ansi(&props.content)
        .into_iter()
        .map(|run| {
            let mut content = MixedTextContent::new(run.text);
            content.color = run.style.color.or(props.color);
            content.background_color = run.background_color;
            content.weight = if props.dim_color {
                Weight::Light
            } else {
                run.style.weight
            };
            content.decoration = if run.style.underline {
                TextDecoration::Underline
            } else {
                TextDecoration::None
            };
            content.italic = run.style.italic;
            content.strikethrough = run.style.strikethrough;
            content.overline = run.style.overline;
            content.invert = run.style.invert;
            content.href = run.hyperlink;
            content
        })
        .collect::<Vec<_>>();

    element!(MixedText(contents, wrap: props.wrap, align: props.align))
}

#[cfg(test)]
mod tests {
    use crate::prelude::*;

    #[test]
    fn test_ansi_component_parses_styles_and_wraps() {
        let canvas = element! {
            View(width: 7) {
                Ansi(content: "\x1b[31;1mred\x1b[0m plain")
            }
        }
        .render(None);

        assert_eq!(canvas.to_string(), "red\nplain\n");
        let red = canvas.resolved_text_style(0, 0).unwrap();
        assert_eq!(red.color, Some(Color::DarkRed));
        assert_eq!(red.weight, Weight::Bold);
        assert_eq!(canvas.resolved_text_style(0, 1).unwrap().color, None);

        let canvas = element!(Ansi(content: "\x1b[53mover\x1b[55m plain".to_string())).render(None);
        assert!(canvas.resolved_text_style(0, 0).unwrap().overline);
        assert!(!canvas.resolved_text_style(5, 0).unwrap().overline);
    }

    #[test]
    fn test_ansi_component_uses_default_color_until_sgr_overrides() {
        let canvas = element!(Ansi(
            color: Some(Color::Blue),
            content: "base \x1b[31mred\x1b[0m base".to_string(),
        ))
        .render(None);

        assert_eq!(canvas.to_string(), "base red base\n");
        assert_eq!(
            canvas.resolved_text_style(0, 0).unwrap().color,
            Some(Color::Blue)
        );
        assert_eq!(
            canvas.resolved_text_style(5, 0).unwrap().color,
            Some(Color::DarkRed)
        );
        assert_eq!(
            canvas.resolved_text_style(9, 0).unwrap().color,
            Some(Color::Blue)
        );
    }

    #[test]
    fn test_ansi_component_dim_color_and_osc8_links() {
        let canvas = element!(Ansi(
            dim_color: true,
            content: "\x1b]8;;https://example.com\x07link\x1b]8;;\x07".to_string(),
        ))
        .render(None);

        assert_eq!(canvas.to_string(), "link\n");
        assert_eq!(
            canvas.resolved_text_style(0, 0).unwrap().weight,
            Weight::Light
        );
        assert_eq!(
            canvas.hyperlink_at(0, 0).as_deref(),
            Some("https://example.com")
        );
    }

    #[test]
    fn test_ansi_component_keeps_osc8_link_across_sgr_reset() {
        let canvas = element!(Ansi(
            content: "\x1b]8;;https://example.com\x07\x1b[31mred\x1b[0m plain\x1b]8;;\x07".to_string(),
        ))
        .render(None);

        assert_eq!(canvas.to_string(), "red plain\n");
        assert_eq!(
            canvas.resolved_text_style(0, 0).unwrap().color,
            Some(Color::DarkRed)
        );
        assert_eq!(canvas.resolved_text_style(4, 0).unwrap().color, None);
        assert_eq!(
            canvas.hyperlink_at(4, 0).as_deref(),
            Some("https://example.com")
        );
    }
}
