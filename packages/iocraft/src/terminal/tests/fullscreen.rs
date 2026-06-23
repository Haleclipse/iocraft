use super::super::*;
use crate::prelude::*;

#[test]
fn test_plan_terminal_fullscreen_diff_patches_matches_cc_alt_screen_anchor_and_park() {
    let mut diff = vec![TerminalPatch::Stdout("body".to_string())];
    let plan = plan_terminal_fullscreen_diff_patches(TerminalFullscreenDiffPatchOptions {
        fullscreen: true,
        has_diff: !diff.is_empty(),
        erase_before_paint: false,
        terminal_rows: Some(24),
    });

    assert_eq!(
        plan.pre_diff_patch,
        Some(TerminalPatch::Stdout("\x1b[H".to_string()))
    );
    assert_eq!(
        plan.post_diff_patch,
        Some(TerminalPatch::Stdout("\x1b[24;1H".to_string()))
    );
    plan.apply_to(&mut diff);
    assert_eq!(
        terminal_patches_to_ansi(&diff, true),
        "\x1b[Hbody\x1b[24;1H"
    );

    let erase = plan_terminal_fullscreen_diff_patches(TerminalFullscreenDiffPatchOptions {
        fullscreen: true,
        has_diff: true,
        erase_before_paint: true,
        terminal_rows: Some(10),
    });
    assert_eq!(
        erase.pre_diff_patch,
        Some(TerminalPatch::Stdout("\x1b[2J\x1b[H".to_string()))
    );
    assert_eq!(
        erase.post_diff_patch,
        Some(TerminalPatch::Stdout("\x1b[10;1H".to_string()))
    );

    assert!(
        plan_terminal_fullscreen_diff_patches(TerminalFullscreenDiffPatchOptions {
            fullscreen: false,
            has_diff: true,
            erase_before_paint: true,
            terminal_rows: Some(24),
        })
        .is_empty()
    );
    assert!(
        plan_terminal_fullscreen_diff_patches(TerminalFullscreenDiffPatchOptions {
            fullscreen: true,
            has_diff: false,
            erase_before_paint: true,
            terminal_rows: Some(24),
        })
        .is_empty()
    );

    let no_size = plan_terminal_fullscreen_diff_patches(TerminalFullscreenDiffPatchOptions {
        fullscreen: true,
        has_diff: true,
        erase_before_paint: false,
        terminal_rows: None,
    });
    assert_eq!(
        no_size.pre_diff_patch,
        Some(TerminalPatch::Stdout("\x1b[H".to_string()))
    );
    assert_eq!(no_size.post_diff_patch, None);
}

#[test]
fn test_terminal_fullscreen_canvas_diff_patches_use_absolute_rows_and_sparse_prefixes() {
    let mut prev = Canvas::new(6, 2);
    prev.subview_mut(0, 0, 0, 0, 6, 2)
        .set_text(0, 0, "abcdef", CanvasTextStyle::default());
    prev.subview_mut(0, 0, 0, 0, 6, 2)
        .set_text(0, 1, "stable", CanvasTextStyle::default());
    prev.clear_damage();

    let mut next = Canvas::new(6, 1);
    next.subview_mut(0, 0, 0, 0, 6, 1)
        .set_text(0, 0, "abXYef", CanvasTextStyle::default());
    next.clear_damage();

    let diff = terminal_fullscreen_canvas_diff_patches(
        Some(&prev),
        &next,
        TerminalFullscreenCanvasDiffOptions {
            top_row: 1,
            force_full_repaint: false,
        },
    );
    let ansi = terminal_patches_to_ansi(&diff, true);

    assert!(
        ansi.starts_with("\x1b[2;3HXYef"),
        "changed row should be addressed from first changed column: {ansi:?}"
    );
    assert!(
        ansi.contains("\x1b[3;1H\x1b[2K"),
        "removed rows should be cleared in-place: {ansi:?}"
    );
    assert!(
        !ansi.contains("stable"),
        "unchanged removed content must not be rewritten: {ansi:?}"
    );

    let full = terminal_fullscreen_canvas_diff_patches(
        None,
        &next,
        TerminalFullscreenCanvasDiffOptions {
            top_row: 2,
            force_full_repaint: false,
        },
    );
    let full_ansi = terminal_patches_to_ansi(&full, true);
    assert!(
        full_ansi.starts_with("\x1b[3;1HabXYef"),
        "first fullscreen write should address canvas origin absolutely: {full_ansi:?}"
    );
}

