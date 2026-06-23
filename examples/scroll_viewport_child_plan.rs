//! Demonstrates scroll viewport child culling/cache planning.
//!
//! This is the mode-neutral equivalent of CC Ink's `renderScrolledChildren(...)`
//! cache shortcut: a custom retained renderer can avoid fresh layout reads for
//! clean children while still dropping stale caches for culled subtrees.

use iocraft::prelude::*;

fn main() {
    let plan = plan_scroll_viewport_child_render(
        0,
        5,
        false,
        false,
        [
            ScrollViewportChildInput {
                key: "cached visible row",
                top: 99,
                height: 99,
                cached_top: Some(0),
                cached_height: Some(1),
                dirty: false,
            },
            ScrollViewportChildInput {
                key: "dirty offscreen growth",
                top: -4,
                height: 3,
                cached_top: Some(-4),
                cached_height: Some(1),
                dirty: true,
            },
            ScrollViewportChildInput {
                key: "clean after growth",
                top: 3,
                height: 1,
                cached_top: Some(1),
                cached_height: Some(1),
                dirty: false,
            },
        ],
    );

    for decision in plan {
        println!(
            "{:<24} visible={} used_cached={} allow_prev={} refresh_top={:?} drop_cache={}",
            decision.key,
            decision.visible,
            decision.used_cached_layout,
            decision.allow_previous_screen,
            decision.refresh_cached_top,
            decision.drop_subtree_cache,
        );
    }
}
