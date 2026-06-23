#![allow(unused_imports)]
use super::super::*;
use crate::prelude::*;
use crossterm::{csi, style::Colored};

#[test]
fn test_render_metadata_does_not_affect_canvas_equality() {
    let mut a = Canvas::new(10, 3);
    let b = Canvas::new(10, 3);
    a.set_scroll_hint(ScrollHint {
        top: 0,
        bottom: 2,
        delta: 1,
    });
    a.force_full_repaint();
    a.mark_damage(DamageRegion {
        x: 2,
        y: 1,
        width: 3,
        height: 1,
    });

    assert!(
        a == b,
        "render metadata is handled explicitly by the render loop"
    );
    assert_eq!(
        a.scroll_hint(),
        Some(ScrollHint {
            top: 0,
            bottom: 2,
            delta: 1,
        })
    );
    assert!(a.should_force_full_repaint());
    assert_eq!(
        a.damage_region(),
        Some(DamageRegion {
            x: 2,
            y: 1,
            width: 3,
            height: 1,
        })
    );
}

#[test]
fn test_damage_regions_union_and_track_rows() {
    let mut canvas = Canvas::new(10, 5);
    canvas.mark_damage(DamageRegion {
        x: 2,
        y: 1,
        width: 3,
        height: 2,
    });
    canvas.mark_damage(DamageRegion {
        x: 1,
        y: 3,
        width: 4,
        height: 1,
    });

    assert_eq!(
        canvas.damage_region(),
        Some(DamageRegion {
            x: 1,
            y: 1,
            width: 4,
            height: 3,
        })
    );
    assert!(!canvas.row_is_damaged(0));
    assert!(canvas.row_is_damaged(1));
    assert!(canvas.row_is_damaged(3));
    assert!(!canvas.row_is_damaged(4));

    canvas.clear_damage();
    assert_eq!(canvas.damage_region(), None);
}

#[test]
fn test_damage_regions_clip_to_canvas_bounds() {
    let mut canvas = Canvas::new(10, 5);
    canvas.mark_damage(DamageRegion {
        x: 8,
        y: 3,
        width: 10,
        height: 10,
    });
    assert_eq!(
        canvas.damage_region(),
        Some(DamageRegion {
            x: 8,
            y: 3,
            width: 2,
            height: 2,
        })
    );

    canvas.mark_damage(DamageRegion {
        x: 10,
        y: 1,
        width: 2,
        height: 1,
    });
    assert_eq!(
        canvas.damage_region(),
        Some(DamageRegion {
            x: 8,
            y: 3,
            width: 2,
            height: 2,
        }),
        "fully off-canvas damage should be ignored"
    );
}

#[test]
fn test_canvas_diff_each_reports_cells_overlays_growth_and_shrink() {
    let mut prev = Canvas::new(4, 2);
    prev.subview_mut(0, 0, 0, 0, 4, 2)
        .set_text(0, 0, "ab", CanvasTextStyle::default());
    prev.subview_mut(0, 0, 0, 0, 4, 2)
        .set_text(0, 1, "zz", CanvasTextStyle::default());

    let mut next = Canvas::new(5, 1);
    next.subview_mut(0, 0, 0, 0, 5, 1)
        .set_text(0, 0, "ac", CanvasTextStyle::default());
    next.set_overlay(2, 0, StyleOverlay::inverse());
    next.subview_mut(0, 0, 0, 0, 5, 1)
        .set_text(4, 0, "d", CanvasTextStyle::default());

    let changes = prev.diff(&next);
    let coords = changes
        .iter()
        .map(|change| (change.x, change.y))
        .collect::<Vec<_>>();
    assert_eq!(
        coords,
        vec![(1, 0), (2, 0), (4, 0), (0, 1), (1, 1), (2, 1), (3, 1)]
    );
    assert_eq!(
        changes[0]
            .removed
            .as_ref()
            .and_then(|cell| cell.cell.text()),
        Some("b")
    );
    assert_eq!(
        changes[0].added.as_ref().and_then(|cell| cell.cell.text()),
        Some("c")
    );
    assert_eq!(
        changes[1].removed.as_ref().and_then(|cell| cell.overlay),
        None
    );
    assert_eq!(
        changes[1].added.as_ref().and_then(|cell| cell.overlay),
        Some(StyleOverlay::inverse())
    );
    assert!(changes[2].removed.is_none());
    assert_eq!(
        changes[2].added.as_ref().and_then(|cell| cell.cell.text()),
        Some("d")
    );
    assert_eq!(
        changes[3]
            .removed
            .as_ref()
            .and_then(|cell| cell.cell.text()),
        Some("z")
    );
    assert!(changes[3].added.is_none());

    let mut first = None;
    let stopped = prev.diff_each(&next, |change| {
        first = Some((change.x, change.y));
        true
    });
    assert!(stopped);
    assert_eq!(first, Some((1, 0)));
}

#[test]
fn test_canvas_packed_screen_interns_cells_styles_links_and_metadata() {
    let mut canvas = Canvas::new(5, 2);
    let style = CanvasTextStyle {
        color: Some(Color::Red),
        weight: Weight::Bold,
        ..Default::default()
    };
    canvas.subview_mut(0, 0, 0, 0, 5, 2).set_text_with_link(
        0,
        0,
        "A好",
        style,
        Some("https://example.com"),
    );
    canvas.set_overlay(0, 0, StyleOverlay::selection_background(Color::Blue));
    canvas.mark_no_select_region(0, 0, 1, 1);
    canvas.mark_soft_wrap_continuation(1, 2);
    canvas.mark_damage(DamageRegion {
        x: 0,
        y: 0,
        width: 2,
        height: 1,
    });

    let mut pools = CanvasPackedCellPools::new();
    let packed = canvas.pack_with(&mut pools);

    assert_eq!(packed.width, 5);
    assert_eq!(packed.height, 2);
    assert_eq!(packed.cells.len(), 10);
    assert_eq!(
        pools.character(packed.cell(0, 0).unwrap().char_id),
        Some("A")
    );
    assert_eq!(
        pools.character(packed.cell(1, 0).unwrap().char_id),
        Some("好")
    );
    assert_eq!(
        pools.character(packed.cell(2, 0).unwrap().char_id),
        Some("")
    );
    assert_eq!(
        packed.cell(1, 0).unwrap().width,
        CanvasPackedCellWidth::Wide
    );
    assert_eq!(
        packed.cell(2, 0).unwrap().width,
        CanvasPackedCellWidth::WidthTail
    );
    assert_eq!(
        pools.hyperlink(packed.cell(0, 0).unwrap().hyperlink_id),
        Some("https://example.com")
    );
    assert_eq!(
        pools.style(packed.cell(0, 0).unwrap().style_id),
        Some(CanvasResolvedStyle {
            text: style.with_overlay(&StyleOverlay::selection_background(Color::Blue)),
            background_color: Some(Color::Blue),
        })
    );
    assert!(packed.is_no_select(0, 0));
    assert_eq!(packed.soft_wrap_continuation(1), 2);
    assert!(packed.damage_region.is_some());
    assert!(packed.is_empty_cell(4, 1));

    let second = canvas.pack_with(&mut pools);
    assert_eq!(
        packed, second,
        "stable pools make snapshots directly comparable"
    );
}

#[test]
fn test_canvas_packed_screen_diff_uses_damage_and_shrink_regions_like_cc_screen() {
    let mut prev = Canvas::new(4, 2);
    {
        let mut view = prev.subview_mut(0, 0, 0, 0, 4, 2);
        view.set_text(0, 0, "ab", CanvasTextStyle::default());
        view.set_text(0, 1, "zzzz", CanvasTextStyle::default());
    }
    prev.clear_damage();

    let mut next = prev.clone();
    next.subview_mut(0, 0, 0, 0, 4, 2)
        .set_text(1, 0, "c", CanvasTextStyle::default());
    next.mark_damage(DamageRegion {
        x: 1,
        y: 0,
        width: 1,
        height: 1,
    });

    let mut pools = CanvasPackedCellPools::new();
    let prev = prev.pack_with(&mut pools);
    let next = next.pack_with(&mut pools);
    let changes = prev.diff(&next);
    assert_eq!(changes.len(), 1);
    assert_eq!((changes[0].x, changes[0].y), (1, 0));
    assert_eq!(
        pools.character(changes[0].removed.unwrap().char_id),
        Some("b")
    );
    assert_eq!(
        pools.character(changes[0].added.unwrap().char_id),
        Some("c")
    );

    let mut first = None;
    let stopped = prev.diff_each(&next, |change| {
        first = Some((change.x, change.y));
        true
    });
    assert!(stopped);
    assert_eq!(first, Some((1, 0)));

    let shrunk = Canvas::new(4, 1).pack_with(&mut pools);
    let shrink_changes = prev.diff(&shrunk);
    assert_eq!(shrink_changes.len(), 4);
    assert_eq!(
        shrink_changes
            .iter()
            .map(|change| (change.x, change.y, change.added.is_none()))
            .collect::<Vec<_>>(),
        vec![(0, 1, true), (1, 1, true), (2, 1, true), (3, 1, true)]
    );
}