#[test]
fn test_terminal_fullscreen_packed_canvas_diff_patches_use_sparse_packed_rows() {
    let mut pools = CanvasPackedCellPools::new();
    let mut line_cache = CanvasPackedLineCache::new();
    let style = CanvasResolvedStyle::default();
    let mut prev = CanvasPackedScreen::new(6, 2);
    prev.write_line_with_cache(&mut pools, &mut line_cache, 0, 0, "abcdef", style, None);
    prev.write_line_with_cache(&mut pools, &mut line_cache, 0, 1, "stable", style, None);
    prev.clear_damage();

    let mut next = CanvasPackedScreen::new(6, 1);
    next.write_line_with_cache(&mut pools, &mut line_cache, 0, 0, "abXYef", style, None);
    next.clear_damage();

    let mut style_cache = CanvasStyleTransitionCache::new();
    let diff = terminal_fullscreen_packed_canvas_diff_patches(
        Some(&prev),
        &next,
        &pools,
        &mut style_cache,
        TerminalFullscreenCanvasDiffOptions {
            top_row: 1,
            force_full_repaint: false,
        },
    );
    let ansi = terminal_patches_to_ansi(&diff, true);

    assert!(
        ansi.starts_with("\x1b[2;3HXYef"),
        "packed changed row should be addressed from first changed column: {ansi:?}"
    );
    assert!(
        ansi.contains("\x1b[3;1H\x1b[2K"),
        "packed removed rows should be cleared in-place: {ansi:?}"
    );
    assert!(
        !ansi.contains("stable"),
        "unchanged removed packed content must not be rewritten: {ansi:?}"
    );

    let full = terminal_fullscreen_packed_canvas_diff_patches(
        None,
        &next,
        &pools,
        &mut style_cache,
        TerminalFullscreenCanvasDiffOptions {
            top_row: 2,
            force_full_repaint: false,
        },
    );
    let full_ansi = terminal_patches_to_ansi(&full, true);
    assert!(
        full_ansi.starts_with("\x1b[3;1HabXYef"),
        "first packed fullscreen write should address origin absolutely: {full_ansi:?}"
    );
}

#[test]
fn test_plan_terminal_fullscreen_packed_canvas_frame_patches_orders_scroll_diff_and_cursor() {
    let mut pools = CanvasPackedCellPools::new();
    let mut line_cache = CanvasPackedLineCache::new();
    let style = CanvasResolvedStyle::default();
    let mut prev = CanvasPackedScreen::new(6, 2);
    prev.write_line_with_cache(&mut pools, &mut line_cache, 0, 0, "old", style, None);
    prev.clear_damage();

    let mut next = CanvasPackedScreen::new(6, 2);
    next.write_line_with_cache(&mut pools, &mut line_cache, 0, 0, "new", style, None);
    next.clear_damage();

    let mut style_cache = CanvasStyleTransitionCache::new();
    let plan = plan_terminal_fullscreen_packed_canvas_frame_patches(
        Some(&prev),
        &next,
        &pools,
        &mut style_cache,
        TerminalFullscreenCanvasFramePatchOptions {
            canvas_diff: TerminalFullscreenCanvasDiffOptions {
                top_row: 0,
                force_full_repaint: false,
            },
            scroll_patch_ansi: Some("\x1b[1;2r\x1b[1S\x1b[r\x1b[H".to_string()),
            erase_before_paint: false,
            terminal_rows: Some(24),
            optimize: true,
        },
    );
    let ansi = terminal_patches_to_ansi(&plan.patches, true);

    assert!(plan.had_scroll_patch);
    assert_eq!(plan.content_patch_count, 3);
    assert!(
        ansi.starts_with("\x1b[H\x1b[1;2r\x1b[1S\x1b[r\x1b[H\x1b[1;1Hnew"),
        "cursor anchor must precede DECSTBM and packed row repairs: {ansi:?}"
    );
    assert!(
        ansi.ends_with("\x1b[24;1H"),
        "packed fullscreen patch plan should park the cursor after content diff: {ansi:?}"
    );
}

