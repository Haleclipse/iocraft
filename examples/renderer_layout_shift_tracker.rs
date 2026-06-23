//! Demonstrates the explicit layout-shift tracker for retained renderers.
//!
//! CC Ink keeps a module-global `layoutShifted` flag to force a broad damage
//! backstop when node positions/sizes change. iocraft exposes the same idea as a
//! typed helper for custom renderers: you provide stable node keys and layout
//! snapshots, then decide how to invalidate your retained canvas.

use iocraft::prelude::*;

fn main() {
    let mut tracker = RendererLayoutShiftTracker::new();

    let first = [
        (
            "root",
            RendererLayoutSnapshot {
                x: 0,
                y: 0,
                width: 20,
                height: 4,
            },
        ),
        (
            "child",
            RendererLayoutSnapshot {
                x: 0,
                y: 1,
                width: 20,
                height: 1,
            },
        ),
    ];
    println!("first frame shifted: {}", tracker.update(first));

    let second = [
        (
            "root",
            RendererLayoutSnapshot {
                x: 0,
                y: 0,
                width: 20,
                height: 4,
            },
        ),
        (
            "child",
            RendererLayoutSnapshot {
                x: 0,
                y: 2,
                width: 20,
                height: 1,
            },
        ),
    ];
    println!("second frame shifted: {}", tracker.update(second));

    if tracker.snapshot(&"child").is_some() {
        println!("custom renderer should mark broad damage before unsafe blits");
    }
}
