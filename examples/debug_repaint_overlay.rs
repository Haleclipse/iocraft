//! Demonstrates retained-canvas debug repaint visualization.
//!
//! `Canvas::debug_repaint_overlay` clones the next canvas and overlays cells
//! that changed or were explicitly damaged. It is useful for tests and benchmark
//! harnesses that need to see what a sparse renderer would repaint.

use iocraft::prelude::*;

fn main() {
    let mut previous = Canvas::new(16, 1);
    previous.subview_mut(0, 0, 0, 0, 16, 1).set_text(
        0,
        0,
        "hello world",
        CanvasTextStyle::default(),
    );

    let mut next = Canvas::new(16, 1);
    next.subview_mut(0, 0, 0, 0, 16, 1)
        .set_text(0, 0, "hello rust", CanvasTextStyle::default());
    next.mark_damage(DamageRegion {
        x: 12,
        y: 0,
        width: 2,
        height: 1,
    });

    let visualized = Canvas::debug_repaint_overlay(
        Some(&previous),
        &next,
        StyleOverlay::selection_background(Color::Yellow),
    );

    visualized.write_ansi(std::io::stdout()).unwrap();
}