#[test]
fn test_plan_terminal_fullscreen_canvas_frame_patches_orders_scroll_diff_and_cursor_like_cc() {
    let mut prev = Canvas::new(6, 2);
    prev.subview_mut(0, 0, 0, 0, 6, 2)
        .set_text(0, 0, "old", CanvasTextStyle::default());
    prev.clear_damage();

    let mut next = Canvas::new(6, 2);
    next.subview_mut(0, 0, 0, 0, 6, 2)
        .set_text(0, 0, "new", CanvasTextStyle::default());
    next.clear_damage();

    let plan = plan_terminal_fullscreen_canvas_frame_patches(
        Some(&prev),
        &next,
        TerminalFullscreenCanvasFramePatchOptions {
            canvas_diff: TerminalFullscreenCanvasDiffOptions {
                top_row: 0,
                force_full_repaint: false,
            },
            scroll_patch_ansi: Some("\x1b[1;2r\x1b[1S\x1b[r\x1b[H".to_string()),
            erase_before_paint: false,
            terminal_rows: Some(24),
            optimize: true,
        },
    );
    let ansi = terminal_patches_to_ansi(&plan.patches, true);

    assert!(plan.had_scroll_patch);
    assert_eq!(plan.content_patch_count, 3);
    assert!(
        ansi.starts_with("\x1b[H\x1b[1;2r\x1b[1S\x1b[r\x1b[H\x1b[1;1Hnew"),
        "cursor anchor must precede DECSTBM and row repairs: {ansi:?}"
    );
    assert!(
        ansi.ends_with("\x1b[24;1H"),
        "fullscreen patch plan should park the cursor after content diff: {ansi:?}"
    );

    let empty = plan_terminal_fullscreen_canvas_frame_patches(
        Some(&next),
        &next,
        TerminalFullscreenCanvasFramePatchOptions::default(),
    );
    assert!(empty.is_empty());
    assert_eq!(empty.content_patch_count, 0);
}

