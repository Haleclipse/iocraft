//! Demonstrates mode-neutral retained-canvas diffing.
//!
//! `Canvas::diff_each` mirrors CC Ink's `screen.diffEach` at the public helper
//! level: it compares retained cells plus post-render overlays without writing
//! to the terminal or changing screen mode. Custom renderers can feed these
//! changes into their own patch serializer.

use iocraft::prelude::*;

fn main() {
    let mut prev = Canvas::new(8, 2);
    prev.subview_mut(0, 0, 0, 0, 8, 2)
        .set_text(0, 0, "hello", CanvasTextStyle::default());
    prev.subview_mut(0, 0, 0, 0, 8, 2)
        .set_text(0, 1, "old", CanvasTextStyle::default());

    let mut next = Canvas::new(8, 1);
    next.subview_mut(0, 0, 0, 0, 8, 1)
        .set_text(0, 0, "hullo", CanvasTextStyle::default());
    next.set_overlay(1, 0, StyleOverlay::inverse());

    println!(
        "first changed column on row 0: {:?}",
        prev.row_change_start(&next, 0)
    );

    for change in prev.diff(&next) {
        let removed = change
            .removed
            .as_ref()
            .and_then(|cell| cell.cell.text())
            .unwrap_or("∅");
        let added = change
            .added
            .as_ref()
            .and_then(|cell| cell.cell.text())
            .unwrap_or("∅");
        println!(
            "({}, {}) {} -> {} overlay={:?}",
            change.x,
            change.y,
            removed,
            added,
            change.added.as_ref().and_then(|cell| cell.overlay)
        );
    }
}
