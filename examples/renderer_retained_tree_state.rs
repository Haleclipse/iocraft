//! Demonstrates the opt-in retained tree state.
//!
//! `RendererRetainedTreeState` combines CC Ink-style ancestor dirty
//! invalidation with retained node-cache planning, while keeping stable node IDs,
//! traversal, canvas writes, and terminal writes in the caller's control.

use iocraft::prelude::*;

fn main() {
    let mut state = RendererRetainedTreeState::<&'static str>::new();
    state.register_root("root");
    state.attach("transcript", "root");
    state.attach("row-42", "transcript");
    state.clear_dirty();

    let root = CachedLayoutBounds {
        x: 0,
        y: 0,
        width: 80,
        height: 24,
        top: Some(0),
    };
    let transcript = CachedLayoutBounds {
        x: 0,
        y: 1,
        width: 80,
        height: 22,
        top: Some(1),
    };
    let row = CachedLayoutBounds {
        x: 0,
        y: 22,
        width: 80,
        height: 1,
        top: Some(21),
    };

    state.mark_dirty(&"row-42", true);
    state.begin_frame();

    for (key, layout) in [("root", root), ("transcript", transcript), ("row-42", row)] {
        let plan = state.plan_node(RetainedTreeNodeInput {
            key,
            current_layout: layout,
            skip_self_blit: false,
            pending_scroll_delta: false,
            previous_screen_available: true,
            hidden: false,
            absolute: false,
        });
        println!("{key}: {:?}", plan.plan.action);
        state.commit_node_plan(&plan);
    }
}
