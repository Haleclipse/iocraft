//! Demonstrates CC Ink-style App-level fullscreen selection event wiring.
//!
//! Run with:
//!
//! ```text
//! cargo run --example fullscreen_selection_events
//! ```
//!
//! Drag over text to select. Click the linked label to see `View` click
//! handling run before selection link fallback, so the selection hook observes
//! the consumed click and suppresses the fallback URL, matching CC Ink's
//! `dispatchClick(...)` + selection release boundary.

use iocraft::prelude::*;

#[component]
fn Demo(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let mut app = hooks.use_app();
    let (terminal_width, terminal_height) = hooks.use_terminal_size();
    let selection = create_selection_context(&mut hooks);
    let mut clicks = hooks.use_state(|| 0usize);
    let release_status = hooks.use_state(|| "release: none".to_string());
    let last_copy = hooks.use_state(|| "copy-on-select: none".to_string());

    hooks.use_terminal_events(move |event| {
        if matches!(
            event,
            TerminalEvent::Key(KeyEvent {
                code: KeyCode::Char('q'),
                kind: KeyEventKind::Press,
                modifiers,
                ..
            }) if modifiers.is_empty()
        ) {
            app.exit();
        }
    });

    let mut release_status_for_hook = release_status;
    hooks.use_fullscreen_selection_events(selection, true, move |outcome| {
        if let FullscreenSelectionDispatchOutcome::Mouse(
            FullscreenSelectionEventOutcome::Release(release),
        ) = outcome
        {
            let link = release
                .hyperlink
                .unwrap_or_else(|| "<suppressed or none>".to_string());
            release_status_for_hook.set(format!(
                "release: dragging={} click={} link={link}",
                release.was_dragging,
                release.click.is_some()
            ));
        }
    });

    let mut last_copy_for_hook = last_copy;
    hooks.use_copy_on_select_text(selection, true, move |text| {
        last_copy_for_hook.set(format!("copy-on-select: {text:?}"));
    });
    hooks.use_selection_bg_color(selection, Color::DarkBlue);
    hooks.use_selection_overlay(selection);

    element! {
        View(
            width: terminal_width,
            height: terminal_height,
            flex_direction: FlexDirection::Column,
            padding: 1,
            row_gap: 1,
        ) {
            Text(content: "Fullscreen selection event hook demo · drag text · click docs · q quits")
            View(
                width: 18,
                height: 1,
                on_click: move |_| clicks += 1,
            ) {
                Link(
                    url: "https://example.com/docs".to_string(),
                    label: Some("clickable docs".to_string()),
                )
            }
            Text(content: "Selectable body text: the quick brown fox jumps over the lazy dog.")
            Text(content: format!("view clicks={}", clicks.get()), color: Color::Green)
            Text(content: release_status.read().clone(), color: Color::Yellow)
            Text(content: last_copy.read().clone(), color: Color::Cyan)
        }
    }
}

fn main() -> std::io::Result<()> {
    smol::block_on(element!(Demo).fullscreen())
}
