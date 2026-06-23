use std::{
    env,
    sync::{Mutex, OnceLock},
};

pub(super) fn is_synchronized_output_supported_with_env(
    mut env_lookup: impl FnMut(&str) -> Option<String>,
) -> bool {
    // Mirrors Claude Code's Ink fork: tmux proxies the bytes but does not make
    // DEC 2026 atomic, so skip BSU/ESU there to avoid parser work and false
    // safety. Known modern terminals opt in via TERM_PROGRAM/TERM/env hints.
    if env_lookup("TMUX").is_some() {
        return false;
    }

    let term_program = env_lookup("TERM_PROGRAM").unwrap_or_default();
    matches!(
        term_program.as_str(),
        "iTerm.app" | "WezTerm" | "WarpTerminal" | "ghostty" | "contour" | "vscode" | "alacritty"
    ) || env_lookup("TERM").is_some_and(|term| {
        term.contains("kitty")
            || term == "xterm-ghostty"
            || term.starts_with("foot")
            || term.contains("alacritty")
    }) || env_lookup("KITTY_WINDOW_ID").is_some()
        || env_lookup("ZED_TERM").is_some()
        || env_lookup("WT_SESSION").is_some()
        || env_lookup("VTE_VERSION")
            .and_then(|v| v.parse::<u32>().ok())
            .is_some_and(|v| v >= 6800)
}

/// Returns whether the current terminal should use DEC 2026 synchronized output.
///
/// This is the Rust counterpart to CC Ink's
/// `isSynchronizedOutputSupported()` / `SYNC_OUTPUT_SUPPORTED` gate. Tmux is
/// explicitly disabled because it proxies BSU/ESU bytes without preserving
/// atomicity; known modern terminals opt in via `TERM_PROGRAM`, `TERM`, or
/// terminal-specific environment hints.
pub fn is_synchronized_output_supported() -> bool {
    is_synchronized_output_supported_with_env(|key| env::var(key).ok())
}

pub(super) fn detect_terminal_from_env(
    mut env_lookup: impl FnMut(&str) -> Option<String>,
) -> Option<String> {
    if env_lookup("TERM").is_some_and(|term| term == "xterm-ghostty") {
        return Some("ghostty".to_string());
    }
    if env_lookup("TERM").is_some_and(|term| term.contains("kitty")) {
        return Some("kitty".to_string());
    }
    if let Some(term_program) = env_lookup("TERM_PROGRAM") {
        return Some(term_program);
    }
    if env_lookup("TMUX").is_some() {
        return Some("tmux".to_string());
    }
    if env_lookup("STY").is_some() {
        return Some("screen".to_string());
    }
    if env_lookup("KITTY_WINDOW_ID").is_some() {
        return Some("kitty".to_string());
    }
    if env_lookup("WT_SESSION").is_some() {
        return Some("windows-terminal".to_string());
    }
    env_lookup("TERM")
}

pub(super) fn supports_extended_keys_with_env(
    mut env_lookup: impl FnMut(&str) -> Option<String>,
) -> bool {
    // Mirrors CC Ink's terminal.ts `supportsExtendedKeys()` allowlist. Kitty
    // keyboard / modifyOtherKeys are not safe to enable just because a terminal
    // ignores unknown CSI; xterm.js over SSH can emit sequences the parser does
    // not handle, so only known-good terminals opt in.
    detect_terminal_from_env(&mut env_lookup).is_some_and(|terminal| {
        matches!(
            terminal.as_str(),
            "iTerm.app" | "kitty" | "WezTerm" | "ghostty" | "tmux" | "windows-terminal"
        )
    })
}

/// Returns whether the current terminal is allowed to enable extended key reporting.
///
/// This mirrors CC Ink's `supportsExtendedKeys()` allowlist for Kitty keyboard
/// protocol / xterm modifyOtherKeys. It intentionally does not enable solely
/// because a terminal might ignore unknown CSI sequences: xterm.js and unknown
/// SSH clients can emit sequences the parser cannot safely interpret.
pub fn supports_extended_keys() -> bool {
    supports_extended_keys_with_env(|key| env::var(key).ok())
}

