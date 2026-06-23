use super::super::*;
use crate::prelude::*;

#[test]
fn test_should_clear_terminal_screen_matches_cc_ink_frame_helper() {
    let base = TerminalFrameBounds {
        screen_height: 3,
        viewport_width: 80,
        viewport_height: 10,
    };

    assert_eq!(should_clear_terminal_screen(base, base), None);
    assert_eq!(
        should_clear_terminal_screen(
            base,
            TerminalFrameBounds {
                viewport_width: 100,
                ..base
            }
        ),
        Some(TerminalClearReason::Resize)
    );
    assert_eq!(
        should_clear_terminal_screen(
            base,
            TerminalFrameBounds {
                viewport_height: 8,
                ..base
            }
        ),
        Some(TerminalClearReason::Resize)
    );
    assert_eq!(
        should_clear_terminal_screen(
            base,
            TerminalFrameBounds {
                screen_height: 10,
                ..base
            }
        ),
        Some(TerminalClearReason::Offscreen)
    );
    assert_eq!(
        should_clear_terminal_screen(
            TerminalFrameBounds {
                screen_height: 10,
                ..base
            },
            base
        ),
        Some(TerminalClearReason::Offscreen)
    );
}

#[test]
fn test_analyze_terminal_inline_diff_matches_cc_log_update_geometry_guards() {
    let base = TerminalInlineDiffBounds {
        prev_screen_height: 8,
        next_screen_height: 8,
        prev_viewport_width: 80,
        prev_viewport_height: 10,
        next_viewport_width: 80,
        next_viewport_height: 10,
    };

    assert_eq!(
        analyze_terminal_inline_diff(base),
        TerminalInlineDiffAnalysis {
            clear_reason: None,
            unreachable_rows: 0,
            growing: false,
            shrinking: false,
        }
    );
    assert_eq!(
        analyze_terminal_inline_diff(TerminalInlineDiffBounds {
            next_viewport_height: 8,
            ..base
        })
        .clear_reason,
        Some(TerminalClearReason::Resize)
    );
    assert_eq!(
        analyze_terminal_inline_diff(TerminalInlineDiffBounds {
            next_viewport_width: 100,
            ..base
        })
        .clear_reason,
        Some(TerminalClearReason::Resize)
    );
    assert_eq!(
        analyze_terminal_inline_diff(TerminalInlineDiffBounds {
            next_viewport_height: 12,
            ..base
        })
        .clear_reason,
        None,
        "main-screen log-update does not clear solely because the viewport grew taller"
    );

    let shrink_to_fits = analyze_terminal_inline_diff(TerminalInlineDiffBounds {
        prev_screen_height: 12,
        next_screen_height: 10,
        ..base
    });
    assert_eq!(
        shrink_to_fits.clear_reason,
        Some(TerminalClearReason::Offscreen)
    );
    assert_eq!(shrink_to_fits.unreachable_rows, 3);
    assert!(shrink_to_fits.shrinking);

    let steady_overflow = analyze_terminal_inline_diff(TerminalInlineDiffBounds {
        prev_screen_height: 12,
        next_screen_height: 12,
        ..base
    });
    assert_eq!(steady_overflow.clear_reason, None);
    assert_eq!(
        steady_overflow.unreachable_rows, 3,
        "custom renderers should clear if sparse diffs touch this top prefix"
    );

    let growing_after_full_viewport = analyze_terminal_inline_diff(TerminalInlineDiffBounds {
        prev_screen_height: 10,
        next_screen_height: 11,
        ..base
    });
    assert_eq!(growing_after_full_viewport.clear_reason, None);
    assert_eq!(
        growing_after_full_viewport.unreachable_rows, 1,
        "cursorRestoreScroll contributes one unreachable top row"
    );
    assert!(growing_after_full_viewport.growing);

    let shrink_clear_too_large = analyze_terminal_inline_diff(TerminalInlineDiffBounds {
        prev_screen_height: 100,
        next_screen_height: 70,
        prev_viewport_height: 20,
        next_viewport_height: 20,
        ..base
    });
    assert_eq!(
        shrink_clear_too_large.clear_reason,
        Some(TerminalClearReason::Offscreen)
    );
}

