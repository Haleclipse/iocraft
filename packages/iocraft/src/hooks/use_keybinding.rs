use crate::{
    hooks::UseTerminalEvents, Hooks, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, TerminalEvent,
};

mod private {
    pub trait Sealed {}
    impl Sealed for crate::Hooks<'_, '_> {}
}

/// A parsed key combination, e.g. `Ctrl+S` or `Shift+Tab`.
///
/// Usually constructed from a string via [`FromStr`](core::str::FromStr) /
/// [`TryFrom<&str>`], using a `modifier+modifier+key` syntax:
///
/// ```
/// # use iocraft::hooks::KeyBinding;
/// let _: KeyBinding = "ctrl+s".parse().unwrap();
/// let _: KeyBinding = "ctrl+shift+k".parse().unwrap();
/// let _: KeyBinding = "esc".parse().unwrap();
/// let _: KeyBinding = "f5".parse().unwrap();
/// let _: KeyBinding = "alt+enter".parse().unwrap();
/// ```
///
/// Supported modifiers: `ctrl`, `alt` (or `option`), `shift`, `super` (or `cmd` / `meta` /
/// `win`). Supported keys: single characters, `enter`/`return`, `esc`/`escape`, `tab`,
/// `backtab`, `space`, `backspace`, `delete`/`del`, `insert`, `home`, `end`,
/// `pageup`/`pgup`, `pagedown`/`pgdn`, `up`, `down`, `left`, `right`, and `f1`–`f24`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct KeyBinding {
    /// The modifier keys that must be held.
    pub modifiers: KeyModifiers,
    /// The non-modifier key.
    pub code: KeyCode,
}

impl KeyBinding {
    /// Returns `true` if the given key event matches this binding.
    ///
    /// Character comparison is case-insensitive, since terminals report shifted
    /// characters in upper case (`Shift+a` arrives as `Char('A')` + `SHIFT`).
    pub fn matches(&self, event: &KeyEvent) -> bool {
        if event.kind == KeyEventKind::Release {
            return false;
        }
        let code_matches = match (self.code, event.code) {
            (KeyCode::Char(a), KeyCode::Char(b)) => a.eq_ignore_ascii_case(&b),
            (a, b) => a == b,
        };
        code_matches && self.modifiers == event.modifiers
    }
}

/// An error produced when parsing a [`KeyBinding`] from a string.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyBindingParseError(String);

impl core::fmt::Display for KeyBindingParseError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "invalid key binding: {}", self.0)
    }
}

impl std::error::Error for KeyBindingParseError {}

impl core::str::FromStr for KeyBinding {
    type Err = KeyBindingParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut modifiers = KeyModifiers::empty();
        let mut code: Option<KeyCode> = None;
        for part in s.split('+') {
            let part = part.trim();
            let lower = part.to_ascii_lowercase();
            match lower.as_str() {
                "ctrl" | "control" => modifiers |= KeyModifiers::CONTROL,
                "alt" | "option" => modifiers |= KeyModifiers::ALT,
                "shift" => modifiers |= KeyModifiers::SHIFT,
                "super" | "cmd" | "meta" | "win" => modifiers |= KeyModifiers::SUPER,
                _ => {
                    if code.is_some() {
                        return Err(KeyBindingParseError(format!(
                            "{s:?} contains more than one non-modifier key"
                        )));
                    }
                    code = Some(parse_key_code(&lower, part, s)?);
                }
            }
        }
        let Some(code) = code else {
            return Err(KeyBindingParseError(format!(
                "{s:?} does not contain a non-modifier key"
            )));
        };
        // Shifted characters arrive from the terminal in upper case; normalize the
        // binding the same way so `shift+a` matches `Char('A')`.
        let code = match code {
            KeyCode::Char(c) if modifiers.contains(KeyModifiers::SHIFT) => {
                KeyCode::Char(c.to_ascii_uppercase())
            }
            other => other,
        };
        Ok(Self { modifiers, code })
    }
}

