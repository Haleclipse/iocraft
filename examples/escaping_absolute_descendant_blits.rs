//! Demonstrates retained blit repair for absolute descendants.
//!
//! If a clean parent subtree is restored from the previous canvas, cached
//! absolute descendants that paint outside the parent's layout box also need to
//! be restored. This mirrors CC Ink's `blitEscapingAbsoluteDescendants(...)` as
//! opt-in planning plus canvas-application helpers for custom retained renderers.

use iocraft::prelude::*;

fn main() {
    let parent = CachedClearRegion {
        x: 10,
        y: 5,
        width: 8,
        height: 4,
    };

    let repairs = plan_escaping_absolute_descendant_blits(
        parent,
        [
            AbsoluteDescendantRect {
                key: "inside menu shadow",
                rect: CachedClearRegion {
                    x: 11,
                    y: 6,
                    width: 2,
                    height: 1,
                },
            },
            AbsoluteDescendantRect {
                key: "floating menu above parent",
                rect: CachedClearRegion {
                    x: 10,
                    y: 3,
                    width: 8,
                    height: 2,
                },
            },
        ],
    );

    for repair in &repairs {
        println!("{} -> blit {:?}", repair.key, repair.rect);
    }

    let mut previous = Canvas::new(24, 8);
    previous
        .subview_mut(0, 0, 0, 0, 24, 8)
        .set_text(10, 3, "floating", CanvasTextStyle::default());
    previous.clear_damage();

    let mut next = Canvas::new(24, 8);
    let applied = apply_escaping_absolute_descendant_blits_to_canvas(&mut next, &previous, repairs);
    println!("canvas repairs applied: {:?}", applied);
}