#[test]
fn test_plan_terminal_inline_canvas_diff_scans_unreachable_rows_like_cc_log_update() {
    let bounds = TerminalInlineDiffBounds {
        prev_screen_height: 12,
        next_screen_height: 12,
        prev_viewport_width: 80,
        prev_viewport_height: 10,
        next_viewport_width: 80,
        next_viewport_height: 10,
    };

    let mut previous = Canvas::new(12, 12);
    previous
        .subview_mut(0, 0, 0, 0, 12, 12)
        .set_text(0, 0, "old top", CanvasTextStyle::default());
    previous.clear_damage();

    let mut next = Canvas::new(12, 12);
    next.subview_mut(0, 0, 0, 0, 12, 12)
        .set_text(0, 0, "new top", CanvasTextStyle::default());
    next.clear_damage();

    let decision = plan_terminal_inline_canvas_diff(&previous, &next, bounds);
    assert_eq!(decision.analysis.unreachable_rows, 3);
    assert_eq!(decision.clear_reason, Some(TerminalClearReason::Offscreen));
    assert_eq!(
        decision.debug,
        Some(TerminalInlineDiffResetDebug {
            trigger_y: 0,
            prev_line: "old top".to_string(),
            next_line: "new top".to_string(),
        })
    );

    let mut reachable_next = previous.clone();
    reachable_next.subview_mut(0, 0, 0, 0, 12, 12).set_text(
        0,
        5,
        "safe row",
        CanvasTextStyle::default(),
    );
    reachable_next.clear_damage();
    let reachable = plan_terminal_inline_canvas_diff(&previous, &reachable_next, bounds);
    assert_eq!(reachable.clear_reason, None);
    assert_eq!(reachable.debug, None);

    let resize = plan_terminal_inline_canvas_diff(
        &previous,
        &next,
        TerminalInlineDiffBounds {
            next_viewport_width: 100,
            ..bounds
        },
    );
    assert_eq!(resize.clear_reason, Some(TerminalClearReason::Resize));
    assert_eq!(resize.debug, None);
}

#[test]
fn test_plan_terminal_inline_packed_canvas_diff_scans_unreachable_rows_like_cc_log_update() {
    let bounds = TerminalInlineDiffBounds {
        prev_screen_height: 12,
        next_screen_height: 12,
        prev_viewport_width: 80,
        prev_viewport_height: 10,
        next_viewport_width: 80,
        next_viewport_height: 10,
    };
    let mut pools = CanvasPackedCellPools::new();
    let mut line_cache = CanvasPackedLineCache::new();
    let style = CanvasResolvedStyle::default();

    let mut previous = CanvasPackedScreen::new(12, 12);
    previous.write_line_with_cache(&mut pools, &mut line_cache, 0, 0, "old top", style, None);
    previous.clear_damage();

    let mut next = CanvasPackedScreen::new(12, 12);
    next.write_line_with_cache(&mut pools, &mut line_cache, 0, 0, "new top", style, None);

    let decision = plan_terminal_inline_packed_canvas_diff(&previous, &next, &pools, bounds);
    assert_eq!(decision.analysis.unreachable_rows, 3);
    assert_eq!(decision.clear_reason, Some(TerminalClearReason::Offscreen));
    assert_eq!(
        decision.debug,
        Some(TerminalInlineDiffResetDebug {
            trigger_y: 0,
            prev_line: "old top".to_string(),
            next_line: "new top".to_string(),
        })
    );

    let mut reachable_next = previous.clone();
    reachable_next.write_line_with_cache(
        &mut pools,
        &mut line_cache,
        0,
        5,
        "safe row",
        style,
        None,
    );
    let reachable =
        plan_terminal_inline_packed_canvas_diff(&previous, &reachable_next, &pools, bounds);
    assert_eq!(reachable.clear_reason, None);
    assert_eq!(reachable.debug, None);
}

