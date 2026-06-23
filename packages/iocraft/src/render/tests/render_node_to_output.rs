use super::super::*;
use crate::prelude::*;

#[test]
fn test_renderer_node_generation_state_prevents_key_reuse_blits() {
    let mut generations = RendererNodeGenerationState::<&'static str>::new();
    let mut cache = RendererNodeCache::<RendererStableNodeId<&'static str>>::new();
    let layout = CachedLayoutBounds {
        x: 1,
        y: 2,
        width: 10,
        height: 1,
        top: Some(2),
    };

    let row0 = generations.current_id("row");
    assert_eq!(row0.generation, 0);
    cache.set_layout(row0.clone(), layout);
    assert!(cache.can_blit(&row0, layout));

    let removed = generations.remove(&"row").unwrap();
    assert_eq!(removed, row0);
    cache.remove_layout(&removed);
    let row1 = generations.current_id("row");
    assert_eq!(row1.generation, 1);
    assert_ne!(row1, removed);
    assert!(!cache.can_blit(&row1, layout));

    let remounted = generations.remount("row");
    assert_eq!(remounted.generation, 2);
    assert_eq!(generations.id(&"row"), Some(remounted.clone()));

    let other = generations.current_id("other");
    cache.set_layout(other.clone(), CachedLayoutBounds { y: 3, ..layout });
    let removed = generations.bump_unretained_keys(["row"]);
    assert_eq!(removed, vec![other.clone()]);
    cache.remove_layout(&other);
    assert_eq!(generations.current_id("other").generation, 1);
    assert_eq!(
        generations.len(),
        2,
        "generation tombstones prevent stale key reuse"
    );

    generations.clear();
    assert!(generations.is_empty());
}

#[test]
fn test_renderer_retained_tree_reconciler_maps_logical_keys_to_stable_ids() {
    let mut reconciler = RendererRetainedTreeReconciler::<&'static str>::new();
    let root_id = reconciler.register_root("root");
    let (row_id, parent_id) = reconciler.attach("row", "root");
    let (leaf_id, row_parent_id) = reconciler.attach("leaf", "row");
    assert_eq!(parent_id, root_id);
    assert_eq!(row_parent_id, row_id);
    reconciler.clear_dirty();

    assert!(reconciler.mark_dirty(&"leaf", true));
    assert!(reconciler.is_dirty(&"leaf"));
    assert!(reconciler.is_dirty(&"row"));
    assert!(reconciler.is_dirty(&"root"));

    let root_layout = CachedLayoutBounds {
        x: 0,
        y: 0,
        width: 20,
        height: 4,
        top: Some(0),
    };
    let row_layout = CachedLayoutBounds {
        x: 0,
        y: 1,
        width: 20,
        height: 2,
        top: Some(1),
    };
    let leaf_layout = CachedLayoutBounds {
        x: 0,
        y: 2,
        width: 20,
        height: 1,
        top: Some(2),
    };

    reconciler.begin_frame();
    for (key, layout) in [
        ("root", root_layout),
        ("row", row_layout),
        ("leaf", leaf_layout),
    ] {
        let plan = reconciler.plan_node(RetainedLogicalTreeNodeInput {
            key,
            current_layout: layout,
            skip_self_blit: false,
            pending_scroll_delta: false,
            previous_screen_available: true,
            hidden: false,
            absolute: false,
        });
        assert_eq!(plan.plan.action, RetainedNodeRenderAction::Render);
        reconciler.commit_node_plan(&plan);
    }
    assert_eq!(
        reconciler
            .retained_state()
            .frame_state()
            .cache()
            .layout(&leaf_id),
        Some(leaf_layout)
    );

    let removed = reconciler.remove_subtree(&"row", true);
    assert!(removed.contains(&row_id));
    assert!(removed.contains(&leaf_id));
    assert_eq!(reconciler.current_id("row").generation, 1);
    assert_eq!(reconciler.current_id("leaf").generation, 1);
    assert!(reconciler.is_dirty(&"root"));
    assert!(
        reconciler.begin_frame(),
        "absolute subtree removal poisons next frame blits"
    );

    let root_after_remove = reconciler.plan_node(RetainedLogicalTreeNodeInput {
        key: "root",
        current_layout: root_layout,
        skip_self_blit: false,
        pending_scroll_delta: false,
        previous_screen_available: true,
        hidden: false,
        absolute: false,
    });
    assert_eq!(
        root_after_remove.plan.action,
        RetainedNodeRenderAction::Render
    );
    assert_eq!(
        root_after_remove.plan.pending_clear_regions,
        vec![row_layout.into()]
    );
    assert!(root_after_remove.plan.has_removed_child);

    reconciler.clear();
    assert!(reconciler.generations().is_empty());
}

#[test]
fn test_scroll_fast_path_plan_matches_cc_ink_scrollbox_guards_and_regions() {
    let viewport = CachedClearRegion {
        x: 2,
        y: 10,
        width: 20,
        height: 5,
    };

    assert_eq!(plan_scroll_fast_path(viewport, 0, []), None);
    assert_eq!(plan_scroll_fast_path(viewport, 5, []), None);
    assert_eq!(
        is_scroll_fast_path_content_delta_safe(3, 0),
        true,
        "pure scroll is safe"
    );
    assert_eq!(
        is_scroll_fast_path_content_delta_safe(3, 3),
        true,
        "bottom append matching the scroll delta is safe"
    );
    assert_eq!(
        is_scroll_fast_path_content_delta_safe(-3, -3),
        false,
        "scroll-up plus shrink/removal must fall back to full render"
    );
    assert_eq!(is_scroll_fast_path_content_delta_safe(3, 1), false);

    let plan = plan_scroll_fast_path(
        viewport,
        2,
        [
            CachedClearRegion {
                x: 0,
                y: 12,
                width: 6,
                height: 1,
            },
            CachedClearRegion {
                x: 0,
                y: 15,
                width: 6,
                height: 1,
            },
        ],
    )
    .expect("delta smaller than viewport should plan a fast path");

    assert_eq!(plan.blit_region, viewport);
    assert_eq!(plan.delta, 2);
    assert_eq!(
        plan.edge_region,
        CachedClearRegion {
            x: 2,
            y: 13,
            width: 20,
            height: 2,
        },
        "positive delta repaints the bottom edge rows"
    );
    assert_eq!(
        plan.absolute_repair_regions,
        vec![CachedClearRegion {
            x: 2,
            y: 10,
            width: 20,
            height: 1,
        }],
        "absolute overlay pixels shifted into stable rows need full-width repair"
    );

    let up = plan_scroll_fast_path(
        viewport,
        -2,
        [CachedClearRegion {
            x: 0,
            y: 11,
            width: 6,
            height: 2,
        }],
    )
    .unwrap();
    assert_eq!(
        up.edge_region,
        CachedClearRegion {
            x: 2,
            y: 10,
            width: 20,
            height: 2,
        },
        "negative delta repaints the top edge rows"
    );
    assert_eq!(
        up.absolute_repair_regions,
        vec![CachedClearRegion {
            x: 2,
            y: 13,
            width: 20,
            height: 2,
        }]
    );
}

