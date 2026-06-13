//! Demonstrates event propagation and `trap_keys` — nested FocusScope isolation.
//!
//! The outer form has Tab navigation. Press Enter on "Open Modal" to spawn an
//! inner FocusScope with `trap_keys: true`. While the modal is open, Tab is
//! captured by the modal and the outer form does NOT advance. Press Esc to
//! close the modal.

use iocraft::prelude::*;

#[derive(Default, Props)]
struct FieldProps {
    label: String,
    auto_focus: bool,
}

#[component]
fn Field(mut hooks: Hooks, props: &FieldProps) -> impl Into<AnyElement<'static>> {
    let focus = hooks.use_focus(if props.auto_focus {
        FocusOptions::new().auto_focus()
    } else {
        FocusOptions::new()
    });
    let marker = if focus.is_focused() { ">" } else { " " };
    element! {
        Text(content: format!("{marker} {}", props.label),
             color: if focus.is_focused() { Color::Cyan } else { Color::White })
    }
}

#[component]
fn Modal(_hooks: Hooks) -> impl Into<AnyElement<'static>> {
    element! {
        FocusScope(trap_keys: Some(true)) {
            View(
                border_style: BorderStyle::Round,
                border_color: Color::Yellow,
                flex_direction: FlexDirection::Column,
                padding: 1,
                width: 30,
            ) {
                Text(content: "Modal (Tab trapped)", weight: Weight::Bold, color: Color::Yellow)
                Field(label: "Option A".to_string(), auto_focus: true)
                Field(label: "Option B".to_string())
                Field(label: "Option C".to_string())
                Text(content: "Esc to close", color: Color::Grey)
            }
        }
    }
}

#[component]
fn App(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let mut system = hooks.use_context_mut::<SystemContext>();
    let show_modal = hooks.use_state(|| false);
    let mut should_exit = hooks.use_state(|| false);

    hooks.use_keybinding("enter", {
        let mut show_modal = show_modal;
        move || show_modal.set(true)
    });
    hooks.use_keybinding("esc", {
        let mut show_modal = show_modal;
        move || {
            if show_modal.get() {
                show_modal.set(false);
            } else {
                should_exit.set(true);
            }
        }
    });

    if should_exit.get() {
        system.exit();
    }

    // The inner FocusScope (Modal) must be INSIDE the outer FocusScope for
    // trap_keys to work: children are polled before their parent's hooks, so a
    // nested trapping scope consumes Tab before the ancestor scope sees it.
    element! {
        View(flex_direction: FlexDirection::Column, padding: 1) {
            Text(content: "Event Propagation Demo", weight: Weight::Bold)
            Text(content: "Tab to navigate | Enter to open modal | Esc to close/quit", color: Color::Grey)
            FocusScope {
                View(flex_direction: FlexDirection::Column, margin_top: 1) {
                    Field(label: "Name".to_string(), auto_focus: true)
                    Field(label: "Email".to_string())
                    Field(label: "[Open Modal]".to_string())
                    #(if show_modal.get() {
                        Some(element! { Modal })
                    } else {
                        None
                    })
                }
            }
        }
    }
}

fn main() {
    smol::block_on(element!(App).render_loop()).unwrap();
}