#[test]
fn test_plan_terminal_inline_packed_canvas_frame_patches_emits_full_reset_fallback() {
    let bounds = TerminalInlineDiffBounds {
        prev_screen_height: 12,
        next_screen_height: 12,
        prev_viewport_width: 80,
        prev_viewport_height: 10,
        next_viewport_width: 80,
        next_viewport_height: 10,
    };
    let mut pools = CanvasPackedCellPools::new();
    let mut line_cache = CanvasPackedLineCache::new();
    let mut style_cache = CanvasStyleTransitionCache::new();
    let style = CanvasResolvedStyle::default();

    let mut previous = CanvasPackedScreen::new(12, 12);
    previous.write_line_with_cache(&mut pools, &mut line_cache, 0, 0, "old top", style, None);
    previous.clear_damage();

    let mut next = CanvasPackedScreen::new(12, 12);
    next.write_line_with_cache(&mut pools, &mut line_cache, 0, 0, "new top", style, None);

    let plan = plan_terminal_inline_packed_canvas_frame_patches(
        &previous,
        &next,
        &pools,
        &mut style_cache,
        bounds,
    );
    assert!(plan.requires_clear_repaint());
    assert!(!plan.sparse_diff_safe());
    assert_eq!(
        plan.decision.clear_reason,
        Some(TerminalClearReason::Offscreen)
    );
    assert_eq!(plan.patches.first(), Some(&TerminalPatch::ClearTerminal));
    assert!(matches!(
        plan.patches.get(1),
        Some(TerminalPatch::Stdout(output)) if output.contains("new") && output.contains("top")
    ));

    let mut reachable_next = previous.clone();
    reachable_next.write_line_with_cache(
        &mut pools,
        &mut line_cache,
        0,
        5,
        "safe row",
        style,
        None,
    );
    let safe = plan_terminal_inline_packed_canvas_frame_patches(
        &previous,
        &reachable_next,
        &pools,
        &mut style_cache,
        bounds,
    );
    assert!(safe.sparse_diff_safe());
    assert!(!safe.requires_clear_repaint());
    assert!(safe.patches.is_empty());
}

#[test]
fn test_plan_terminal_inline_canvas_frame_patches_emits_full_reset_fallback() {
    let bounds = TerminalInlineDiffBounds {
        prev_screen_height: 12,
        next_screen_height: 12,
        prev_viewport_width: 80,
        prev_viewport_height: 10,
        next_viewport_width: 80,
        next_viewport_height: 10,
    };

    let mut previous = Canvas::new(12, 12);
    previous
        .subview_mut(0, 0, 0, 0, 12, 12)
        .set_text(0, 0, "old top", CanvasTextStyle::default());
    previous.clear_damage();

    let mut next = Canvas::new(12, 12);
    next.subview_mut(0, 0, 0, 0, 12, 12)
        .set_text(0, 0, "new top", CanvasTextStyle::default());
    next.clear_damage();

    let plan = plan_terminal_inline_canvas_frame_patches(&previous, &next, bounds);
    assert!(plan.requires_clear_repaint());
    assert!(!plan.sparse_diff_safe());
    assert_eq!(
        plan.decision.clear_reason,
        Some(TerminalClearReason::Offscreen)
    );
    assert_eq!(plan.patches.first(), Some(&TerminalPatch::ClearTerminal));
    assert!(matches!(
        plan.patches.get(1),
        Some(TerminalPatch::Stdout(output)) if output.contains("new top")
    ));

    let mut reachable_next = previous.clone();
    reachable_next.subview_mut(0, 0, 0, 0, 12, 12).set_text(
        0,
        5,
        "safe row",
        CanvasTextStyle::default(),
    );
    reachable_next.clear_damage();
    let safe = plan_terminal_inline_canvas_frame_patches(&previous, &reachable_next, bounds);
    assert!(safe.sparse_diff_safe());
    assert!(!safe.requires_clear_repaint());
    assert!(safe.patches.is_empty());
}

