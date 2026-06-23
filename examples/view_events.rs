//! Demonstrates CC Ink-style `View` mouse handlers.
//!
//! Runs inside `AlternateScreen` so terminal mouse tracking is enabled. Move the
//! pointer over the box or click/release it. Press `q` to quit.

use iocraft::prelude::*;

#[component]
fn ViewEventsDemo(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let mut app = hooks.use_app();
    let mut clicked = hooks.use_state(|| 0usize);
    let mut last_click = hooks.use_state(|| None::<(u16, u16, bool, bool)>);
    let mut resize = hooks.use_state(|| None::<(u16, u16)>);
    let mut hovered = hooks.use_state(|| false);

    hooks.use_keybinding("q", move || app.exit());

    element! {
        AlternateScreen {
            View(
                width: 100pct,
                height: 100pct,
                flex_direction: FlexDirection::Column,
                justify_content: JustifyContent::CENTER,
                align_items: AlignItems::CENTER,
            ) {
                Text(content: "Move/click the box · q quits")
                Newline
                View(
                    width: 32,
                    height: 5,
                    border_style: BorderStyle::Round,
                    border_color: if hovered.get() { Color::Green } else { Color::DarkGrey },
                    background_color: if hovered.get() { Some(Color::DarkBlue) } else { None },
                    justify_content: JustifyContent::CENTER,
                    align_items: AlignItems::CENTER,
                    on_mouse_enter: move |_| hovered.set(true),
                    on_mouse_leave: move |_| hovered.set(false),
                    on_click: move |event: ViewClickEvent| {
                        clicked += 1;
                        last_click.set(Some((
                            event.local_column,
                            event.local_row,
                            event.target != event.current_target,
                            event.cell_is_blank,
                        )));
                    },
                    on_resize: move |event: ViewResizeEvent| {
                        resize.set(Some((event.columns, event.rows)));
                    },
                ) {
                    View(width: 24, justify_content: JustifyContent::CENTER) {
                        Text(content: "child target area")
                    }
                    Text(content: format!(
                        "hover={} clicks={} last=(col,row,bubbled,blank) {:?}",
                        hovered.get(),
                        clicked.get(),
                        last_click.get()
                    ))
                    Text(content: format!("resize={:?}", resize.get()))
                }
            }
        }
    }
}

fn main() -> std::io::Result<()> {
    let mut app = element!(ViewEventsDemo);
    smol::block_on(app.render_loop())
}
