//! Demonstrates planning a CC Ink-style retained scroll fast path.
//!
//! This is an optimization-only helper for custom renderers. The plan tells a
//! renderer which viewport to blit, which edge rows to repaint after a shift,
//! and which stable rows need repair because previous absolute-overlay pixels
//! would otherwise be shifted into stale positions. It does not write terminal
//! output or enable DECSTBM by itself.

use iocraft::prelude::*;

fn main() {
    let viewport = CachedClearRegion {
        x: 0,
        y: 4,
        width: 80,
        height: 10,
    };
    let delta = 3;
    let previous_absolute_overlay = CachedClearRegion {
        x: 10,
        y: 8,
        width: 20,
        height: 2,
    };

    if !is_scroll_fast_path_content_delta_safe(delta, 0) {
        println!("fall back to a full viewport render");
        return;
    }

    let Some(plan) = plan_scroll_fast_path(viewport, delta, [previous_absolute_overlay]) else {
        println!("shift too large for the retained scroll fast path");
        return;
    };

    println!("blit {:?}, shift by {} rows", plan.blit_region, plan.delta);
    println!("repaint edge rows: {:?}", plan.edge_region);
    println!(
        "repair rows contaminated by shifted absolute overlays: {:?}",
        plan.absolute_repair_regions
    );

    let previous = Canvas::new(80, 20);
    let mut next = Canvas::new(80, 20);
    if apply_scroll_fast_path_to_canvas(&mut next, &previous, &plan) {
        println!("previous viewport was blitted+shifted; repaint edge/repair rows next");
        println!("fullscreen scroll hint metadata: {:?}", next.scroll_hint());
    }
}
