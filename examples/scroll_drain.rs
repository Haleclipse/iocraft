//! Demonstrates CC Ink-style render-time scroll drain helpers.
//!
//! These helpers are mode-neutral: they compute how much of a pending scroll
//! delta a custom scroll container should apply on this frame. They do not emit
//! DECSTBM, mutate terminal state, or schedule renders.

use iocraft::prelude::*;

fn main() {
    let pending = 40;
    let viewport_height = 10;

    for mode in [ScrollDrainMode::Native, ScrollDrainMode::XtermJs] {
        let step = drain_scroll_delta(mode, pending, viewport_height);
        println!(
            "{mode:?}: apply {} rows now, keep {} pending",
            step.applied, step.remaining
        );
    }

    // Stateful form: a custom scroll container can keep this value in its own
    // component state and explicitly wake another render frame while pending.
    let mut state = ScrollDrainState::with_pending(ScrollDrainMode::XtermJs, pending);
    while state.has_pending_delta() {
        let step = state.drain_frame(viewport_height);
        println!(
            "stateful xterm.js frame: apply {}, remaining {}",
            step.applied,
            state.pending_delta()
        );
    }
}
