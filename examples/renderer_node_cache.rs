//! Demonstrates the CC Ink-style renderer node-cache helper.
//!
//! `RendererNodeCache` is mode-neutral retained-renderer metadata: it stores
//! previous node layout bounds, queues pending clear rectangles, drops culled
//! subtree cache entries, and exposes the one-shot absolute-removal contamination
//! flag used to decide whether a custom renderer may safely blit from a previous
//! screen buffer.

use iocraft::prelude::*;
use std::collections::HashMap;

fn main() {
    let mut cache = RendererNodeCache::new();
    let layout = CachedLayoutBounds {
        x: 2,
        y: 1,
        width: 20,
        height: 3,
        top: Some(1),
    };

    cache.set_layout("message-row", layout);
    println!(
        "can blit unchanged message-row: {}",
        cache.can_blit(&"message-row", layout)
    );

    cache.add_pending_clear("transcript", layout.into(), false);
    println!(
        "pending transcript clears: {:?}",
        cache.take_pending_clears(&"transcript")
    );

    let overlay_clear = CachedClearRegion {
        x: -4,
        y: 0,
        width: 40,
        height: 5,
    };
    println!(
        "visible clear region: {:?}",
        overlay_clear.clipped_to_canvas(80, 24)
    );

    cache.add_pending_clear("root", overlay_clear, true);
    println!(
        "absolute overlay removed; disable prev-screen blit this frame: {}",
        cache.consume_absolute_removed_flag()
    );

    cache.set_layout("branch", layout);
    cache.set_layout("leaf", layout);
    let children = HashMap::from([("branch", vec!["leaf"])]);
    cache.remove_subtree(&"branch", |node| {
        children.get(node).cloned().unwrap_or_default()
    });
    println!(
        "branch cache dropped after culling: {}",
        cache.layout(&"branch").is_none() && cache.layout(&"leaf").is_none()
    );
}