#[test]
fn test_canvas_packed_screen_write_line_cache_matches_cc_output_write_line() {
    let mut pools = CanvasPackedCellPools::new();
    let mut cache = CanvasPackedLineCache::with_max_entries(2);
    let mut style_text = CanvasTextStyle::default();
    style_text.color = Some(Color::Green);
    let style = CanvasResolvedStyle {
        text: style_text,
        background_color: Some(Color::Blue),
    };

    let mut packed = CanvasPackedScreen::new(12, 2);
    let end = packed.write_line_with_cache(
        &mut pools,
        &mut cache,
        1,
        0,
        "A\tB\u{0007}\x1b[31mC",
        style,
        Some("https://example.com"),
    );
    assert_eq!(end, 10);
    assert_eq!(cache.len(), 1);
    assert_eq!(packed.char_in_cell(&pools, 1, 0), Some("A"));
    assert_eq!(packed.char_in_cell(&pools, 8, 0), Some("B"));
    assert_eq!(packed.char_in_cell(&pools, 9, 0), Some("C"));
    assert_eq!(packed.cell_view(&pools, 1, 0).unwrap().style, Some(style));
    assert_eq!(
        packed.cell_view(&pools, 1, 0).unwrap().hyperlink,
        Some("https://example.com")
    );
    assert_eq!(
        packed.cell_view(&pools, 2, 0).unwrap().style,
        Some(CanvasResolvedStyle::default()),
        "TAB expansion writes default-styled spaces like CC Ink output.ts"
    );
    assert_eq!(packed.cell_view(&pools, 2, 0).unwrap().hyperlink, None);

    let mut edge = CanvasPackedScreen::new(4, 1);
    let edge_end = edge.write_line_with_cache(&mut pools, &mut cache, 3, 0, "好", style, None);
    assert_eq!(edge_end, 4);
    assert_eq!(edge.char_in_cell(&pools, 3, 0), Some(" "));
    assert_eq!(
        edge.cell(3, 0).unwrap().width,
        CanvasPackedCellWidth::SpacerHead,
        "wide grapheme at the right edge becomes a SpacerHead placeholder"
    );

    let same_end = packed.write_line_with_cache(
        &mut pools,
        &mut cache,
        1,
        1,
        "A\tB\u{0007}\x1b[31mC",
        style,
        Some("https://example.com"),
    );
    assert_eq!(same_end, 10);
    assert_eq!(cache.len(), 2, "line cache reuses retained cluster entries");
}

#[test]
fn test_canvas_packed_screen_write_line_runs_cache_styles_once_per_run() {
    let mut pools = CanvasPackedCellPools::new();
    let mut cache = CanvasPackedLineCache::new();
    let mut link_style = CanvasTextStyle::default();
    link_style.color = Some(Color::Cyan);
    let linked = CanvasResolvedStyle {
        text: link_style,
        background_color: Some(Color::DarkBlue),
    };
    let plain = CanvasResolvedStyle::default();

    let mut packed = CanvasPackedScreen::new(12, 1);
    let end = packed.write_line_runs_with_cache(
        &mut pools,
        &mut cache,
        0,
        0,
        [
            CanvasPackedLineRun {
                text: "A\t",
                style: linked,
                hyperlink: Some("https://example.com"),
            },
            CanvasPackedLineRun {
                text: "B好",
                style: plain,
                hyperlink: None,
            },
        ],
    );

    assert_eq!(end, 11);
    assert_eq!(cache.len(), 2);
    assert_eq!(packed.char_in_cell(&pools, 0, 0), Some("A"));
    assert_eq!(packed.char_in_cell(&pools, 8, 0), Some("B"));
    assert_eq!(packed.char_in_cell(&pools, 9, 0), Some("好"));
    assert_eq!(
        packed.cell(10, 0).unwrap().width,
        CanvasPackedCellWidth::WidthTail
    );
    assert_eq!(packed.cell_view(&pools, 0, 0).unwrap().style, Some(linked));
    assert_eq!(
        packed.cell_view(&pools, 0, 0).unwrap().hyperlink,
        Some("https://example.com")
    );
    assert_eq!(
        packed.cell_view(&pools, 1, 0).unwrap().style,
        Some(CanvasResolvedStyle::default()),
        "TAB expansion writes default spaces instead of inheriting the styled run"
    );
    assert_eq!(packed.cell_view(&pools, 8, 0).unwrap().style, Some(plain));
    assert_eq!(packed.cell_view(&pools, 8, 0).unwrap().hyperlink, None);

    let style_id = pools.intern_style(linked);
    let hyperlink_id = pools.intern_hyperlink(Some("https://example.com"));
    let mut ids = CanvasPackedScreen::new(4, 1);
    let ids_end = ids.write_line_runs_with_ids(
        &mut pools,
        &mut cache,
        0,
        0,
        [CanvasPackedLineRunIds {
            text: "CD",
            style_id,
            hyperlink_id,
        }],
    );
    assert_eq!(ids_end, 2);
    assert_eq!(ids.cell_view(&pools, 0, 0).unwrap().style, Some(linked));
    assert_eq!(
        ids.cell_view(&pools, 1, 0).unwrap().hyperlink,
        Some("https://example.com")
    );
}

#[test]
fn test_canvas_packed_screen_write_line_runs_reorders_bidi_with_metadata_like_cc_output() {
    let mut pools = CanvasPackedCellPools::new();
    let mut cache = CanvasPackedLineCache::new();
    let mut rtl_style_text = CanvasTextStyle::default();
    rtl_style_text.color = Some(Color::Yellow);
    let rtl_style = CanvasResolvedStyle {
        text: rtl_style_text,
        background_color: None,
    };
    let rtl_style_id = pools.intern_style(rtl_style);
    let ltr_style_id = pools.intern_style(CanvasResolvedStyle::default());
    let link_id = pools.intern_hyperlink(Some("https://rtl.example"));
    let mut packed = CanvasPackedScreen::new(8, 1);

    let end = packed.write_line_runs_with_ids_bidi_mode(
        &mut pools,
        &mut cache,
        0,
        0,
        [
            CanvasPackedLineRunIds {
                text: "אבג",
                style_id: rtl_style_id,
                hyperlink_id: link_id,
            },
            CanvasPackedLineRunIds {
                text: "abc",
                style_id: ltr_style_id,
                hyperlink_id: 0,
            },
        ],
        true,
    );

    assert_eq!(end, 6);
    let rendered = (0..6)
        .filter_map(|x| packed.char_in_cell(&pools, x, 0))
        .collect::<String>();
    assert_eq!(rendered, "abcגבא");
    assert_eq!(
        packed.cell_view(&pools, 0, 0).unwrap().style_id,
        ltr_style_id
    );
    assert_eq!(
        packed.cell_view(&pools, 3, 0).unwrap().style_id,
        rtl_style_id
    );
    assert_eq!(
        packed.cell_view(&pools, 3, 0).unwrap().hyperlink,
        Some("https://rtl.example"),
        "bidi reorder preserves per-grapheme style/link metadata"
    );
}