#[test]
fn test_apply_scroll_fast_path_to_canvas_blits_shifts_and_clears_repairs() {
    let mut previous = Canvas::new(6, 4);
    for (row, text) in ["aaaaaa", "bbbbbb", "cccccc", "dddddd"]
        .into_iter()
        .enumerate()
    {
        previous.subview_mut(0, 0, 0, 0, 6, 4).set_text(
            0,
            row as isize,
            text,
            CanvasTextStyle::default(),
        );
    }

    let plan = plan_scroll_fast_path(
        CachedClearRegion {
            x: 0,
            y: 0,
            width: 6,
            height: 4,
        },
        2,
        [CachedClearRegion {
            x: 2,
            y: 3,
            width: 2,
            height: 1,
        }],
    )
    .unwrap();
    assert_eq!(
        plan.absolute_repair_regions,
        vec![CachedClearRegion {
            x: 0,
            y: 1,
            width: 6,
            height: 1,
        }]
    );

    let mut next = Canvas::new(6, 4);
    assert!(apply_scroll_fast_path_to_canvas(
        &mut next, &previous, &plan
    ));
    assert_eq!(next.get_text(0, 0, 6, 1), "cccccc");
    for y in 1..4 {
        for x in 0..6 {
            assert!(next.cell(x, y).is_some_and(|cell| cell.is_empty()));
        }
    }
    assert_eq!(
        next.damage_region(),
        Some(DamageRegion {
            x: 0,
            y: 0,
            width: 6,
            height: 4,
        })
    );
    assert_eq!(
        next.scroll_hint(),
        Some(ScrollHint {
            top: 0,
            bottom: 3,
            delta: 2,
        })
    );
    assert_eq!(scroll_fast_path_plan_to_scroll_hint(&plan, 5), None);

    let empty_plan = ScrollFastPathPlan {
        blit_region: CachedClearRegion {
            x: 0,
            y: 10,
            width: 6,
            height: 1,
        },
        delta: 1,
        edge_region: CachedClearRegion::default(),
        absolute_repair_regions: Vec::new(),
    };
    assert!(!apply_scroll_fast_path_to_canvas(
        &mut next,
        &previous,
        &empty_plan
    ));
}

#[test]
fn test_scroll_fast_path_child_repairs_match_cc_ink_second_pass_cases() {
    let viewport = CachedClearRegion {
        x: 2,
        y: 10,
        width: 20,
        height: 5,
    };
    let edge = CachedClearRegion {
        x: 2,
        y: 13,
        width: 20,
        height: 2,
    };
    let repairs = plan_scroll_fast_path_child_repairs(
        viewport,
        8,
        2,
        2,
        edge,
        [
            ScrollFastPathChild {
                key: "clean-before",
                top: 2,
                height: 1,
                cached_y: Some(10),
                cached_height: Some(1),
                dirty: false,
            },
            ScrollFastPathChild {
                key: "dirty-stable",
                top: 3,
                height: 1,
                cached_y: Some(11),
                cached_height: Some(1),
                dirty: true,
            },
            ScrollFastPathChild {
                key: "uncached-stable",
                top: 4,
                height: 1,
                cached_y: None,
                cached_height: None,
                dirty: false,
            },
            ScrollFastPathChild {
                key: "dirty-edge",
                top: 5,
                height: 1,
                cached_y: Some(13),
                cached_height: Some(1),
                dirty: true,
            },
        ],
    );

    assert_eq!(
        repairs,
        vec![
            ScrollFastPathChildRepair {
                key: "dirty-stable",
                region: CachedClearRegion {
                    x: 2,
                    y: 11,
                    width: 20,
                    height: 1,
                },
            },
            ScrollFastPathChildRepair {
                key: "uncached-stable",
                region: CachedClearRegion {
                    x: 2,
                    y: 12,
                    width: 20,
                    height: 1,
                },
            },
        ]
    );
}

#[test]
fn test_scroll_fast_path_frame_state_tracks_previous_content_and_repairs() {
    let viewport = CachedClearRegion {
        x: 0,
        y: 5,
        width: 20,
        height: 6,
    };
    let mut state = ScrollFastPathFrameState::new();

    state.begin_frame();
    let first = state.plan_frame(ScrollFastPathFrameInput::<&'static str> {
        viewport,
        content_y: 5,
        scroll_top: 0,
        content_height: 30,
        children: Vec::new(),
    });
    assert_eq!(
        first.fast_path, None,
        "no previous frame means no shift plan"
    );
    assert!(!first.viewport_stable);
    state.record_absolute_rect(CachedClearRegion {
        x: 3,
        y: 7,
        width: 4,
        height: 1,
    });
    state.commit_frame(viewport, 5, 30);

    state.begin_frame();
    assert_eq!(state.previous_absolute_rects().len(), 1);
    let plan = state.plan_frame(ScrollFastPathFrameInput {
        viewport,
        content_y: 3,
        scroll_top: 2,
        content_height: 30,
        children: vec![
            ScrollFastPathChild {
                key: "dirty-stable",
                top: 3,
                height: 1,
                cached_y: Some(8),
                cached_height: Some(1),
                dirty: true,
            },
            ScrollFastPathChild {
                key: "clean-edge",
                top: 7,
                height: 1,
                cached_y: Some(10),
                cached_height: Some(1),
                dirty: false,
            },
        ],
    });
    assert!(plan.viewport_stable);
    assert!(plan.content_delta_safe);
    assert_eq!(plan.delta, 2);
    let fast = plan
        .fast_path
        .expect("stable small scroll should use fast path");
    assert_eq!(fast.edge_region.y, 9);
    assert_eq!(fast.absolute_repair_regions.len(), 1);
    assert_eq!(
        plan.child_repairs,
        vec![ScrollFastPathChildRepair {
            key: "dirty-stable",
            region: CachedClearRegion {
                x: 0,
                y: 6,
                width: 20,
                height: 1,
            },
        }]
    );
    state.commit_frame(viewport, 3, 30);

    state.begin_frame();
    let unsafe_growth = state.plan_frame(ScrollFastPathFrameInput::<&'static str> {
        viewport,
        content_y: 1,
        scroll_top: 4,
        content_height: 31,
        children: Vec::new(),
    });
    assert_eq!(unsafe_growth.delta, 2);
    assert_eq!(unsafe_growth.content_height_delta, 1);
    assert!(!unsafe_growth.content_delta_safe);
    assert_eq!(unsafe_growth.fast_path, None);
}

