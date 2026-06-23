use super::{StdoutHandle, UseEffect, UseOutput, UseRef};
use crate::{ansi, Hooks};
use std::{env, io::IsTerminal};

mod private {
    pub trait Sealed {}
    impl Sealed for crate::Hooks<'_, '_> {}
}

/// Terminal progress state for OSC 9;4 reporting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminalProgressState {
    /// A determinate operation is running. Pass a percentage to [`TerminalNotificationHandle::progress`].
    Running,
    /// The operation completed; this clears the terminal progress indicator.
    Completed,
    /// The operation failed.
    Error,
    /// An indeterminate operation is running.
    Indeterminate,
}

/// Declarative OSC 21337 tab-status presets.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TabStatusKind {
    /// Green idle indicator.
    Idle,
    /// Orange working indicator.
    Busy,
    /// Blue waiting-for-user indicator.
    Waiting,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TabStatusRgb {
    r: u8,
    g: u8,
    b: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TabStatusPreset {
    indicator: TabStatusRgb,
    status: &'static str,
    status_color: TabStatusRgb,
}

fn rgb(r: u8, g: u8, b: u8) -> TabStatusRgb {
    TabStatusRgb { r, g, b }
}

fn tab_status_preset(kind: TabStatusKind) -> TabStatusPreset {
    match kind {
        TabStatusKind::Idle => TabStatusPreset {
            indicator: rgb(0, 215, 95),
            status: "Idle",
            status_color: rgb(136, 136, 136),
        },
        TabStatusKind::Busy => TabStatusPreset {
            indicator: rgb(255, 149, 0),
            status: "Working…",
            status_color: rgb(255, 149, 0),
        },
        TabStatusKind::Waiting => TabStatusPreset {
            indicator: rgb(95, 135, 255),
            status: "Waiting",
            status_color: rgb(95, 135, 255),
        },
    }
}

/// Handle returned by [`UseTerminalNotification::use_terminal_notification`].
#[derive(Clone)]
pub struct TerminalNotificationHandle {
    stdout: StdoutHandle,
}

impl TerminalNotificationHandle {
    fn write_wrapped_osc(&self, parts: Vec<String>) {
        let sequence = ansi::osc_sequence(&parts);
        self.stdout
            .write_control_sequence(ansi::wrap_for_current_multiplexer_sequence(&sequence));
    }

    /// Sends an iTerm2-style notification (`OSC 9`).
    pub fn notify_iterm2(&self, message: impl AsRef<str>, title: Option<&str>) {
        let message = ansi::sanitize_osc_payload(message.as_ref());
        let display = if let Some(title) = title {
            format!("{}:\n{}", ansi::sanitize_osc_payload(title), message)
        } else {
            message
        };
        self.write_wrapped_osc(vec!["9".to_string(), format!("\n\n{display}")]);
    }

    /// Sends a Kitty notification (`OSC 99`).
    pub fn notify_kitty(&self, message: impl AsRef<str>, title: impl AsRef<str>, id: u32) {
        let title = ansi::sanitize_osc_payload(title.as_ref());
        let message = ansi::sanitize_osc_payload(message.as_ref());
        let id = id.min(999_999_999);
        self.write_wrapped_osc(vec!["99".to_string(), format!("i={id}:d=0:p=title"), title]);
        self.write_wrapped_osc(vec!["99".to_string(), format!("i={id}:p=body"), message]);
        self.write_wrapped_osc(vec![
            "99".to_string(),
            format!("i={id}:d=1:a=focus"),
            String::new(),
        ]);
    }

    /// Sends a Ghostty notification (`OSC 777;notify`).
    pub fn notify_ghostty(&self, message: impl AsRef<str>, title: impl AsRef<str>) {
        self.write_wrapped_osc(vec![
            "777".to_string(),
            "notify".to_string(),
            ansi::sanitize_osc_payload(title.as_ref()),
            ansi::sanitize_osc_payload(message.as_ref()),
        ]);
    }

    /// Emits a raw BEL. This is intentionally not wrapped for tmux/screen so
    /// multiplexer bell-action fallback still works, matching CC Ink.
    pub fn notify_bell(&self) {
        self.stdout.write_control_sequence("\x07");
    }

    /// Reports terminal progress via iTerm2-compatible `OSC 9;4` sequences.
    ///
    /// This mirrors CC Ink's `useTerminalNotification().progress(...)` gate:
    /// unsupported terminals and non-TTY stdout are no-ops. `None` clears the
    /// progress indicator.
    pub fn progress(&self, state: Option<TerminalProgressState>, percentage: Option<u8>) {
        if !is_progress_reporting_available() {
            return;
        }
        let sequence = progress_sequence(state, percentage);
        self.stdout
            .write_control_sequence(ansi::wrap_for_current_multiplexer_sequence(&sequence));
    }

    /// Sets an OSC 21337 tab-status preset when supported.
    pub fn set_tab_status(&self, kind: TabStatusKind) {
        if !supports_tab_status() {
            return;
        }
        self.stdout
            .write_control_sequence(ansi::wrap_for_current_multiplexer_sequence(
                &tab_status_sequence(Some(kind)),
            ));
    }

    /// Clears OSC 21337 tab status when supported.
    pub fn clear_tab_status(&self) {
        if !supports_tab_status() {
            return;
        }
        self.stdout
            .write_control_sequence(ansi::wrap_for_current_multiplexer_sequence(
                &tab_status_sequence(None),
            ));
    }
}

/// Hook for side-band terminal notifications and progress reporting.
pub trait UseTerminalNotification: private::Sealed {
    /// Returns a handle for OSC notifications, BEL, and OSC 9;4 progress.
    fn use_terminal_notification(&mut self) -> TerminalNotificationHandle;

    /// Declaratively sets the OSC 21337 tab-status indicator.
    ///
    /// Passing `None` clears a previously emitted status. Like CC Ink's
    /// `useTabStatus`, emission is gated by [`supports_tab_status`] and wrapped
    /// for tmux/screen passthrough.
    fn use_tab_status(&mut self, kind: Option<TabStatusKind>);
}

impl UseTerminalNotification for Hooks<'_, '_> {
    fn use_terminal_notification(&mut self) -> TerminalNotificationHandle {
        let (stdout, _) = self.use_output();
        TerminalNotificationHandle { stdout }
    }

    fn use_tab_status(&mut self, kind: Option<TabStatusKind>) {
        let terminal = self.use_terminal_notification();
        let mut prev_kind = self.use_ref(|| None::<TabStatusKind>);
        self.use_effect(
            move || {
                let previous = prev_kind.get();
                match kind {
                    Some(kind) => terminal.set_tab_status(kind),
                    None if previous.is_some() => terminal.clear_tab_status(),
                    None => {}
                }
                prev_kind.set(kind);
            },
            kind,
        );
    }
}

#[derive(Default)]
struct ProgressCapabilityEnv<'a> {
    is_tty: bool,
    wt_session: bool,
    conemu: bool,
    term_program: Option<&'a str>,
    term_program_version: Option<&'a str>,
}

