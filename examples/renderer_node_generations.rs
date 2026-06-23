//! Demonstrates generation-stamped retained renderer node IDs.
//!
//! CC Ink can cache by DOM object identity. Custom Rust retained renderers often
//! start with logical keys, so this helper adds a generation that changes on
//! removal/remount and prevents stale layout blits after key reuse.

use iocraft::prelude::*;

fn main() {
    let mut generations = RendererNodeGenerationState::<&'static str>::new();
    let mut cache = RendererNodeCache::<RendererStableNodeId<&'static str>>::new();

    let row0 = generations.current_id("row-42");
    let layout = CachedLayoutBounds {
        x: 0,
        y: 4,
        width: 80,
        height: 1,
        top: Some(4),
    };
    cache.set_layout(row0.clone(), layout);
    println!(
        "first id: {row0:?}; can blit={}",
        cache.can_blit(&row0, layout)
    );

    let removed = generations.remove(&"row-42").unwrap();
    cache.remove_layout(&removed);
    let row1 = generations.current_id("row-42");
    println!(
        "reinserted id: {row1:?}; can blit={}",
        cache.can_blit(&row1, layout)
    );
}