#[test]
fn test_optimize_terminal_patches_matches_cc_ink_optimizer_rules() {
    let optimized = optimize_terminal_patches(vec![
        TerminalPatch::Stdout(String::new()),
        TerminalPatch::CursorMove { x: 0, y: 0 },
        TerminalPatch::Clear { count: 0 },
        TerminalPatch::CursorMove { x: 2, y: -1 },
        TerminalPatch::CursorMove { x: -1, y: 3 },
        TerminalPatch::CursorTo { col: 4 },
        TerminalPatch::CursorTo { col: 8 },
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
        TerminalPatch::Stdout("text".to_string()),
        TerminalPatch::Clear { count: 2 },
    ]);

    assert_eq!(
        optimized,
        vec![
            TerminalPatch::CursorMove { x: 1, y: 2 },
            TerminalPatch::CursorTo { col: 8 },
            TerminalPatch::StyleStr("\x1b[31m\x1b[1m".to_string()),
            TerminalPatch::Hyperlink {
                uri: "https://example.test".to_string(),
            },
            TerminalPatch::Stdout("text".to_string()),
            TerminalPatch::Clear { count: 2 },
        ]
    );
}

#[test]
fn test_optimize_terminal_patches_keeps_cursor_move_sum_zero_like_cc_ink() {
    let optimized = optimize_terminal_patches(vec![
        TerminalPatch::CursorMove { x: 1, y: 0 },
        TerminalPatch::CursorMove { x: -1, y: 0 },
    ]);

    assert_eq!(optimized, vec![TerminalPatch::CursorMove { x: 0, y: 0 }]);
}

#[test]
fn test_terminal_scroll_hint_to_ansi_matches_cc_ink_decstbm_patch_guard() {
    let bounds = TerminalScrollHintBounds {
        previous_screen_height: 10,
        next_screen_height: 10,
    };
    assert_eq!(
        terminal_scroll_hint_to_ansi(
            crate::canvas::ScrollHint {
                top: 2,
                bottom: 6,
                delta: 2,
            },
            bounds,
        ),
        Ok("\x1b[3;7r\x1b[2S\x1b[r\x1b[H".to_string())
    );
    assert_eq!(
        terminal_scroll_hint_to_ansi(
            crate::canvas::ScrollHint {
                top: 2,
                bottom: 6,
                delta: -3,
            },
            bounds,
        ),
        Ok("\x1b[3;7r\x1b[3T\x1b[r\x1b[H".to_string())
    );
    assert_eq!(
        terminal_scroll_hint_to_ansi(
            crate::canvas::ScrollHint {
                top: 7,
                bottom: 6,
                delta: 1,
            },
            bounds,
        ),
        Err(TerminalScrollHintRejection::InvalidRegion)
    );
    assert_eq!(
        terminal_scroll_hint_to_ansi(
            crate::canvas::ScrollHint {
                top: 2,
                bottom: 10,
                delta: 1,
            },
            bounds,
        ),
        Err(TerminalScrollHintRejection::OutOfBounds)
    );
    assert_eq!(
        terminal_scroll_hint_to_ansi(
            crate::canvas::ScrollHint {
                top: 2,
                bottom: 6,
                delta: 0,
            },
            bounds,
        ),
        Err(TerminalScrollHintRejection::ZeroDelta)
    );
    assert_eq!(
        terminal_scroll_hint_to_ansi(
            crate::canvas::ScrollHint {
                top: 2,
                bottom: 6,
                delta: 5,
            },
            bounds,
        ),
        Err(TerminalScrollHintRejection::DeltaTooLarge)
    );
}

