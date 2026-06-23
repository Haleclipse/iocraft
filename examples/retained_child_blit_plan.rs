//! Demonstrates mode-neutral sibling blit contamination planning.
//!
//! This mirrors CC Ink's `renderChildren(...)` retained-screen guard without
//! binding iocraft core to a DOM/WeakMap renderer. Custom retained renderers can
//! decide whether each child subtree may see the previous canvas and whether an
//! absolute child should skip its own direct blit.

use iocraft::prelude::*;

fn main() {
    let plan = plan_retained_child_blits(
        false,
        [
            RetainedChildBlitInput {
                key: "dirty clipped viewport",
                dirty: true,
                clips_both_axes: true,
                absolute: false,
                opaque: false,
                has_background: false,
            },
            RetainedChildBlitInput {
                key: "ordinary sibling",
                dirty: false,
                clips_both_axes: false,
                absolute: false,
                opaque: false,
                has_background: false,
            },
            RetainedChildBlitInput {
                key: "transparent absolute overlay",
                dirty: false,
                clips_both_axes: false,
                absolute: true,
                opaque: false,
                has_background: false,
            },
            RetainedChildBlitInput {
                key: "dirty overflowing sibling",
                dirty: true,
                clips_both_axes: false,
                absolute: false,
                opaque: false,
                has_background: false,
            },
            RetainedChildBlitInput {
                key: "later sibling",
                dirty: false,
                clips_both_axes: false,
                absolute: false,
                opaque: false,
                has_background: false,
            },
        ],
    );

    for decision in plan {
        println!(
            "{:<28} allow_previous_screen={} skip_self_blit={}",
            decision.key, decision.allow_previous_screen, decision.skip_self_blit
        );
    }
}