fn parse_key_code(
    lower: &str,
    original: &str,
    full: &str,
) -> Result<KeyCode, KeyBindingParseError> {
    Ok(match lower {
        "enter" | "return" => KeyCode::Enter,
        "esc" | "escape" => KeyCode::Esc,
        "tab" => KeyCode::Tab,
        "backtab" => KeyCode::BackTab,
        "space" => KeyCode::Char(' '),
        "backspace" => KeyCode::Backspace,
        "delete" | "del" => KeyCode::Delete,
        "insert" => KeyCode::Insert,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "pageup" | "pgup" => KeyCode::PageUp,
        "pagedown" | "pgdn" => KeyCode::PageDown,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        _ => {
            if let Some(n) = lower.strip_prefix('f').and_then(|n| n.parse::<u8>().ok()) {
                if (1..=24).contains(&n) {
                    return Ok(KeyCode::F(n));
                }
            }
            let mut chars = original.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) => KeyCode::Char(c),
                _ => {
                    return Err(KeyBindingParseError(format!(
                        "unrecognized key {original:?} in {full:?}"
                    )))
                }
            }
        }
    })
}

impl TryFrom<&str> for KeyBinding {
    type Error = KeyBindingParseError;

    fn try_from(s: &str) -> Result<Self, Self::Error> {
        s.parse()
    }
}

/// `UseKeybinding` provides a declarative way to register keyboard shortcuts.
///
/// Bindings participate in event propagation: they consume the matched key press
/// (via [`stop_propagation`](crate::PropagatedTerminalEvent::stop_propagation)), so a
/// binding in a deeply-nested component shadows the same binding in an ancestor, and
/// enclosing [`FocusScope`](crate::components::FocusScope)s won't also act on the key.
///
/// # Example
///
/// ```
/// # use iocraft::prelude::*;
/// #[component]
/// fn Editor(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
///     let mut saved = hooks.use_state(|| false);
///
///     hooks.use_keybinding("ctrl+s", move || saved.set(true));
///
///     element! {
///         Text(content: if saved.get() { "saved!" } else { "unsaved" })
///     }
/// }
/// ```
pub trait UseKeybinding: private::Sealed {
    /// Registers a handler invoked whenever the given key combination is pressed.
    ///
    /// The matched key event is consumed and will not reach propagation-aware
    /// subscribers in ancestor components.
    ///
    /// # Panics
    ///
    /// Panics if `binding` is not a valid key combination. See [`KeyBinding`] for the
    /// accepted syntax.
    fn use_keybinding<F>(&mut self, binding: &str, f: F)
    where
        F: FnMut() + Send + 'static;
}

