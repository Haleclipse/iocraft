//! Software bidirectional-text reordering for terminals that lack native bidi.
//!
//! CC Ink only applies this workaround on Windows Terminal/conhost and xterm.js
//! hosts. macOS terminal emulators usually implement the Unicode bidi algorithm
//! natively, so reordering there would double-apply it.

use std::{borrow::Cow, env};

use crate::terminal;
use unicode_bidi::BidiInfo;
use unicode_segmentation::UnicodeSegmentation;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BidiGrapheme<T> {
    pub(crate) text: String,
    pub(crate) metadata: T,
}

fn has_rtl_characters(text: &str) -> bool {
    text.chars().any(|ch| {
        matches!(
            ch as u32,
            0x0590..=0x05ff
                | 0xfb1d..=0xfb4f
                | 0x0600..=0x06ff
                | 0x0750..=0x077f
                | 0x08a0..=0x08ff
                | 0xfb50..=0xfdff
                | 0xfe70..=0xfeff
                | 0x0780..=0x07bf
                | 0x0700..=0x074f
        )
    })
}

pub(crate) fn needs_software_bidi() -> bool {
    cfg!(windows) || env::var_os("WT_SESSION").is_some() || terminal::is_xterm_js()
}

pub(crate) fn reorder_bidi_graphemes<T: Clone>(
    graphemes: Vec<BidiGrapheme<T>>,
    enabled: bool,
) -> Vec<BidiGrapheme<T>> {
    if !enabled || graphemes.is_empty() {
        return graphemes;
    }

    let text = graphemes
        .iter()
        .map(|item| item.text.as_str())
        .collect::<String>();
    if !has_rtl_characters(&text) {
        return graphemes;
    }

    let info = BidiInfo::new(&text, None);
    let Some(paragraph) = info.paragraphs.first() else {
        return graphemes;
    };
    let levels_per_char = info.reordered_levels_per_char(paragraph, paragraph.range.clone());
    if levels_per_char.is_empty() {
        return graphemes;
    }

    let mut char_index = 0usize;
    let mut levels = Vec::with_capacity(graphemes.len());
    for grapheme in &graphemes {
        let level = levels_per_char
            .get(char_index)
            .copied()
            .unwrap_or(paragraph.level);
        levels.push(level);
        char_index += grapheme.text.chars().count().max(1);
    }

    if !levels.iter().any(|level| level.is_rtl()) {
        return graphemes;
    }

    BidiInfo::reorder_visual(&levels)
        .into_iter()
        .filter_map(|index| graphemes.get(index).cloned())
        .collect()
}

pub(crate) fn reorder_bidi_text_for_terminal(text: &str) -> Cow<'_, str> {
    reorder_bidi_text(text, needs_software_bidi())
}

pub(crate) fn reorder_bidi_text(text: &str, enabled: bool) -> Cow<'_, str> {
    if !enabled || text.is_empty() || !has_rtl_characters(text) {
        return Cow::Borrowed(text);
    }

    let graphemes = text
        .graphemes(true)
        .map(|text| BidiGrapheme {
            text: text.to_string(),
            metadata: (),
        })
        .collect::<Vec<_>>();
    let reordered = reorder_bidi_graphemes(graphemes, true);
    let rendered = reordered
        .into_iter()
        .map(|item| item.text)
        .collect::<String>();
    if rendered == text {
        Cow::Borrowed(text)
    } else {
        Cow::Owned(rendered)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bidi_reorders_rtl_runs_like_cc_ink() {
        assert_eq!(reorder_bidi_text("אבגabc", true).as_ref(), "abcגבא");
    }

    #[test]
    fn bidi_preserves_grapheme_metadata() {
        let items = vec![
            BidiGrapheme {
                text: "א".to_string(),
                metadata: 1,
            },
            BidiGrapheme {
                text: "ב".to_string(),
                metadata: 2,
            },
            BidiGrapheme {
                text: "ג".to_string(),
                metadata: 3,
            },
            BidiGrapheme {
                text: "a".to_string(),
                metadata: 4,
            },
            BidiGrapheme {
                text: "b".to_string(),
                metadata: 5,
            },
            BidiGrapheme {
                text: "c".to_string(),
                metadata: 6,
            },
        ];

        let reordered = reorder_bidi_graphemes(items, true);
        assert_eq!(
            reordered
                .iter()
                .map(|item| item.text.as_str())
                .collect::<String>(),
            "abcגבא"
        );
        assert_eq!(
            reordered
                .iter()
                .map(|item| item.metadata)
                .collect::<Vec<_>>(),
            vec![4, 5, 6, 3, 2, 1]
        );
    }

    #[test]
    fn bidi_is_noop_when_disabled_or_ltr_only() {
        assert!(matches!(
            reorder_bidi_text("אבגabc", false),
            Cow::Borrowed(_)
        ));
        assert!(matches!(reorder_bidi_text("abc", true), Cow::Borrowed(_)));
    }
}
