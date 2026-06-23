//! Demonstrates the pure CC Ink-style ScrollBox render-offset planner.
//!
//! The planner is useful for custom retained/virtual scroll containers: it
//! models one-shot anchor seek, sticky bottom follow, pending-delta drain, and
//! mounted-range clamp without mutating a component or writing terminal output.
//! The mounted clamp is visual-only: the committed logical target can remain
//! ahead of the mounted range so a virtual list can catch up on a later commit.

use iocraft::prelude::*;

fn main() {
    let plan = plan_scroll_box_render_offset(ScrollBoxRenderOffsetInput {
        current_scroll_top: 13,
        previous_scroll_height: 100,
        scroll_height: 100,
        viewport_height: 10,
        sticky: false,
        pending_delta: Some(20),
        anchor_top: None,
        anchor_offset: 0,
        clamp_min: Some(12),
        clamp_max: Some(14),
        drain_mode: Some(ScrollDrainMode::Native),
    });

    println!("visual_scroll_top: {}", plan.scroll_top);
    println!("committed_scroll_top: {}", plan.committed_scroll_top);
    println!("pending_delta: {:?}", plan.pending_delta);
    println!("drained_delta: {}", plan.drained_delta);
    println!(
        "clamped_to_mounted_range: {}",
        plan.clamped_to_mounted_range
    );
}
