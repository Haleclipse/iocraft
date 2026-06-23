//! Demonstrates `use_input_event`, the propagation-aware variant of `use_input`.
//!
//! Type any character to let the parent handler see it. Type `.` to let the
//! child handler stop propagation before the parent sees the event. Press Esc to
//! exit.

use iocraft::prelude::*;

fn append_log(mut log: State<Vec<String>>, message: impl Into<String>) {
    let mut next = log.read().clone();
    next.push(message.into());
    if next.len() > 6 {
        next.remove(0);
    }
    log.set(next);
}

#[derive(Default, Props)]
struct ChildInputProps {
    handler: HandlerMut<'static, String>,
}

#[component]
fn ChildInput(mut hooks: Hooks, props: &mut ChildInputProps) -> impl Into<AnyElement<'static>> {
    let mut handler = props.handler.take();
    hooks.use_input_event(move |input, _key, event| {
        if input == "." {
            handler("child stopped '.'".to_string());
            event.stop_propagation();
        }
    });

    element! {
        View(border_style: BorderStyle::Round, padding_left: 1, padding_right: 1) {
            Text(content: "child: '.' stops propagation")
        }
    }
}

#[component]
fn UseInputEventDemo(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let mut app = hooks.use_app();
    let log = hooks.use_state(Vec::<String>::new);
    let log_for_parent = log;

    hooks.use_input_event(move |input, key, _event| {
        if key.escape {
            app.exit();
        } else if !input.is_empty() {
            append_log(log_for_parent, format!("parent saw {input:?}"));
        }
    });

    let log_for_child = log;
    let rows = log.read().clone();
    element! {
        View(width: 48, flex_direction: FlexDirection::Column) {
            Text(content: "use_input_event propagation demo · Esc exits")
            ChildInput(handler: move |message: String| append_log(log_for_child, message))
            Newline
            #(rows.into_iter().map(|row| element!(Text(content: row))))
        }
    }
}

fn main() -> std::io::Result<()> {
    let mut app = element!(UseInputEventDemo);
    smol::block_on(app.render_loop())
}
