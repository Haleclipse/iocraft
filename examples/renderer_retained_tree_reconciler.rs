//! Demonstrates logical-key reconciliation for retained renderer experiments.
//!
//! `RendererRetainedTreeReconciler` is a higher-level opt-in building block that
//! maps logical node keys to generation-stamped IDs, updates retained dirty/cache
//! state, and bumps generations when subtrees are removed. It models CC Ink's
//! DOM-object identity semantics without hidden `WeakMap` state.

use iocraft::prelude::*;

fn main() {
    let mut reconciler = RendererRetainedTreeReconciler::<&'static str>::new();
    let root = reconciler.register_root("root");
    let (row, _) = reconciler.attach("row-42", "root");
    println!("root={root:?} row={row:?}");

    reconciler.mark_dirty(&"row-42", true);
    reconciler.begin_frame();

    let plan = reconciler.plan_node(RetainedLogicalTreeNodeInput {
        key: "row-42",
        current_layout: CachedLayoutBounds {
            x: 0,
            y: 2,
            width: 80,
            height: 1,
            top: Some(2),
        },
        skip_self_blit: false,
        pending_scroll_delta: false,
        previous_screen_available: true,
        hidden: false,
        absolute: false,
    });
    println!("row plan: {:?}", plan.plan.action);
    reconciler.commit_node_plan(&plan);

    let removed = reconciler.remove_subtree(&"row-42", false);
    println!("removed stable ids: {removed:?}");
    println!("new row generation: {:?}", reconciler.current_id("row-42"));
}
