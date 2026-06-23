//! Demonstrates CC Ink-style virtual-scroll range planning.
//!
//! This is a pure helper: it does not render, mutate a `ScrollBox`, or read
//! the terminal. A component can render `top_spacer`, `range`, and
//! `bottom_spacer`, then pass `clamp_min` / `clamp_max` to a `ScrollBoxHandle`.

use iocraft::prelude::*;

fn main() {
    let mut heights = vec![None; 1_000];
    // Pretend a few rows near the current viewport were measured by layout.
    for height in &mut heights[95..110] {
        *height = Some(2);
    }

    let plan = plan_virtual_scroll_range(
        &heights,
        VirtualScrollInput {
            scroll_top: Some(300),
            pending_delta: 120,
            viewport_height: 20,
            is_sticky: false,
            ..Default::default()
        },
        VirtualScrollConfig::default(),
    );

    println!("range: {}..{}", plan.range.start, plan.range.end);
    println!("top spacer rows: {}", plan.top_spacer);
    println!("bottom spacer rows: {}", plan.bottom_spacer);
    println!("target scroll top: {}", plan.target_scroll_top);
    println!(
        "clamp bounds for ScrollBoxHandle::set_clamp_bounds: {:?}..{:?}",
        plan.clamp_min, plan.clamp_max
    );
    println!("snapshot bin: {}", plan.snapshot_bin);
}
