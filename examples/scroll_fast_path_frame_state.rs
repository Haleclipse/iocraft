//! Demonstrates stateful retained ScrollBox fast-path planning.
//!
//! `ScrollFastPathFrameState` remembers the previous content wrapper position
//! and previous absolute overlay rects. A custom fullscreen renderer can use the
//! resulting plan to blit the old viewport, shift rows, repaint edge rows, and
//! repair stable rows without making this optimization part of iocraft's default
//! renderer.

use iocraft::prelude::*;

fn main() {
    let viewport = CachedClearRegion {
        x: 0,
        y: 4,
        width: 60,
        height: 10,
    };
    let mut state = ScrollFastPathFrameState::new();

    // Frame 1: render normally and record an absolute overlay that may need
    // repair if its pixels are shifted by a future scroll blit.
    state.begin_frame();
    state.record_absolute_rect(CachedClearRegion {
        x: 8,
        y: 6,
        width: 20,
        height: 1,
    });
    state.commit_frame(viewport, 4, 200);

    // Frame 2: content_y moved up by 3 rows, so a small scroll fast path is safe.
    state.begin_frame();
    let plan = state.plan_frame(ScrollFastPathFrameInput {
        viewport,
        content_y: 1,
        scroll_top: 3,
        content_height: 200,
        children: vec![ScrollFastPathChild {
            key: "streaming-row",
            top: 7,
            height: 2,
            cached_y: Some(8),
            cached_height: Some(1),
            dirty: true,
        }],
    });

    println!("delta: {}", plan.delta);
    println!("fast path available: {}", plan.fast_path.is_some());
    if let Some(fast) = &plan.fast_path {
        println!("blit region: {:?}", fast.blit_region);
        println!("edge repaint: {:?}", fast.edge_region);
        println!("absolute repairs: {:?}", fast.absolute_repair_regions);
    }
    println!("child repairs: {:?}", plan.child_repairs);

    let previous = Canvas::new(60, 14);
    let mut next = Canvas::new(60, 14);
    let application = apply_scroll_fast_path_frame_plan(
        &mut next,
        &previous,
        &plan,
        Some(ScrollFastPathTerminalFramePatchRequest {
            bounds: TerminalScrollHintBounds {
                previous_screen_height: 14,
                next_screen_height: 14,
            },
            options: TerminalScrollHintPatchOptions::fullscreen_synchronized(),
        }),
    )
    .expect("bounds are valid");
    println!("canvas skeleton applied: {}", application.canvas_applied);
    println!("scroll hint: {:?}", next.scroll_hint());

    if let Some(patch) = &application.terminal_patch {
        println!("fullscreen DECSTBM patch: {:?}", patch.scroll_patch_ansi);
        println!("edge rows to repaint: {:?}", patch.edge_region);
        println!("absolute repairs: {:?}", patch.absolute_repair_regions);
        println!("child repairs: {:?}", patch.child_repairs);

        let mut previous_for_diff = previous.clone();
        let shifted = application.shift_previous_canvas_for_terminal_diff(&mut previous_for_diff);
        println!("previous canvas shifted before sparse diff: {shifted}");

        let terminal_frame = plan_terminal_fullscreen_canvas_frame_patches(
            Some(&previous_for_diff),
            &next,
            TerminalFullscreenCanvasFramePatchOptions {
                canvas_diff: TerminalFullscreenCanvasDiffOptions {
                    top_row: 0,
                    force_full_repaint: false,
                },
                scroll_patch_ansi: Some(patch.scroll_patch_ansi.clone()),
                erase_before_paint: false,
                terminal_rows: Some(24),
                optimize: true,
            },
        );
        println!(
            "final fullscreen patch count: {}",
            terminal_frame.patches.len()
        );
    } else if let Some(reason) = application.terminal_patch_skip_reason {
        println!("terminal patch skipped: {reason:?}");
    }
}