#[test]
fn test_apply_scroll_fast_path_frame_plan_to_canvas_clears_child_repairs() {
    let mut previous = Canvas::new(8, 5);
    {
        let mut view = previous.subview_mut(0, 0, 0, 0, 8, 5);
        for row in 0..5 {
            view.set_text(
                0,
                row as isize,
                &format!("row{row}"),
                CanvasTextStyle::default(),
            );
        }
    }
    previous.clear_damage();

    let fast_path = ScrollFastPathPlan {
        blit_region: CachedClearRegion {
            x: 0,
            y: 0,
            width: 8,
            height: 5,
        },
        delta: 1,
        edge_region: CachedClearRegion {
            x: 0,
            y: 4,
            width: 8,
            height: 1,
        },
        absolute_repair_regions: vec![CachedClearRegion {
            x: 0,
            y: 1,
            width: 8,
            height: 1,
        }],
    };
    let frame_plan = ScrollFastPathFramePlan {
        delta: 1,
        content_height_delta: 0,
        viewport_stable: true,
        content_delta_safe: true,
        fast_path: Some(fast_path),
        child_repairs: vec![ScrollFastPathChildRepair {
            key: "dirty-stable",
            region: CachedClearRegion {
                x: 0,
                y: 2,
                width: 8,
                height: 1,
            },
        }],
    };

    let mut next = Canvas::new(8, 5);
    assert!(apply_scroll_fast_path_frame_plan_to_canvas(
        &mut next,
        &previous,
        &frame_plan
    ));
    assert_eq!(next.get_text(0, 1, 8, 1), "");
    assert_eq!(next.get_text(0, 2, 8, 1), "");
    assert_eq!(next.get_text(0, 4, 8, 1), "");
    assert_eq!(
        next.scroll_hint(),
        Some(ScrollHint {
            top: 0,
            bottom: 4,
            delta: 1,
        })
    );

    let no_fast_path = ScrollFastPathFramePlan::<&'static str> {
        fast_path: None,
        child_repairs: Vec::new(),
        delta: 0,
        content_height_delta: 0,
        viewport_stable: true,
        content_delta_safe: true,
    };
    assert!(!apply_scroll_fast_path_frame_plan_to_canvas(
        &mut next,
        &previous,
        &no_fast_path
    ));
}

#[test]
fn test_scroll_fast_path_frame_plan_to_terminal_patch_is_fullscreen_opt_in_bridge() {
    let frame_plan = ScrollFastPathFramePlan {
        delta: 1,
        content_height_delta: 0,
        viewport_stable: true,
        content_delta_safe: true,
        fast_path: Some(ScrollFastPathPlan {
            blit_region: CachedClearRegion {
                x: 0,
                y: 1,
                width: 8,
                height: 4,
            },
            delta: 1,
            edge_region: CachedClearRegion {
                x: 0,
                y: 4,
                width: 8,
                height: 1,
            },
            absolute_repair_regions: vec![CachedClearRegion {
                x: 0,
                y: 2,
                width: 8,
                height: 1,
            }],
        }),
        child_repairs: vec![ScrollFastPathChildRepair {
            key: "dirty-stable",
            region: CachedClearRegion {
                x: 0,
                y: 3,
                width: 8,
                height: 1,
            },
        }],
    };

    let patch = scroll_fast_path_frame_plan_to_terminal_patch(
        &frame_plan,
        8,
        TerminalScrollHintBounds {
            previous_screen_height: 6,
            next_screen_height: 6,
        },
    )
    .unwrap()
    .expect("full-width small scroll should produce a terminal patch");
    assert_eq!(
        patch.scroll_hint,
        ScrollHint {
            top: 1,
            bottom: 4,
            delta: 1,
        }
    );
    assert_eq!(patch.scroll_patch_ansi, "\x1b[2;5r\x1b[1S\x1b[r\x1b[H");
    assert_eq!(patch.edge_region.y, 4);
    assert_eq!(patch.absolute_repair_regions.len(), 1);
    assert_eq!(patch.child_repairs[0].key, "dirty-stable");

    assert!(
        scroll_fast_path_frame_plan_to_terminal_patch(
            &frame_plan,
            9,
            TerminalScrollHintBounds {
                previous_screen_height: 6,
                next_screen_height: 6,
            },
        )
        .unwrap()
        .is_none(),
        "partial-width retained scrolls must not emit DECSTBM"
    );
    assert_eq!(
        scroll_fast_path_frame_plan_to_terminal_patch(
            &frame_plan,
            8,
            TerminalScrollHintBounds {
                previous_screen_height: 4,
                next_screen_height: 6,
            },
        ),
        Err(TerminalScrollHintRejection::OutOfBounds)
    );

    assert_eq!(
        plan_scroll_fast_path_frame_terminal_patch(
            &frame_plan,
            8,
            ScrollFastPathTerminalFramePatchRequest {
                bounds: TerminalScrollHintBounds {
                    previous_screen_height: 4,
                    next_screen_height: 6,
                },
                options: TerminalScrollHintPatchOptions::default(),
            },
        ),
        Ok(Some(ScrollFastPathTerminalFramePatchPlan::Skip(
            TerminalScrollHintPatchSkipReason::NotFullscreen,
        ))),
        "safety gate skips before validating bounds, matching log-update's altScreen/decstbmSafe branch"
    );
}