fn is_progress_reporting_available_from_env(env: ProgressCapabilityEnv<'_>) -> bool {
    if !env.is_tty || env.wt_session {
        return false;
    }
    if env.conemu {
        return true;
    }
    let Some(version) = env.term_program_version.and_then(parse_version_prefix) else {
        return false;
    };
    match env.term_program.unwrap_or_default() {
        "ghostty" => version_gte(version, (1, 2, 0)),
        "iTerm.app" => version_gte(version, (3, 6, 6)),
        _ => false,
    }
}

/// Returns whether OSC 9;4 progress reporting is available.
///
/// Supported terminals mirror CC Ink: ConEmu, Ghostty 1.2.0+, and iTerm2
/// 3.6.6+. Windows Terminal is explicitly excluded because it treats OSC 9;4
/// as notifications rather than progress.
pub fn is_progress_reporting_available() -> bool {
    let term_program = env::var("TERM_PROGRAM").ok();
    let term_program_version = env::var("TERM_PROGRAM_VERSION").ok();
    is_progress_reporting_available_from_env(ProgressCapabilityEnv {
        is_tty: std::io::stdout().is_terminal(),
        wt_session: env::var_os("WT_SESSION").is_some(),
        conemu: env::var_os("ConEmuANSI").is_some()
            || env::var_os("ConEmuPID").is_some()
            || env::var_os("ConEmuTask").is_some(),
        term_program: term_program.as_deref(),
        term_program_version: term_program_version.as_deref(),
    })
}

/// Returns whether OSC 21337 tab-status emission is enabled.
///
/// This mirrors CC Ink's unstable-gate policy: only Anthropic internal users
/// receive the sequence while the spec is evolving.
pub fn supports_tab_status() -> bool {
    env::var("USER_TYPE").is_ok_and(|value| value == "ant")
}

fn hex_color(c: TabStatusRgb) -> String {
    format!("#{:02x}{:02x}{:02x}", c.r, c.g, c.b)
}

