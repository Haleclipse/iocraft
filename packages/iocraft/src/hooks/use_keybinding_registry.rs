use super::{UseContext, UseState, UseTerminalEvents};
use crate::{Hook, Hooks, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, PropagatedTerminalEvent};
use std::{collections::HashSet, sync::Arc, time::Instant};

mod private {
    pub trait Sealed {}
    impl Sealed for crate::Hooks<'_, '_> {}
}

const CHORD_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(1000);

/// One key in a keybinding chord.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct ParsedKeystroke {
    /// Normalized key name (`"escape"`, `"enter"`, `"a"`, `"pageup"`, ...).
    pub key: String,
    /// Ctrl/Control modifier.
    pub ctrl: bool,
    /// Alt/Option modifier.
    pub alt: bool,
    /// Shift modifier.
    pub shift: bool,
    /// Meta modifier. Like CC's resolver, Alt and Meta compare as one logical terminal modifier.
    pub meta: bool,
    /// Super/Cmd/Win modifier, distinct from Alt/Meta.
    pub super_key: bool,
}

/// A parsed chord sequence.
pub type ParsedChord = Vec<ParsedKeystroke>;

/// One raw keybinding entry.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KeybindingEntry {
    /// Keystroke or chord string, such as `"ctrl+s"` or `"ctrl+x ctrl+k"`.
    pub chord: String,
    /// Action name. `None` explicitly unbinds the chord.
    pub action: Option<String>,
}

impl KeybindingEntry {
    /// Creates a binding from `chord` to `action`.
    pub fn new(chord: impl Into<String>, action: impl Into<String>) -> Self {
        Self {
            chord: chord.into(),
            action: Some(action.into()),
        }
    }

    /// Creates an explicit unbinding for `chord`.
    pub fn unbound(chord: impl Into<String>) -> Self {
        Self {
            chord: chord.into(),
            action: None,
        }
    }
}

/// A block of keybindings scoped to a context.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KeybindingBlock {
    /// Context name (`"Global"`, `"Chat"`, `"Scroll"`, custom names, ...).
    pub context: String,
    /// Bindings in this context. Later entries win when chords collide.
    pub bindings: Vec<KeybindingEntry>,
}

impl KeybindingBlock {
    /// Creates a keybinding block.
    pub fn new(context: impl Into<String>, bindings: Vec<KeybindingEntry>) -> Self {
        Self {
            context: context.into(),
            bindings,
        }
    }
}

/// Parsed keybinding entry used by the resolver.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedBinding {
    /// Parsed chord sequence.
    pub chord: ParsedChord,
    /// Action name. `None` means explicitly unbound.
    pub action: Option<String>,
    /// Binding context.
    pub context: String,
}

/// Result of resolving a key through the action registry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeybindingResolveResult {
    /// A complete action matched.
    Match {
        /// Matched action name.
        action: String,
    },
    /// No binding matched.
    None,
    /// A binding explicitly unbound this chord.
    Unbound,
    /// The key started or continued a chord.
    ChordStarted {
        /// Pending chord prefix that should be completed by a later key.
        pending: ParsedChord,
    },
    /// A pending chord was cancelled by Escape, timeout, or an invalid continuation.
    ChordCancelled,
}

fn parse_keystroke(input: &str) -> ParsedKeystroke {
    let mut keystroke = ParsedKeystroke::default();
    for part in input.split('+') {
        let lower = part.trim().to_ascii_lowercase();
        match lower.as_str() {
            "ctrl" | "control" => keystroke.ctrl = true,
            "alt" | "opt" | "option" => keystroke.alt = true,
            "shift" => keystroke.shift = true,
            "meta" => keystroke.meta = true,
            "cmd" | "command" | "super" | "win" => keystroke.super_key = true,
            "esc" => keystroke.key = "escape".to_string(),
            "return" => keystroke.key = "enter".to_string(),
            "space" => keystroke.key = " ".to_string(),
            "↑" => keystroke.key = "up".to_string(),
            "↓" => keystroke.key = "down".to_string(),
            "←" => keystroke.key = "left".to_string(),
            "→" => keystroke.key = "right".to_string(),
            other => keystroke.key = other.to_string(),
        }
    }
    keystroke
}

/// Parses a chord string such as `"ctrl+x ctrl+s"`.
pub fn parse_keybinding_chord(input: &str) -> ParsedChord {
    if input == " " {
        return vec![parse_keystroke("space")];
    }
    input
        .trim()
        .split_whitespace()
        .map(parse_keystroke)
        .collect()
}

