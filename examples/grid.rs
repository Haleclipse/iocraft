//! Demonstrates CSS Grid layout with named template areas.
//!
//! The dashboard below is defined entirely by `grid_template_areas` — each child
//! simply names the area it belongs to. Try resizing your terminal: the `1fr`
//! tracks flex while the fixed tracks keep their size.
//!
//! Press Ctrl+C to exit.

use iocraft::prelude::*;

#[derive(Default, Props)]
struct PanelProps {
    title: String,
    color: Option<Color>,
    area: String,
}

#[component]
fn Panel(props: &PanelProps) -> impl Into<AnyElement<'static>> {
    element! {
        View(
            grid_area: &*props.area,
            border_style: BorderStyle::Round,
            border_color: props.color.unwrap_or(Color::DarkGrey),
            padding_left: 1,
        ) {
            Text(content: &props.title, color: props.color, weight: Weight::Bold)
        }
    }
}

#[component]
fn Dashboard(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let (width, height) = hooks.use_terminal_size();

    element! {
        View(
            display: Display::Grid,
            grid_template_columns: "20 1fr 1fr",
            grid_template_rows: "3 1fr 1fr 3",
            grid_template_areas: r#"
                "header header header"
                "nav    main   main"
                "nav    stats  logs"
                "footer footer footer"
            "#,
            width: width.max(40),
            height: height.max(12),
        ) {
            Panel(area: "header", title: "iocraft grid dashboard", color: Color::Cyan)
            Panel(area: "nav",    title: "navigation",            color: Color::Blue)
            Panel(area: "main",   title: "main content",          color: Color::Green)
            Panel(area: "stats",  title: "stats",                 color: Color::Magenta)
            Panel(area: "logs",   title: "logs",                  color: Color::Yellow)
            Panel(area: "footer", title: "status: all systems go")
        }
    }
}

fn main() {
    smol::block_on(element!(Dashboard).fullscreen()).unwrap();
}
