use crate::style::Color;
use crossterm::{
    csi,
    style::{Attribute, Colored},
};
use std::{
    env,
    io::{self, IsTerminal, Write},
    sync::OnceLock,
};

pub(crate) fn sgr_reset(w: &mut impl Write) -> io::Result<()> {
    w.write_all(csi!("0m").as_bytes())
}

pub(crate) fn erase_to_eol(w: &mut impl Write) -> io::Result<()> {
    w.write_all(csi!("K").as_bytes())
}

pub(crate) fn sgr_attr(w: &mut impl Write, attr: Attribute) -> io::Result<()> {
    write!(w, csi!("{}m"), attr.sgr())
}

pub(crate) fn sgr_fg(w: &mut impl Write, color: Color) -> io::Result<()> {
    write!(
        w,
        csi!("{}m"),
        Colored::ForegroundColor(normalize_color_for_terminal(color))
    )
}

pub(crate) fn sgr_bg(w: &mut impl Write, color: Color) -> io::Result<()> {
    write!(
        w,
        csi!("{}m"),
        Colored::BackgroundColor(normalize_color_for_terminal(color))
    )
}

pub(crate) fn sgr_underline_color(w: &mut impl Write, color: Color) -> io::Result<()> {
    write!(
        w,
        csi!("{}m"),
        Colored::UnderlineColor(normalize_color_for_terminal(color))
    )
}

fn should_clamp_truecolor_for_tmux_with_env(
    mut env_lookup: impl FnMut(&str) -> Option<String>,
) -> bool {
    // Mirrors CC Ink's colorize.ts tmux clamp: default tmux often fails to
    // re-emit truecolor background SGR to the outer terminal unless users have
    // configured Tc/RGB passthrough. Downgrade RGB colors to ANSI-256 unless the
    // explicit escape hatch is set.
    env_lookup("TMUX").is_some() && env_lookup("CLAUDE_CODE_TMUX_TRUECOLOR").is_none()
}

fn should_clamp_truecolor_for_tmux() -> bool {
    static CLAMP: OnceLock<bool> = OnceLock::new();
    *CLAMP.get_or_init(|| should_clamp_truecolor_for_tmux_with_env(|key| env::var(key).ok()))
}

fn rgb_to_ansi256(r: u8, g: u8, b: u8) -> u8 {
    if r == g && g == b {
        if r < 8 {
            return 16;
        }
        if r > 248 {
            return 231;
        }
        return (((r as f32 - 8.0) / 247.0 * 24.0).round() as u8) + 232;
    }

    let r = (r as f32 / 255.0 * 5.0).round() as u8;
    let g = (g as f32 / 255.0 * 5.0).round() as u8;
    let b = (b as f32 / 255.0 * 5.0).round() as u8;
    16 + (36 * r) + (6 * g) + b
}

#[cfg(test)]
fn normalize_color_for_terminal_with_env(
    color: Color,
    env_lookup: impl FnMut(&str) -> Option<String>,
) -> Color {
    if should_clamp_truecolor_for_tmux_with_env(env_lookup) {
        if let Color::Rgb { r, g, b } = color {
            return Color::AnsiValue(rgb_to_ansi256(r, g, b));
        }
    }
    color
}

fn normalize_color_for_terminal(color: Color) -> Color {
    if should_clamp_truecolor_for_tmux() {
        if let Color::Rgb { r, g, b } = color {
            return Color::AnsiValue(rgb_to_ansi256(r, g, b));
        }
    }
    color
}

fn sanitize_hyperlink_href(href: &str) -> String {
    href.chars().filter(|ch| !ch.is_control()).collect()
}

fn base36_u32(mut value: u32) -> String {
    if value == 0 {
        return "0".to_string();
    }
    let mut out = Vec::new();
    while value > 0 {
        let digit = (value % 36) as u8;
        out.push(match digit {
            0..=9 => b'0' + digit,
            _ => b'a' + (digit - 10),
        });
        value /= 36;
    }
    out.reverse();
    String::from_utf8(out).expect("base36 digits are ascii")
}

fn osc8_id(url: &str) -> String {
    // Mirrors CC Ink's termio/osc.ts `osc8Id`: JS bitwise math keeps a signed
    // 32-bit accumulator, then `>>> 0` formats it as unsigned base36.
    let mut hash: i32 = 0;
    for ch in url.chars() {
        hash = hash
            .wrapping_shl(5)
            .wrapping_sub(hash)
            .wrapping_add(ch as i32);
    }
    base36_u32(hash as u32)
}