pub(super) fn has_cursor_up_viewport_yank_bug_with_env(
    is_windows: bool,
    mut env_lookup: impl FnMut(&str) -> Option<String>,
) -> bool {
    // Mirrors CC Ink's terminal.ts `hasCursorUpViewportYankBug()`: conhost's
    // cursor positioning can follow the cursor into scrollback, and WT_SESSION
    // catches WSL/linux processes whose output still routes through Windows
    // Terminal/ConPTY.
    is_windows || env_lookup("WT_SESSION").is_some()
}

/// Returns whether cursor-up movements can yank the visible viewport into
/// scrollback on this host terminal.
///
/// This is the iocraft counterpart to CC Ink's
/// `hasCursorUpViewportYankBug()`. App-level renderers can use it to disable
/// high-frequency inline streaming effects on Windows/conhost-like terminals,
/// where relative cursor-up movement above the live viewport can visibly jump
/// the user's scroll position.
pub fn has_cursor_up_viewport_yank_bug() -> bool {
    has_cursor_up_viewport_yank_bug_with_env(cfg!(windows), |key| env::var(key).ok())
}

pub(super) fn clear_terminal_sequence_with_env(
    is_windows: bool,
    mut env_lookup: impl FnMut(&str) -> Option<String>,
) -> &'static str {
    const MODERN_CLEAR: &str = "\x1b[2J\x1b[3J\x1b[H";
    const LEGACY_WINDOWS_CLEAR: &str = "\x1b[2J\x1b[0f";

    if !is_windows {
        return MODERN_CLEAR;
    }

    let wt_session = env_lookup("WT_SESSION").is_some();
    let term_program = env_lookup("TERM_PROGRAM");
    let term_program_version = env_lookup("TERM_PROGRAM_VERSION").is_some();
    let msystem = env_lookup("MSYSTEM").is_some();

    let is_mintty = term_program.as_deref() == Some("mintty") || msystem;
    let is_vscode_conpty = term_program.as_deref() == Some("vscode") && term_program_version;
    let modern_windows_terminal = wt_session || is_vscode_conpty || is_mintty;

    if modern_windows_terminal {
        MODERN_CLEAR
    } else {
        LEGACY_WINDOWS_CLEAR
    }
}

/// Returns the terminal clear sequence used for a full clear, including
/// scrollback when the host terminal supports it.
///
/// This mirrors CC Ink's `getClearTerminalSequence()`: non-Windows terminals
/// and modern Windows terminals receive `ESC[2J ESC[3J ESC[H`; legacy Windows
/// consoles receive `ESC[2J ESC[0f` because they cannot reliably purge
/// scrollback and use HVP for cursor home.
pub fn clear_terminal_sequence() -> &'static str {
    clear_terminal_sequence_with_env(cfg!(windows), |key| env::var(key).ok())
}

pub(super) fn is_xterm_js_with_env_and_xtversion(
    mut env_lookup: impl FnMut(&str) -> Option<String>,
    xtversion_name: Option<&str>,
) -> bool {
    env_lookup("TERM_PROGRAM").is_some_and(|value| value == "vscode")
        || xtversion_name.is_some_and(|name| name.starts_with("xterm.js"))
}

/// Returns a DECRQM query sequence (`CSI ? mode $ p`).
///
/// Terminals that support the queried DEC private mode reply with DECRPM,
/// parsed as [`TerminalResponse::Decrpm`] by [`parse_terminal_response`].
pub fn decrqm_query_sequence(mode: u32) -> String {
    format!("\x1b[?{mode}$p")
}