#[test]
fn test_apply_scroll_fast_path_frame_plan_combines_canvas_and_terminal_patch() {
    let mut previous = Canvas::new(6, 4);
    {
        let mut view = previous.subview_mut(0, 0, 0, 0, 6, 4);
        for row in 0..4 {
            view.set_text(
                0,
                row as isize,
                &format!("r{row}"),
                CanvasTextStyle::default(),
            );
        }
    }
    previous.clear_damage();

    let frame_plan = ScrollFastPathFramePlan {
        delta: 1,
        content_height_delta: 0,
        viewport_stable: true,
        content_delta_safe: true,
        fast_path: Some(ScrollFastPathPlan {
            blit_region: CachedClearRegion {
                x: 0,
                y: 0,
                width: 6,
                height: 4,
            },
            delta: 1,
            edge_region: CachedClearRegion {
                x: 0,
                y: 3,
                width: 6,
                height: 1,
            },
            absolute_repair_regions: Vec::new(),
        }),
        child_repairs: vec![ScrollFastPathChildRepair {
            key: "dirty-stable",
            region: CachedClearRegion {
                x: 0,
                y: 1,
                width: 6,
                height: 1,
            },
        }],
    };

    let mut next = Canvas::new(6, 4);
    let application = apply_scroll_fast_path_frame_plan(
        &mut next,
        &previous,
        &frame_plan,
        Some(ScrollFastPathTerminalFramePatchRequest {
            bounds: TerminalScrollHintBounds {
                previous_screen_height: 4,
                next_screen_height: 4,
            },
            options: TerminalScrollHintPatchOptions::fullscreen_synchronized(),
        }),
    )
    .unwrap();
    assert!(application.canvas_applied);
    assert_eq!(next.get_text(0, 1, 6, 1), "");
    assert_eq!(next.get_text(0, 3, 6, 1), "");
    assert_eq!(
        application
            .terminal_patch
            .as_ref()
            .unwrap()
            .scroll_patch_ansi,
        "\x1b[1;4r\x1b[1S\x1b[r\x1b[H"
    );
    assert_eq!(application.terminal_patch_skip_reason, None);

    let mut previous_for_diff = previous.clone();
    assert!(application.shift_previous_canvas_for_terminal_diff(&mut previous_for_diff));
    assert_eq!(previous_for_diff.get_text(0, 0, 6, 1), "r1");
    assert_eq!(previous_for_diff.get_text(0, 3, 6, 1), "");

    let mut skipped = Canvas::new(6, 4);
    let application = apply_scroll_fast_path_frame_plan(
        &mut skipped,
        &previous,
        &frame_plan,
        Some(ScrollFastPathTerminalFramePatchRequest {
            bounds: TerminalScrollHintBounds {
                previous_screen_height: 0,
                next_screen_height: 0,
            },
            options: TerminalScrollHintPatchOptions::default(),
        }),
    )
    .unwrap();
    assert!(application.canvas_applied);
    assert!(application.terminal_patch.is_none());
    assert_eq!(
        application.terminal_patch_skip_reason,
        Some(TerminalScrollHintPatchSkipReason::NotFullscreen)
    );

    let mut canvas_only = Canvas::new(6, 4);
    let application =
        apply_scroll_fast_path_frame_plan(&mut canvas_only, &previous, &frame_plan, None).unwrap();
    assert!(application.canvas_applied);
    assert!(application.terminal_patch.is_none());
    assert_eq!(application.terminal_patch_skip_reason, None);
}

#[test]
fn test_retained_child_blit_plan_matches_cc_ink_contamination_guards() {
    let decisions = plan_retained_child_blits(
        false,
        [
            RetainedChildBlitInput {
                key: "clean-before",
                dirty: false,
                clips_both_axes: false,
                absolute: false,
                opaque: false,
                has_background: false,
            },
            RetainedChildBlitInput {
                key: "dirty-clipped",
                dirty: true,
                clips_both_axes: true,
                absolute: false,
                opaque: false,
                has_background: false,
            },
            RetainedChildBlitInput {
                key: "clean-normal-after-clipped",
                dirty: false,
                clips_both_axes: false,
                absolute: false,
                opaque: false,
                has_background: false,
            },
            RetainedChildBlitInput {
                key: "absolute-transparent-after-clipped",
                dirty: false,
                clips_both_axes: false,
                absolute: true,
                opaque: false,
                has_background: false,
            },
            RetainedChildBlitInput {
                key: "absolute-opaque-after-clipped",
                dirty: false,
                clips_both_axes: false,
                absolute: true,
                opaque: true,
                has_background: false,
            },
            RetainedChildBlitInput {
                key: "dirty-unclipped",
                dirty: true,
                clips_both_axes: false,
                absolute: false,
                opaque: false,
                has_background: false,
            },
            RetainedChildBlitInput {
                key: "after-unclipped",
                dirty: false,
                clips_both_axes: false,
                absolute: false,
                opaque: false,
                has_background: false,
            },
        ],
    );

    assert_eq!(
        decisions,
        vec![
            RetainedChildBlitDecision {
                key: "clean-before",
                allow_previous_screen: true,
                skip_self_blit: false,
            },
            RetainedChildBlitDecision {
                key: "dirty-clipped",
                allow_previous_screen: true,
                skip_self_blit: false,
            },
            RetainedChildBlitDecision {
                key: "clean-normal-after-clipped",
                allow_previous_screen: true,
                skip_self_blit: false,
            },
            RetainedChildBlitDecision {
                key: "absolute-transparent-after-clipped",
                allow_previous_screen: true,
                skip_self_blit: true,
            },
            RetainedChildBlitDecision {
                key: "absolute-opaque-after-clipped",
                allow_previous_screen: true,
                skip_self_blit: false,
            },
            RetainedChildBlitDecision {
                key: "dirty-unclipped",
                allow_previous_screen: true,
                skip_self_blit: false,
            },
            RetainedChildBlitDecision {
                key: "after-unclipped",
                allow_previous_screen: false,
                skip_self_blit: false,
            },
        ]
    );

    let removed = plan_retained_child_blits(
        true,
        [RetainedChildBlitInput {
            key: "removed-parent-child",
            dirty: false,
            clips_both_axes: false,
            absolute: false,
            opaque: false,
            has_background: false,
        }],
    );
    assert_eq!(
        removed,
        vec![RetainedChildBlitDecision {
            key: "removed-parent-child",
            allow_previous_screen: false,
            skip_self_blit: false,
        }]
    );
}