/// Parses keybinding blocks into a flat list. Later entries win during resolution.
pub fn parse_keybinding_blocks(blocks: &[KeybindingBlock]) -> Vec<ParsedBinding> {
    blocks
        .iter()
        .flat_map(|block| {
            block.bindings.iter().map(|binding| ParsedBinding {
                chord: parse_keybinding_chord(&binding.chord),
                action: binding.action.clone(),
                context: block.context.clone(),
            })
        })
        .collect()
}

fn key_display_name(key: &str) -> String {
    match key {
        "escape" => "Esc".to_string(),
        " " => "Space".to_string(),
        "enter" => "Enter".to_string(),
        "backspace" => "Backspace".to_string(),
        "delete" => "Delete".to_string(),
        "up" => "↑".to_string(),
        "down" => "↓".to_string(),
        "left" => "←".to_string(),
        "right" => "→".to_string(),
        "pageup" => "PageUp".to_string(),
        "pagedown" => "PageDown".to_string(),
        "home" => "Home".to_string(),
        "end" => "End".to_string(),
        other => other.to_string(),
    }
}

/// Converts a parsed keystroke to display text.
pub fn keybinding_keystroke_to_string(keystroke: &ParsedKeystroke) -> String {
    let mut parts = Vec::new();
    if keystroke.ctrl {
        parts.push("ctrl".to_string());
    }
    if keystroke.alt {
        parts.push("alt".to_string());
    }
    if keystroke.shift {
        parts.push("shift".to_string());
    }
    if keystroke.meta {
        parts.push("meta".to_string());
    }
    if keystroke.super_key {
        parts.push("cmd".to_string());
    }
    parts.push(key_display_name(&keystroke.key));
    parts.join("+")
}

/// Converts a parsed chord to display text.
pub fn keybinding_chord_to_string(chord: &[ParsedKeystroke]) -> String {
    chord
        .iter()
        .map(keybinding_keystroke_to_string)
        .collect::<Vec<_>>()
        .join(" ")
}

fn key_name_from_event(event: &KeyEvent) -> Option<String> {
    let name = match event.code {
        KeyCode::Char(ch) => ch.to_ascii_lowercase().to_string(),
        KeyCode::Enter => "enter".to_string(),
        KeyCode::Esc => "escape".to_string(),
        KeyCode::Tab | KeyCode::BackTab => "tab".to_string(),
        KeyCode::Backspace => "backspace".to_string(),
        KeyCode::Delete => "delete".to_string(),
        KeyCode::Insert => "insert".to_string(),
        KeyCode::Home => "home".to_string(),
        KeyCode::End => "end".to_string(),
        KeyCode::PageUp => "pageup".to_string(),
        KeyCode::PageDown => "pagedown".to_string(),
        KeyCode::Up => "up".to_string(),
        KeyCode::Down => "down".to_string(),
        KeyCode::Left => "left".to_string(),
        KeyCode::Right => "right".to_string(),
        KeyCode::F(n) => format!("f{n}"),
        _ => return None,
    };
    Some(name)
}

fn keystroke_from_event(event: &KeyEvent) -> Option<ParsedKeystroke> {
    let key = key_name_from_event(event)?;
    let alt_meta = event
        .modifiers
        .intersects(KeyModifiers::ALT | KeyModifiers::META);
    let effective_alt_meta = if event.code == KeyCode::Esc {
        false
    } else {
        alt_meta
    };
    Some(ParsedKeystroke {
        key,
        ctrl: event.modifiers.contains(KeyModifiers::CONTROL),
        alt: effective_alt_meta,
        shift: event.modifiers.contains(KeyModifiers::SHIFT),
        meta: effective_alt_meta,
        super_key: event.modifiers.contains(KeyModifiers::SUPER),
    })
}

fn keystrokes_equal(a: &ParsedKeystroke, b: &ParsedKeystroke) -> bool {
    a.key == b.key
        && a.ctrl == b.ctrl
        && a.shift == b.shift
        && (a.alt || a.meta) == (b.alt || b.meta)
        && a.super_key == b.super_key
}

fn chord_prefix_matches(prefix: &[ParsedKeystroke], binding: &ParsedBinding) -> bool {
    if prefix.len() >= binding.chord.len() {
        return false;
    }
    prefix
        .iter()
        .zip(binding.chord.iter())
        .all(|(a, b)| keystrokes_equal(a, b))
}

fn chord_exactly_matches(chord: &[ParsedKeystroke], binding: &ParsedBinding) -> bool {
    chord.len() == binding.chord.len()
        && chord
            .iter()
            .zip(binding.chord.iter())
            .all(|(a, b)| keystrokes_equal(a, b))
}