const ADDITIONAL_HYPERLINK_TERMINALS: &[&str] = &[
    "ghostty",
    "Hyper",
    "kitty",
    "alacritty",
    "iTerm.app",
    "iTerm2",
];

pub(crate) fn supports_hyperlinks_with_env(
    mut env_lookup: impl FnMut(&str) -> Option<String>,
    stdout_supported: bool,
) -> bool {
    // Mirrors CC Ink's supports-hyperlinks.ts wrapper: trust the base
    // supports-hyperlinks result first, then allowlist terminals that are known
    // to support OSC 8 but may not be detected by the library.
    if stdout_supported {
        return true;
    }

    if env_lookup("TERM_PROGRAM")
        .is_some_and(|term| ADDITIONAL_HYPERLINK_TERMINALS.contains(&term.as_str()))
    {
        return true;
    }

    if env_lookup("LC_TERMINAL")
        .is_some_and(|term| ADDITIONAL_HYPERLINK_TERMINALS.contains(&term.as_str()))
    {
        return true;
    }

    if env_lookup("TERM").is_some_and(|term| term.contains("kitty")) {
        return true;
    }

    false
}

fn supports_hyperlinks_base_stdout() -> bool {
    if env::var("FORCE_HYPERLINK").is_ok_and(|value| value != "0") {
        return true;
    }
    if env::var("NETLIFY").is_ok_and(|value| !value.is_empty()) {
        return true;
    }
    std::io::stdout().is_terminal()
        && (env::var_os("WT_SESSION").is_some()
            || env::var("TERM_PROGRAM").is_ok_and(|term| {
                matches!(
                    term.as_str(),
                    "iTerm.app" | "WezTerm" | "vscode" | "ghostty" | "zed"
                )
            })
            || env::var("TERM")
                .is_ok_and(|term| matches!(term.as_str(), "alacritty" | "xterm-kitty"))
            || env::var("VTE_VERSION")
                .ok()
                .and_then(|value| value.parse::<u32>().ok())
                .is_some_and(|version| version >= 5000))
}

/// Returns whether stdout should emit OSC 8 hyperlink metadata.
///
/// This mirrors CC Ink's `supportsHyperlinks(...)` helper: it trusts the
/// upstream supports-hyperlinks stdout probe and extends it with the fork's
/// additional terminal allowlist (`ghostty`, Hyper, kitty, Alacritty, iTerm2,
/// LC_TERMINAL preservation through tmux, and `TERM=xterm-kitty`).
pub fn supports_hyperlinks() -> bool {
    supports_hyperlinks_with_env(|key| env::var(key).ok(), supports_hyperlinks_base_stdout())
}

pub(crate) fn hyperlink_open(w: &mut impl Write, href: &str) -> io::Result<()> {
    let href = sanitize_hyperlink_href(href);
    let id = osc8_id(&href);
    write!(w, "\x1b]8;id={id};{href}\x1b\\")
}

pub(crate) fn hyperlink_close(w: &mut impl Write) -> io::Result<()> {
    w.write_all(b"\x1b]8;;\x1b\\")
}

fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0];
        let b1 = *chunk.get(1).unwrap_or(&0);
        let b2 = *chunk.get(2).unwrap_or(&0);
        out.push(TABLE[(b0 >> 2) as usize] as char);
        out.push(TABLE[(((b0 & 0b0000_0011) << 4) | (b1 >> 4)) as usize] as char);
        if chunk.len() > 1 {
            out.push(TABLE[(((b1 & 0b0000_1111) << 2) | (b2 >> 6)) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(TABLE[(b2 & 0b0011_1111) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

/// Terminal multiplexer passthrough wrapper to use for escape sequences.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MultiplexerPassthrough {
    /// Do not wrap the sequence.
    None,
    /// Wrap for tmux DCS passthrough, doubling inner ESC bytes.
    Tmux,
    /// Wrap for GNU screen passthrough.
    Screen,
}

/// Wraps a terminal escape sequence for multiplexer passthrough.
///
/// Mirrors CC Ink's `wrapForMultiplexer(...)` / `tmuxPassthrough(...)`:
/// tmux requires inner ESC bytes to be doubled, while GNU screen wraps the
/// payload as-is. BEL is intentionally not escaped.
pub(crate) fn wrap_for_multiplexer_sequence(
    sequence: &str,
    multiplexer: MultiplexerPassthrough,
) -> String {
    match multiplexer {
        MultiplexerPassthrough::None => sequence.to_string(),
        MultiplexerPassthrough::Tmux => {
            let escaped = sequence.replace('\x1b', "\x1b\x1b");
            format!("\x1bPtmux;{escaped}\x1b\\")
        }
        MultiplexerPassthrough::Screen => format!("\x1bP{sequence}\x1b\\"),
    }
}

/// Detects the current terminal multiplexer passthrough mode from environment.
///
/// This mirrors CC Ink's `wrapForMultiplexer(...)`: tmux and GNU screen need
/// DCS passthrough for OSC notification/progress sequences to reach the outer
/// terminal, while raw BEL must remain unwrapped so tmux can use it as a bell.
pub(crate) fn current_multiplexer_passthrough() -> MultiplexerPassthrough {
    if env::var_os("TMUX").is_some() {
        MultiplexerPassthrough::Tmux
    } else if env::var_os("STY").is_some() {
        MultiplexerPassthrough::Screen
    } else {
        MultiplexerPassthrough::None
    }
}

/// Wraps a terminal escape sequence for the current multiplexer environment.
pub(crate) fn wrap_for_current_multiplexer_sequence(sequence: &str) -> String {
    wrap_for_multiplexer_sequence(sequence, current_multiplexer_passthrough())
}

/// Builds an OSC sequence using the same terminator policy as CC Ink: Kitty
/// receives ST to avoid audible bells; other terminals use BEL.
pub(crate) fn osc_sequence(parts: &[String]) -> String {
    let terminator = if env::var_os("KITTY_WINDOW_ID").is_some()
        || env::var("TERM").is_ok_and(|term| term.contains("kitty"))
    {
        "\x1b\\"
    } else {
        "\x07"
    };
    format!("\x1b]{}{}", parts.join(";"), terminator)
}

/// Filters OSC payload text so user-provided notification strings cannot
/// terminate the sequence and inject terminal controls.
pub(crate) fn sanitize_osc_payload(text: &str) -> String {
    text.chars()
        .filter(|ch| matches!(ch, '\n' | '\t') || !ch.is_control())
        .collect()
}

/// Builds a raw OSC 52 clipboard-write sequence for the system clipboard.
///
/// This is the Rust terminal-output counterpart to CC Ink's `setClipboard` raw
/// sequence (`ESC ] 52 ; c ; <base64> BEL`). Multiplexer/native-clipboard
/// transports remain an application concern; this helper provides the terminal
/// escape sequence needed by fullscreen selection copy.
pub(crate) fn osc52_clipboard_sequence(text: &str) -> String {
    format!("\x1b]52;c;{}\x07", base64_encode(text.as_bytes()))
}

pub(crate) fn osc52_clipboard_sequence_for_multiplexer(
    text: &str,
    multiplexer: MultiplexerPassthrough,
) -> String {
    wrap_for_multiplexer_sequence(&osc52_clipboard_sequence(text), multiplexer)
}

pub(crate) fn osc52_clipboard(w: &mut (impl Write + ?Sized), text: &str) -> io::Result<()> {
    w.write_all(osc52_clipboard_sequence(text).as_bytes())
}

pub(crate) fn osc52_clipboard_for_multiplexer(
    w: &mut (impl Write + ?Sized),
    text: &str,
    multiplexer: MultiplexerPassthrough,
) -> io::Result<()> {
    w.write_all(osc52_clipboard_sequence_for_multiplexer(text, multiplexer).as_bytes())
}

pub(crate) fn terminal_title(w: &mut (impl Write + ?Sized), title: &str) -> io::Result<()> {
    w.write_all(b"\x1b]0;")?;
    for ch in title.chars().filter(|ch| !ch.is_control()) {
        write!(w, "{ch}")?;
    }
    w.write_all(b"\x07")
}

#[cfg(test)]
mod tests {
    fn supports_hyperlinks_env<'a>(
        pairs: &'a [(&'a str, &'a str)],
        stdout_supported: bool,
    ) -> bool {
        super::supports_hyperlinks_with_env(
            |key| {
                pairs
                    .iter()
                    .find_map(|(k, v)| (*k == key).then(|| (*v).to_string()))
            },
            stdout_supported,
        )
    }

    #[test]
    fn supports_hyperlinks_matches_cc_extra_terminal_allowlist() {
        assert!(supports_hyperlinks_env(&[], true));
        assert!(supports_hyperlinks_env(
            &[("TERM_PROGRAM", "ghostty")],
            false
        ));
        assert!(supports_hyperlinks_env(&[("TERM_PROGRAM", "kitty")], false));
        assert!(supports_hyperlinks_env(&[("LC_TERMINAL", "iTerm2")], false));
        assert!(supports_hyperlinks_env(&[("TERM", "xterm-kitty")], false));
        assert!(!supports_hyperlinks_env(&[("TERM_PROGRAM", "dumb")], false));
    }

    fn normalize_color_env(color: super::Color, pairs: &[(&str, &str)]) -> super::Color {
        super::normalize_color_for_terminal_with_env(color, |key| {
            pairs
                .iter()
                .find_map(|(k, v)| (*k == key).then(|| (*v).to_string()))
        })
    }

    #[test]
    fn tmux_truecolor_clamp_matches_cc_ink_colorize_gate() {
        let claude_orange = super::Color::Rgb {
            r: 215,
            g: 119,
            b: 87,
        };
        assert_eq!(normalize_color_env(claude_orange, &[]), claude_orange);
        assert_eq!(
            normalize_color_env(claude_orange, &[("TMUX", "/tmp/tmux")]),
            super::Color::AnsiValue(174)
        );
        assert_eq!(
            normalize_color_env(
                claude_orange,
                &[("TMUX", "/tmp/tmux"), ("CLAUDE_CODE_TMUX_TRUECOLOR", "1")]
            ),
            claude_orange
        );
        assert_eq!(
            normalize_color_env(
                super::Color::Rgb {
                    r: 240,
                    g: 240,
                    b: 240
                },
                &[("TMUX", "/tmp/tmux")]
            ),
            super::Color::AnsiValue(255)
        );
    }

    #[test]
    fn osc8_id_matches_cc_ink_hash_and_sanitizes_href() {
        assert_eq!(super::osc8_id("https://example.com"), "ags5vy");

        let mut buf = Vec::new();
        super::hyperlink_open(&mut buf, "https://safe.example/\x1b]0;owned\x07").unwrap();
        let output = String::from_utf8(buf).unwrap();
        assert_eq!(
            output,
            "\x1b]8;id=6cevo;https://safe.example/]0;owned\x1b\\"
        );
        assert!(!output.contains("\x1b]0;owned"));
    }

    #[test]
    fn osc52_clipboard_sequence_base64_encodes_utf8_text() {
        assert_eq!(super::osc52_clipboard_sequence(""), "\x1b]52;c;\x07");
        assert_eq!(super::osc52_clipboard_sequence("f"), "\x1b]52;c;Zg==\x07");
        assert_eq!(super::osc52_clipboard_sequence("fo"), "\x1b]52;c;Zm8=\x07");
        assert_eq!(super::osc52_clipboard_sequence("foo"), "\x1b]52;c;Zm9v\x07");
        assert_eq!(
            super::osc52_clipboard_sequence("中文"),
            "\x1b]52;c;5Lit5paH\x07"
        );
    }

    #[test]
    fn multiplexer_passthrough_wraps_clipboard_sequences() {
        let raw = "\x1b]52;c;Y29weQ==\x07";
        assert_eq!(
            super::wrap_for_multiplexer_sequence(raw, super::MultiplexerPassthrough::Tmux),
            "\x1bPtmux;\x1b\x1b]52;c;Y29weQ==\x07\x1b\\"
        );
        assert_eq!(
            super::wrap_for_multiplexer_sequence(raw, super::MultiplexerPassthrough::Screen),
            "\x1bP\x1b]52;c;Y29weQ==\x07\x1b\\"
        );
        assert_eq!(
            super::osc52_clipboard_sequence_for_multiplexer(
                "copy",
                super::MultiplexerPassthrough::Tmux,
            ),
            "\x1bPtmux;\x1b\x1b]52;c;Y29weQ==\x07\x1b\\"
        );
    }

    #[test]
    fn terminal_title_filters_control_chars() {
        let mut buf = Vec::new();
        super::terminal_title(&mut buf, "safe\x1b]2;owned\x07").unwrap();
        let output = String::from_utf8(buf).unwrap();
        assert_eq!(output, "\x1b]0;safe]2;owned\x07");
        assert!(!output.contains("\x1b]2;owned"));
    }
}