#[test]
fn test_scroll_viewport_child_render_plan_matches_cc_ink_culling_cache_rules() {
    let decisions = plan_scroll_viewport_child_render(
        0,
        6,
        false,
        false,
        [
            ScrollViewportChildInput {
                key: "clean-cached-visible",
                top: 99,
                height: 99,
                cached_top: Some(0),
                cached_height: Some(1),
                dirty: false,
            },
            ScrollViewportChildInput {
                key: "dirty-culled-above",
                top: -4,
                height: 3,
                cached_top: Some(-4),
                cached_height: Some(1),
                dirty: true,
            },
            ScrollViewportChildInput {
                key: "clean-after-height-shift",
                top: 4,
                height: 1,
                cached_top: Some(2),
                cached_height: Some(1),
                dirty: false,
            },
            ScrollViewportChildInput {
                key: "dirty-visible",
                top: 5,
                height: 1,
                cached_top: Some(5),
                cached_height: Some(1),
                dirty: true,
            },
            ScrollViewportChildInput {
                key: "clean-after-rendered-dirty",
                top: 5,
                height: 1,
                cached_top: Some(5),
                cached_height: Some(1),
                dirty: false,
            },
            ScrollViewportChildInput {
                key: "clean-culled-below",
                top: 6,
                height: 1,
                cached_top: Some(6),
                cached_height: Some(1),
                dirty: false,
            },
        ],
    );

    assert_eq!(
        decisions,
        vec![
            ScrollViewportChildDecision {
                key: "clean-cached-visible",
                visible: true,
                top: 0,
                height: 1,
                used_cached_layout: true,
                allow_previous_screen: true,
                refresh_cached_top: None,
                drop_subtree_cache: false,
            },
            ScrollViewportChildDecision {
                key: "dirty-culled-above",
                visible: false,
                top: -4,
                height: 3,
                used_cached_layout: false,
                allow_previous_screen: false,
                refresh_cached_top: Some(-4),
                drop_subtree_cache: true,
            },
            ScrollViewportChildDecision {
                key: "clean-after-height-shift",
                visible: true,
                top: 4,
                height: 1,
                used_cached_layout: false,
                allow_previous_screen: true,
                refresh_cached_top: Some(4),
                drop_subtree_cache: false,
            },
            ScrollViewportChildDecision {
                key: "dirty-visible",
                visible: true,
                top: 5,
                height: 1,
                used_cached_layout: false,
                allow_previous_screen: true,
                refresh_cached_top: Some(5),
                drop_subtree_cache: false,
            },
            ScrollViewportChildDecision {
                key: "clean-after-rendered-dirty",
                visible: true,
                top: 5,
                height: 1,
                used_cached_layout: false,
                allow_previous_screen: false,
                refresh_cached_top: Some(5),
                drop_subtree_cache: false,
            },
            ScrollViewportChildDecision {
                key: "clean-culled-below",
                visible: false,
                top: 6,
                height: 1,
                used_cached_layout: false,
                allow_previous_screen: false,
                refresh_cached_top: Some(6),
                drop_subtree_cache: true,
            },
        ]
    );

    let preserve = plan_scroll_viewport_child_render(
        0,
        1,
        true,
        true,
        [ScrollViewportChildInput {
            key: "removed-parent-visible",
            top: 0,
            height: 1,
            cached_top: None,
            cached_height: None,
            dirty: false,
        }],
    );
    assert_eq!(
        preserve,
        vec![ScrollViewportChildDecision {
            key: "removed-parent-visible",
            visible: true,
            top: 0,
            height: 1,
            used_cached_layout: false,
            allow_previous_screen: false,
            refresh_cached_top: None,
            drop_subtree_cache: false,
        }]
    );
}

#[test]
fn test_escaping_absolute_descendant_blits_match_cc_ink_parent_blit_repair() {
    let parent = CachedClearRegion {
        x: 10,
        y: 5,
        width: 8,
        height: 4,
    };
    let blits = plan_escaping_absolute_descendant_blits(
        parent,
        [
            AbsoluteDescendantRect {
                key: "inside",
                rect: CachedClearRegion {
                    x: 11,
                    y: 6,
                    width: 2,
                    height: 1,
                },
            },
            AbsoluteDescendantRect {
                key: "left",
                rect: CachedClearRegion {
                    x: 8,
                    y: 6,
                    width: 3,
                    height: 1,
                },
            },
            AbsoluteDescendantRect {
                key: "right",
                rect: CachedClearRegion {
                    x: 17,
                    y: 6,
                    width: 3,
                    height: 1,
                },
            },
            AbsoluteDescendantRect {
                key: "above",
                rect: CachedClearRegion {
                    x: 12,
                    y: 4,
                    width: 2,
                    height: 2,
                },
            },
            AbsoluteDescendantRect {
                key: "below",
                rect: CachedClearRegion {
                    x: 12,
                    y: 8,
                    width: 2,
                    height: 2,
                },
            },
            AbsoluteDescendantRect {
                key: "empty",
                rect: CachedClearRegion {
                    x: 0,
                    y: 0,
                    width: 0,
                    height: 1,
                },
            },
        ],
    );

    assert_eq!(
        blits,
        vec![
            EscapingAbsoluteDescendantBlit {
                key: "left",
                rect: CachedClearRegion {
                    x: 8,
                    y: 6,
                    width: 3,
                    height: 1,
                },
            },
            EscapingAbsoluteDescendantBlit {
                key: "right",
                rect: CachedClearRegion {
                    x: 17,
                    y: 6,
                    width: 3,
                    height: 1,
                },
            },
            EscapingAbsoluteDescendantBlit {
                key: "above",
                rect: CachedClearRegion {
                    x: 12,
                    y: 4,
                    width: 2,
                    height: 2,
                },
            },
            EscapingAbsoluteDescendantBlit {
                key: "below",
                rect: CachedClearRegion {
                    x: 12,
                    y: 8,
                    width: 2,
                    height: 2,
                },
            },
        ]
    );
}

#[test]
fn test_apply_escaping_absolute_descendant_blits_to_canvas_restores_overflow_cells() {
    let parent = CachedClearRegion {
        x: 10,
        y: 5,
        width: 8,
        height: 4,
    };
    let blits = plan_escaping_absolute_descendant_blits(
        parent,
        [
            AbsoluteDescendantRect {
                key: "inside",
                rect: CachedClearRegion {
                    x: 11,
                    y: 6,
                    width: 2,
                    height: 1,
                },
            },
            AbsoluteDescendantRect {
                key: "left",
                rect: CachedClearRegion {
                    x: 8,
                    y: 6,
                    width: 3,
                    height: 1,
                },
            },
            AbsoluteDescendantRect {
                key: "above",
                rect: CachedClearRegion {
                    x: 12,
                    y: 4,
                    width: 2,
                    height: 2,
                },
            },
        ],
    );

    let mut previous = Canvas::new(20, 10);
    {
        let mut view = previous.subview_mut(0, 0, 0, 0, 20, 10);
        view.set_text(10, 6, "parent", CanvasTextStyle::default());
        view.set_text(8, 6, "LFT", CanvasTextStyle::default());
        view.set_text(12, 4, "AB", CanvasTextStyle::default());
    }
    previous.clear_damage();

    let mut next = Canvas::new(20, 10);
    next.blit_region_from(
        &previous,
        parent.x as usize,
        parent.y as usize,
        parent.width as usize,
        parent.height as usize,
    );
    assert_eq!(next.get_text(8, 6, 2, 1), "");
    assert_eq!(next.get_text(12, 4, 2, 1), "");

    let applied = apply_escaping_absolute_descendant_blits_to_canvas(&mut next, &previous, blits);
    assert_eq!(next.get_text(8, 6, 3, 1), "LFT");
    assert_eq!(next.get_text(12, 4, 2, 1), "AB");
    assert_eq!(
        applied,
        vec![
            EscapingAbsoluteDescendantCanvasBlit {
                key: "left",
                region: DamageRegion {
                    x: 8,
                    y: 6,
                    width: 3,
                    height: 1,
                },
            },
            EscapingAbsoluteDescendantCanvasBlit {
                key: "above",
                region: DamageRegion {
                    x: 12,
                    y: 4,
                    width: 2,
                    height: 2,
                },
            },
        ]
    );
}

