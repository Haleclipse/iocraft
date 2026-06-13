//! Demonstrates `StaticOutput` — permanent log output above a live progress bar.
//!
//! Completed tasks scroll into the terminal history while the active task and
//! progress bar continue rendering in the live area below.

use iocraft::prelude::*;
use std::time::Duration;

const TASKS: &[&str] = &[
    "Compiling core",
    "Compiling utils",
    "Compiling renderer",
    "Compiling layout",
    "Linking",
    "Optimizing",
    "Done",
];

#[component]
fn Build(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let mut system = hooks.use_context_mut::<SystemContext>();
    let mut completed = hooks.use_state(Vec::<String>::new);
    let mut current = hooks.use_state(|| 0usize);
    let mut progress = hooks.use_state(|| 0.0f32);

    hooks.use_future(async move {
        loop {
            smol::Timer::after(Duration::from_millis(80)).await;
            progress.set((progress.get() + 5.0).min(100.0));
            if progress.get() >= 100.0 {
                let idx = current.get();
                if idx < TASKS.len() {
                    completed.write().push(format!("  ✓ {}", TASKS[idx]));
                    current.set(idx + 1);
                    progress.set(0.0);
                }
            }
        }
    });

    if current.get() >= TASKS.len() {
        system.exit();
    }

    let task_name = TASKS.get(current.get()).unwrap_or(&"");

    element! {
        View(flex_direction: FlexDirection::Column) {
            StaticOutput(items: completed.read().clone())
            View(padding: 1, flex_direction: FlexDirection::Column) {
                Text(content: format!("  Building: {task_name}"), color: Color::Yellow, weight: Weight::Bold)
                View(border_style: BorderStyle::Round, border_color: Color::Blue, width: 40) {
                    View(width: Percent(progress.get()), height: 1, background_color: Color::Green)
                }
                Text(content: format!("  {:.0}%", progress.get()), color: Color::Grey)
            }
        }
    }
}

fn main() {
    smol::block_on(element!(Build).render_loop()).unwrap();
}