#[test]
fn test_canvas_packed_screen_ansi_row_writer_matches_cc_sparse_row_shape() {
    let mut pools = CanvasPackedCellPools::new();
    let mut style_cache = CanvasStyleTransitionCache::new();
    let mut screen = CanvasPackedScreen::new(6, 1);
    let mut linked_text = CanvasTextStyle::default();
    linked_text.color = Some(Color::Green);
    let linked_style = CanvasResolvedStyle {
        text: linked_text,
        background_color: None,
    };
    screen.set_cell_text(
        &mut pools,
        2,
        0,
        "A",
        linked_style,
        Some("https://example.com"),
        CanvasPackedCellWidth::Normal,
    );
    screen.set_cell_text(
        &mut pools,
        4,
        0,
        "B",
        CanvasResolvedStyle::default(),
        None,
        CanvasPackedCellWidth::Normal,
    );

    let row = screen
        .ansi_row_with_style_cache(&pools, &mut style_cache, 0, 0)
        .unwrap();
    assert!(
        row.starts_with("\x1b[2C"),
        "leading empty cells are skipped with cursor-forward movement: {row:?}"
    );
    assert!(row.contains("https://example.com"));
    assert!(row.contains("A"));
    assert!(row.contains("\x1b[1C"), "gap before B is sparse: {row:?}");
    assert!(row.contains("B"));
    assert!(
        row.contains("\x1b]8;;\x1b\\"),
        "OSC 8 link is closed: {row:?}"
    );
    assert!(
        row.contains("\x1b[K"),
        "row writer clears through EOL: {row:?}"
    );
    assert!(
        style_cache.len() >= 2,
        "style transitions are cached for packed sparse row writers"
    );
}

#[test]
fn test_canvas_packed_output_queue_matches_cc_output_get_ordering() {
    let mut pools = CanvasPackedCellPools::new();
    let mut cache = CanvasPackedLineCache::new();
    let style = CanvasResolvedStyle::default();

    let mut src = CanvasPackedScreen::new(8, 3);
    src.write_line_with_cache(&mut pools, &mut cache, 0, 0, "OLD0", style, None);
    src.write_line_with_cache(&mut pools, &mut cache, 0, 1, "OLD1", style, None);

    let mut output = CanvasPackedOutput::new(8, 3);
    output.blit(src, 0, 0, 4, 2);
    output.clear(
        DamageRegion {
            x: 0,
            y: 0,
            width: 4,
            height: 1,
        },
        true,
    );
    output.clip(CanvasPackedOutputClip {
        x1: Some(2),
        x2: Some(5),
        y1: Some(0),
        y2: Some(3),
    });
    output.write(0, 2, "ABCDE", style, None, None);
    output.unclip();
    output.no_select(DamageRegion {
        x: 1,
        y: 1,
        width: 2,
        height: 1,
    });

    let screen = output.get(&mut pools);
    assert_eq!(screen.char_in_cell(&pools, 0, 0), Some(" "));
    assert_eq!(screen.char_in_cell(&pools, 0, 1), Some("O"));
    assert_eq!(screen.char_in_cell(&pools, 3, 1), Some("1"));
    assert_eq!(screen.char_in_cell(&pools, 0, 2), Some(" "));
    assert_eq!(screen.char_in_cell(&pools, 1, 2), Some(" "));
    assert_eq!(screen.char_in_cell(&pools, 2, 2), Some("C"));
    assert_eq!(screen.char_in_cell(&pools, 4, 2), Some("E"));
    assert!(screen.is_no_select(1, 1));
    assert!(screen.is_no_select(2, 1));
    assert!(
        screen
            .damage_region()
            .is_some_and(|damage| damage.y == 0 && damage.height >= 3),
        "clear, blit, and clipped write regions are all represented in damage"
    );
    assert_eq!(output.line_cache_len(), 1);
}

#[test]
fn test_canvas_packed_output_soft_wrap_survives_vertical_clip_like_cc_output() {
    let mut pools = CanvasPackedCellPools::new();
    let style = CanvasResolvedStyle::default();
    let mut output = CanvasPackedOutput::new(8, 2);
    output.clip(CanvasPackedOutputClip {
        x1: None,
        x2: None,
        y1: Some(1),
        y2: Some(2),
    });
    output.write(0, 0, "prev\ncont", style, None, Some(vec![false, true]));

    let screen = output.get(&mut pools);
    assert_eq!(screen.char_in_cell(&pools, 0, 0), Some(" "));
    assert_eq!(screen.char_in_cell(&pools, 0, 1), Some("c"));
    assert_eq!(
        screen.soft_wrap_continuation(1),
        4,
        "the clipped previous row's content end is retained for soft-wrap copy"
    );
}

#[test]
fn test_canvas_packed_style_overlay_helpers_match_cc_style_pool_overlays() {
    let mut pools = CanvasPackedCellPools::new();
    let base_style = CanvasResolvedStyle {
        text: CanvasTextStyle {
            color: Some(Color::Green),
            weight: Weight::Light,
            italic: true,
            invert: true,
            ..Default::default()
        },
        background_color: Some(Color::Red),
    };
    let base_id = pools.intern_style(base_style);

    let inverse_id = pools.style_id_with_inverse(base_id);
    assert_eq!(
        inverse_id, base_id,
        "already-inverted styles intern back to the base ID"
    );

    let selection_id = pools.style_id_with_selection_background(base_id, Color::Blue);
    let selection = pools.style(selection_id).unwrap();
    assert_eq!(selection.text.color, Some(Color::Green));
    assert_eq!(selection.text.weight, Weight::Light);
    assert!(selection.text.italic);
    assert!(
        !selection.text.invert,
        "selection background disables inverse like CC Ink"
    );
    assert_eq!(selection.background_color, Some(Color::Blue));
    assert!(selection.is_visible_on_space());

    let current_id = pools.style_id_with_current_match(base_id, Color::Yellow);
    let current = pools.style(current_id).unwrap();
    assert_eq!(current.text.color, Some(Color::Yellow));
    assert_eq!(
        current.background_color, None,
        "current match strips existing background"
    );
    assert_eq!(current.text.weight, Weight::Bold);
    assert!(current.text.underline);
    assert!(current.text.invert);
    assert!(current.text.italic, "unowned style fields are preserved");

    let fallback_id = pools.style_id_with_overlay(u32::MAX, StyleOverlay::inverse());
    assert_eq!(
        pools.style(fallback_id).unwrap(),
        CanvasResolvedStyle::default().with_overlay(StyleOverlay::inverse())
    );
}

#[test]
fn test_canvas_packed_screen_row_change_start_matches_cc_damage_scan() {
    let mut pools = CanvasPackedCellPools::new();
    let mut prev = CanvasPackedScreen::new(6, 2);
    prev.set_cell_text(
        &mut pools,
        1,
        0,
        "好",
        CanvasResolvedStyle::default(),
        None,
        CanvasPackedCellWidth::Wide,
    );
    prev.set_cell_text(
        &mut pools,
        0,
        1,
        "z",
        CanvasResolvedStyle::default(),
        None,
        CanvasPackedCellWidth::Normal,
    );
    prev.clear_damage();

    let mut next = prev.clone();
    let tail_index = next.index(2, 0).unwrap();
    next.cells[tail_index] = CanvasPackedCell::default();
    assert_eq!(
        prev.row_change_start(&next, 0),
        Some(1),
        "tail-only differences repaint from the wide head"
    );

    let mut damaged = prev.clone();
    damaged.mark_damage(DamageRegion {
        x: 4,
        y: 0,
        width: 1,
        height: 1,
    });
    assert_eq!(prev.row_change_start(&damaged, 0), Some(4));
    damaged.clear_damage();
    assert_eq!(damaged.damage_region(), None);
    assert_eq!(prev.row_change_start(&damaged, 0), None);

    let shrunk = CanvasPackedScreen::new(6, 1);
    assert_eq!(prev.row_change_start(&shrunk, 1), Some(0));

    let empty_growth = CanvasPackedScreen::new(6, 3);
    assert_eq!(shrunk.row_change_start(&empty_growth, 2), None);

    let mut non_empty_growth = empty_growth.clone();
    non_empty_growth.set_cell_text(
        &mut pools,
        3,
        2,
        "g",
        CanvasResolvedStyle::default(),
        None,
        CanvasPackedCellWidth::Normal,
    );
    non_empty_growth.clear_damage();
    assert_eq!(shrunk.row_change_start(&non_empty_growth, 2), Some(3));

    let resized = CanvasPackedScreen::new(5, 2);
    assert_eq!(prev.row_change_start(&resized, 0), Some(0));
}