#[test]
fn test_retained_node_render_plan_matches_cc_ink_node_blit_and_clear_guards() {
    let current = CachedLayoutBounds {
        x: 1,
        y: 2,
        width: 10,
        height: 3,
        top: Some(2),
    };
    let moved = CachedLayoutBounds { y: 4, ..current };

    let clean_blit = plan_retained_node_render(RetainedNodeRenderInput {
        current_layout: current,
        cached_layout: Some(current),
        dirty: false,
        skip_self_blit: false,
        pending_scroll_delta: false,
        previous_screen_available: true,
        hidden: false,
        absolute: true,
        pending_clears: Vec::new(),
    });
    assert_eq!(
        clean_blit,
        RetainedNodeRenderPlan {
            action: RetainedNodeRenderAction::Blit,
            blit_region: Some(current.into()),
            clear_old_region: None,
            clear_old_from_absolute: false,
            pending_clear_regions: Vec::new(),
            has_removed_child: false,
            position_changed: false,
            layout_shifted: false,
            drop_subtree_cache: false,
            record_absolute_rect: true,
        }
    );

    let dirty_same_position = plan_retained_node_render(RetainedNodeRenderInput {
        current_layout: current,
        cached_layout: Some(current),
        dirty: true,
        skip_self_blit: false,
        pending_scroll_delta: false,
        previous_screen_available: true,
        hidden: false,
        absolute: false,
        pending_clears: Vec::new(),
    });
    assert_eq!(dirty_same_position.action, RetainedNodeRenderAction::Render);
    assert_eq!(dirty_same_position.clear_old_region, Some(current.into()));
    assert!(!dirty_same_position.layout_shifted);

    let moved_clean = plan_retained_node_render(RetainedNodeRenderInput {
        current_layout: moved,
        cached_layout: Some(current),
        dirty: false,
        skip_self_blit: false,
        pending_scroll_delta: false,
        previous_screen_available: true,
        hidden: false,
        absolute: false,
        pending_clears: Vec::new(),
    });
    assert_eq!(moved_clean.action, RetainedNodeRenderAction::Render);
    assert_eq!(moved_clean.clear_old_region, Some(current.into()));
    assert!(moved_clean.position_changed);
    assert!(moved_clean.layout_shifted);

    let pending_clear = CachedClearRegion {
        x: 0,
        y: 1,
        width: 2,
        height: 1,
    };
    let removed_child = plan_retained_node_render(RetainedNodeRenderInput {
        current_layout: current,
        cached_layout: Some(current),
        dirty: false,
        skip_self_blit: false,
        pending_scroll_delta: true,
        previous_screen_available: true,
        hidden: false,
        absolute: false,
        pending_clears: vec![pending_clear],
    });
    assert_eq!(removed_child.action, RetainedNodeRenderAction::Render);
    assert_eq!(removed_child.pending_clear_regions, vec![pending_clear]);
    assert!(removed_child.has_removed_child);
    assert!(removed_child.layout_shifted);

    let hidden = plan_retained_node_render(RetainedNodeRenderInput {
        current_layout: current,
        cached_layout: Some(current),
        dirty: true,
        skip_self_blit: false,
        pending_scroll_delta: false,
        previous_screen_available: true,
        hidden: true,
        absolute: true,
        pending_clears: vec![pending_clear],
    });
    assert_eq!(hidden.action, RetainedNodeRenderAction::Hidden);
    assert_eq!(hidden.clear_old_region, Some(current.into()));
    assert!(hidden.clear_old_from_absolute);
    assert!(hidden.drop_subtree_cache);
    assert!(hidden.layout_shifted);
    assert!(hidden.pending_clear_regions.is_empty());
}

#[test]
fn test_apply_retained_node_render_plan_to_canvas_blits_and_clears() {
    let mut previous = Canvas::new(8, 4);
    {
        let mut view = previous.subview_mut(0, 0, 0, 0, 8, 4);
        for row in 0..4 {
            view.set_text(
                0,
                row as isize,
                &format!("row{row}"),
                CanvasTextStyle::default(),
            );
        }
    }
    previous.clear_damage();

    let pending_clear = CachedClearRegion {
        x: 0,
        y: 1,
        width: 8,
        height: 1,
    };
    let blit_plan = RetainedNodeRenderPlan {
        action: RetainedNodeRenderAction::Blit,
        blit_region: Some(CachedClearRegion {
            x: 0,
            y: 0,
            width: 8,
            height: 4,
        }),
        clear_old_region: None,
        clear_old_from_absolute: false,
        pending_clear_regions: vec![pending_clear],
        has_removed_child: true,
        position_changed: false,
        layout_shifted: true,
        drop_subtree_cache: false,
        record_absolute_rect: false,
    };

    let mut next = Canvas::new(8, 4);
    let applied = apply_retained_node_render_plan_to_canvas(&mut next, &previous, &blit_plan);
    assert_eq!(next.get_text(0, 0, 8, 1), "row0");
    assert_eq!(next.get_text(0, 1, 8, 1), "");
    assert_eq!(next.get_text(0, 2, 8, 1), "row2");
    assert_eq!(
        applied.blitted_region,
        Some(DamageRegion {
            x: 0,
            y: 0,
            width: 8,
            height: 4,
        })
    );
    assert_eq!(
        applied.pending_clear_regions,
        vec![DamageRegion {
            x: 0,
            y: 1,
            width: 8,
            height: 1,
        }]
    );

    let clear_plan = RetainedNodeRenderPlan {
        action: RetainedNodeRenderAction::Render,
        blit_region: None,
        clear_old_region: Some(CachedClearRegion {
            x: 0,
            y: 2,
            width: 8,
            height: 1,
        }),
        clear_old_from_absolute: false,
        pending_clear_regions: Vec::new(),
        has_removed_child: false,
        position_changed: true,
        layout_shifted: true,
        drop_subtree_cache: false,
        record_absolute_rect: false,
    };
    let mut dirty_next = previous.clone();
    let applied =
        apply_retained_node_render_plan_to_canvas(&mut dirty_next, &previous, &clear_plan);
    assert_eq!(dirty_next.get_text(0, 2, 8, 1), "");
    assert_eq!(
        applied.cleared_old_region,
        Some(DamageRegion {
            x: 0,
            y: 2,
            width: 8,
            height: 1,
        })
    );
    assert_eq!(applied.blitted_region, None);
}

