//! Demonstrates interval and animation-frame hooks.
//!
//! `use_interval` runs recurring callbacks. `use_animation_timer` and
//! `use_animation_frame` use a CC Ink-style shared animation clock;
//! `use_animation_frame` also pauses when the component is outside the terminal
//! viewport or slows when terminal focus is lost.

use iocraft::prelude::*;
use std::time::Duration;

#[component]
fn IntervalDemo(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let mut app = hooks.use_app();
    let count = hooks.use_state(|| 0u8);
    let mut count_for_interval = count;

    hooks.use_interval(
        move || {
            count_for_interval += 1;
        },
        Some(Duration::from_millis(120)),
    );

    let timer = hooks.use_animation_timer(Duration::from_millis(120));
    let frame = hooks.use_animation_frame(Some(Duration::from_millis(120)));
    if count.get() >= 3 {
        app.exit();
    }

    element! {
        View(flex_direction: FlexDirection::Column) {
            Text(content: format!("interval ticks={}", count.get()))
            Text(content: format!("shared timer={}ms", timer))
            Text(content: format!(
                "animation visible={} time={}ms",
                frame.viewport.is_visible,
                frame.time_ms,
            ))
            Text(content: format!("default frame interval={}ms", FRAME_INTERVAL.as_millis()))
        }
    }
}

fn main() -> std::io::Result<()> {
    let mut app = element!(IntervalDemo);
    smol::block_on(app.render_loop())
}