#[test]
fn test_canvas_packed_screen_clear_blit_and_shift_match_cc_screen_helpers() {
    let mut pools = CanvasPackedCellPools::new();

    let mut source_canvas = Canvas::new(4, 3);
    {
        let mut view = source_canvas.subview_mut(0, 0, 0, 0, 4, 3);
        view.set_text(0, 0, "好x", CanvasTextStyle::default());
        view.set_text(0, 1, "b", CanvasTextStyle::default());
        view.set_text(0, 2, "c", CanvasTextStyle::default());
    }
    source_canvas.mark_no_select_region(0, 1, 1, 1);
    source_canvas.mark_soft_wrap_continuation(1, 3);
    source_canvas.clear_damage();

    let source = source_canvas.pack_with(&mut pools);
    let mut packed = Canvas::new(4, 3).pack_with(&mut pools);
    let blit_region = packed
        .blit_region_from(&source, 0, 0, 1, 1)
        .expect("wide-head blit should damage copied head and repaired tail");
    assert_eq!(
        blit_region,
        DamageRegion {
            x: 0,
            y: 0,
            width: 2,
            height: 1,
        }
    );
    assert_eq!(
        pools.character(packed.cell(0, 0).unwrap().char_id),
        Some("好")
    );
    assert_eq!(
        packed.cell(0, 0).unwrap().width,
        CanvasPackedCellWidth::Wide
    );
    assert_eq!(
        packed.cell(1, 0).unwrap().width,
        CanvasPackedCellWidth::WidthTail
    );

    let clear_region = packed
        .clear_region(1, 0, 1, 1)
        .expect("clearing a wide tail repairs the wide head");
    assert_eq!(
        clear_region,
        DamageRegion {
            x: 0,
            y: 0,
            width: 2,
            height: 1,
        }
    );
    assert!(packed.is_empty_cell(0, 0));
    assert!(packed.is_empty_cell(1, 0));

    let mut scrolling = source.clone();
    assert!(scrolling.shift_rows(0, 2, 1));
    assert_eq!(
        pools.character(scrolling.cell(0, 0).unwrap().char_id),
        Some("b")
    );
    assert_eq!(
        pools.character(scrolling.cell(0, 1).unwrap().char_id),
        Some("c")
    );
    assert!(scrolling.is_empty_cell(0, 2));
    assert!(scrolling.is_no_select(0, 0));
    assert_eq!(scrolling.soft_wrap_continuation(0), 3);
    assert_eq!(
        scrolling.damage_region, source.damage_region,
        "packed shiftRows is damage-neutral"
    );
}

#[test]
fn test_canvas_packed_screen_blit_excluding_clears_matches_cc_output_guard() {
    let mut pools = CanvasPackedCellPools::new();
    let mut source_canvas = Canvas::new(6, 4);
    {
        let mut view = source_canvas.subview_mut(0, 0, 0, 0, 6, 4);
        view.set_text(0, 0, "aaaaaa", CanvasTextStyle::default());
        view.set_text(0, 1, "bbbbbb", CanvasTextStyle::default());
        view.set_text(0, 2, "cccccc", CanvasTextStyle::default());
        view.set_text(0, 3, "dddddd", CanvasTextStyle::default());
    }
    source_canvas.mark_no_select_region(1, 1, 1, 1);
    source_canvas.mark_no_select_region(1, 2, 2, 1);
    source_canvas.mark_soft_wrap_continuation(2, 6);
    source_canvas.clear_damage();
    let source = source_canvas.pack_with(&mut pools);

    let mut packed = CanvasPackedScreen::new(6, 4);
    packed.set_cell_text(
        &mut pools,
        1,
        0,
        "0",
        CanvasResolvedStyle::default(),
        None,
        CanvasPackedCellWidth::Normal,
    );
    packed.set_cell_text(
        &mut pools,
        1,
        1,
        "1",
        CanvasResolvedStyle::default(),
        None,
        CanvasPackedCellWidth::Normal,
    );
    packed.set_cell_text(
        &mut pools,
        1,
        2,
        "2",
        CanvasResolvedStyle::default(),
        None,
        CanvasPackedCellWidth::Normal,
    );
    packed.set_cell_text(
        &mut pools,
        1,
        3,
        "3",
        CanvasResolvedStyle::default(),
        None,
        CanvasPackedCellWidth::Normal,
    );
    packed.damage_region = None;

    let absolute_clear = DamageRegion {
        x: 1,
        y: 1,
        width: 4,
        height: 2,
    };
    let copied = packed.blit_region_from_excluding_clears(&source, 1, 0, 4, 4, &[absolute_clear]);
    assert_eq!(
        copied,
        vec![
            DamageRegion {
                x: 1,
                y: 0,
                width: 4,
                height: 1,
            },
            DamageRegion {
                x: 1,
                y: 3,
                width: 4,
                height: 1,
            },
        ]
    );
    assert_eq!(
        pools.character(packed.cell(1, 0).unwrap().char_id),
        Some("a")
    );
    assert_eq!(
        pools.character(packed.cell(1, 1).unwrap().char_id),
        Some("1")
    );
    assert_eq!(
        pools.character(packed.cell(1, 2).unwrap().char_id),
        Some("2")
    );
    assert_eq!(
        pools.character(packed.cell(1, 3).unwrap().char_id),
        Some("d")
    );
    assert!(
        !packed.is_no_select(1, 1),
        "excluded row keeps destination metadata"
    );
    assert_eq!(
        packed.soft_wrap_continuation(2),
        0,
        "excluded row keeps destination soft-wrap"
    );

    let partial_clear = DamageRegion {
        x: 2,
        y: 2,
        width: 1,
        height: 1,
    };
    let copied_partial =
        packed.blit_region_from_excluding_clears(&source, 1, 2, 4, 1, &[partial_clear]);
    assert_eq!(
        copied_partial,
        vec![DamageRegion {
            x: 1,
            y: 2,
            width: 4,
            height: 1,
        }]
    );
    assert_eq!(
        pools.character(packed.cell(1, 2).unwrap().char_id),
        Some("c")
    );
    assert!(packed.is_no_select(1, 2));
    assert_eq!(packed.soft_wrap_continuation(2), 6);
}

#[test]
fn test_canvas_packed_screen_cell_views_match_cc_cell_at_helpers() {
    let mut pools = CanvasPackedCellPools::new();
    let style = CanvasResolvedStyle {
        text: CanvasTextStyle {
            color: Some(Color::Green),
            weight: Weight::Bold,
            ..Default::default()
        },
        background_color: Some(Color::Blue),
    };
    let mut packed = CanvasPackedScreen::new(4, 1);
    assert!(packed.set_cell_text(
        &mut pools,
        0,
        0,
        "A",
        style,
        Some("https://example.com"),
        CanvasPackedCellWidth::Normal,
    ));
    assert!(packed.set_cell_text(
        &mut pools,
        1,
        0,
        "好",
        CanvasResolvedStyle::default(),
        None,
        CanvasPackedCellWidth::Wide,
    ));

    let view = packed.cell_view(&pools, 0, 0).expect("cell in bounds");
    assert_eq!(view.character, "A");
    assert_eq!(view.style, Some(style));
    assert_eq!(view.hyperlink, Some("https://example.com"));
    assert_eq!(view.width, CanvasPackedCellWidth::Normal);
    assert_eq!(packed.char_in_cell(&pools, 0, 0), Some("A"));
    assert_eq!(packed.char_in_cell(&pools, 2, 0), Some(""));
    assert_eq!(packed.char_in_cell(&pools, 99, 0), None);

    let tail = packed
        .cell_view_at_index(&pools, 2)
        .expect("wide tail in bounds");
    assert_eq!(tail.character, "");
    assert_eq!(tail.width, CanvasPackedCellWidth::WidthTail);
    assert_eq!(
        packed
            .visible_cell_view(&pools, 1, 0, None)
            .unwrap()
            .character,
        "好"
    );
    assert_eq!(packed.visible_cell_view(&pools, 2, 0, None), None);
}

