#![allow(unused_imports)]
use super::super::*;
use crate::prelude::*;
use crossterm::{csi, style::Colored};

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
