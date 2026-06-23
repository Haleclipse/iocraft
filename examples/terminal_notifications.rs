//! Demonstrates CC Ink-style side-band terminal notifications.
//!
//! Keys:
//! - `i`: iTerm2 OSC 9 notification
//! - `k`: Kitty OSC 99 notification
//! - `g`: Ghostty OSC 777 notification
//! - `b`: BEL fallback
//! - `p`: indeterminate OSC 9;4 progress (if supported)
//! - `c`: clear progress
//! - `1`/`2`/`3`: set idle/busy/waiting OSC 21337 tab status
//! - `0`: clear tab status
//! - `q`: quit

use iocraft::prelude::*;
use std::io;

#[component]
fn TerminalNotificationsDemo(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let mut app = hooks.use_app();
    let terminal = hooks.use_terminal_notification();
    let last = hooks.use_state(|| "press a key".to_string());
    let tab_status = hooks.use_state(|| None::<TabStatusKind>);
    hooks.use_tab_status(tab_status.get());

    hooks.use_keybinding("q", move || app.exit());

    let terminal_iterm = terminal.clone();
    let mut last_iterm = last;
    hooks.use_keybinding("i", move || {
        terminal_iterm.notify_iterm2("Build finished", Some("iocraft"));
        last_iterm.set("sent iTerm2 OSC 9 notification".to_string());
    });

    let terminal_kitty = terminal.clone();
    let mut last_kitty = last;
    hooks.use_keybinding("k", move || {
        terminal_kitty.notify_kitty("Background task finished", "iocraft", 42);
        last_kitty.set("sent Kitty OSC 99 notification".to_string());
    });

    let terminal_ghostty = terminal.clone();
    let mut last_ghostty = last;
    hooks.use_keybinding("g", move || {
        terminal_ghostty.notify_ghostty("Background task finished", "iocraft");
        last_ghostty.set("sent Ghostty OSC 777 notification".to_string());
    });

    let terminal_bell = terminal.clone();
    let mut last_bell = last;
    hooks.use_keybinding("b", move || {
        terminal_bell.notify_bell();
        last_bell.set("sent BEL".to_string());
    });

    let terminal_progress = terminal.clone();
    let mut last_progress = last;
    hooks.use_keybinding("p", move || {
        terminal_progress.progress(Some(TerminalProgressState::Indeterminate), None);
        last_progress.set("requested indeterminate OSC 9;4 progress".to_string());
    });

    let terminal_clear = terminal.clone();
    let mut last_clear = last;
    hooks.use_keybinding("c", move || {
        terminal_clear.progress(None, None);
        last_clear.set("cleared OSC 9;4 progress".to_string());
    });

    let mut idle_status = tab_status;
    let mut idle_last = last;
    hooks.use_keybinding("1", move || {
        idle_status.set(Some(TabStatusKind::Idle));
        idle_last.set("set idle tab status".to_string());
    });
    let mut busy_status = tab_status;
    let mut busy_last = last;
    hooks.use_keybinding("2", move || {
        busy_status.set(Some(TabStatusKind::Busy));
        busy_last.set("set busy tab status".to_string());
    });
    let mut waiting_status = tab_status;
    let mut waiting_last = last;
    hooks.use_keybinding("3", move || {
        waiting_status.set(Some(TabStatusKind::Waiting));
        waiting_last.set("set waiting tab status".to_string());
    });
    let mut clear_status = tab_status;
    let mut clear_last = last;
    hooks.use_keybinding("0", move || {
        clear_status.set(None);
        clear_last.set("cleared tab status".to_string());
    });

    element! {
        View(width: 80, flex_direction: FlexDirection::Column, padding: 1) {
            Text(content: "Terminal notifications", weight: Weight::Bold, color: Color::Cyan)
            Text(content: "i iTerm2 · k Kitty · g Ghostty · b bell · p progress · c clear · 1/2/3/0 tab status · q quit")
            Text(content: format!("last: {}", &*last.read()), color: Color::Grey)
            Text(content: format!("progress OSC 9;4 available: {}", is_progress_reporting_available()))
        }
    }
}

fn main() -> io::Result<()> {
    smol::block_on(element!(TerminalNotificationsDemo).render_loop())
}