#[test]
fn test_canvas_packed_screen_visible_cell_helper_matches_cc_visible_cell_at_index() {
    let mut pools = CanvasPackedCellPools::new();
    let mut packed = CanvasPackedScreen::new(6, 1);
    assert_eq!(packed.visible_cell(&pools, 0, 0, None), None);

    let fg_only = CanvasResolvedStyle {
        text: CanvasTextStyle {
            color: Some(Color::Green),
            ..Default::default()
        },
        background_color: None,
    };
    let fg_id = pools.intern_style(fg_only);
    assert!(!fg_only.is_visible_on_space());
    assert!(!pools.style_visible_on_space(fg_id));
    assert!(packed.set_cell_text(
        &mut pools,
        0,
        0,
        " ",
        fg_only,
        None,
        CanvasPackedCellWidth::Normal,
    ));
    assert!(packed.visible_cell(&pools, 0, 0, None).is_some());
    assert_eq!(packed.visible_cell(&pools, 0, 0, Some(fg_id)), None);
    assert!(packed.visible_cell(&pools, 0, 0, Some(0)).is_some());

    let visible_space_style = CanvasResolvedStyle {
        text: CanvasTextStyle {
            underline: true,
            ..Default::default()
        },
        background_color: Some(Color::Blue),
    };
    let visible_id = pools.intern_style(visible_space_style);
    assert!(visible_space_style.is_visible_on_space());
    assert!(pools.style_visible_on_space(visible_id));
    assert!(packed.set_cell_text(
        &mut pools,
        1,
        0,
        " ",
        visible_space_style,
        None,
        CanvasPackedCellWidth::Normal,
    ));
    assert!(packed
        .visible_cell(&pools, 1, 0, Some(visible_id))
        .is_some());

    assert!(packed.set_cell_text(
        &mut pools,
        2,
        0,
        " ",
        CanvasResolvedStyle::default(),
        Some("https://example.com"),
        CanvasPackedCellWidth::Normal,
    ));
    assert!(packed.visible_cell(&pools, 2, 0, Some(0)).is_some());

    assert!(packed.set_cell_text(
        &mut pools,
        3,
        0,
        "x",
        CanvasResolvedStyle::default(),
        None,
        CanvasPackedCellWidth::Normal,
    ));
    assert!(packed.visible_cell_at_index(&pools, 3, None).is_some());

    assert!(packed.set_cell_text(
        &mut pools,
        4,
        0,
        "好",
        CanvasResolvedStyle::default(),
        None,
        CanvasPackedCellWidth::Wide,
    ));
    assert!(packed.visible_cell(&pools, 4, 0, None).is_some());
    assert_eq!(packed.visible_cell(&pools, 5, 0, None), None);
}

#[test]
fn test_canvas_packed_screen_reset_reuses_and_clears_like_cc_screen() {
    let mut pools = CanvasPackedCellPools::new();
    let mut packed = CanvasPackedScreen::new(2, 2);
    packed.set_cell_text(
        &mut pools,
        0,
        0,
        "x",
        CanvasResolvedStyle::default(),
        Some("https://example.com"),
        CanvasPackedCellWidth::Normal,
    );
    packed.mark_no_select_region(0, 0, 2, 2);
    packed.soft_wrap[1] = 2;
    assert!(packed.damage_region.is_some());

    packed.reset(3, 1);
    assert_eq!((packed.width, packed.height), (3, 1));
    assert_eq!(packed.cells.len(), 3);
    assert_eq!(packed.no_select.len(), 3);
    assert_eq!(packed.soft_wrap, vec![0]);
    assert_eq!(packed.damage_region, None);
    assert!(packed.cells.iter().all(|cell| cell.is_empty()));
    assert!(packed.no_select.iter().all(|marked| !marked));
}

#[test]
fn test_canvas_packed_screen_migrate_transient_pools_matches_cc_screen_pool_reset() {
    let mut pools = CanvasPackedCellPools::new();
    pools.intern_char("unused-before-reset");
    pools.intern_hyperlink(Some("https://unused.example"));
    let style = CanvasResolvedStyle {
        text: CanvasTextStyle {
            color: Some(Color::Yellow),
            weight: Weight::Bold,
            ..Default::default()
        },
        background_color: Some(Color::Blue),
    };
    let style_id = pools.intern_style(style);

    let mut packed = CanvasPackedScreen::new(2, 1);
    assert!(packed.set_cell_text(
        &mut pools,
        0,
        0,
        "好",
        style,
        Some("https://example.com"),
        CanvasPackedCellWidth::Normal,
    ));
    let old_cell = packed.cell(0, 0).unwrap();
    assert_eq!(old_cell.style_id, style_id);
    assert!(
        old_cell.char_id > 2,
        "old transient char pool has unused IDs"
    );
    assert!(
        old_cell.hyperlink_id > 1,
        "old transient hyperlink pool has unused IDs"
    );

    let old_pools = pools.clone();
    let mut next_pools = old_pools.fork_with_transient_pools_cleared();
    assert_eq!(next_pools.char_len(), 2);
    assert_eq!(next_pools.hyperlink_len(), 1);
    assert_eq!(next_pools.style(style_id), Some(style));
    packed.damage_region = None;

    assert!(packed.migrate_transient_pools(&old_pools, &mut next_pools));
    let migrated = packed.cell(0, 0).unwrap();
    assert_ne!(migrated.char_id, old_cell.char_id);
    assert_ne!(migrated.hyperlink_id, old_cell.hyperlink_id);
    assert_eq!(next_pools.character(migrated.char_id), Some("好"));
    assert_eq!(
        next_pools.hyperlink(migrated.hyperlink_id),
        Some("https://example.com")
    );
    assert_eq!(
        migrated.style_id, style_id,
        "style IDs remain session-lived"
    );
    assert_eq!(next_pools.style(migrated.style_id), Some(style));
    assert_eq!(
        packed.damage_region, None,
        "pool migration is output-neutral"
    );
}

#[test]
fn test_canvas_packed_screen_set_cell_repairs_wide_relationships_like_cc_screen() {
    let mut pools = CanvasPackedCellPools::new();
    let mut packed = CanvasPackedScreen::new(5, 1);
    let default_style = CanvasResolvedStyle::default();

    assert!(packed.set_cell_text(
        &mut pools,
        1,
        0,
        "好",
        default_style,
        None,
        CanvasPackedCellWidth::Wide,
    ));
    assert_eq!(
        pools.character(packed.cell(1, 0).unwrap().char_id),
        Some("好")
    );
    assert_eq!(
        packed.cell(1, 0).unwrap().width,
        CanvasPackedCellWidth::Wide
    );
    assert_eq!(
        packed.cell(2, 0).unwrap().width,
        CanvasPackedCellWidth::WidthTail
    );
    assert_eq!(
        packed.damage_region,
        Some(DamageRegion {
            x: 1,
            y: 0,
            width: 2,
            height: 1,
        })
    );

    packed.damage_region = None;
    assert!(packed.set_cell_text(
        &mut pools,
        2,
        0,
        "x",
        default_style,
        None,
        CanvasPackedCellWidth::Normal,
    ));
    assert!(
        packed.is_empty_cell(1, 0),
        "overwriting a tail clears its wide head"
    );
    assert_eq!(
        pools.character(packed.cell(2, 0).unwrap().char_id),
        Some("x")
    );
    assert_eq!(
        packed.damage_region,
        Some(DamageRegion {
            x: 1,
            y: 0,
            width: 2,
            height: 1,
        })
    );

    packed.damage_region = None;
    assert!(packed.set_cell_text(
        &mut pools,
        2,
        0,
        "界",
        default_style,
        None,
        CanvasPackedCellWidth::Wide,
    ));
    assert_eq!(
        packed.cell(2, 0).unwrap().width,
        CanvasPackedCellWidth::Wide
    );
    assert_eq!(
        packed.cell(3, 0).unwrap().width,
        CanvasPackedCellWidth::WidthTail
    );
    packed.damage_region = None;
    assert!(packed.set_cell_text(
        &mut pools,
        2,
        0,
        "n",
        default_style,
        None,
        CanvasPackedCellWidth::Normal,
    ));
    assert!(
        packed.is_empty_cell(3, 0),
        "overwriting a wide head clears its tail"
    );
}

