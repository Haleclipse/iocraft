//! Demonstrates the post-render style overlay cursor in TextInput.
//!
//! The cursor is rendered as an inverted character (SGR Reverse) via the
//! overlay system — no background-color View hack. This works correctly
//! with wide characters (CJK, emoji) and automatically appears/disappears
//! as focus moves between fields.
//!
//! Tab to switch fields, type to edit, Ctrl+C to exit.

use iocraft::prelude::*;

#[derive(Default, Props)]
struct FieldProps {
    label: String,
    placeholder: String,
    value: Option<State<String>>,
    has_focus: bool,
}

#[component]
fn Field(props: &FieldProps) -> impl Into<AnyElement<'static>> {
    let Some(mut value) = props.value else {
        panic!("value is required");
    };

    element! {
        View(
            border_style: if props.has_focus { BorderStyle::Round } else { BorderStyle::Single },
            border_color: if props.has_focus { Color::Cyan } else { Color::DarkGrey },
        ) {
            View(width: 12) {
                Text(content: format!("{}: ", props.label), color: if props.has_focus { Color::Cyan } else { Color::Grey })
            }
            View(width: 30) {
                #(if value.read().is_empty() && !props.has_focus {
                    element! { Text(content: &props.placeholder, color: Color::DarkGrey) }.into_any()
                } else {
                    element! {
                        TextInput(
                            has_focus: props.has_focus,
                            value: value.to_string(),
                            on_change: move |v| value.set(v),
                        )
                    }.into_any()
                })
            }
        }
    }
}

#[component]
fn App(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let name = hooks.use_state(String::new);
    let email = hooks.use_state(String::new);
    let note = hooks.use_state(String::new);
    let mut focus = hooks.use_state(|| 0u8);

    hooks.use_terminal_events(move |event| {
        if let TerminalEvent::Key(KeyEvent { code, kind, .. }) = event {
            if kind == KeyEventKind::Release {
                return;
            }
            match code {
                KeyCode::Tab => focus.set((focus.get() + 1) % 3),
                KeyCode::BackTab => focus.set((focus.get() + 2) % 3),
                _ => {}
            }
        }
    });

    element! {
        View(flex_direction: FlexDirection::Column, padding: 2) {
            Text(content: "Cursor Overlay Demo", weight: Weight::Bold)
            Text(content: "Tab to switch fields. Try typing CJK or emoji.", color: Color::Grey)
            View(flex_direction: FlexDirection::Column, margin_top: 1, width: 50) {
                Field(label: "Name", placeholder: "e.g. 太郎", value: name, has_focus: focus == 0)
                Field(label: "Email", placeholder: "user@example.com", value: email, has_focus: focus == 1)
                Field(label: "Note", placeholder: "🎉 try emoji here", value: note, has_focus: focus == 2)
            }
        }
    }
}

fn main() {
    smol::block_on(element!(App).render_loop()).unwrap();
}
