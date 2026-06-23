//! Demonstrates the opt-in retained-frame state helper.
//!
//! `RendererRetainedFrameState` combines the CC Ink-style node-cache metadata
//! and retained-node planning helpers without becoming iocraft's default
//! renderer. Custom renderers still own traversal, dirty invalidation, and the
//! actual canvas blits/clears.

use iocraft::prelude::*;

fn main() {
    let mut state = RendererRetainedFrameState::<&'static str>::new();
    let layout = CachedLayoutBounds {
        x: 0,
        y: 0,
        width: 40,
        height: 3,
        top: Some(0),
    };

    state.begin_frame();
    let first = state.plan_node(RetainedFrameNodeInput {
        key: "transcript-row",
        current_layout: layout,
        dirty: true,
        skip_self_blit: false,
        pending_scroll_delta: false,
        previous_screen_available: true,
        hidden: false,
        absolute: false,
    });
    println!("first frame action: {:?}", first.plan.action);
    state.commit_node_plan(&first);

    state.begin_frame();
    let clean = state.plan_node(RetainedFrameNodeInput {
        key: "transcript-row",
        current_layout: layout,
        dirty: false,
        skip_self_blit: false,
        pending_scroll_delta: false,
        previous_screen_available: true,
        hidden: false,
        absolute: false,
    });
    println!("clean frame action: {:?}", clean.plan.action);
    println!("blit region: {:?}", clean.plan.blit_region);
    state.commit_node_plan(&clean);

    state.queue_child_clear(
        "transcript-row",
        CachedClearRegion {
            x: 2,
            y: 1,
            width: 10,
            height: 1,
        },
        true,
    );
    let absolute_removed = state.begin_frame();
    let with_removed_child = state.plan_node(RetainedFrameNodeInput {
        key: "transcript-row",
        current_layout: layout,
        dirty: false,
        skip_self_blit: false,
        pending_scroll_delta: false,
        previous_screen_available: true,
        hidden: false,
        absolute: false,
    });
    println!("absolute removed at frame start: {absolute_removed}");
    println!("layout shifted: {}", state.layout_shifted());
    println!(
        "pending clears: {:?}",
        with_removed_child.plan.pending_clear_regions
    );
}
