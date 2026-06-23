//! Demonstrates retained node blit/clear planning for custom renderers.
//!
//! This is the opt-in, Rust-first equivalent of the first-stage decision in CC
//! Ink's `renderNodeToOutput(...)`: clean unchanged nodes can blit, dirty or
//! moved nodes clear their cached rectangle before rendering, and removed
//! children trigger a layout-shift backstop.

use iocraft::prelude::*;

fn main() {
    let cached = CachedLayoutBounds {
        x: 2,
        y: 1,
        width: 20,
        height: 3,
        top: Some(1),
    };
    let current = CachedLayoutBounds { y: 2, ..cached };
    let removed_child_clear = CachedClearRegion {
        x: 2,
        y: 4,
        width: 20,
        height: 1,
    };

    let plan = plan_retained_node_render(RetainedNodeRenderInput {
        current_layout: current,
        cached_layout: Some(cached),
        dirty: false,
        skip_self_blit: false,
        pending_scroll_delta: false,
        previous_screen_available: true,
        hidden: false,
        absolute: false,
        pending_clears: vec![removed_child_clear],
    });

    println!("action: {:?}", plan.action);
    println!("clear old region: {:?}", plan.clear_old_region);
    println!("pending child clears: {:?}", plan.pending_clear_regions);
    println!("layout shifted: {}", plan.layout_shifted);

    let mut previous = Canvas::new(24, 6);
    previous.subview_mut(0, 0, 0, 0, 24, 6).set_text(
        2,
        2,
        "cached node pixels",
        CanvasTextStyle::default(),
    );
    previous.clear_damage();
    let mut next = Canvas::new(24, 6);
    let applied = apply_retained_node_render_plan_to_canvas(&mut next, &previous, &plan);
    println!("canvas blitted region: {:?}", applied.blitted_region);
    println!(
        "canvas cleared old region: {:?}",
        applied.cleared_old_region
    );
    println!("canvas pending clears: {:?}", applied.pending_clear_regions);
}