#[test]
fn test_plan_terminal_scroll_hint_patch_matches_cc_ink_decstbm_safe_gate() {
    let hint = crate::canvas::ScrollHint {
        top: 2,
        bottom: 6,
        delta: 2,
    };
    let bounds = TerminalScrollHintBounds {
        previous_screen_height: 10,
        next_screen_height: 10,
    };

    assert_eq!(
        plan_terminal_scroll_hint_patch(
            hint,
            bounds,
            TerminalScrollHintPatchOptions {
                fullscreen: false,
                synchronized_output: true,
            },
        ),
        Ok(TerminalScrollHintPatchPlan::Skip(
            TerminalScrollHintPatchSkipReason::NotFullscreen,
        )),
    );
    assert_eq!(
        plan_terminal_scroll_hint_patch(
            hint,
            bounds,
            TerminalScrollHintPatchOptions {
                fullscreen: true,
                synchronized_output: false,
            },
        ),
        Ok(TerminalScrollHintPatchPlan::Skip(
            TerminalScrollHintPatchSkipReason::NotSynchronized,
        )),
    );
    assert_eq!(
        plan_terminal_scroll_hint_patch(
            hint,
            TerminalScrollHintBounds {
                previous_screen_height: 0,
                next_screen_height: 0,
            },
            TerminalScrollHintPatchOptions::default(),
        ),
        Ok(TerminalScrollHintPatchPlan::Skip(
            TerminalScrollHintPatchSkipReason::NotFullscreen,
        )),
        "unsafe modes skip before validating geometry, like CC Ink's altScreen/decstbmSafe gate"
    );
    assert_eq!(
        plan_terminal_scroll_hint_patch(
            hint,
            bounds,
            TerminalScrollHintPatchOptions::fullscreen_synchronized(),
        ),
        Ok(TerminalScrollHintPatchPlan::Emit(
            "\x1b[3;7r\x1b[2S\x1b[r\x1b[H".to_string(),
        )),
    );
    assert!(TerminalScrollHintPatchOptions::fullscreen_synchronized().is_decstbm_safe());
}

#[test]
fn test_terminal_patches_to_ansi_matches_cc_ink_write_diff_to_terminal() {
    let diff = vec![
        TerminalPatch::CursorHide,
        TerminalPatch::CursorMove { x: -2, y: 3 },
        TerminalPatch::CursorTo { col: 5 },
        TerminalPatch::CarriageReturn,
        TerminalPatch::Clear { count: 2 },
        TerminalPatch::Hyperlink {
            uri: "https://example.com".to_string(),
        },
        TerminalPatch::Stdout("hi".to_string()),
        TerminalPatch::Hyperlink { uri: String::new() },
        TerminalPatch::StyleStr("\x1b[0m".to_string()),
        TerminalPatch::CursorShow,
    ];

    let body = concat!(
        "\x1b[?25l",
        "\x1b[2D\x1b[3B",
        "\x1b[5G",
        "\r",
        "\x1b[2K\x1b[1A\x1b[2K\x1b[G",
        "\x1b]8;id=ags5vy;https://example.com\x1b\\",
        "hi",
        "\x1b]8;;\x1b\\",
        "\x1b[0m",
        "\x1b[?25h",
    );
    assert_eq!(
        terminal_patches_to_ansi(&diff, false),
        format!("\x1b[?2026h{body}\x1b[?2026l")
    );
    assert_eq!(terminal_patches_to_ansi(&diff, true), body);
    assert_eq!(terminal_patches_to_ansi(&[], false), "");
}

#[test]
fn test_write_terminal_patches_writes_serialized_diff() {
    let diff = [TerminalPatch::Stdout("patch".to_string())];
    let mut output = Vec::new();

    write_terminal_patches(&mut output, &diff, true).unwrap();

    assert_eq!(String::from_utf8(output).unwrap(), "patch");
}