#[test]
fn test_canvas_packed_screen_style_and_no_select_metadata_match_cc_helpers() {
    let mut canvas = Canvas::new(4, 1);
    canvas
        .subview_mut(0, 0, 0, 0, 4, 1)
        .set_text(0, 0, "A好", CanvasTextStyle::default());
    canvas.clear_damage();

    let mut pools = CanvasPackedCellPools::new();
    let mut packed = canvas.pack_with(&mut pools);
    let mut highlighted_text = CanvasTextStyle::default();
    highlighted_text.color = Some(Color::Yellow);
    highlighted_text.weight = Weight::Bold;
    let highlighted = CanvasResolvedStyle {
        text: highlighted_text,
        background_color: Some(Color::Blue),
    };

    assert!(packed.set_cell_style(&mut pools, 0, 0, highlighted));
    let highlighted_id = packed.cell(0, 0).unwrap().style_id;
    assert_eq!(pools.style(highlighted_id), Some(highlighted));
    assert_eq!(
        packed.damage_region,
        Some(DamageRegion {
            x: 0,
            y: 0,
            width: 1,
            height: 1,
        })
    );

    let tail_before = packed.cell(2, 0).unwrap();
    assert!(!packed.set_cell_style_id(2, 0, highlighted_id));
    assert_eq!(packed.cell(2, 0).unwrap(), tail_before);

    let damage_before_no_select = packed.damage_region;
    assert!(packed.mark_no_select_region(1, 0, 10, 1));
    assert!(packed.is_no_select(1, 0));
    assert!(packed.is_no_select(3, 0));
    assert_eq!(
        packed.damage_region, damage_before_no_select,
        "noSelect metadata does not affect terminal damage"
    );
}

#[test]
fn test_canvas_packed_screen_style_overlay_marks_damage_and_wide_head() {
    let mut pools = CanvasPackedCellPools::new();
    let mut line_cache = CanvasPackedLineCache::new();
    let mut packed = CanvasPackedScreen::new(4, 1);
    packed.write_line_with_cache(
        &mut pools,
        &mut line_cache,
        0,
        0,
        "好",
        CanvasResolvedStyle::default(),
        None,
    );
    packed.clear_damage();

    assert!(packed.apply_style_overlay(&mut pools, 1, 0, StyleOverlay::inverse()));
    let head_style = pools
        .style(packed.cell(0, 0).unwrap().style_id)
        .expect("overlay style interned");
    assert!(head_style.text.invert);
    assert_eq!(
        packed.cell(1, 0).unwrap().width,
        CanvasPackedCellWidth::WidthTail
    );
    assert_eq!(
        packed.damage_region(),
        Some(DamageRegion {
            x: 0,
            y: 0,
            width: 1,
            height: 1,
        })
    );

    assert!(packed.apply_style_overlay_region(
        &mut pools,
        2,
        0,
        2,
        1,
        StyleOverlay::selection_background(Color::Blue),
    ));
    let blank_style = pools
        .style(packed.cell(2, 0).unwrap().style_id)
        .expect("blank overlay style interned");
    assert_eq!(blank_style.background_color, Some(Color::Blue));
    assert!(
        packed.visible_cell(&pools, 2, 0, None).is_some(),
        "background overlay on a packed blank space must be sparse-render-visible"
    );
}

#[test]
fn test_canvas_packed_screen_word_bounds_match_cc_double_click_shape() {
    let mut pools = CanvasPackedCellPools::new();
    let mut line_cache = CanvasPackedLineCache::new();
    let style = CanvasResolvedStyle::default();
    let mut packed = CanvasPackedScreen::new(20, 1);
    packed.write_line_with_cache(
        &mut pools,
        &mut line_cache,
        0,
        0,
        "run /usr/bin/bash!",
        style,
        None,
    );

    assert_eq!(
        packed.word_bounds_at(&pools, 6, 0),
        Some((4, 16)),
        "path punctuation should stay in the same word class like CC Ink selection"
    );
    assert_eq!(packed.word_bounds_at(&pools, 17, 0), Some((17, 17)));
}

#[test]
fn test_canvas_packed_screen_word_bounds_steps_from_wide_tail_and_no_selects() {
    let mut pools = CanvasPackedCellPools::new();
    let mut line_cache = CanvasPackedLineCache::new();
    let style = CanvasResolvedStyle::default();
    let mut packed = CanvasPackedScreen::new(8, 1);
    packed.write_line_with_cache(&mut pools, &mut line_cache, 0, 0, "中a x", style, None);

    assert_eq!(
        packed.word_bounds_at(&pools, 1, 0),
        Some((0, 2)),
        "double-clicking a wide tail should select from the head cell"
    );
    packed.mark_no_select_region(0, 0, 1, 1);
    assert_eq!(packed.word_bounds_at(&pools, 0, 0), None);
    assert_eq!(
        packed.word_bounds_at(&pools, 1, 0),
        None,
        "wide-tail fallback should still respect noSelect on the head"
    );
}

#[test]
fn test_canvas_packed_screen_selection_range_for_word_and_line() {
    let mut pools = CanvasPackedCellPools::new();
    let mut line_cache = CanvasPackedLineCache::new();
    let style = CanvasResolvedStyle::default();
    let mut packed = CanvasPackedScreen::new(8, 2);
    packed.write_line_with_cache(&mut pools, &mut line_cache, 0, 0, "one two", style, None);
    packed.write_line_with_cache(&mut pools, &mut line_cache, 0, 1, "row", style, None);

    let word = packed
        .selection_range_for_word_at(&pools, 5, 0)
        .expect("word selection range");
    assert_eq!(packed.selected_text(&pools, word), "two");

    let line = packed
        .selection_range_for_line(1)
        .expect("line selection range");
    assert_eq!(packed.selected_text(&pools, line), "row");
    assert_eq!(packed.selection_range_for_line(2), None);
}

#[test]
fn test_selection_state_packed_multi_click_and_extend_match_screen() {
    let mut pools = CanvasPackedCellPools::new();
    let mut line_cache = CanvasPackedLineCache::new();
    let style = CanvasResolvedStyle::default();
    let mut packed = CanvasPackedScreen::new(16, 1);
    packed.write_line_with_cache(
        &mut pools,
        &mut line_cache,
        0,
        0,
        "one two three",
        style,
        None,
    );

    let mut selection = SelectionState::new();
    selection.start_multi_click_packed(&packed, &pools, 4, 0, SelectionClickCount::Double);
    assert_eq!(selection.selected_text_packed(&packed, &pools), "two");

    selection.extend_span_selection_packed(&packed, &pools, 10, 0);
    assert_eq!(selection.selected_text_packed(&packed, &pools), "two three");
}

#[test]
fn test_selection_state_packed_line_overlay_and_capture_rows() {
    let mut pools = CanvasPackedCellPools::new();
    let mut line_cache = CanvasPackedLineCache::new();
    let style = CanvasResolvedStyle::default();
    let mut packed = CanvasPackedScreen::new(8, 3);
    packed.write_line_with_cache(&mut pools, &mut line_cache, 0, 0, "row0", style, None);
    packed.write_line_with_cache(&mut pools, &mut line_cache, 0, 1, "row1", style, None);
    packed.write_line_with_cache(&mut pools, &mut line_cache, 0, 2, "row2", style, None);

    let mut selection = SelectionState::new();
    assert!(selection.select_line_at_packed(&packed, 1));
    assert_eq!(selection.selected_text_packed(&packed, &pools), "row1");

    let mut overlayed = packed.clone();
    assert!(selection.apply_overlay_packed(
        &mut overlayed,
        &mut pools,
        StyleOverlay::selection_background(Color::Blue),
    ));
    assert_eq!(
        pools
            .style(overlayed.cell(0, 1).unwrap().style_id)
            .unwrap_or_default()
            .background_color,
        Some(Color::Blue)
    );

    selection.move_focus(3, 2);
    selection.capture_scrolled_rows_packed(&packed, &pools, 1, 1, SelectionCaptureSide::Above);
    selection.shift_rows(-2, 0, 2, packed.width);
    assert_eq!(
        selection.selected_text_packed(&packed, &pools),
        "row1\nrow0",
        "packed capture should preserve copied text for the off-screen debt"
    );
}

#[test]
fn test_selection_controller_packed_double_click_drag_copy_and_take() {
    let mut pools = CanvasPackedCellPools::new();
    let mut line_cache = CanvasPackedLineCache::new();
    let style = CanvasResolvedStyle::default();
    let mut packed = CanvasPackedScreen::new(16, 1);
    packed.write_line_with_cache(
        &mut pools,
        &mut line_cache,
        0,
        0,
        "one two three",
        style,
        None,
    );
    let mut controller = SelectionController::new();

    controller.handle_left_press_packed(&packed, &pools, 4, 0, 1_000, false);
    let outcome = controller.handle_left_press_packed(&packed, &pools, 4, 0, 1_200, false);
    assert_eq!(outcome.kind, SelectionMousePressKind::Double);
    assert!(outcome.cancel_pending_hyperlink);
    assert_eq!(controller.selected_text_packed(&packed, &pools), "two");

    controller.handle_drag_packed(&packed, &pools, 10, 0);
    assert_eq!(
        controller.selected_text_packed(&packed, &pools),
        "two three"
    );
    assert!(controller.handle_release());
    assert_eq!(
        controller
            .copy_on_select_text_packed(&packed, &pools)
            .as_deref(),
        Some("two three")
    );
    assert_eq!(controller.copy_on_select_text_packed(&packed, &pools), None);
    assert_eq!(
        controller.take_selected_text_packed(&packed, &pools),
        "two three"
    );
    assert!(!controller.has_selection());
}