fn escape_tab_status_text(text: &str) -> String {
    text.replace('\\', "\\\\").replace(';', "\\;")
}

fn tab_status_sequence(kind: Option<TabStatusKind>) -> String {
    let payload = match kind {
        Some(kind) => {
            let preset = tab_status_preset(kind);
            format!(
                "indicator={};status={};status-color={}",
                hex_color(preset.indicator),
                escape_tab_status_text(preset.status),
                hex_color(preset.status_color)
            )
        }
        None => "indicator=;status=;status-color=".to_string(),
    };
    ansi::osc_sequence(&["21337".to_string(), payload])
}

fn progress_sequence(state: Option<TerminalProgressState>, percentage: Option<u8>) -> String {
    let pct = percentage.unwrap_or(0).min(100).to_string();
    let (op, value) = match state {
        None | Some(TerminalProgressState::Completed) => ("0", String::new()),
        Some(TerminalProgressState::Running) => ("1", pct),
        Some(TerminalProgressState::Error) => ("2", pct),
        Some(TerminalProgressState::Indeterminate) => ("3", String::new()),
    };
    ansi::osc_sequence(&["9".to_string(), "4".to_string(), op.to_string(), value])
}

fn parse_version_prefix(raw: &str) -> Option<(u32, u32, u32)> {
    let start = raw.find(|ch: char| ch.is_ascii_digit())?;
    let mut nums = [0u32; 3];
    let mut idx = 0usize;
    let mut current = String::new();
    for ch in raw[start..].chars() {
        if ch.is_ascii_digit() {
            current.push(ch);
        } else if ch == '.' {
            if idx >= 3 || current.is_empty() {
                break;
            }
            nums[idx] = current.parse().ok()?;
            idx += 1;
            current.clear();
        } else {
            break;
        }
    }
    if idx < 3 && !current.is_empty() {
        nums[idx] = current.parse().ok()?;
        idx += 1;
    }
    (idx > 0).then_some((nums[0], nums[1], nums[2]))
}

fn version_gte(a: (u32, u32, u32), b: (u32, u32, u32)) -> bool {
    a >= b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_capability_matches_cc_gate() {
        assert!(is_progress_reporting_available_from_env(
            ProgressCapabilityEnv {
                is_tty: true,
                conemu: true,
                ..Default::default()
            }
        ));
        assert!(!is_progress_reporting_available_from_env(
            ProgressCapabilityEnv {
                is_tty: true,
                conemu: true,
                wt_session: true,
                ..Default::default()
            }
        ));
        assert!(is_progress_reporting_available_from_env(
            ProgressCapabilityEnv {
                is_tty: true,
                term_program: Some("ghostty"),
                term_program_version: Some("1.2.0"),
                ..Default::default()
            }
        ));
        assert!(!is_progress_reporting_available_from_env(
            ProgressCapabilityEnv {
                is_tty: true,
                term_program: Some("ghostty"),
                term_program_version: Some("1.1.9"),
                ..Default::default()
            }
        ));
        assert!(is_progress_reporting_available_from_env(
            ProgressCapabilityEnv {
                is_tty: true,
                term_program: Some("iTerm.app"),
                term_program_version: Some("3.6.6beta"),
                ..Default::default()
            }
        ));
    }

    #[test]
    fn progress_sequence_clamps_and_clears() {
        let running = progress_sequence(Some(TerminalProgressState::Running), Some(150));
        assert!(running.starts_with("\x1b]9;4;1;100"));
        assert!(running.ends_with('\x07') || running.ends_with("\x1b\\"));

        let clear = progress_sequence(None, None);
        assert!(clear.starts_with("\x1b]9;4;0;"));
    }

    #[test]
    fn tab_status_sequence_matches_cc_presets_and_clear() {
        let busy = tab_status_sequence(Some(TabStatusKind::Busy));
        assert!(
            busy.starts_with("\x1b]21337;indicator=#ff9500;status=Working…;status-color=#ff9500")
        );
        assert!(busy.ends_with('\x07') || busy.ends_with("\x1b\\"));

        let clear = tab_status_sequence(None);
        assert!(clear.starts_with("\x1b]21337;indicator=;status=;status-color="));
    }

    #[test]
    fn parse_version_prefix_coerces_like_semver() {
        assert_eq!(parse_version_prefix("version 3.6.6beta"), Some((3, 6, 6)));
        assert_eq!(parse_version_prefix("1.2"), Some((1, 2, 0)));
        assert_eq!(parse_version_prefix("none"), None);
    }
}