#[test]
fn test_terminal_fullscreen_canvas_frame_state_tracks_previous_and_scroll_hint() {
    let mut state = TerminalFullscreenCanvasFrameState::new();
    let mut first = Canvas::new(6, 3);
    first
        .subview_mut(0, 0, 0, 0, 6, 3)
        .set_text(0, 0, "aaa", CanvasTextStyle::default());
    first
        .subview_mut(0, 0, 0, 0, 6, 3)
        .set_text(0, 1, "bbb", CanvasTextStyle::default());
    first
        .subview_mut(0, 0, 0, 0, 6, 3)
        .set_text(0, 2, "ccc", CanvasTextStyle::default());
    first.clear_damage();

    let initial = state.plan_frame(
        &first,
        TerminalFullscreenCanvasFramePatchOptions {
            terminal_rows: Some(24),
            ..TerminalFullscreenCanvasFramePatchOptions::default()
        },
    );
    assert!(!initial.is_empty());
    assert_eq!(state.previous().unwrap().get_text(0, 0, 6, 1), "aaa");

    let identical = state.plan_frame(
        &first,
        TerminalFullscreenCanvasFramePatchOptions {
            terminal_rows: Some(24),
            ..TerminalFullscreenCanvasFramePatchOptions::default()
        },
    );
    assert!(
        identical.is_empty(),
        "stateful identical frame should be a no-op"
    );

    let mut next = Canvas::new(6, 3);
    next.subview_mut(0, 0, 0, 0, 6, 3)
        .set_text(0, 0, "bbb", CanvasTextStyle::default());
    next.subview_mut(0, 0, 0, 0, 6, 3)
        .set_text(0, 1, "ccc", CanvasTextStyle::default());
    next.subview_mut(0, 0, 0, 0, 6, 3)
        .set_text(0, 2, "ddd", CanvasTextStyle::default());
    next.clear_damage();

    let scroll = state
        .plan_scroll_frame(
            &next,
            Some(crate::canvas::ScrollHint {
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
        .unwrap();
    let ansi = terminal_patches_to_ansi(&scroll.frame.patches, true);
    assert!(scroll.had_scroll_patch());
    assert!(
        ansi.contains("ddd"),
        "incoming edge row should be repainted: {ansi:?}"
    );
    assert_eq!(state.previous().unwrap().get_text(0, 2, 6, 1), "ddd");

    state.reset();
    let first_again = state
        .plan_scroll_frame(
            &next,
            Some(crate::canvas::ScrollHint {
                top: 0,
                bottom: 2,
                delta: 1,
            }),
            TerminalScrollHintPatchOptions::fullscreen_synchronized(),
            TerminalFullscreenCanvasFramePatchOptions::default(),
        )
        .unwrap();
    assert_eq!(first_again.scroll_hint_plan, None);
    assert!(!first_again.had_scroll_patch());
}

#[test]
fn test_terminal_fullscreen_packed_canvas_frame_state_tracks_previous_and_scroll_hint() {
    let mut pools = CanvasPackedCellPools::new();
    let mut line_cache = CanvasPackedLineCache::new();
    let style = CanvasResolvedStyle::default();
    let mut state = TerminalFullscreenPackedCanvasFrameState::new();
    let mut first = CanvasPackedScreen::new(6, 3);
    first.write_line_with_cache(&mut pools, &mut line_cache, 0, 0, "aaa", style, None);
    first.write_line_with_cache(&mut pools, &mut line_cache, 0, 1, "bbb", style, None);
    first.write_line_with_cache(&mut pools, &mut line_cache, 0, 2, "ccc", style, None);
    first.clear_damage();

    let initial = state.plan_frame(
        &first,
        &pools,
        TerminalFullscreenCanvasFramePatchOptions {
            terminal_rows: Some(24),
            ..TerminalFullscreenCanvasFramePatchOptions::default()
        },
    );
    assert!(!initial.is_empty());
    assert_eq!(
        state.previous().unwrap().char_in_cell(&pools, 0, 0),
        Some("a")
    );

    let identical = state.plan_frame(
        &first,
        &pools,
        TerminalFullscreenCanvasFramePatchOptions {
            terminal_rows: Some(24),
            ..TerminalFullscreenCanvasFramePatchOptions::default()
        },
    );
    assert!(identical.is_empty());

    let mut next = CanvasPackedScreen::new(6, 3);
    next.write_line_with_cache(&mut pools, &mut line_cache, 0, 0, "bbb", style, None);
    next.write_line_with_cache(&mut pools, &mut line_cache, 0, 1, "ccc", style, None);
    next.write_line_with_cache(&mut pools, &mut line_cache, 0, 2, "ddd", style, None);
    next.clear_damage();

    let scroll = state
        .plan_scroll_frame(
            &next,
            &pools,
            Some(crate::canvas::ScrollHint {
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
        .unwrap();
    let ansi = terminal_patches_to_ansi(&scroll.frame.patches, true);
    assert!(scroll.had_scroll_patch());
    assert!(
        ansi.contains("ddd"),
        "packed incoming edge should repaint: {ansi:?}"
    );
    assert_eq!(
        state.previous().unwrap().char_in_cell(&pools, 0, 2),
        Some("d")
    );
}

#[test]
fn test_plan_terminal_fullscreen_canvas_scroll_frame_patches_shifts_previous_like_cc_decstbm() {
    let mut prev = Canvas::new(6, 3);
    prev.subview_mut(0, 0, 0, 0, 6, 3)
        .set_text(0, 0, "aaa", CanvasTextStyle::default());
    prev.subview_mut(0, 0, 0, 0, 6, 3)
        .set_text(0, 1, "bbb", CanvasTextStyle::default());
    prev.subview_mut(0, 0, 0, 0, 6, 3)
        .set_text(0, 2, "ccc", CanvasTextStyle::default());
    prev.clear_damage();

    let mut next = Canvas::new(6, 3);
    next.subview_mut(0, 0, 0, 0, 6, 3)
        .set_text(0, 0, "bbb", CanvasTextStyle::default());
    next.subview_mut(0, 0, 0, 0, 6, 3)
        .set_text(0, 1, "ccc", CanvasTextStyle::default());
    next.subview_mut(0, 0, 0, 0, 6, 3)
        .set_text(0, 2, "ddd", CanvasTextStyle::default());
    next.clear_damage();

    let hint = crate::canvas::ScrollHint {
        top: 0,
        bottom: 2,
        delta: 1,
    };
    let plan = plan_terminal_fullscreen_canvas_scroll_frame_patches(
        &prev,
        &next,
        Some(hint),
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
    .unwrap();
    let ansi = terminal_patches_to_ansi(&plan.frame.patches, true);

    assert_eq!(
        plan.scroll_hint_plan,
        Some(TerminalScrollHintPatchPlan::Emit(
            "\x1b[1;3r\x1b[1S\x1b[r\x1b[H".to_string()
        ))
    );
    assert!(plan.had_scroll_patch());
    assert_eq!(plan.frame.content_patch_count, 3);
    assert!(
        ansi.starts_with("\x1b[H\x1b[1;3r\x1b[1S\x1b[r\x1b[H\x1b[3;1Hddd"),
        "scroll frame should shift previous baseline and repaint only incoming edge rows: {ansi:?}"
    );
    assert!(
        !ansi.contains("\x1b[1;1Hbbb") && !ansi.contains("\x1b[2;1Hccc"),
        "shifted baseline should avoid rewriting stable scrolled rows: {ansi:?}"
    );

    let skipped = plan_terminal_fullscreen_canvas_scroll_frame_patches(
        &prev,
        &next,
        Some(hint),
        TerminalScrollHintPatchOptions {
            fullscreen: false,
            synchronized_output: true,
        },
        TerminalFullscreenCanvasFramePatchOptions {
            scroll_patch_ansi: Some("unsafe-scroll-patch".to_string()),
            ..TerminalFullscreenCanvasFramePatchOptions::default()
        },
    )
    .unwrap();
    assert_eq!(
        skipped.scroll_hint_plan,
        Some(TerminalScrollHintPatchPlan::Skip(
            TerminalScrollHintPatchSkipReason::NotFullscreen
        ))
    );
    assert!(!skipped.had_scroll_patch());
    assert!(
        !terminal_patches_to_ansi(&skipped.frame.patches, true).contains("unsafe-scroll-patch"),
        "safety-gated helper must drop caller-provided scroll prefixes when a supplied hint is skipped"
    );
}

#[test]
fn test_plan_terminal_fullscreen_packed_canvas_scroll_frame_patches_shifts_previous_like_cc_decstbm(
) {
    let mut pools = CanvasPackedCellPools::new();
    let mut line_cache = CanvasPackedLineCache::new();
    let style = CanvasResolvedStyle::default();
    let mut prev = CanvasPackedScreen::new(6, 3);
    prev.write_line_with_cache(&mut pools, &mut line_cache, 0, 0, "aaa", style, None);
    prev.write_line_with_cache(&mut pools, &mut line_cache, 0, 1, "bbb", style, None);
    prev.write_line_with_cache(&mut pools, &mut line_cache, 0, 2, "ccc", style, None);
    prev.clear_damage();

    let mut next = CanvasPackedScreen::new(6, 3);
    next.write_line_with_cache(&mut pools, &mut line_cache, 0, 0, "bbb", style, None);
    next.write_line_with_cache(&mut pools, &mut line_cache, 0, 1, "ccc", style, None);
    next.write_line_with_cache(&mut pools, &mut line_cache, 0, 2, "ddd", style, None);
    next.clear_damage();

    let hint = crate::canvas::ScrollHint {
        top: 0,
        bottom: 2,
        delta: 1,
    };
    let mut style_cache = CanvasStyleTransitionCache::new();
    let plan = plan_terminal_fullscreen_packed_canvas_scroll_frame_patches(
        &prev,
        &next,
        &pools,
        &mut style_cache,
        Some(hint),
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
    .unwrap();
    let ansi = terminal_patches_to_ansi(&plan.frame.patches, true);

    assert!(matches!(
        plan.scroll_hint_plan,
        Some(TerminalScrollHintPatchPlan::Emit(ref patch))
            if patch == "\x1b[1;3r\x1b[1S\x1b[r\x1b[H"
    ));
    assert!(plan.had_scroll_patch());
    assert_eq!(plan.frame.content_patch_count, 3);
    assert!(
        ansi.starts_with("\x1b[H\x1b[1;3r\x1b[1S\x1b[r\x1b[H\x1b[3;1Hddd"),
        "packed scroll frame should shift previous baseline before row diff: {ansi:?}"
    );
    assert!(
        !ansi.contains("\x1b[1;1Hbbb") && !ansi.contains("\x1b[2;1Hccc"),
        "packed shifted baseline should avoid rewriting stable scrolled rows: {ansi:?}"
    );
}
