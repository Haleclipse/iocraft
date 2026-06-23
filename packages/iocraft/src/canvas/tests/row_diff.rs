#![allow(unused_imports)]
use super::super::*;
use crate::prelude::*;
use crossterm::{csi, style::Colored};

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
