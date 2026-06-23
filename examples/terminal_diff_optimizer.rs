//! Demonstrates CC Ink-style terminal patch optimization and serialization.
//!
//! These are mode-neutral utilities for custom renderers: optimization works on
//! an in-memory patch list, frame/inline-clear heuristics work on geometry only,
//! stateful fullscreen frame planners retain previous canvases in memory, and
//! serialization returns ANSI text without changing terminal modes. Writing the
//! ANSI is still opt-in application policy.

use iocraft::prelude::*;

fn main() {
    let patches = vec![
        TerminalPatch::Stdout(String::new()),
        TerminalPatch::CursorMove { x: 1, y: 0 },
        TerminalPatch::CursorMove { x: 0, y: 2 },
        TerminalPatch::StyleStr("\x1b[31m".to_string()),
        TerminalPatch::StyleStr("\x1b[1m".to_string()),
        TerminalPatch::Hyperlink {
            uri: "https://example.test".to_string(),
        },
        TerminalPatch::Hyperlink {
            uri: "https://example.test".to_string(),
        },
        TerminalPatch::CursorHide,
        TerminalPatch::CursorShow,
        TerminalPatch::Stdout("rendered text".to_string()),
    ];

    let optimized = optimize_terminal_patches(patches);
    println!("optimized patches:\n{optimized:#?}");

    let ansi = terminal_patches_to_ansi(&optimized, true);
    println!(
        "serialized without DEC 2026 markers: {:?}",
        ansi.escape_debug()
    );

    let mut fullscreen_diff = vec![TerminalPatch::Stdout("changed rows".to_string())];
    let fullscreen_cursor_plan =
        plan_terminal_fullscreen_diff_patches(TerminalFullscreenDiffPatchOptions {
            fullscreen: true,
            has_diff: !fullscreen_diff.is_empty(),
            erase_before_paint: false,
            terminal_rows: Some(24),
        });
    fullscreen_cursor_plan.apply_to(&mut fullscreen_diff);
    println!(
        "fullscreen anchored diff: {:?}",
        terminal_patches_to_ansi(&fullscreen_diff, true).escape_debug()
    );

    let mut previous = Canvas::new(10, 2);
    previous
        .subview_mut(0, 0, 0, 0, 10, 2)
        .set_text(0, 0, "row zero", CanvasTextStyle::default());
    previous
        .subview_mut(0, 0, 0, 0, 10, 2)
        .set_text(0, 1, "old row", CanvasTextStyle::default());
    previous.clear_damage();

    let mut next = Canvas::new(10, 2);
    next.subview_mut(0, 0, 0, 0, 10, 2)
        .set_text(0, 0, "row zero", CanvasTextStyle::default());
    next.subview_mut(0, 0, 0, 0, 10, 2)
        .set_text(0, 1, "new row", CanvasTextStyle::default());
    next.clear_damage();

    let frame_plan = plan_terminal_fullscreen_canvas_frame_patches(
        Some(&previous),
        &next,
        TerminalFullscreenCanvasFramePatchOptions {
            canvas_diff: TerminalFullscreenCanvasDiffOptions {
                top_row: 0,
                force_full_repaint: false,
            },
            scroll_patch_ansi: None,
            erase_before_paint: false,
            terminal_rows: Some(24),
            optimize: true,
        },
    );
    println!(
        "fullscreen canvas frame diff: {:?}",
        terminal_patches_to_ansi(&frame_plan.patches, true).escape_debug()
    );

    let mut pools = CanvasPackedCellPools::new();
    let mut line_cache = CanvasPackedLineCache::new();
    let mut packed_style_cache = CanvasStyleTransitionCache::new();
    let mut packed_previous = CanvasPackedScreen::new(10, 2);
    packed_previous.write_line_with_cache(
        &mut pools,
        &mut line_cache,
        0,
        0,
        "packed old",
        CanvasResolvedStyle::default(),
        None,
    );
    packed_previous.clear_damage();
    let mut packed_next = CanvasPackedScreen::new(10, 2);
    packed_next.write_line_with_cache(
        &mut pools,
        &mut line_cache,
        0,
        0,
        "packed new",
        CanvasResolvedStyle::default(),
        None,
    );
    packed_next.clear_damage();
    let packed_frame_plan = plan_terminal_fullscreen_packed_canvas_frame_patches(
        Some(&packed_previous),
        &packed_next,
        &pools,
        &mut packed_style_cache,
        TerminalFullscreenCanvasFramePatchOptions {
            canvas_diff: TerminalFullscreenCanvasDiffOptions {
                top_row: 0,
                force_full_repaint: false,
            },
            scroll_patch_ansi: None,
            erase_before_paint: false,
            terminal_rows: Some(24),
            optimize: true,
        },
    );
    println!(
        "fullscreen packed canvas frame diff: {:?}",
        terminal_patches_to_ansi(&packed_frame_plan.patches, true).escape_debug()
    );

    let mut packed_scroll_prev = CanvasPackedScreen::new(10, 3);
    packed_scroll_prev.write_line_with_cache(
        &mut pools,
        &mut line_cache,
        0,
        0,
        "scroll a",
        CanvasResolvedStyle::default(),
        None,
    );
    packed_scroll_prev.write_line_with_cache(
        &mut pools,
        &mut line_cache,
        0,
        1,
        "scroll b",
        CanvasResolvedStyle::default(),
        None,
    );
    packed_scroll_prev.write_line_with_cache(
        &mut pools,
        &mut line_cache,
        0,
        2,
        "scroll c",
        CanvasResolvedStyle::default(),
        None,
    );
    packed_scroll_prev.clear_damage();
    let mut packed_scroll_next = CanvasPackedScreen::new(10, 3);
    packed_scroll_next.write_line_with_cache(
        &mut pools,
        &mut line_cache,
        0,
        0,
        "scroll b",
        CanvasResolvedStyle::default(),
        None,
    );
    packed_scroll_next.write_line_with_cache(
        &mut pools,
        &mut line_cache,
        0,
        1,
        "scroll c",
        CanvasResolvedStyle::default(),
        None,
    );
    packed_scroll_next.write_line_with_cache(
        &mut pools,
        &mut line_cache,
        0,
        2,
        "scroll d",
        CanvasResolvedStyle::default(),
        None,
    );
    packed_scroll_next.clear_damage();
    let packed_scroll_plan = plan_terminal_fullscreen_packed_canvas_scroll_frame_patches(
        &packed_scroll_prev,
        &packed_scroll_next,
        &pools,
        &mut packed_style_cache,
        Some(ScrollHint {
            top: 0,
            bottom: 2,
            delta: 1,
        }),
        TerminalScrollHintPatchOptions::fullscreen_synchronized(),
        TerminalFullscreenCanvasFramePatchOptions {
            canvas_diff: TerminalFullscreenCanvasDiffOptions {
                top_row: 0,
                force_full_repaint: false,
            },
            scroll_patch_ansi: None,
            erase_before_paint: false,
            terminal_rows: Some(24),
            optimize: true,
        },
    )
    .expect("valid fullscreen scroll hint");
    println!(
        "fullscreen packed DECSTBM frame: scroll={}, {:?}",
        packed_scroll_plan.had_scroll_patch(),
        terminal_patches_to_ansi(&packed_scroll_plan.frame.patches, true).escape_debug()
    );

    let mut stateful_packed = TerminalFullscreenPackedCanvasFrameState::new();
    let stateful_first = stateful_packed.plan_frame(
        &packed_scroll_prev,
        &pools,
        TerminalFullscreenCanvasFramePatchOptions {
            terminal_rows: Some(24),
            ..TerminalFullscreenCanvasFramePatchOptions::default()
        },
    );
    let stateful_scroll = stateful_packed
        .plan_scroll_frame(
            &packed_scroll_next,
            &pools,
            Some(ScrollHint {
                top: 0,
                bottom: 2,
                delta: 1,
            }),
            TerminalScrollHintPatchOptions::fullscreen_synchronized(),
            TerminalFullscreenCanvasFramePatchOptions {
                terminal_rows: Some(24),
                ..TerminalFullscreenCanvasFramePatchOptions::default()
            },
        )
        .expect("stateful packed scroll frame");
    println!(
        "stateful packed fullscreen frames: first_patches={}, scroll={}",
        stateful_first.patches.len(),
        stateful_scroll.had_scroll_patch()
    );

    let clear_reason = should_clear_terminal_screen(
        TerminalFrameBounds {
            screen_height: 3,
            viewport_width: 80,
            viewport_height: 10,
        },
        TerminalFrameBounds {
            screen_height: 10,
            viewport_width: 80,
            viewport_height: 10,
        },
    );
    println!("clear reason for viewport-filling frame: {clear_reason:?}");

    let inline_bounds = TerminalInlineDiffBounds {
        prev_screen_height: 12,
        next_screen_height: 12,
        prev_viewport_width: 80,
        prev_viewport_height: 10,
        next_viewport_width: 80,
        next_viewport_height: 10,
    };
    let inline = analyze_terminal_inline_diff(inline_bounds);
    println!(
        "main-screen unreachable top rows before sparse diff fallback: {}",
        inline.unreachable_rows
    );

    let mut inline_previous = Canvas::new(10, 12);
    inline_previous.subview_mut(0, 0, 0, 0, 10, 12).set_text(
        0,
        0,
        "old top",
        CanvasTextStyle::default(),
    );
    inline_previous.clear_damage();
    let mut inline_next = Canvas::new(10, 12);
    inline_next.subview_mut(0, 0, 0, 0, 10, 12).set_text(
        0,
        0,
        "new top",
        CanvasTextStyle::default(),
    );
    inline_next.clear_damage();
    let inline_decision =
        plan_terminal_inline_canvas_diff(&inline_previous, &inline_next, inline_bounds);
    println!(
        "main-screen sparse diff decision: clear={:?}, debug={:?}",
        inline_decision.clear_reason, inline_decision.debug
    );

    let inline_fallback =
        plan_terminal_inline_canvas_frame_patches(&inline_previous, &inline_next, inline_bounds);
    println!(
        "main-screen clear fallback patch count: {}",
        inline_fallback.patches.len()
    );

    let mut inline_packed_previous = CanvasPackedScreen::new(10, 12);
    inline_packed_previous.write_line_with_cache(
        &mut pools,
        &mut line_cache,
        0,
        0,
        "packed old",
        CanvasResolvedStyle::default(),
        None,
    );
    inline_packed_previous.clear_damage();
    let mut inline_packed_next = CanvasPackedScreen::new(10, 12);
    inline_packed_next.write_line_with_cache(
        &mut pools,
        &mut line_cache,
        0,
        0,
        "packed new",
        CanvasResolvedStyle::default(),
        None,
    );
    let inline_packed_decision = plan_terminal_inline_packed_canvas_diff(
        &inline_packed_previous,
        &inline_packed_next,
        &pools,
        inline_bounds,
    );
    let mut inline_packed_style_cache = CanvasStyleTransitionCache::new();
    let inline_packed_fallback = plan_terminal_inline_packed_canvas_frame_patches(
        &inline_packed_previous,
        &inline_packed_next,
        &pools,
        &mut inline_packed_style_cache,
        inline_bounds,
    );
    println!(
        "main-screen packed diff decision: clear={:?}, fallback patches={}",
        inline_packed_decision.clear_reason,
        inline_packed_fallback.patches.len()
    );
}