#[test]
fn test_selection_controller_packed_release_hyperlink_and_scroll_jump() {
    let mut pools = CanvasPackedCellPools::new();
    let mut line_cache = CanvasPackedLineCache::new();
    let style = CanvasResolvedStyle::default();
    let mut linked = CanvasPackedScreen::new(8, 1);
    linked.write_line_with_cache(
        &mut pools,
        &mut line_cache,
        0,
        0,
        "中x",
        style,
        Some("https://linked.example"),
    );
    let mut controller = SelectionController::new();
    controller.handle_left_press_packed(&linked, &pools, 1, 0, 1_000, false);
    let release = controller.handle_release_at_packed(&linked, &pools, 1, 0, false);
    assert_eq!(release.click, Some(SelectionPoint { col: 1, row: 0 }));
    assert_eq!(
        release.hyperlink.as_deref(),
        Some("https://linked.example"),
        "packed release should resolve wide-tail OSC 8 hyperlinks"
    );

    let mut packed = CanvasPackedScreen::new(6, 3);
    packed.write_line_with_cache(&mut pools, &mut line_cache, 0, 0, "row0", style, None);
    packed.write_line_with_cache(&mut pools, &mut line_cache, 0, 1, "row1", style, None);
    packed.write_line_with_cache(&mut pools, &mut line_cache, 0, 2, "row2", style, None);
    let mut controller = SelectionController::new();
    controller.handle_left_press_packed(&packed, &pools, 0, 0, 2_000, false);
    controller.handle_drag_packed(&packed, &pools, 3, 2);
    controller.handle_release();

    let outcome = controller.translate_for_scroll_jump_packed(&packed, &pools, 1, 0, 2);
    packed.shift_rows(0, 2, 1);
    assert_eq!(
        outcome,
        SelectionScrollOutcome {
            translated: true,
            cleared: false,
        }
    );
    assert_eq!(
        controller.selected_text_packed(&packed, &pools),
        "row0\nrow1\nrow2"
    );
}

#[test]
fn test_selection_controller_packed_follow_scroll_preserves_copied_text() {
    let mut pools = CanvasPackedCellPools::new();
    let mut line_cache = CanvasPackedLineCache::new();
    let style = CanvasResolvedStyle::default();
    let mut packed = CanvasPackedScreen::new(6, 3);
    packed.write_line_with_cache(&mut pools, &mut line_cache, 0, 0, "row0", style, None);
    packed.write_line_with_cache(&mut pools, &mut line_cache, 0, 1, "row1", style, None);
    packed.write_line_with_cache(&mut pools, &mut line_cache, 0, 2, "row2", style, None);
    let mut controller = SelectionController::new();
    controller.handle_left_press_packed(&packed, &pools, 0, 0, 1_000, false);
    controller.handle_drag_packed(&packed, &pools, 3, 2);
    controller.handle_release();

    let outcome = controller.translate_for_follow_scroll_packed(&packed, &pools, 1, 0, 2);
    packed.shift_rows(0, 2, 1);

    assert!(outcome.translated);
    assert!(!outcome.cleared);
    assert_eq!(
        controller.selected_text_packed(&packed, &pools),
        "row0\nrow1\nrow2"
    );
}

#[test]
fn test_selection_controller_packed_drag_autoscroll_captures_and_shifts_anchor() {
    let mut pools = CanvasPackedCellPools::new();
    let mut line_cache = CanvasPackedLineCache::new();
    let style = CanvasResolvedStyle::default();
    let mut packed = CanvasPackedScreen::new(6, 4);
    packed.write_line_with_cache(&mut pools, &mut line_cache, 0, 1, "row1", style, None);
    packed.write_line_with_cache(&mut pools, &mut line_cache, 0, 2, "row2", style, None);
    let mut controller = SelectionController::new();
    controller.handle_left_press_packed(&packed, &pools, 0, 1, 1_000, false);
    controller.handle_drag_packed(&packed, &pools, 3, 4);

    assert_eq!(
        controller.drag_scroll_direction(1, 3),
        Some(SelectionDragScrollDirection::Down)
    );
    let outcome = controller.translate_for_drag_autoscroll_packed(
        &packed,
        &pools,
        SelectionDragScrollDirection::Down,
        1,
        1,
        3,
    );

    assert!(outcome.translated);
    assert_eq!(
        controller.selection().anchor(),
        Some(SelectionPoint { col: 0, row: 1 })
    );
    assert_eq!(controller.selection.virtual_anchor_row, Some(0));
    assert_eq!(
        controller.selection.scrolled_off_above,
        vec!["row1".to_string()]
    );
}

#[test]
fn test_canvas_packed_screen_selection_text_matches_cc_get_selected_text_shape() {
    let mut pools = CanvasPackedCellPools::new();
    let mut line_cache = CanvasPackedLineCache::new();
    let style = CanvasResolvedStyle::default();
    let mut packed = CanvasPackedScreen::new(8, 2);
    packed.write_line_with_cache(&mut pools, &mut line_cache, 0, 0, "A好 ", style, None);
    packed.write_line_with_cache(&mut pools, &mut line_cache, 0, 1, "world", style, None);
    packed.soft_wrap[1] = 4;
    packed.mark_no_select_region(1, 1, 1, 1);

    assert_eq!(
        packed.selected_text(
            &pools,
            SelectionRange::new(
                SelectionPoint { col: 0, row: 0 },
                SelectionPoint { col: 4, row: 1 },
            ),
        ),
        "A好 wrld",
        "packed selected text should skip wide tails/noSelect and join soft-wrap continuations"
    );
}

#[test]
fn test_canvas_packed_screen_apply_selection_overlay_skips_no_select_and_marks_damage() {
    let mut pools = CanvasPackedCellPools::new();
    let mut line_cache = CanvasPackedLineCache::new();
    let style = CanvasResolvedStyle::default();
    let mut packed = CanvasPackedScreen::new(6, 1);
    packed.write_line_with_cache(&mut pools, &mut line_cache, 0, 0, "abcdef", style, None);
    packed.clear_damage();
    packed.mark_no_select_region(2, 0, 1, 1);

    assert!(packed.apply_selection_overlay(
        &mut pools,
        SelectionRange::new(
            SelectionPoint { col: 1, row: 0 },
            SelectionPoint { col: 3, row: 0 },
        ),
        StyleOverlay::selection_background(Color::Blue),
    ));
    let is_blue = |screen: &CanvasPackedScreen, pools: &CanvasPackedCellPools, x| {
        pools
            .style(screen.cell(x, 0).unwrap().style_id)
            .unwrap_or_default()
            .background_color
            == Some(Color::Blue)
    };

    assert!(is_blue(&packed, &pools, 1));
    assert!(
        !is_blue(&packed, &pools, 2),
        "noSelect cells are not highlighted"
    );
    assert!(is_blue(&packed, &pools, 3));
    assert_eq!(
        packed.damage_region(),
        Some(DamageRegion {
            x: 1,
            y: 0,
            width: 3,
            height: 1,
        })
    );
}

#[test]
fn test_canvas_packed_screen_scan_text_positions_is_case_insensitive_and_wide_aware() {
    let mut pools = CanvasPackedCellPools::new();
    let mut line_cache = CanvasPackedLineCache::new();
    let style = CanvasResolvedStyle::default();
    let mut packed = CanvasPackedScreen::new(12, 1);
    packed.write_line_with_cache(&mut pools, &mut line_cache, 0, 0, "ab中c AB", style, None);

    assert_eq!(
        packed.scan_text_positions(&pools, "中c"),
        vec![TextMatchPosition {
            row: 0,
            col: 2,
            len: 3,
        }],
        "packed match spans should be terminal-cell based and include wide tails"
    );
    assert_eq!(
        packed.scan_text_positions(&pools, "ab"),
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
        "packed search should be case-insensitive and non-overlapping"
    );
}

