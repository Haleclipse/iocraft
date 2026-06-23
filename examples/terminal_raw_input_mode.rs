//! Demonstrates terminal-side mode sequences for an opt-in raw input backend.
//!
//! By default this does not enable OS raw mode or read stdin. It serializes the
//! bracketed-paste/focus/mouse/keyboard/cursor sequences an application may pair
//! with `TerminalRawInputFrontend` or `TerminalRawInputFallibleEventStream`, and
//! also shows the higher-level session guard where OS raw mode remains an
//! explicit opt-in. `modifyOtherKeys` is kept explicit and gated with
//! `supports_extended_keys()`.

use iocraft::prelude::*;
use std::io::Write as _;

fn main() -> std::io::Result<()> {
    let extended_keys = supports_extended_keys();
    let options = TerminalRawInputModeOptions {
        mouse_capture: true,
        keyboard_enhancement_flags: extended_keys
            .then_some(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES),
        xterm_modify_other_keys: extended_keys,
        ..Default::default()
    };

    let mut enter = Vec::new();
    write_terminal_raw_input_mode_enter(&mut enter, options)?;

    let mut exit = Vec::new();
    write_terminal_raw_input_mode_exit(&mut exit, options)?;

    let mut guarded = Vec::new();
    {
        let mut guard = TerminalRawInputModeGuard::enter(&mut guarded, options)?;
        guard
            .writer_mut()
            .write_all(b"<raw input UI writes here>")?;
    }

    let session_options = TerminalRawInputSessionOptions {
        terminal_modes: options,
        // Keep this example safe to run in any shell. A real caller-owned stdin
        // backend may set this to true around its raw byte reader.
        enable_os_raw_mode: false,
    };
    let mut session = TerminalRawInputSessionGuard::enter(Vec::new(), session_options)?;
    session
        .writer_mut()
        .write_all(b"<caller-owned raw byte reader active here>")?;
    let session_bytes = session.exit()?;

    println!("enter bytes: {:?}", String::from_utf8_lossy(&enter));
    println!("exit bytes: {:?}", String::from_utf8_lossy(&exit));
    println!("guarded bytes: {:?}", String::from_utf8_lossy(&guarded));
    println!(
        "session bytes: {:?}",
        String::from_utf8_lossy(&session_bytes)
    );
    Ok(())
}
