#![allow(unused_imports)]
use super::super::*;
use crate::prelude::*;
use crossterm::{csi, style::Colored};

#[test]
fn test_scan_text_positions_is_case_insensitive_and_wide_aware() {
    let mut canvas = Canvas::new(12, 1);
    canvas
        .subview_mut(0, 0, 0, 0, 12, 1)
        .set_text(0, 0, "ab中c AB", CanvasTextStyle::default());

    assert_eq!(
        canvas.scan_text_positions("中c"),
        vec![TextMatchPosition {
            row: 0,
            col: 2,
            len: 3,
        }],
        "match spans should be measured in terminal cells, including wide tails"
    );
    assert_eq!(
        canvas.scan_text_positions("ab"),
        vec![
            TextMatchPosition {
                row: 0,
                col: 0,
                len: 2,
            },
            TextMatchPosition {
                row: 0,
                col: 6,
                len: 2,
            },
        ],
        "search should be case-insensitive and non-overlapping"
    );
}

#[test]
fn test_scan_text_positions_region_returns_subtree_relative_positions() {
    let mut canvas = Canvas::new(16, 3);
    canvas
        .subview_mut(0, 0, 0, 0, 16, 3)
        .set_text(2, 1, "xx lazy 中c", CanvasTextStyle::default());
    canvas.mark_no_select_region(5, 1, 4, 1);

    assert_eq!(
        canvas.scan_text_positions_region(5, 1, 10, 1, "lazy"),
        Vec::<TextMatchPosition>::new(),
        "region scanning should respect noSelect metadata"
    );
    assert_eq!(
        canvas.scan_text_positions_region(8, 1, 8, 1, "中c"),
        vec![TextMatchPosition {
            row: 0,
            col: 2,
            len: 3,
        }],
        "positions should be relative to the scanned region and wide-aware"
    );
}

#[test]
fn test_apply_search_highlight_skips_no_select_and_marks_damage() {
    let mut canvas = Canvas::new(12, 1);
    canvas
        .subview_mut(0, 0, 0, 0, 12, 1)
        .set_text(0, 0, "foo foo", CanvasTextStyle::default());
    canvas.mark_no_select_region(0, 0, 3, 1);

    let applied = canvas.apply_search_highlight(
        "foo",
        StyleOverlay {
            invert: Some(true),
            ..Default::default()
        },
    );

    assert!(applied);
    for col in 0..3 {
        assert!(!canvas.resolved_text_style(col, 0).unwrap().invert);
    }
    for col in 4..7 {
        assert!(canvas.resolved_text_style(col, 0).unwrap().invert);
    }
    assert_eq!(
        canvas.damage_region(),
        Some(DamageRegion {
            x: 4,
            y: 0,
            width: 3,
            height: 1,
        }),
        "search highlight should damage only highlighted selectable cells"
    );
}

#[test]
fn test_apply_positioned_highlight_translates_and_clips_row_offset() {
    let mut canvas = Canvas::new(8, 2);
    canvas
        .subview_mut(0, 0, 0, 0, 8, 2)
        .set_text(0, 1, "target", CanvasTextStyle::default());
    let positions = vec![TextMatchPosition {
        row: 0,
        col: 0,
        len: 6,
    }];

    assert!(!canvas.apply_positioned_highlight(
        &positions,
        -1,
        0,
        StyleOverlay {
            invert: Some(true),
            ..Default::default()
        },
    ));
    assert!(canvas.apply_positioned_highlight(
        &positions,
        1,
        0,
        StyleOverlay {
            invert: Some(true),
            ..Default::default()
        },
    ));
    assert!(canvas.resolved_text_style(0, 1).unwrap().invert);
    assert!(canvas.resolved_text_style(5, 1).unwrap().invert);
}

// ----- P1-5: wide character / grapheme cluster tests -----