#[test]
fn test_canvas_packed_screen_scan_text_positions_region_returns_relative_positions() {
    let mut pools = CanvasPackedCellPools::new();
    let mut line_cache = CanvasPackedLineCache::new();
    let style = CanvasResolvedStyle::default();
    let mut packed = CanvasPackedScreen::new(16, 3);
    packed.write_line_with_cache(
        &mut pools,
        &mut line_cache,
        2,
        1,
        "xx lazy 中c",
        style,
        None,
    );
    packed.mark_no_select_region(5, 1, 4, 1);

    assert_eq!(
        packed.scan_text_positions_region(&pools, 5, 1, 10, 1, "lazy"),
        Vec::<TextMatchPosition>::new(),
        "packed region scanning should respect noSelect metadata"
    );
    assert_eq!(
        packed.scan_text_positions_region(&pools, 8, 1, 8, 1, "中c"),
        vec![TextMatchPosition {
            row: 0,
            col: 2,
            len: 3,
        }],
        "packed region positions should be relative to the scanned region"
    );
}

#[test]
fn test_canvas_packed_screen_apply_search_highlight_skips_no_select_and_marks_damage() {
    let mut pools = CanvasPackedCellPools::new();
    let mut line_cache = CanvasPackedLineCache::new();
    let style = CanvasResolvedStyle::default();
    let mut packed = CanvasPackedScreen::new(12, 1);
    packed.write_line_with_cache(&mut pools, &mut line_cache, 0, 0, "foo foo", style, None);
    packed.clear_damage();
    packed.mark_no_select_region(0, 0, 3, 1);

    assert!(packed.apply_search_highlight(&mut pools, "foo", StyleOverlay::inverse()));
    let is_inverted = |screen: &CanvasPackedScreen, pools: &CanvasPackedCellPools, x| {
        pools
            .style(screen.cell(x, 0).unwrap().style_id)
            .unwrap_or_default()
            .text
            .invert
    };

    for col in 0..3 {
        assert!(!is_inverted(&packed, &pools, col));
    }
    for col in 4..7 {
        assert!(is_inverted(&packed, &pools, col));
    }
    assert_eq!(
        packed.damage_region(),
        Some(DamageRegion {
            x: 4,
            y: 0,
            width: 3,
            height: 1,
        })
    );
}

#[test]
fn test_canvas_packed_screen_apply_positioned_highlight_translates_row_offset() {
    let mut pools = CanvasPackedCellPools::new();
    let mut line_cache = CanvasPackedLineCache::new();
    let style = CanvasResolvedStyle::default();
    let mut packed = CanvasPackedScreen::new(8, 2);
    packed.write_line_with_cache(&mut pools, &mut line_cache, 0, 1, "target", style, None);
    let positions = vec![TextMatchPosition {
        row: 0,
        col: 0,
        len: 6,
    }];

    assert!(!packed.apply_positioned_highlight(
        &mut pools,
        &positions,
        -1,
        0,
        StyleOverlay::inverse(),
    ));
    assert!(packed.apply_positioned_highlight(
        &mut pools,
        &positions,
        1,
        0,
        StyleOverlay::inverse(),
    ));
    let is_inverted = |screen: &CanvasPackedScreen, pools: &CanvasPackedCellPools, x| {
        pools
            .style(screen.cell(x, 1).unwrap().style_id)
            .unwrap_or_default()
            .text
            .invert
    };
    assert!(is_inverted(&packed, &pools, 0));
    assert!(is_inverted(&packed, &pools, 5));
}

#[test]
fn test_canvas_packed_screen_hyperlink_at_prefers_osc8_and_wide_tail() {
    let mut pools = CanvasPackedCellPools::new();
    let mut line_cache = CanvasPackedLineCache::new();
    let style = CanvasResolvedStyle::default();
    let mut packed = CanvasPackedScreen::new(20, 1);
    packed.write_line_with_cache(
        &mut pools,
        &mut line_cache,
        0,
        0,
        "中x",
        style,
        Some("https://linked.example"),
    );

    assert_eq!(
        packed.hyperlink_at(&pools, 0, 0).as_deref(),
        Some("https://linked.example")
    );
    assert_eq!(
        packed.hyperlink_at(&pools, 1, 0).as_deref(),
        Some("https://linked.example"),
        "packed wide-character tail should resolve the head cell's OSC 8 link"
    );
}

#[test]
fn test_canvas_packed_screen_plain_text_url_at_trims_sentence_punctuation() {
    let mut pools = CanvasPackedCellPools::new();
    let mut line_cache = CanvasPackedLineCache::new();
    let style = CanvasResolvedStyle::default();
    let mut packed = CanvasPackedScreen::new(40, 1);
    packed.write_line_with_cache(
        &mut pools,
        &mut line_cache,
        0,
        0,
        "see https://example.com/foo).",
        style,
        None,
    );

    assert_eq!(
        packed.hyperlink_at(&pools, 8, 0).as_deref(),
        Some("https://example.com/foo")
    );
    assert_eq!(packed.hyperlink_at(&pools, 29, 0), None);
}

#[test]
fn test_canvas_packed_screen_plain_text_url_at_chooses_scheme_under_click() {
    let mut pools = CanvasPackedCellPools::new();
    let mut line_cache = CanvasPackedLineCache::new();
    let style = CanvasResolvedStyle::default();
    let mut packed = CanvasPackedScreen::new(50, 1);
    packed.write_line_with_cache(
        &mut pools,
        &mut line_cache,
        0,
        0,
        "https://a.com,https://b.com",
        style,
        None,
    );

    assert_eq!(
        packed.hyperlink_at(&pools, 8, 0).as_deref(),
        Some("https://a.com")
    );
    assert_eq!(
        packed.hyperlink_at(&pools, 20, 0).as_deref(),
        Some("https://b.com")
    );
}

#[test]
fn test_canvas_packed_screen_plain_text_url_at_respects_no_select_boundaries() {
    let mut pools = CanvasPackedCellPools::new();
    let mut line_cache = CanvasPackedLineCache::new();
    let style = CanvasResolvedStyle::default();
    let mut packed = CanvasPackedScreen::new(30, 1);
    packed.write_line_with_cache(
        &mut pools,
        &mut line_cache,
        0,
        0,
        "https://example.com",
        style,
        None,
    );
    packed.mark_no_select_region(0, 0, 5, 1);

    assert_eq!(packed.hyperlink_at(&pools, 2, 0), None);
    assert_eq!(packed.hyperlink_at(&pools, 8, 0), None);
}

#[test]
fn test_canvas_packed_screen_debug_repaint_overlay_marks_changed_and_damaged_cells() {
    let mut pools = CanvasPackedCellPools::new();
    let mut line_cache = CanvasPackedLineCache::new();
    let style = CanvasResolvedStyle::default();
    let mut prev = CanvasPackedScreen::new(5, 1);
    prev.write_line_with_cache(&mut pools, &mut line_cache, 0, 0, "abcde", style, None);
    prev.clear_damage();
    prev.mark_damage(DamageRegion {
        x: 4,
        y: 0,
        width: 1,
        height: 1,
    });

    let mut next = CanvasPackedScreen::new(5, 1);
    next.write_line_with_cache(&mut pools, &mut line_cache, 0, 0, "abXde", style, None);
    next.clear_damage();
    next.mark_damage(DamageRegion {
        x: 2,
        y: 0,
        width: 1,
        height: 1,
    });

    let overlayed = CanvasPackedScreen::debug_repaint_overlay(
        Some(&prev),
        &next,
        &mut pools,
        StyleOverlay::inverse(),
    );
    let cell_inverted = |screen: &CanvasPackedScreen, pools: &CanvasPackedCellPools, x| {
        pools
            .style(screen.cell(x, 0).unwrap().style_id)
            .unwrap_or_default()
            .text
            .invert
    };

    assert!(!cell_inverted(&overlayed, &pools, 1));
    assert!(cell_inverted(&overlayed, &pools, 2));
    assert!(!cell_inverted(&overlayed, &pools, 3));
    assert!(cell_inverted(&overlayed, &pools, 4));
    assert_eq!(
        overlayed.damage_region(),
        Some(DamageRegion {
            x: 2,
            y: 0,
            width: 3,
            height: 1,
        })
    );
}
