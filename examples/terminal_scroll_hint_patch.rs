//! Demonstrates fullscreen DECSTBM scroll-hint patch serialization.
//!
//! This is a low-level, opt-in helper for custom fullscreen renderers. It only
//! serializes the same guarded `DECSTBM + SU/SD + reset + home` sequence CC Ink
//! emits before its sparse diff; it does not enter fullscreen, start
//! synchronized output, or write to the terminal by itself.

use iocraft::prelude::*;

fn main() {
    let hint = ScrollHint {
        top: 2,
        bottom: 8,
        delta: 3,
    };
    let bounds = TerminalScrollHintBounds {
        previous_screen_height: 12,
        next_screen_height: 12,
    };

    let options = TerminalScrollHintPatchOptions::fullscreen_synchronized();
    match plan_terminal_scroll_hint_patch(hint, bounds, options) {
        Ok(TerminalScrollHintPatchPlan::Emit(sequence)) => {
            println!("serialized scroll patch: {:?}", sequence);
        }
        Ok(TerminalScrollHintPatchPlan::Skip(reason)) => {
            println!("fall back to a normal diff because DECSTBM is unsafe: {reason:?}");
        }
        Err(reason) => {
            println!("fall back to a normal full-region diff: {reason:?}");
        }
    }
}