/// Returns the DA1 query sequence (`CSI c`).
///
/// CC Ink uses this as a universal sentinel because all VT100-compatible
/// terminals respond to DA1.
pub fn da1_query_sequence() -> &'static str {
    "\x1b[c"
}

/// Returns the DA2 query sequence (`CSI > c`).
pub fn da2_query_sequence() -> &'static str {
    "\x1b[>c"
}

/// Returns the Kitty keyboard flags query sequence (`CSI ? u`).
pub fn kitty_keyboard_query_sequence() -> &'static str {
    "\x1b[?u"
}

/// Returns the DECXCPR cursor-position query sequence (`CSI ? 6 n`).
///
/// The DEC-private `?` marker matches CC Ink and avoids ambiguity with modified
/// function-key reports such as Shift+F3.
pub fn cursor_position_query_sequence() -> &'static str {
    "\x1b[?6n"
}

pub(super) fn osc_color_query_sequence_with_env(
    code: u32,
    mut env_lookup: impl FnMut(&str) -> Option<String>,
) -> String {
    let terminator = if detect_terminal_from_env(&mut env_lookup).as_deref() == Some("kitty") {
        "\x1b\\"
    } else {
        "\x07"
    };
    format!("\x1b]{code};?{terminator}")
}

/// Returns an OSC dynamic color query sequence, such as OSC 10 or OSC 11.
///
/// The `?` data slot asks the terminal to reply with the current value. As in
/// CC Ink's `osc(...)` helper, Kitty receives an ST terminator to avoid audible
/// bells; other terminals receive BEL.
pub fn osc_color_query_sequence(code: u32) -> String {
    osc_color_query_sequence_with_env(code, |key| env::var(key).ok())
}

/// Returns the XTVERSION query sequence (`CSI > 0 q`).
///
/// Terminals that support XTVERSION reply with `DCS > | name ST`, for example
/// `xterm.js(5.5.0)`. The query travels through the pty, so it can identify a
/// remote client terminal even when `TERM_PROGRAM` is not forwarded over SSH.
pub fn xtversion_query_sequence() -> &'static str {
    "\x1b[>0q"
}

/// Parses an XTVERSION response (`DCS > | name ST` or BEL-terminated form).
///
/// Returns the terminal name/version payload, for example `xterm.js(5.5.0)`.
pub fn parse_xtversion_response(response: &str) -> Option<&str> {
    let body = response.strip_prefix("\x1bP>|")?;
    if let Some(body) = body.strip_suffix("\x1b\\") {
        return Some(body);
    }
    body.strip_suffix('\x07')
}

pub(super) fn parse_numeric_params(params: &str) -> Option<Vec<u32>> {
    if params.is_empty() {
        return Some(Vec::new());
    }
    params
        .split(';')
        .map(|param| param.parse::<u32>().ok())
        .collect()
}

static XTVERSION_NAME: OnceLock<Mutex<Option<String>>> = OnceLock::new();

fn xtversion_name_state() -> &'static Mutex<Option<String>> {
    XTVERSION_NAME.get_or_init(|| Mutex::new(None))
}

/// Records the terminal name reported by an XTVERSION response.
pub fn set_xtversion_name(name: impl Into<String>) {
    let mut guard = xtversion_name_state()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if guard.is_none() {
        *guard = Some(name.into());
    }
}

/// Returns the terminal name previously recorded from XTVERSION, if any.
pub fn xtversion_name() -> Option<String> {
    xtversion_name_state()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

/// Returns whether the host terminal is xterm.js-based.
///
/// This combines the fast `TERM_PROGRAM=vscode` environment check with the
/// XTVERSION probe result set via [`set_xtversion_name`], matching CC Ink's
/// `isXtermJs()` fallback for SSH sessions where environment variables are not
/// forwarded.
pub fn is_xterm_js() -> bool {
    let xtversion = xtversion_name();
    is_xterm_js_with_env_and_xtversion(|key| env::var(key).ok(), xtversion.as_deref())
}
