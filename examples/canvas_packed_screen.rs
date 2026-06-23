//! Demonstrates opt-in packed canvas snapshots.
//!
//! CC Ink stores screen cells as interned character/style/link IDs in packed
//! arrays for fast retained diffs. iocraft keeps `Canvas` typed by default, but
//! custom renderers and benchmarks can opt into the same shape with
//! `CanvasPackedCellPools` + `Canvas::pack_with(...)`, packed diffing, direct
//! packed writes, output-operation queues, styled run line-cluster caching, resolved cell views, sparse visible-cell
//! checks, row-change prefix planning, snapshot-local selection text/overlay,
//! word/line selection range planning, packed selection state/controller helpers,
//! search highlight scanning/overlay,
//! hyperlink/plain-text URL lookup,
//! clear/blit/absolute-clear-guard/shift helpers, debug repaint overlays,
//! reusable screen reset, and transient pool migration.

use iocraft::prelude::*;

fn main() {
    let mut canvas = Canvas::new(8, 2);
    let mut style = CanvasTextStyle::default();
    style.color = Some(Color::Green);
    style.weight = Weight::Bold;

    {
        let mut view = canvas.subview_mut(0, 0, 0, 0, 8, 2);
        view.set_text_with_link(0, 0, "A好", style, Some("https://example.com"));
        view.mark_no_select_region(0, 0, 1, 1);
        view.mark_soft_wrap_continuation(1, 2);
    }
    canvas.set_overlay(0, 0, StyleOverlay::selection_background(Color::Blue));

    let mut pools = CanvasPackedCellPools::new();
    let packed = canvas.pack_with(&mut pools);
    let first = packed.cell(0, 0).expect("cell in bounds");
    let wide = packed.cell(1, 0).expect("wide cell in bounds");

    println!("packed size: {}x{}", packed.width, packed.height);
    println!(
        "first char id {} -> {:?}",
        first.char_id,
        pools.character(first.char_id)
    );
    println!(
        "first style id {} -> {:?}",
        first.style_id,
        pools.style(first.style_id)
    );
    println!(
        "first link id {} -> {:?}",
        first.hyperlink_id,
        pools.hyperlink(first.hyperlink_id)
    );
    println!("wide cell width: {:?}", wide.width);
    if let Some(view) = packed.cell_view(&pools, 0, 0) {
        println!(
            "resolved cell view: char={:?} style={:?} link={:?}",
            view.character, view.style, view.hyperlink
        );
    }
    println!(
        "wide tail character: {:?}",
        packed.char_in_cell(&pools, 2, 0)
    );
    println!(
        "wide tail hyperlink lookup: {:?}",
        packed.hyperlink_at(&pools, 2, 0)
    );
    println!(
        "row 1 soft-wrap marker: {}",
        packed.soft_wrap_continuation(1)
    );
    println!("cell 0,0 no-select: {}", packed.is_no_select(0, 0));

    let mut next_canvas = canvas.clone();
    next_canvas
        .subview_mut(0, 0, 0, 0, 8, 2)
        .set_text(0, 0, "B", style);
    next_canvas.mark_damage(DamageRegion {
        x: 0,
        y: 0,
        width: 1,
        height: 1,
    });
    let next = next_canvas.pack_with(&mut pools);
    println!(
        "row 0 change starts at: {:?}",
        packed.row_change_start(&next, 0)
    );
    for change in packed.diff(&next) {
        println!(
            "diff at {},{}: {:?} -> {:?}",
            change.x, change.y, change.removed, change.added
        );
    }
    let debug_overlay = CanvasPackedScreen::debug_repaint_overlay(
        Some(&packed),
        &next,
        &mut pools,
        StyleOverlay::current_match(Color::Yellow),
    );
    let debug_style = debug_overlay
        .cell(0, 0)
        .and_then(|cell| pools.style(cell.style_id));
    println!("packed debug repaint style at 0,0: {debug_style:?}");
    let selected_text = debug_overlay.selected_text(
        &pools,
        SelectionRange::new(
            SelectionPoint { col: 0, row: 0 },
            SelectionPoint { col: 2, row: 0 },
        ),
    );
    let mut selected_overlay = debug_overlay.clone();
    let selection_overlay_applied = selected_overlay.apply_selection_overlay(
        &mut pools,
        SelectionRange::new(
            SelectionPoint { col: 0, row: 0 },
            SelectionPoint { col: 2, row: 0 },
        ),
        StyleOverlay::selection_background(Color::Blue),
    );
    let word_range = selected_overlay.selection_range_for_word_at(&pools, 0, 0);
    let line_range = selected_overlay.selection_range_for_line(0);
    let mut packed_selection_state = SelectionState::new();
    packed_selection_state.start_multi_click_packed(
        &selected_overlay,
        &pools,
        0,
        0,
        SelectionClickCount::Double,
    );
    let packed_selection_state_text =
        packed_selection_state.selected_text_packed(&selected_overlay, &pools);
    let mut packed_selection_controller = SelectionController::new();
    packed_selection_controller.handle_left_press_packed(
        &selected_overlay,
        &pools,
        0,
        0,
        1_000,
        false,
    );
    packed_selection_controller.handle_drag_packed(&selected_overlay, &pools, 2, 0);
    packed_selection_controller.handle_release();
    let packed_selection_controller_text =
        packed_selection_controller.selected_text_packed(&selected_overlay, &pools);
    let search_matches = selected_overlay.scan_text_positions(&pools, "b");
    let search_highlighted = selected_overlay.apply_search_highlight(
        &mut pools,
        "b",
        StyleOverlay::current_match(Color::Yellow),
    );
    println!("packed selected text: {selected_text:?}");
    println!("packed selection overlay applied: {selection_overlay_applied}");
    println!("packed word/line ranges: {word_range:?}/{line_range:?}");
    println!("packed selection state text: {packed_selection_state_text:?}");
    println!("packed selection controller text: {packed_selection_controller_text:?}");
    println!("packed search matches: {search_matches:?}, highlighted={search_highlighted}");

    let mut retained = next.clone();
    let cleared = retained.clear_region(1, 0, 1, 1);
    let restored = retained.blit_region_from(&packed, 0, 0, 2, 1);
    let guarded_blits = retained.blit_region_from_excluding_clears(
        &packed,
        0,
        0,
        2,
        2,
        &[DamageRegion {
            x: 0,
            y: 1,
            width: 2,
            height: 1,
        }],
    );
    let shifted = retained.shift_rows(0, 1, 1);
    let mut accent_text = CanvasTextStyle::default();
    accent_text.color = Some(Color::Yellow);
    accent_text.weight = Weight::Bold;
    let styled = retained.set_cell_style(
        &mut pools,
        0,
        0,
        CanvasResolvedStyle {
            text: accent_text,
            background_color: None,
        },
    );
    let selection_style_id = retained
        .cell(0, 0)
        .map(|cell| pools.style_id_with_selection_background(cell.style_id, Color::Blue));
    let selection_styled = selection_style_id
        .map(|style_id| retained.set_cell_style_id(0, 0, style_id))
        .unwrap_or(false);
    let no_select = retained.mark_no_select_region(0, 0, 2, 1);
    let mut direct = CanvasPackedScreen::new(4, 1);
    direct.set_cell_text(
        &mut pools,
        0,
        0,
        "好",
        CanvasResolvedStyle::default(),
        None,
        CanvasPackedCellWidth::Wide,
    );
    let mut line_cache = CanvasPackedLineCache::new();
    let mut line_written = CanvasPackedScreen::new(10, 1);
    let mut link_text = CanvasTextStyle::default();
    link_text.color = Some(Color::Cyan);
    let line_end = line_written.write_line_runs_with_cache(
        &mut pools,
        &mut line_cache,
        0,
        0,
        [
            CanvasPackedLineRun {
                text: "A\t",
                style: CanvasResolvedStyle {
                    text: link_text,
                    background_color: None,
                },
                hyperlink: Some("https://example.com"),
            },
            CanvasPackedLineRun {
                text: "B",
                style: CanvasResolvedStyle::default(),
                hyperlink: None,
            },
        ],
    );
    let mut output_queue = CanvasPackedOutput::new(10, 1);
    output_queue.clip(CanvasPackedOutputClip {
        x1: Some(2),
        x2: Some(6),
        y1: None,
        y2: None,
    });
    output_queue.write(0, 0, "abcdef", CanvasResolvedStyle::default(), None, None);
    output_queue.unclip();
    output_queue.no_select(DamageRegion {
        x: 2,
        y: 0,
        width: 2,
        height: 1,
    });
    let queued_screen = output_queue.get(&mut pools);
    let queued_char = queued_screen
        .char_in_cell(&pools, 2, 0)
        .unwrap_or(" ")
        .to_string();
    let queued_no_select = queued_screen.is_no_select(2, 0);
    let mut packed_style_cache = CanvasStyleTransitionCache::new();
    let queued_ansi = queued_screen
        .ansi_row_with_style_cache(&pools, &mut packed_style_cache, 0, 0)
        .expect("serialize packed row");
    let mut next_generation = pools.fork_with_transient_pools_cleared();
    let migrated = direct.migrate_transient_pools(&pools, &mut next_generation);
    let sparse_visible = direct.visible_cell(&next_generation, 0, 0, None).is_some();
    let sparse_tail_visible = direct.visible_cell(&next_generation, 1, 0, None).is_some();
    let mut reusable = direct.clone();
    reusable.reset(2, 2);
    println!("clear damage: {cleared:?}");
    println!("restored damage: {restored:?}");
    println!("guarded blit spans: {guarded_blits:?}");
    println!("shifted rows: {shifted}");
    println!("styled packed cell: {styled}");
    println!("selection-style overlay id applied: {selection_styled}");
    println!("marked packed no-select: {no_select}");
    println!("direct packed wide tail: {:?}", direct.cell(1, 0));
    println!(
        "cached packed line end: {line_end}, entries={}",
        line_cache.len()
    );
    println!("queued output char/no-select: {queued_char:?}/{queued_no_select}");
    println!("queued packed ANSI row bytes: {}", queued_ansi.len());
    println!("migrated transient pools: {migrated}");
    println!("sparse visible head/tail: {sparse_visible}/{sparse_tail_visible}");
    println!(
        "reset reusable packed screen: {}x{} dirty={:?}",
        reusable.width, reusable.height, reusable.damage_region
    );
}
