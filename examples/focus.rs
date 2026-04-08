//! Demonstrates the `FocusScope` + `use_focus` + `use_focus_manager` API.
//!
//! Compare this with `examples/form.rs`, which manually tracks a `State<u32>` index and
//! hand-rolls Tab / Shift+Tab cycling. This example does the same job declaratively: each
//! input registers itself with `use_focus`, and the surrounding `FocusScope` handles
//! Tab navigation automatically.
//!
//! Keys:
//!   - Tab / Shift+Tab : cycle through fields
//!   - Enter on Submit : exit
//!   - Esc             : clear focus
//!   - q (no focus)    : quit

use iocraft::prelude::*;

#[derive(Default, Props)]
struct FieldProps {
    label: String,
    value: Option<State<String>>,
    auto_focus: bool,
}

#[component]
fn Field(mut hooks: Hooks, props: &FieldProps) -> impl Into<AnyElement<'static>> {
    // Each Field registers itself as a focusable. The first field passes auto_focus = true,
    // so it grabs focus on initial mount.
    let focus = hooks.use_focus(if props.auto_focus {
        FocusOptions::new().auto_focus()
    } else {
        FocusOptions::new()
    });
    let mut value = props.value.expect("value required");
    let has_focus = focus.is_focused();

    element! {
        View(
            border_style: if has_focus { BorderStyle::Round } else { BorderStyle::None },
            border_color: Color::Blue,
            padding: if has_focus { 0 } else { 1 },
        ) {
            View(width: 15) {
                Text(content: format!("{}: ", props.label))
            }
            View(background_color: Color::DarkGrey, width: 30, height: 1) {
                TextInput(
                    has_focus: has_focus,
                    value: value.to_string(),
                    on_change: move |new_value| value.set(new_value),
                )
            }
        }
    }
}

#[component]
fn Submit(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let focus = hooks.use_focus(FocusOptions::new());
    let mut system = hooks.use_context_mut::<SystemContext>();
    let mut should_submit = hooks.use_state(|| false);
    let has_focus = focus.is_focused();

    // We listen for Enter/Space ourselves only when focused. Note that no manual focus
    // index tracking is needed — `focus.is_focused()` is the single source of truth.
    // We can't move `system` (RefMut) into the Send closure, so flip a state flag instead
    // and act on it in the next render — the standard iocraft idiom.
    hooks.use_terminal_events(move |event| {
        if !has_focus {
            return;
        }
        if let TerminalEvent::Key(KeyEvent {
            code,
            kind: KeyEventKind::Press,
            ..
        }) = event
        {
            if matches!(code, KeyCode::Enter | KeyCode::Char(' ')) {
                should_submit.set(true);
            }
        }
    });

    if should_submit.get() {
        system.exit();
    }

    element! {
        View(
            border_style: if has_focus { BorderStyle::Round } else { BorderStyle::None },
            border_color: Color::Green,
            padding: if has_focus { 0 } else { 1 },
        ) {
            Text(content: "Submit", color: Color::White, weight: Weight::Bold)
        }
    }
}

#[component]
fn FocusBar(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    // Demonstrates the imperative side: a non-focusable status bar that can drive focus
    // programmatically via the FocusManager handle.
    let manager = hooks.use_focus_manager();
    let active_label = match manager.active() {
        Some(id) => format!("active id = {}", id.as_u64()),
        None => "no focus".to_string(),
    };
    element! {
        Text(content: format!("[{}]", active_label), color: Color::Grey)
    }
}

#[component]
fn Form(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let first = hooks.use_state(String::new);
    let last = hooks.use_state(String::new);
    let bio = hooks.use_state(String::new);

    element! {
        View(
            flex_direction: FlexDirection::Column,
            align_items: AlignItems::Center,
            margin: 2,
        ) {
            View(flex_direction: FlexDirection::Column, align_items: AlignItems::Center, margin_bottom: 1) {
                Text(content: "What's your name?", color: Color::White, weight: Weight::Bold)
                Text(content: "Tab cycles fields. Enter on Submit to exit.", color: Color::Grey)
            }
            FocusScope {
                View(flex_direction: FlexDirection::Column, align_items: AlignItems::Center) {
                    Field(label: "First Name".to_string(), value: first, auto_focus: true)
                    Field(label: "Last Name".to_string(), value: last)
                    Field(label: "Life Story".to_string(), value: bio)
                    Submit
                    FocusBar
                }
            }
        }
    }
}

fn main() {
    smol::block_on(element! { Form }.render_loop()).unwrap();
}