#[derive(Clone, Debug, Default)]
struct KeybindingRuntimeState {
    pending_chord: Option<(ParsedChord, Instant)>,
    active_contexts: HashSet<String>,
}

/// Copyable handle to the action-based keybinding registry.
#[derive(Clone, Default)]
pub struct KeybindingContext {
    state: Option<super::State<KeybindingRuntimeState>>,
    bindings: Arc<Vec<ParsedBinding>>,
}

impl KeybindingContext {
    /// Creates a disabled no-op keybinding context.
    pub fn disabled() -> Self {
        Self {
            state: None,
            bindings: Arc::new(Vec::new()),
        }
    }

    fn new(state: super::State<KeybindingRuntimeState>, bindings: Vec<ParsedBinding>) -> Self {
        Self {
            state: Some(state),
            bindings: Arc::new(bindings),
        }
    }

    /// Returns whether this handle is backed by a live provider.
    pub fn is_enabled(&self) -> bool {
        self.state.is_some()
    }

    fn with_ref<R>(&self, f: impl FnOnce(&KeybindingRuntimeState) -> R) -> Option<R> {
        let state = self.state?;
        let guard = state.try_read()?;
        Some(f(&guard))
    }

    fn with_mut<R>(&self, f: impl FnOnce(&mut KeybindingRuntimeState) -> R) -> Option<R> {
        let mut state = self.state?;
        let mut guard = state.try_write()?;
        Some(f(&mut guard))
    }

    /// Registers an active context. Active contexts are included before the
    /// caller's own context and `Global` when resolving actions.
    pub fn register_active_context(&self, context: impl Into<String>) {
        let context = context.into();
        self.with_mut(|state| {
            state.active_contexts.insert(context);
        });
    }

    /// Unregisters an active context.
    pub fn unregister_active_context(&self, context: &str) {
        self.with_mut(|state| {
            state.active_contexts.remove(context);
        });
    }

