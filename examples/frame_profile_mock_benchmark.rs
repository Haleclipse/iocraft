//! Deterministic mock-terminal frame profiling for benchmark harnesses.
//!
//! This avoids requiring a real TTY: the mock render loop produces canvases and
//! emits the same `RenderFrameProfile` events as `render_loop().on_frame_profile`.
//! It is useful when comparing retained-canvas optimizations in CI.

use futures::StreamExt;
use iocraft::prelude::*;
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

#[component]
fn BenchApp(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let mut system = hooks.use_context_mut::<SystemContext>();
    let mut tick = hooks.use_state(|| 0usize);

    hooks.use_future(async move {
        for _ in 0..8 {
            smol::Timer::after(Duration::from_millis(1)).await;
            tick += 1;
        }
    });

    if tick >= 8 {
        system.exit();
    }

    let lines = (0..20)
        .map(|i| format!("row {i:02} tick {tick}"))
        .collect::<Vec<_>>()
        .join("\n");

    element! {
        View(width: 32, height: 8) {
            ScrollView(scrollbar: Some(false)) {
                Text(content: lines)
            }
        }
    }
}

fn main() {
    let stats = Arc::new(Mutex::new(RenderFrameProfileStats::default()));
    let stats_for_callback = stats.clone();

    let frames = smol::block_on(
        element!(BenchApp)
            .mock_terminal_render_loop_with_profile(MockTerminalConfig::default(), move |event| {
                stats_for_callback.lock().unwrap().record(&event);
            })
            .collect::<Vec<_>>(),
    );

    let stats = stats.lock().unwrap();
    println!(
        "frames={} canvases={} repaint_ratio={:.2} avg_frame={:?} avg_changed_cells={:.1} max_changed_cells={}",
        stats.frames,
        frames.len(),
        stats.repaint_ratio(),
        stats.average_duration(),
        stats.average_changed_cells(),
        stats.max_changed_cells,
    );
}
