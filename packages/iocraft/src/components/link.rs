use crate::{component, components::Text, element, AnyElement, Props};

/// Props for [`Link`].
#[non_exhaustive]
#[derive(Default, Props)]
pub struct LinkProps {
    /// Hyperlink target.
    pub url: String,

    /// Text to display. Defaults to [`Self::url`].
    pub label: Option<String>,

    /// Text to display when hyperlinks are disabled. Defaults to the label/url.
    pub fallback: Option<String>,

    /// Whether to emit OSC 8 hyperlink metadata.
    ///
    /// Defaults to CC Ink-style terminal support detection. Set `Some(true)` to
    /// force OSC 8 metadata or `Some(false)` to force the fallback text.
    pub enabled: Option<bool>,
}

/// Renders text with OSC 8 hyperlink metadata.
///
/// This is the iocraft counterpart to CC Ink's `<Link>` helper. It wraps
/// [`Text`] with `href` when enabled so fullscreen click handling and terminal
/// hyperlink support share the same screen-buffer metadata.
#[component]
pub fn Link(props: &LinkProps) -> impl Into<AnyElement<'static>> {
    let label = props.label.clone().unwrap_or_else(|| props.url.clone());
    let enabled = props
        .enabled
        .unwrap_or_else(crate::ansi::supports_hyperlinks);
    if enabled && !props.url.is_empty() {
        element!(Text(content: label, href: props.url.clone()))
    } else {
        element!(Text(content: props.fallback.clone().unwrap_or(label)))
    }
}

#[cfg(test)]
mod tests {
    use crate::prelude::*;

    #[test]
    fn test_link_renders_osc8_hyperlink_metadata() {
        let mut link = element!(Link(
            url: "https://example.com".to_string(),
            label: Some("docs".to_string()),
            enabled: Some(true),
        ));
        let canvas = link.render(None);

        assert_eq!(canvas.to_string(), "docs\n");
        assert_eq!(
            canvas.hyperlink_at(1, 0).as_deref(),
            Some("https://example.com")
        );
    }

    #[test]
    fn test_link_fallback_disables_hyperlink_metadata() {
        let mut link = element!(Link(
            url: "https://example.com".to_string(),
            label: Some("docs".to_string()),
            fallback: Some("plain docs".to_string()),
            enabled: Some(false),
        ));
        let canvas = link.render(None);

        assert_eq!(canvas.to_string(), "plain docs\n");
        assert_eq!(canvas.hyperlink_at(1, 0), None);
    }
}
