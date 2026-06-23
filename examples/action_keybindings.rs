//! Demonstrates CC-style action keybindings with contexts and chords.
//!
//! `KeybindingProvider` maps keystrokes to action names. Components register
//! action handlers with `use_action_keybinding*`, while active contexts can
//! override Global bindings. Chords such as `ctrl+x ctrl+k` are handled by the
//! provider and consume their prefix while waiting for the second key.

use iocraft::prelude::*;
use std::io;

#[component]
fn ActionKeybindingDemo(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let mut app = hooks.use_app();
    let keybindings = hooks.use_keybinding_context();
    let mut palette_active = hooks.use_state(|| false);
    let log = hooks.use_state(|| vec!["Ready.".to_string()]);

    hooks.use_register_keybinding_context("Palette", palette_active.get());

    hooks.use_action_keybinding("app:quit", move || app.exit());
    hooks.use_action_keybinding("app:togglePalette", {
        let mut log = log;
        move || {
            let next = !palette_active.get();
            palette_active.set(next);
            log.write().push(format!("palette active = {next}"));
        }
    });
    hooks.use_action_keybinding_with_options(
        "palette:choose",
        ActionKeybindingOptions {
            context: "Palette".to_string(),
            active: true,
        },
        {
            let mut log = log;
            move || {
                log.write().push("palette chose item".to_string());
                true
            }
        },
    );
    hooks.use_action_keybinding_with_options(
        "chat:killAgents",
        ActionKeybindingOptions {
            context: "Chat".to_string(),
            active: true,
        },
        {
            let mut log = log;
            move || {
                log.write().push("ran ctrl+x ctrl+k chord".to_string());
                true
            }
        },
    );

    let toggle = keybindings
        .display_text("app:togglePalette", "Global")
        .unwrap_or_else(|| "?".to_string());
    let choose = keybindings
        .display_text("palette:choose", "Palette")
        .unwrap_or_else(|| "?".to_string());
    let chord = keybindings
        .display_text("chat:killAgents", "Chat")
        .unwrap_or_else(|| "?".to_string());

    element! {
        View(width: 88, flex_direction: FlexDirection::Column, padding: 1) {
            Text(content: "Action keybindings", weight: Weight::Bold, color: Color::Cyan)
            Text(content: format!("{toggle} toggle Palette · {choose} choose while Palette active · {chord} chord · q quit"), color: Color::Grey)
            Text(content: format!("active contexts: {:?}", keybindings.active_contexts()))
            View(margin_top: 1, border_style: BorderStyle::Round, border_color: Color::DarkGrey, flex_direction: FlexDirection::Column) {
                #(log.read().iter().rev().take(6).rev().map(|entry| element! { Text(content: format!("  {entry}")) }))
            }
        }
    }
}

fn main() -> io::Result<()> {
    let bindings = vec![
        KeybindingBlock::new(
            "Global",
            vec![
                KeybindingEntry::new("q", "app:quit"),
                KeybindingEntry::new("ctrl+t", "app:togglePalette"),
            ],
        ),
        KeybindingBlock::new(
            "Palette",
            vec![KeybindingEntry::new("ctrl+t", "palette:choose")],
        ),
        KeybindingBlock::new(
            "Chat",
            vec![KeybindingEntry::new("ctrl+x ctrl+k", "chat:killAgents")],
        ),
    ];

    smol::block_on(
        element! {
            KeybindingProvider(bindings) {
                ActionKeybindingDemo
            }
        }
        .render_loop(),
    )
}