impl UseKeybinding for Hooks<'_, '_> {
    fn use_keybinding<F>(&mut self, binding: &str, mut f: F)
    where
        F: FnMut() + Send + 'static,
    {
        let binding: KeyBinding = binding
            .parse()
            .unwrap_or_else(|e: KeyBindingParseError| panic!("{e}"));
        self.use_propagated_terminal_events(move |propagated| {
            if let TerminalEvent::Key(event) = propagated.event() {
                if binding.matches(event) {
                    f();
                    propagated.stop_propagation();
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prelude::*;
    use futures::stream::{self, StreamExt};
    use macro_rules_attribute::apply;
    use smol_macros::test;

    fn binding(s: &str) -> KeyBinding {
        s.parse().unwrap()
    }

    fn press(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
        }
    }

    #[test]
    fn test_parse_basic() {
        assert_eq!(
            binding("ctrl+s"),
            KeyBinding {
                modifiers: KeyModifiers::CONTROL,
                code: KeyCode::Char('s'),
            }
        );
        assert_eq!(
            binding("Ctrl+Shift+K"),
            KeyBinding {
                modifiers: KeyModifiers::CONTROL | KeyModifiers::SHIFT,
                code: KeyCode::Char('K'),
            }
        );
        assert_eq!(
            binding("esc"),
            KeyBinding {
                modifiers: KeyModifiers::empty(),
                code: KeyCode::Esc,
            }
        );
        assert_eq!(binding("f12").code, KeyCode::F(12));
        assert_eq!(binding("alt+enter").code, KeyCode::Enter);
        assert_eq!(binding("cmd+c").modifiers, KeyModifiers::SUPER);
        assert_eq!(binding("space").code, KeyCode::Char(' '));
        assert_eq!(binding("pgdn").code, KeyCode::PageDown);
    }

    #[test]
    fn test_parse_errors() {
        assert!("ctrl+".parse::<KeyBinding>().is_err());
        assert!("ctrl+s+k".parse::<KeyBinding>().is_err());
        assert!("ctrl+notakey".parse::<KeyBinding>().is_err());
        assert!("f99".parse::<KeyBinding>().is_err());
        assert!("".parse::<KeyBinding>().is_err());
    }

    #[test]
    fn test_matches() {
        let b = binding("ctrl+s");
        assert!(b.matches(&press(KeyCode::Char('s'), KeyModifiers::CONTROL)));
        // Case-insensitive character comparison.
        assert!(b.matches(&press(KeyCode::Char('S'), KeyModifiers::CONTROL)));
        // Modifiers must match exactly.
        assert!(!b.matches(&press(
            KeyCode::Char('s'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT
        )));
        assert!(!b.matches(&press(KeyCode::Char('s'), KeyModifiers::empty())));
        // Releases never match.
        assert!(!b.matches(&KeyEvent {
            code: KeyCode::Char('s'),
            modifiers: KeyModifiers::CONTROL,
            kind: KeyEventKind::Release,
        }));
        // Shifted bindings match the upper-case character terminals report.
        let b = binding("shift+a");
        assert!(b.matches(&press(KeyCode::Char('A'), KeyModifiers::SHIFT)));
    }

    #[component]
    fn SaveDemo(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let mut saved = hooks.use_state(|| false);
        hooks.use_keybinding("ctrl+s", move || saved.set(true));
        if saved.get() {
            system.exit();
        }
        element!(Text(content: if saved.get() { "saved" } else { "unsaved" }))
    }

    #[apply(test!)]
    async fn test_keybinding_fires_and_renders() {
        let canvases: Vec<_> = element!(SaveDemo)
            .mock_terminal_render_loop(MockTerminalConfig::with_events(stream::iter(vec![
                TerminalEvent::Key(KeyEvent {
                    code: KeyCode::Char('s'),
                    modifiers: KeyModifiers::CONTROL,
                    kind: KeyEventKind::Press,
                }),
            ])))
            .collect()
            .await;
        let last = canvases.last().unwrap().to_string();
        assert!(last.contains("saved"), "binding should fire: {last:?}");
    }

    /// A keybinding in a nested component consumes the key: an enclosing
    /// FocusScope must not also act on it.
    #[component]
    fn TabStealer(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut count = hooks.use_state(|| 0u32);
        hooks.use_keybinding("tab", move || count += 1);
        element!(Text(content: format!("stolen:{}", count)))
    }

    #[derive(Default, Props)]
    struct ItemProps {
        label: String,
    }

    #[component]
    fn Item(mut hooks: Hooks, props: &ItemProps) -> impl Into<AnyElement<'static>> {
        let focus = hooks.use_focus(FocusOptions::default());
        element!(Text(content: format!(
            "{}{}", props.label, if focus.is_focused() { "*" } else { " " }
        )))
    }

    #[component]
    fn Outer(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let mut presses = hooks.use_state(|| 0u32);
        hooks.use_terminal_events(move |e| {
            if let TerminalEvent::Key(KeyEvent {
                kind: KeyEventKind::Press,
                ..
            }) = e
            {
                presses += 1;
            }
        });
        if presses.get() >= 1 {
            system.exit();
        }
        element! {
            FocusScope {
                View(flex_direction: FlexDirection::Column) {
                    Item(label: "a".to_string())
                    TabStealer
                }
            }
        }
    }

    #[apply(test!)]
    async fn test_keybinding_consumes_key_from_enclosing_scope() {
        let canvases: Vec<_> = element!(Outer)
            .mock_terminal_render_loop(MockTerminalConfig::with_events(stream::iter(vec![
                TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Tab)),
            ])))
            .collect()
            .await;
        let last = canvases.last().unwrap().to_string();
        // The nested binding fired...
        assert!(last.contains("stolen:1"), "binding should fire: {last:?}");
        // ...and consumed the Tab, so the FocusScope never advanced focus.
        assert!(
            !last.contains("a*"),
            "scope must not act on a consumed Tab: {last:?}"
        );
    }
}