#[test]
fn test_renderer_layout_shift_tracker_matches_cc_ink_layout_shift_flag() {
    let mut tracker = RendererLayoutShiftTracker::<&'static str>::new();
    let root = RendererLayoutSnapshot {
        x: 0,
        y: 0,
        width: 10,
        height: 3,
    };
    let child = RendererLayoutSnapshot {
        x: 0,
        y: 1,
        width: 10,
        height: 1,
    };

    assert!(!tracker.update([("root", root), ("child", child)]));
    assert_eq!(tracker.len(), 2);
    assert_eq!(tracker.snapshot(&"child"), Some(child));
    assert!(!tracker.update([("root", root), ("child", child)]));

    assert!(tracker.update([
        ("root", root),
        ("child", RendererLayoutSnapshot { y: 2, ..child },),
    ]));
    assert!(
        tracker.update([("root", root)]),
        "removed child shifts layout"
    );
    assert!(
        tracker.update([("root", root), ("child", child)]),
        "added child shifts layout"
    );

    tracker.clear();
    assert!(tracker.is_empty());
    assert!(!tracker.update_from_layouts([(
        "root",
        CachedLayoutBounds {
            x: 0,
            y: 0,
            width: 10,
            height: 3,
            top: Some(0),
        },
    )]));
    assert_eq!(
        tracker.snapshot(&"root"),
        Some(RendererLayoutSnapshot {
            x: 0,
            y: 0,
            width: 10,
            height: 3,
        })
    );
}

#[test]
fn test_renderer_node_cache_matches_cc_node_cache_semantics() {
    let mut cache = RendererNodeCache::<&'static str>::new();
    let layout = CachedLayoutBounds {
        x: 1,
        y: 2,
        width: 10,
        height: 3,
        top: Some(2),
    };

    assert!(!cache.can_blit(&"child", layout));
    cache.set_layout("child", layout);
    assert_eq!(cache.layout(&"child"), Some(layout));
    assert!(cache.can_blit(&"child", layout));
    assert!(!cache.can_blit(&"child", CachedLayoutBounds { y: 3, ..layout }));

    cache.add_pending_clear("parent", layout.into(), false);
    assert!(!cache.consume_absolute_removed_flag());
    assert_eq!(cache.take_pending_clears(&"parent"), vec![layout.into()]);
    assert!(cache.take_pending_clears(&"parent").is_empty());

    let negative_clear = CachedClearRegion {
        x: 4,
        y: -1,
        width: 6,
        height: 2,
    };
    assert_eq!(
        negative_clear.clipped_to_canvas(8, 4),
        Some(DamageRegion {
            x: 4,
            y: 0,
            width: 4,
            height: 1,
        })
    );

    cache.add_pending_clear("parent", negative_clear, true);
    assert!(cache.consume_absolute_removed_flag());
    assert!(!cache.consume_absolute_removed_flag());
    assert_eq!(cache.remove_layout(&"child"), Some(layout));

    cache.set_layout("root", layout);
    cache.set_layout("branch", CachedLayoutBounds { x: 2, ..layout });
    cache.set_layout("leaf", CachedLayoutBounds { x: 3, ..layout });
    cache.set_layout("sibling", CachedLayoutBounds { x: 4, ..layout });
    cache.add_pending_clear("branch", layout.into(), false);
    let children = std::collections::HashMap::from([
        ("root", vec!["branch", "sibling"]),
        ("branch", vec!["leaf"]),
    ]);
    cache.remove_subtree(&"branch", |node| {
        children.get(node).cloned().unwrap_or_default()
    });
    assert_eq!(cache.layout(&"branch"), None);
    assert_eq!(cache.layout(&"leaf"), None);
    assert_eq!(cache.layout(&"root"), Some(layout));
    assert_eq!(cache.layout(&"sibling").map(|bounds| bounds.x), Some(4));
    assert!(cache.take_pending_clears(&"branch").is_empty());

    cache.clear();
    assert_eq!(cache.layout(&"child"), None);
    assert!(cache.take_pending_clears(&"parent").is_empty());
    assert!(!cache.consume_absolute_removed_flag());
}

#[test]
fn test_renderer_retained_frame_state_integrates_cache_plan_and_commit() {
    let mut state = RendererRetainedFrameState::<&'static str>::new();
    let root = CachedLayoutBounds {
        x: 0,
        y: 0,
        width: 10,
        height: 3,
        top: Some(0),
    };
    let child = CachedLayoutBounds {
        x: 1,
        y: 1,
        width: 4,
        height: 1,
        top: Some(1),
    };

    assert!(!state.begin_frame());
    let first = state.plan_node(RetainedFrameNodeInput {
        key: "root",
        current_layout: root,
        dirty: true,
        skip_self_blit: false,
        pending_scroll_delta: false,
        previous_screen_available: true,
        hidden: false,
        absolute: false,
    });
    assert_eq!(first.plan.action, RetainedNodeRenderAction::Render);
    assert!(!state.layout_shifted());
    state.commit_node_plan(&first);
    assert_eq!(state.cache().layout(&"root"), Some(root));

    assert!(!state.begin_frame());
    let clean = state.plan_node(RetainedFrameNodeInput {
        key: "root",
        current_layout: root,
        dirty: false,
        skip_self_blit: false,
        pending_scroll_delta: false,
        previous_screen_available: true,
        hidden: false,
        absolute: false,
    });
    assert_eq!(clean.plan.action, RetainedNodeRenderAction::Blit);
    assert_eq!(clean.plan.blit_region, Some(root.into()));
    state.commit_node_plan(&clean);

    state.queue_child_clear("root", child.into(), true);
    assert!(
        state.begin_frame(),
        "absolute child clear should poison next-frame blits"
    );
    let with_removed_child = state.plan_node(RetainedFrameNodeInput {
        key: "root",
        current_layout: root,
        dirty: false,
        skip_self_blit: false,
        pending_scroll_delta: false,
        previous_screen_available: true,
        hidden: false,
        absolute: false,
    });
    assert_eq!(
        with_removed_child.plan.pending_clear_regions,
        vec![child.into()]
    );
    assert!(with_removed_child.plan.has_removed_child);
    assert!(state.layout_shifted());
    state.commit_node_plan(&with_removed_child);

    state.cache_mut().set_layout("overlay", child);
    state
        .cache_mut()
        .set_layout("leaf", CachedLayoutBounds { x: 2, ..child });
    assert!(!state.begin_frame());
    let hidden_overlay = state.plan_node(RetainedFrameNodeInput {
        key: "overlay",
        current_layout: child,
        dirty: true,
        skip_self_blit: false,
        pending_scroll_delta: false,
        previous_screen_available: true,
        hidden: true,
        absolute: true,
    });
    assert_eq!(hidden_overlay.plan.action, RetainedNodeRenderAction::Hidden);
    assert!(hidden_overlay.plan.drop_subtree_cache);
    assert!(state.absolute_clear_this_frame());
    let children = std::collections::HashMap::from([("overlay", vec!["leaf"])]);
    state.commit_node_plan_with_children(&hidden_overlay, |node| {
        children.get(node).cloned().unwrap_or_default()
    });
    assert_eq!(state.cache().layout(&"overlay"), None);
    assert_eq!(state.cache().layout(&"leaf"), None);
    assert_eq!(state.cache().layout(&"root"), Some(root));
}

