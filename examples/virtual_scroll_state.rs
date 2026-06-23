//! Demonstrates the stateful CC Ink-style virtual-scroll helper.
//!
//! `VirtualScrollState` owns measured row heights by stable item key, scales
//! them on terminal-column changes, and freezes the previous range briefly so a
//! resize does not churn mounted rows while measurements settle.

use iocraft::prelude::*;

fn main() {
    let keys = (0..200).collect::<Vec<_>>();
    let mut state = VirtualScrollState::new();

    for key in &keys {
        state.set_height(*key, if key % 7 == 0 { 4 } else { 2 });
    }
    state.set_columns(100);

    let config = VirtualScrollConfig {
        overscan_rows: 8,
        max_mounted_items: 60,
        ..Default::default()
    };

    let first = state.plan(
        &keys,
        VirtualScrollInput {
            scroll_top: Some(120),
            viewport_height: 24,
            is_sticky: false,
            ..Default::default()
        },
        config,
    );
    println!("initial range: {}..{}", first.range.start, first.range.end);

    state.set_columns(50);
    if state.take_skip_measurement() {
        println!("skip one stale pre-resize measurement pass");
    }

    let resized = state.plan(
        &keys,
        VirtualScrollInput {
            scroll_top: Some(120),
            viewport_height: 24,
            is_sticky: false,
            ..Default::default()
        },
        config,
    );
    println!(
        "post-resize frozen range: {}..{} (remaining freeze passes: {})",
        resized.range.start,
        resized.range.end,
        state.freeze_remaining()
    );
    println!("scaled height for item 0: {:?}", state.height(&0));
}
