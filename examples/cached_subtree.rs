//! Demonstrates `CachedSubtree`, an explicit clean-subtree blit cache.
//!
//! The expensive block is rendered once for a cache key, then restored from the
//! retained canvas on later frames. Press `r` to change the key and re-render;
//! press `q` to quit.

use iocraft::prelude::*;

#[component]
fn ExpensiveBlock(_hooks: Hooks, props: &ExpensiveBlockProps) -> impl Into<AnyElement<'static>> {
    let lines = (0..8)
        .map(|i| format!("cached row {i} · revision {}", props.revision))
        .collect::<Vec<_>>()
        .join("\n");
    element! {
        View(border_style: BorderStyle::Round, border_color: Color::Blue, padding: 1) {
            Text(content: lines)
        }
    }
}

#[derive(Default, Props)]
struct ExpensiveBlockProps {
    revision: u32,
}

#[component]
fn CachedSubtreeDemo(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let mut app = hooks.use_app();
    let mut ticks = hooks.use_state(|| 0u32);
    let mut revision = hooks.use_state(|| 0u32);

    hooks.use_interval(
        move || {
            ticks += 1;
        },
        Some(std::time::Duration::from_millis(250)),
    );
    hooks.use_input(move |input, _key| {
        if input == "q" {
            app.exit();
        } else if input == "r" {
            revision += 1;
        }
    });

    element! {
        View(width: 48, flex_direction: FlexDirection::Column) {
            Text(content: format!("ticks={} · revision={} · r re-renders · q quits", ticks.get(), revision.get()))
            CachedSubtree(cache_key: format!("revision-{}", revision.get())) {
                ExpensiveBlock(revision: revision.get())
            }
        }
    }
}

fn main() -> std::io::Result<()> {
    let mut app = element!(CachedSubtreeDemo);
    smol::block_on(app.render_loop())
}