#[test]
fn test_renderer_retained_tree_state_integrates_dirty_tree_and_cache() {
    let mut state = RendererRetainedTreeState::<&'static str>::new();
    let root = CachedLayoutBounds {
        x: 0,
        y: 0,
        width: 20,
        height: 5,
        top: Some(0),
    };
    let branch = CachedLayoutBounds {
        x: 0,
        y: 1,
        width: 20,
        height: 3,
        top: Some(1),
    };
    let leaf = CachedLayoutBounds {
        x: 0,
        y: 2,
        width: 20,
        height: 1,
        top: Some(2),
    };
    let overlay = CachedLayoutBounds {
        x: 2,
        y: 1,
        width: 5,
        height: 1,
        top: Some(1),
    };

    state.register_root("root");
    state.attach("branch", "root");
    state.attach("leaf", "branch");
    state.attach("overlay", "root");
    state.clear_dirty();

    state.mark_dirty(&"leaf", true);
    assert!(state.is_dirty(&"leaf"));
    assert!(state.is_dirty(&"branch"));
    assert!(state.is_dirty(&"root"));
    assert!(!state.is_dirty(&"overlay"));

    state.begin_frame();
    let root_plan = state.plan_node(RetainedTreeNodeInput {
        key: "root",
        current_layout: root,
        skip_self_blit: false,
        pending_scroll_delta: false,
        previous_screen_available: true,
        hidden: false,
        absolute: false,
    });
    assert_eq!(root_plan.plan.action, RetainedNodeRenderAction::Render);
    state.commit_node_plan(&root_plan);
    assert!(!state.is_dirty(&"root"));
    assert!(state.is_dirty(&"branch"));

    let branch_plan = state.plan_node(RetainedTreeNodeInput {
        key: "branch",
        current_layout: branch,
        skip_self_blit: false,
        pending_scroll_delta: false,
        previous_screen_available: true,
        hidden: false,
        absolute: false,
    });
    let leaf_plan = state.plan_node(RetainedTreeNodeInput {
        key: "leaf",
        current_layout: leaf,
        skip_self_blit: false,
        pending_scroll_delta: false,
        previous_screen_available: true,
        hidden: false,
        absolute: false,
    });
    assert_eq!(branch_plan.plan.action, RetainedNodeRenderAction::Render);
    assert_eq!(leaf_plan.plan.action, RetainedNodeRenderAction::Render);
    state.commit_node_plan(&branch_plan);
    state.commit_node_plan(&leaf_plan);
    assert!(!state.is_dirty(&"branch"));
    assert!(!state.is_dirty(&"leaf"));
    assert_eq!(state.frame_state().cache().layout(&"leaf"), Some(leaf));

    state
        .frame_state_mut()
        .cache_mut()
        .set_layout("overlay", overlay);
    let removed = state.remove_subtree(&"overlay", true);
    assert_eq!(removed, vec!["overlay"]);
    assert!(state.is_dirty(&"root"));
    assert!(
        state.begin_frame(),
        "absolute removal poisons next frame blits"
    );
    let root_after_remove = state.plan_node(RetainedTreeNodeInput {
        key: "root",
        current_layout: root,
        skip_self_blit: false,
        pending_scroll_delta: false,
        previous_screen_available: true,
        hidden: false,
        absolute: false,
    });
    assert_eq!(
        root_after_remove.plan.action,
        RetainedNodeRenderAction::Render
    );
    assert_eq!(
        root_after_remove.plan.pending_clear_regions,
        vec![overlay.into()]
    );
    assert!(root_after_remove.plan.has_removed_child);
    assert!(state.frame_state().layout_shifted());
}

#[test]
fn test_renderer_dirty_tree_marks_ancestors_and_removes_subtrees() {
    let mut tree = RendererDirtyTree::<&'static str>::new();
    tree.register_root("root");
    tree.attach("branch", "root");
    tree.attach("leaf", "branch");
    tree.attach("sibling", "root");
    assert_eq!(tree.child_keys(&"root"), vec!["branch", "sibling"]);

    tree.mark_dirty(&"leaf", true);
    assert!(tree.is_dirty(&"leaf"));
    assert!(tree.is_dirty(&"branch"));
    assert!(tree.is_dirty(&"root"));
    assert!(!tree.is_dirty(&"sibling"));
    assert!(tree.is_measure_dirty(&"leaf"));
    assert!(!tree.is_measure_dirty(&"branch"));

    tree.clear_node(&"branch");
    assert!(!tree.is_dirty(&"branch"));
    assert!(tree.is_dirty(&"root"));
    tree.clear_dirty();
    assert!(tree.dirty_nodes().next().is_none());

    let mut removed = tree.remove_subtree(&"branch");
    removed.sort_unstable();
    assert_eq!(removed, vec!["branch", "leaf"]);
    assert_eq!(tree.parent(&"leaf"), None);
    assert!(
        tree.is_dirty(&"root"),
        "removing a child dirties the parent"
    );
    assert!(!tree.is_dirty(&"branch"));
    assert!(!tree.is_dirty(&"leaf"));

    tree.attach("sibling", "branch");
    assert_eq!(tree.parent(&"sibling"), Some(&"branch"));
    assert_eq!(tree.child_keys(&"branch"), vec!["sibling"]);
    tree.register_root("sibling");
    assert_eq!(tree.parent(&"sibling"), None);
    tree.clear();
    assert!(tree.dirty_nodes().next().is_none());
    assert_eq!(tree.parent(&"sibling"), None);
}