    /// Returns the current active context names.
    pub fn active_contexts(&self) -> Vec<String> {
        self.with_ref(|state| state.active_contexts.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Returns the currently pending chord, if any and not timed out.
    pub fn pending_chord(&self) -> Option<ParsedChord> {
        self.with_ref(|state| {
            state.pending_chord.as_ref().and_then(|(pending, started)| {
                (started.elapsed() <= CHORD_TIMEOUT).then(|| pending.clone())
            })
        })
        .flatten()
    }

    fn set_pending_chord(&self, pending: Option<ParsedChord>) {
        self.with_mut(|state| {
            state.pending_chord = pending.map(|chord| (chord, Instant::now()));
        });
    }

    /// Clears pending chord state.
    pub fn clear_pending_chord(&self) {
        self.set_pending_chord(None);
    }

    /// Returns the display string for `action` in `context`, using the last
    /// matching binding just like CC's `getBindingDisplayText(...)`.
    pub fn display_text(&self, action: &str, context: &str) -> Option<String> {
        self.bindings
            .iter()
            .rev()
            .find(|binding| binding.context == context && binding.action.as_deref() == Some(action))
            .map(|binding| keybinding_chord_to_string(&binding.chord))
    }

    /// Resolves a key event against active contexts plus `context` and `Global`.
    pub fn resolve_key_event(&self, event: &KeyEvent, context: &str) -> KeybindingResolveResult {
        self.with_mut(|state| resolve_key_event_with_state(&self.bindings, state, event, context))
            .unwrap_or(KeybindingResolveResult::None)
    }
}

fn resolve_key_event_with_state(
    bindings: &[ParsedBinding],
    state: &mut KeybindingRuntimeState,
    event: &KeyEvent,
    context: &str,
) -> KeybindingResolveResult {
    if event.kind == KeyEventKind::Release {
        return KeybindingResolveResult::None;
    }
    let pending = state.pending_chord.as_ref().and_then(|(pending, started)| {
        (started.elapsed() <= CHORD_TIMEOUT).then(|| pending.clone())
    });
    if pending.is_none() && state.pending_chord.is_some() {
        state.pending_chord = None;
    }
    if event.code == KeyCode::Esc && pending.is_some() {
        state.pending_chord = None;
        return KeybindingResolveResult::ChordCancelled;
    }

    let Some(current) = keystroke_from_event(event) else {
        if pending.is_some() {
            state.pending_chord = None;
            return KeybindingResolveResult::ChordCancelled;
        }
        return KeybindingResolveResult::None;
    };
    let mut test_chord = pending.unwrap_or_default();
    test_chord.push(current);

    let mut contexts = state.active_contexts.iter().cloned().collect::<Vec<_>>();
    contexts.push(context.to_string());
    contexts.push("Global".to_string());
    let context_set: HashSet<_> = contexts.into_iter().collect();

    let context_bindings = bindings
        .iter()
        .filter(|binding| context_set.contains(&binding.context))
        .collect::<Vec<_>>();

    let mut chord_winners: Vec<(String, Option<String>)> = Vec::new();
    for binding in &context_bindings {
        if binding.chord.len() > test_chord.len() && chord_prefix_matches(&test_chord, binding) {
            let display = keybinding_chord_to_string(&binding.chord);
            if let Some((_, action)) = chord_winners.iter_mut().find(|(key, _)| key == &display) {
                *action = binding.action.clone();
            } else {
                chord_winners.push((display, binding.action.clone()));
            }
        }
    }
    if chord_winners.iter().any(|(_, action)| action.is_some()) {
        state.pending_chord = Some((test_chord.clone(), Instant::now()));
        return KeybindingResolveResult::ChordStarted {
            pending: test_chord,
        };
    }

    let exact = context_bindings
        .iter()
        .filter(|binding| chord_exactly_matches(&test_chord, binding))
        .last();
    if let Some(binding) = exact {
        state.pending_chord = None;
        if let Some(action) = &binding.action {
            return KeybindingResolveResult::Match {
                action: action.clone(),
            };
        }
        return KeybindingResolveResult::Unbound;
    }

    if test_chord.len() > 1 {
        state.pending_chord = None;
        return KeybindingResolveResult::ChordCancelled;
    }
    KeybindingResolveResult::None
}

/// Creates an action keybinding context owned by the current component.
pub fn create_keybinding_context(
    hooks: &mut Hooks<'_, '_>,
    bindings: Vec<ParsedBinding>,
) -> KeybindingContext {
    KeybindingContext::new(hooks.use_state(KeybindingRuntimeState::default), bindings)
}

/// Options for action keybinding hooks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActionKeybindingOptions {
    /// Binding context for this handler. Defaults to `Global`.
    pub context: String,
    /// Whether the handler should observe events.
    pub active: bool,
}

impl Default for ActionKeybindingOptions {
    fn default() -> Self {
        Self {
            context: "Global".to_string(),
            active: true,
        }
    }
}

#[derive(Default)]
struct ActiveContextRegistrationHook {
    keybindings: KeybindingContext,
    context: Option<String>,
    active: bool,
}

impl ActiveContextRegistrationHook {
    fn update(&mut self, keybindings: KeybindingContext, context: &str, active: bool) {
        if self.active && self.context.as_deref() != Some(context) {
            if let Some(old) = self.context.take() {
                self.keybindings.unregister_active_context(&old);
            }
            self.active = false;
        }
        self.keybindings = keybindings;
        if active && !self.active {
            self.keybindings
                .register_active_context(context.to_string());
            self.context = Some(context.to_string());
            self.active = true;
        } else if !active && self.active {
            if let Some(old) = self.context.take() {
                self.keybindings.unregister_active_context(&old);
            }
            self.active = false;
        }
    }
}

impl Drop for ActiveContextRegistrationHook {
    fn drop(&mut self) {
        if self.active {
            if let Some(context) = self.context.take() {
                self.keybindings.unregister_active_context(&context);
            }
        }
    }
}

impl Hook for ActiveContextRegistrationHook {}

/// Hooks for CC-style action keybindings.
pub trait UseActionKeybindings: private::Sealed {
    /// Returns the nearest action keybinding context, or a disabled no-op handle.
    fn use_keybinding_context(&mut self) -> KeybindingContext;

    /// Registers a context as active while this hook is mounted.
    fn use_register_keybinding_context(&mut self, context: &str, active: bool);

    /// Registers an action handler in the Global context.
    fn use_action_keybinding<F>(&mut self, action: &str, handler: F)
    where
        F: FnMut() + Send + 'static;

    /// Registers an action handler with explicit context/active options.
    ///
    /// Returning `false` lets the event fall through to later handlers, matching
    /// CC's `useKeybindings` convention.
    fn use_action_keybinding_with_options<F>(
        &mut self,
        action: &str,
        options: ActionKeybindingOptions,
        handler: F,
    ) where
        F: FnMut() -> bool + Send + 'static;
}

impl UseActionKeybindings for Hooks<'_, '_> {
    fn use_keybinding_context(&mut self) -> KeybindingContext {
        self.try_use_context::<KeybindingContext>()
            .map(|context| context.clone())
            .unwrap_or_else(KeybindingContext::disabled)
    }

