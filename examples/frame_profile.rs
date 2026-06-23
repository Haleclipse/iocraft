//! Demonstrates opt-in render-loop frame profiling.
//!
//! This mirrors CC Ink's `onFrame` profiling shape in a Rust-native way: the
//! callback receives timings, retained-canvas change counts, and repaint reasons
//! but iocraft does not log or collect analytics by default.

use iocraft::prelude::*;
use std::sync::{Arc, Mutex};

#[component]
fn App(hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let mut system = hooks.use_context_mut::<SystemContext>();
    system.exit();

    element! {
        View(border_style: BorderStyle::Round, border_color: Color::Blue) {
            Text(content: "profiled frame")
        }
    }
}

fn main() -> std::io::Result<()> {
    let stats = Arc::new(Mutex::new(RenderFrameProfileStats::default()));
    let stats_for_callback = stats.clone();

    smol::block_on(element!(App).render_loop().on_frame_profile(move |event| {
        eprintln!(
            "frame {:?}: update={:?} layout={:?} draw={:?} write={:?} changed_cells={} repaint={:?}",
            event.duration,
            event.phases.update,
            event.phases.layout,
            event.phases.draw,
            event.phases.terminal_write,
            event.phases.changed_cells,
            event.repaint.as_ref().map(|repaint| repaint.reason),
        );
        stats_for_callback.lock().unwrap().record(&event);
    }))?;

    let stats = stats.lock().unwrap();
    eprintln!(
        "summary: frames={} repaint_ratio={:.2} avg_frame={:?} max_changed_cells={}",
        stats.frames,
        stats.repaint_ratio(),
        stats.average_duration(),
        stats.max_changed_cells,
    );

    Ok(())
}
