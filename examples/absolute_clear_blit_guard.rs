//! Demonstrates the retained-canvas absolute-clear blit guard.
//!
//! CC Ink's output layer skips prev-screen blits for rows covered by a removed
//! absolute-positioned node's clear. Otherwise a clean sibling blit can restore
//! stale overlay/menu pixels from the previous frame. iocraft exposes the same
//! mode-neutral building block on `Canvas` for custom retained renderers.

use iocraft::prelude::*;

fn main() {
    let mut prev = Canvas::new(12, 4);
    prev.subview_mut(0, 0, 0, 0, 12, 4)
        .set_text(0, 0, "stable top", CanvasTextStyle::default());
    prev.subview_mut(0, 0, 0, 0, 12, 4)
        .set_text(0, 2, "old overlay", CanvasTextStyle::default());

    let mut next = Canvas::new(12, 4);
    let absolute_clear = DamageRegion {
        x: 0,
        y: 2,
        width: 12,
        height: 1,
    };

    // Apply the clear/damage for the removed absolute node, then blit stable
    // content while excluding the contaminated row.
    next.clear_region(
        absolute_clear.x,
        absolute_clear.y,
        absolute_clear.width,
        absolute_clear.height,
    );
    next.blit_region_from_excluding_clears(&prev, 0, 0, 12, 4, &[absolute_clear]);

    println!("{}", next.get_text(0, 0, 12, 4));
}