    fn use_register_keybinding_context(&mut self, context: &str, active: bool) {
        let keybindings = self.use_keybinding_context();
        let hook = self.use_hook(ActiveContextRegistrationHook::default);
        hook.update(keybindings, context, active);
    }

    fn use_action_keybinding<F>(&mut self, action: &str, mut handler: F)
    where
        F: FnMut() + Send + 'static,
    {
        self.use_action_keybinding_with_options(
            action,
            ActionKeybindingOptions::default(),
            move || {
                handler();
                true
            },
        );
    }

    fn use_action_keybinding_with_options<F>(
        &mut self,
        action: &str,
        options: ActionKeybindingOptions,
        mut handler: F,
    ) where
        F: FnMut() -> bool + Send + 'static,
    {
        let action = action.to_string();
        let keybindings = self.use_keybinding_context();
        self.use_propagated_terminal_events(move |propagated: &PropagatedTerminalEvent| {
            if !options.active || !keybindings.is_enabled() || propagated.is_propagation_stopped() {
                return;
            }
            let crate::TerminalEvent::Key(event) = propagated.event() else {
                return;
            };
            match keybindings.resolve_key_event(event, &options.context) {
                KeybindingResolveResult::Match { action: matched } if matched == action => {
                    if handler() {
                        propagated.stop_propagation();
                    }
                }
                KeybindingResolveResult::ChordStarted { .. } | KeybindingResolveResult::Unbound => {
                    propagated.stop_propagation();
                }
                KeybindingResolveResult::ChordCancelled => {
                    // Cancel consumes Escape/invalid continuation only while a chord was active.
                    propagated.stop_propagation();
                }
                _ => {}
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctrl(ch: char) -> KeyEvent {
        let mut event = KeyEvent::new(KeyEventKind::Press, KeyCode::Char(ch));
        event.modifiers = KeyModifiers::CONTROL;
        event
    }

    #[test]
    fn parse_chord_aliases_and_display() {
        let chord = parse_keybinding_chord("ctrl+x ctrl+k");
        assert_eq!(keybinding_chord_to_string(&chord), "ctrl+x ctrl+k");
        let space = parse_keybinding_chord(" ");
        assert_eq!(keybinding_chord_to_string(&space), "Space");
        let arrows = parse_keybinding_chord("shift+↑");
        assert_eq!(keybinding_chord_to_string(&arrows), "shift+↑");
    }

    #[test]
    fn action_keybinding_chord_resolves_registered_action() {
        let bindings = parse_keybinding_blocks(&[KeybindingBlock::new(
            "Chat",
            vec![KeybindingEntry::new("ctrl+x ctrl+k", "chat:killAgents")],
        )]);
        let mut state = KeybindingRuntimeState::default();
        state.active_contexts.insert("Chat".to_string());

        assert!(matches!(
            resolve_key_event_with_state(&bindings, &mut state, &ctrl('x'), "Chat"),
            KeybindingResolveResult::ChordStarted { .. }
        ));
        assert_eq!(
            resolve_key_event_with_state(&bindings, &mut state, &ctrl('k'), "Chat"),
            KeybindingResolveResult::Match {
                action: "chat:killAgents".to_string()
            }
        );
    }

    #[test]
    fn action_keybinding_active_context_later_binding_wins_over_global() {
        let bindings = parse_keybinding_blocks(&[
            KeybindingBlock::new("Global", vec![KeybindingEntry::new("ctrl+t", "app:global")]),
            KeybindingBlock::new(
                "Palette",
                vec![KeybindingEntry::new("ctrl+t", "palette:toggle")],
            ),
        ]);
        let mut state = KeybindingRuntimeState::default();
        state.active_contexts.insert("Palette".to_string());

        assert_eq!(
            resolve_key_event_with_state(&bindings, &mut state, &ctrl('t'), "Global"),
            KeybindingResolveResult::Match {
                action: "palette:toggle".to_string()
            }
        );
    }

    #[test]
    fn explicit_unbinding_suppresses_longer_chord_prefix() {
        let bindings = parse_keybinding_blocks(&[KeybindingBlock::new(
            "Global",
            vec![
                KeybindingEntry::new("ctrl+x ctrl+k", "chat:killAgents"),
                KeybindingEntry::unbound("ctrl+x ctrl+k"),
                KeybindingEntry::new("ctrl+x", "prefix:single"),
            ],
        )]);
        let mut state = KeybindingRuntimeState::default();
        assert_eq!(
            resolve_key_event_with_state(&bindings, &mut state, &ctrl('x'), "Global"),
            KeybindingResolveResult::Match {
                action: "prefix:single".to_string()
            }
        );
    }
}
