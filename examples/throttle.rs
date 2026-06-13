//! Demonstrates frame throttling with `.max_fps()`.
//!
//! A counter increments as fast as possible via use_future. Without throttling,
//! every increment triggers a render (thousands of fps). With throttling, updates
//! coalesce into at most N frames per second, reducing CPU usage dramatically
//! while the counter still reaches the same final value.
//!
//! The example defaults to 10fps to make the coalescing visually obvious.
//! Press Ctrl+C to exit.

use iocraft::prelude::*;
use std::time::Duration;

#[component]
fn Counter(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let mut count = hooks.use_state(|| 0u64);
    let mut renders = hooks.use_state(|| 0u64);

    hooks.use_future(async move {
        loop {
            smol::Timer::after(Duration::from_millis(1)).await;
            count += 1;
            if count.get() >= 500 {
                break;
            }
        }
    });

    renders += 1;

    element! {
        View(flex_direction: FlexDirection::Column, padding: 1) {
            Text(content: "Frame Throttle Demo (10 fps)", weight: Weight::Bold, color: Color::Cyan)
            Text(content: format!("counter: {} / 500", count), color: Color::White)
            Text(content: format!("renders: {}", renders), color: Color::Yellow)
            Text(content: format!(
                "efficiency: {} updates coalesced per frame",
                if renders.get() > 0 { count.get() / renders.get() } else { 0 }
            ), color: Color::Grey)
        }
    }
}

fn main() {
    smol::block_on(element!(Counter).render_loop().max_fps(10)).unwrap();
}
