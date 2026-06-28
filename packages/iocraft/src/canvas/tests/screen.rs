#![allow(unused_imports)]
use super::super::*;
use crate::prelude::*;
use crossterm::{csi, style::Colored};

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

    // line 1: all 6 cells are emitted with the background. The row is
    // already full width, so no EL is needed at the right margin.
    write!(expected, csi!("0m")).unwrap();
    write!(expected, csi!("{}m"), Colored::BackgroundColor(Color::Red)).unwrap();
    write!(expected, "      ").unwrap();
    write!(expected, csi!("0m")).unwrap();
    write!(expected, "\r\n").unwrap();

    // line 2
    write!(expected, csi!("{}m"), Colored::BackgroundColor(Color::Red)).unwrap();
    write!(expected, "      ").unwrap();
    write!(expected, csi!("0m")).unwrap();
    write!(expected, "\r\n").unwrap();

    // line 3
    write!(expected, csi!("{}m"), Colored::BackgroundColor(Color::Red)).unwrap();
    write!(expected, "      ").unwrap();
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
fn test_ansi_omits_erase_for_full_rows() {
    let mut canvas = Canvas::new(10, 1);

    canvas
        .subview_mut(0, 0, 0, 0, 10, 1)
        .set_text(0, 0, "1234512345", CanvasTextStyle::default());

    let mut actual = Vec::new();
    canvas.write_ansi(&mut actual).unwrap();

    let mut expected = Vec::new();
    write!(expected, csi!("0m")).unwrap();
    write!(expected, "1234512345").unwrap();
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
    canvas
        .subview_mut(0, 0, 0, 0, 10, 5)
        .set_text(0, 0, "0123456789", CanvasTextStyle::default());
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
    canvas
        .subview_mut(0, 0, 0, 0, 10, 3)
        .set_text(0, 1, "abcdefghij", CanvasTextStyle::default());
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
