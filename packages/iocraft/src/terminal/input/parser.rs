use super::*;

/// Action encoded by an SGR mouse input sequence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminalParsedMouseAction {
    /// `M` terminator: button press or drag/motion update.
    Press,
    /// `m` terminator: button release.
    Release,
}

/// SGR mouse event parsed from raw terminal input.
///
/// This mirrors CC Ink's `ParsedMouse`: `button` is the raw SGR button code,
/// and `column` / `row` are the 1-indexed coordinates reported by the
/// terminal sequence. Wheel events are intentionally left as raw sequences so a
/// higher key parser can route them as wheel-up/wheel-down keys, matching CC
/// Ink's `parseMouseEvent(...)` split.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalParsedMouse {
    /// Raw SGR button code. Low bits identify the button; bit `0x20` marks
    /// drag/motion; bit `0x40` marks wheel events.
    pub button: u16,
    /// Whether the sequence ended with press/drag (`M`) or release (`m`).
    pub action: TerminalParsedMouseAction,
    /// 1-indexed terminal column from the SGR sequence.
    pub column: u16,
    /// 1-indexed terminal row from the SGR sequence.
    pub row: u16,
    /// Original escape sequence bytes decoded as UTF-8.
    pub sequence: String,
}

/// Keypress parsed from a raw terminal input sequence.
///
/// This mirrors CC Ink's `ParsedKey` shape closely enough for custom stdin
/// frontends: printable keys use their literal sequence, named/special keys set
/// [`Self::name`], CSI-u / modifyOtherKeys modifiers are decoded, and raw mouse
/// wheel sequences become `wheelup` / `wheeldown` keys.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TerminalParsedKey {
    /// CC Ink-style key name (`"return"`, `"left"`, `"space"`, `"f1"`, etc.).
    /// `None` means the sequence is ordinary text or an unmapped function key.
    pub name: Option<String>,
    /// Whether Ctrl/Control was encoded.
    pub ctrl: bool,
    /// Whether Alt/Option was encoded.
    pub meta: bool,
    /// Whether Shift was encoded or inferred for uppercase ASCII.
    pub shift: bool,
    /// Historical CC Ink `option` flag for double-ESC function-key sequences.
    pub option: bool,
    /// Whether Super/Cmd/Win was encoded by CSI-u / modifyOtherKeys.
    pub super_key: bool,
    /// Whether the parsed name is an F-key.
    pub fn_key: bool,
    /// Raw escape sequence or text used to parse this key.
    pub sequence: Option<String>,
    /// Raw sequence before CC Ink's special `return` normalization.
    pub raw: Option<String>,
    /// Function-key code fragment such as `"[D"` or `"[15~"`.
    pub code: Option<String>,
    /// Whether this key came from a bracketed paste. `parse_terminal_key_sequence`
    /// always returns `false`; paste grouping is represented by
    /// [`TerminalParsedInput::Paste`].
    pub is_pasted: bool,
}

impl TerminalParsedKey {
    /// Converts this parsed keypress into the CC Ink `InputEvent`-style key
    /// flags and text input string.
    pub fn to_input_event(&self) -> TerminalParsedInputEvent {
        terminal_parsed_key_to_input_event(self)
    }
}

/// CC Ink `InputEvent.key`-style flags derived from a [`TerminalParsedKey`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TerminalParsedInputKey {
    /// Up arrow key.
    pub up_arrow: bool,
    /// Down arrow key.
    pub down_arrow: bool,
    /// Left arrow key.
    pub left_arrow: bool,
    /// Right arrow key.
    pub right_arrow: bool,
    /// Page Down key.
    pub page_down: bool,
    /// Page Up key.
    pub page_up: bool,
    /// Mouse wheel up event.
    pub wheel_up: bool,
    /// Mouse wheel down event.
    pub wheel_down: bool,
    /// Home key.
    pub home: bool,
    /// End key.
    pub end: bool,
    /// Enter/Return key.
    pub return_key: bool,
    /// Escape key.
    pub escape: bool,
    /// Whether Ctrl/Control was held.
    pub ctrl: bool,
    /// Whether Shift was held or inferred from uppercase input.
    pub shift: bool,
    /// Function key (`F1`-style).
    pub fn_key: bool,
    /// Tab key.
    pub tab: bool,
    /// Backspace key.
    pub backspace: bool,
    /// Delete key.
    pub delete: bool,
    /// Alt/Option/meta key. Escape itself also sets this, matching CC Ink.
    pub meta: bool,
    /// Super/Cmd/Win key.
    pub super_key: bool,
}

/// CC Ink `InputEvent`-style result for a raw terminal key sequence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalParsedInputEvent {
    /// Parsed keypress metadata.
    pub keypress: TerminalParsedKey,
    /// Derived key flags.
    pub key: TerminalParsedInputKey,
    /// Text input delivered to high-level handlers after CC Ink's filtering
    /// rules for special keys, meta prefixes, CSI-u, and modifyOtherKeys.
    pub input: String,
}

/// Parses a single raw terminal input token as a CC Ink-style keypress.
///
/// Pair this with [`TerminalInputTokenizer::feed`] when a custom frontend wants
/// key interpretation instead of just sequence boundary splitting. It is
/// mode-neutral and performs no terminal I/O.
pub fn parse_terminal_key_sequence(sequence: &str) -> TerminalParsedKey {
    parse_terminal_key_sequence_impl(sequence)
}

/// Parses a single raw terminal input token into a CC Ink `InputEvent`-style
/// key/input pair.
pub fn parse_terminal_input_event(sequence: &str) -> TerminalParsedInputEvent {
    parse_terminal_key_sequence(sequence).to_input_event()
}

/// Converts raw input bytes using CC Ink's `inputToString(Buffer)` rules.
///
/// A single byte with the high bit set is interpreted as an ESC-prefixed Meta
/// key by subtracting 128 from the byte, matching the fork's legacy stdin path.
/// Other byte chunks are decoded as UTF-8, replacing malformed sequences just
/// like JavaScript `String(buffer)` / `Buffer.toString('utf8')`.
pub fn terminal_input_bytes_to_string(input: &[u8]) -> String {
    if input.len() == 1 && input[0] > 127 {
        let mut output = String::from("\x1b");
        output.push((input[0] - 128) as char);
        output
    } else {
        String::from_utf8_lossy(input).into_owned()
    }
}

/// Raw terminal input after CC Ink-style response, paste, and mouse parsing.
///
/// A sequence recognized by [`parse_terminal_response`] is emitted as
/// [`TerminalParsedInput::Response`] and should not be treated as a keypress or
/// literal prompt text. Bracketed paste and non-wheel SGR mouse input are also
/// separated from ordinary text/escape sequences for custom frontends.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TerminalParsedInput {
    /// Ordinary text bytes decoded as UTF-8.
    Text(String),
    /// A complete non-response escape/control sequence beginning with ESC.
    Sequence(String),
    /// A high-level key/input event parsed from ordinary text or an escape sequence.
    Key(TerminalParsedInputEvent),
    /// A bracketed paste payload. Escape sequences between
    /// [`BRACKETED_PASTE_START`] and [`BRACKETED_PASTE_END`] are preserved as
    /// literal text, matching CC Ink's `parseMultipleKeypresses(...)`.
    Paste(String),
    /// A parsed non-wheel SGR mouse event.
    Mouse(TerminalParsedMouse),
    /// A parsed terminal query response.
    Response(TerminalResponse),
}

impl TerminalParsedInput {
    /// Converts parsed raw input into iocraft terminal events.
    ///
    /// This is a convenience bridge for custom raw-stdin frontends: responses,
    /// paste payloads, SGR/X10 wheel reports, and non-wheel SGR mouse events can
    /// be forwarded into the normal iocraft event system. Key/input events are
    /// mapped to crossterm-style [`TerminalEvent::Key`] values where possible;
    /// batched printable text is split into per-character key events to match
    /// crossterm's event model. Use [`TerminalInputParser::feed`] directly when
    /// you need the exact CC Ink `InputEvent` batch shape.
    pub fn into_terminal_events(self) -> Vec<TerminalEvent> {
        terminal_parsed_input_to_events(self)
    }
}

/// High-level CC Ink-style streaming parser for raw terminal input.
///
/// This is the Rust counterpart to `parseMultipleKeypresses(...)`: it owns a
/// [`TerminalInputTokenizer`], groups bracketed paste, parses terminal query
/// responses, separates non-wheel SGR mouse events, and converts remaining text
/// or escape sequences into [`TerminalParsedInputEvent`] values.
///
/// It is mode-neutral and performs no terminal I/O. Use it in custom raw stdin
/// frontends before forwarding responses/mouse/key events into an application.
#[derive(Clone, Debug)]
pub struct TerminalInputParser {
    tokenizer: TerminalInputTokenizer,
    in_paste: bool,
    paste_buffer: String,
}

impl Default for TerminalInputParser {
    fn default() -> Self {
        Self::new()
    }
}

impl TerminalInputParser {
    /// Creates a stdin-oriented parser. X10 mouse payload handling is enabled,
    /// matching CC Ink's `createTokenizer({x10Mouse: true})` for input streams.
    pub fn new() -> Self {
        Self::with_x10_mouse(true)
    }

    /// Creates a parser with explicit X10 mouse tokenization control.
    pub fn with_x10_mouse(x10_mouse: bool) -> Self {
        Self {
            tokenizer: TerminalInputTokenizer::with_x10_mouse(x10_mouse),
            in_paste: false,
            paste_buffer: String::new(),
        }
    }

    /// Feeds a raw input chunk and returns parsed key/paste/mouse/response events.
    pub fn feed(&mut self, input: &str) -> Vec<TerminalParsedInput> {
        let tokens = self.tokenizer.feed(input);
        tokens_to_parsed_inputs(
            tokens,
            &mut self.in_paste,
            &mut self.paste_buffer,
            false,
            true,
        )
    }

    /// Feeds raw input bytes after applying CC Ink's `inputToString(Buffer)` rules.
    pub fn feed_bytes(&mut self, input: &[u8]) -> Vec<TerminalParsedInput> {
        self.feed(&terminal_input_bytes_to_string(input))
    }

    /// Feeds a raw input chunk and converts parsed input into iocraft terminal events.
    ///
    /// This is useful for custom frontends that own raw stdin but want to reuse
    /// iocraft's normal event propagation hooks. For exact CC Ink-style batched
    /// `input` strings, use [`Self::feed`] instead.
    pub fn feed_events(&mut self, input: &str) -> Vec<TerminalEvent> {
        terminal_parsed_inputs_to_events(self.feed(input))
    }

    /// Feeds raw input bytes and converts parsed input into iocraft terminal events.
    pub fn feed_bytes_events(&mut self, input: &[u8]) -> Vec<TerminalEvent> {
        terminal_parsed_inputs_to_events(self.feed_bytes(input))
    }

    /// Flushes buffered input. Unterminated paste payloads are emitted when non-empty.
    pub fn flush(&mut self) -> Vec<TerminalParsedInput> {
        let tokens = self.tokenizer.flush();
        tokens_to_parsed_inputs(
            tokens,
            &mut self.in_paste,
            &mut self.paste_buffer,
            true,
            true,
        )
    }

    /// Flushes buffered input and converts it into iocraft terminal events.
    pub fn flush_events(&mut self) -> Vec<TerminalEvent> {
        terminal_parsed_inputs_to_events(self.flush())
    }

    /// Clears tokenizer and paste state.
    pub fn reset(&mut self) {
        self.tokenizer.reset();
        self.in_paste = false;
        self.paste_buffer.clear();
    }

    /// Returns the tokenizer's currently buffered incomplete sequence, if any.
    pub fn buffered(&self) -> &str {
        self.tokenizer.buffered()
    }

    /// Returns whether an incomplete escape/control sequence is buffered.
    pub fn has_incomplete_sequence(&self) -> bool {
        self.tokenizer.has_incomplete_sequence()
    }

    /// Returns whether the parser is currently inside bracketed paste mode.
    pub fn is_in_paste(&self) -> bool {
        self.in_paste
    }

    /// Returns the CC Ink-style timeout a custom frontend should use before
    /// calling [`Self::flush`] / [`Self::flush_events`], if a sequence is incomplete.
    ///
    /// Match `App.tsx`: use 50ms normally and 500ms while in bracketed paste.
    /// If the underlying stdin still reports queued bytes, re-arm this timeout
    /// instead of flushing so delayed mouse/CSI continuations are not split.
    pub fn pending_flush_timeout(&self) -> Option<Duration> {
        self.has_incomplete_sequence().then_some(if self.in_paste {
            TERMINAL_INPUT_PASTE_TIMEOUT
        } else {
            TERMINAL_INPUT_NORMAL_TIMEOUT
        })
    }

    /// Returns whether an expired incomplete-sequence timer should actually flush.
    ///
    /// Pass `true` when the underlying input source reports queued bytes (for
    /// example Node's `stdin.readableLength > 0`). In that case CC Ink re-arms
    /// the timer instead of flushing a likely-continuing ESC/CSI sequence.
    pub fn should_flush_incomplete(&self, input_available: bool) -> bool {
        self.has_incomplete_sequence() && !input_available
    }
}

/// Converts a batch of parsed raw input into iocraft terminal events.
///
/// This is a mode-neutral bridge for custom raw-stdin plumbing. It does not
/// enable raw mode or write terminal escape sequences; it only translates the
/// already-parsed output of [`TerminalInputParser`] into events accepted by
/// iocraft's existing hooks and render loop.
pub fn terminal_parsed_inputs_to_events<I>(inputs: I) -> Vec<TerminalEvent>
where
    I: IntoIterator<Item = TerminalParsedInput>,
{
    let mut events = Vec::new();
    for input in inputs {
        events.extend(terminal_parsed_input_to_events(input));
    }
    events
}

/// Converts one parsed raw input item into zero or more iocraft terminal events.
pub fn terminal_parsed_input_to_events(input: TerminalParsedInput) -> Vec<TerminalEvent> {
    match input {
        TerminalParsedInput::Text(text) => text_to_key_events(&text),
        TerminalParsedInput::Sequence(sequence) => {
            parsed_input_event_to_terminal_events(parse_terminal_input_event(&sequence))
        }
        TerminalParsedInput::Key(event) => parsed_input_event_to_terminal_events(event),
        TerminalParsedInput::Paste(text) => vec![TerminalEvent::Paste(text)],
        TerminalParsedInput::Mouse(mouse) => vec![TerminalEvent::FullscreenMouse(
            sgr_mouse_to_fullscreen_mouse_event(&mouse),
        )],
        TerminalParsedInput::Response(response) => vec![TerminalEvent::Response(response)],
    }
}

fn parsed_input_event_to_terminal_events(event: TerminalParsedInputEvent) -> Vec<TerminalEvent> {
    if let Some(mouse) = parsed_input_event_to_wheel_mouse_event(&event) {
        return vec![TerminalEvent::FullscreenMouse(mouse)];
    }

    let modifiers = parsed_key_modifiers(&event.keypress);
    if let Some(code) = parsed_input_event_key_code(&event) {
        return vec![TerminalEvent::Key(KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
        })];
    }

    text_to_key_events_with_modifiers(&event.input, modifiers)
}

fn text_to_key_events(text: &str) -> Vec<TerminalEvent> {
    text_to_key_events_with_modifiers(text, KeyModifiers::empty())
}

fn text_to_key_events_with_modifiers(text: &str, modifiers: KeyModifiers) -> Vec<TerminalEvent> {
    text.chars()
        .map(|ch| {
            TerminalEvent::Key(KeyEvent {
                code: KeyCode::Char(ch),
                modifiers,
                kind: KeyEventKind::Press,
            })
        })
        .collect()
}

fn parsed_input_event_key_code(event: &TerminalParsedInputEvent) -> Option<KeyCode> {
    let key = &event.key;
    if key.up_arrow {
        return Some(KeyCode::Up);
    }
    if key.down_arrow {
        return Some(KeyCode::Down);
    }
    if key.left_arrow {
        return Some(KeyCode::Left);
    }
    if key.right_arrow {
        return Some(KeyCode::Right);
    }
    if key.page_up {
        return Some(KeyCode::PageUp);
    }
    if key.page_down {
        return Some(KeyCode::PageDown);
    }
    if key.home {
        return Some(KeyCode::Home);
    }
    if key.end {
        return Some(KeyCode::End);
    }
    if key.return_key || event.keypress.name.as_deref() == Some("enter") {
        return Some(KeyCode::Enter);
    }
    if key.escape {
        return Some(KeyCode::Esc);
    }
    if key.backspace {
        return Some(KeyCode::Backspace);
    }
    if key.delete {
        return Some(KeyCode::Delete);
    }
    if key.tab {
        return Some(if key.shift {
            KeyCode::BackTab
        } else {
            KeyCode::Tab
        });
    }
    if event.keypress.name.as_deref() == Some("insert") {
        return Some(KeyCode::Insert);
    }
    if key.fn_key {
        if let Some(number) = event
            .keypress
            .name
            .as_deref()
            .and_then(|name| name.strip_prefix('f'))
            .and_then(|number| number.parse::<u8>().ok())
        {
            return Some(KeyCode::F(number));
        }
    }
    if let Some(ch) = single_char(&event.input) {
        return Some(KeyCode::Char(ch));
    }
    None
}

fn parsed_key_modifiers(keypress: &TerminalParsedKey) -> KeyModifiers {
    let mut modifiers = KeyModifiers::empty();
    if keypress.ctrl {
        modifiers.insert(KeyModifiers::CONTROL);
    }
    if keypress.meta || keypress.option {
        modifiers.insert(KeyModifiers::ALT);
    }
    if keypress.shift {
        modifiers.insert(KeyModifiers::SHIFT);
    }
    if keypress.super_key {
        modifiers.insert(KeyModifiers::SUPER);
    }
    modifiers
}

fn sgr_mouse_to_fullscreen_mouse_event(mouse: &TerminalParsedMouse) -> FullscreenMouseEvent {
    let kind = if mouse.button & 0x20 != 0 && mouse.button & 0x03 == 0x03 {
        MouseEventKind::Moved
    } else if mouse.button & 0x20 != 0 {
        MouseEventKind::Drag(mouse_button_from_sgr_code(mouse.button))
    } else {
        match mouse.action {
            TerminalParsedMouseAction::Press => {
                MouseEventKind::Down(mouse_button_from_sgr_code(mouse.button))
            }
            TerminalParsedMouseAction::Release => {
                MouseEventKind::Up(mouse_button_from_sgr_code(mouse.button))
            }
        }
    };

    FullscreenMouseEvent {
        modifiers: mouse_modifiers_from_sgr_code(mouse.button),
        column: mouse.column.saturating_sub(1),
        row: mouse.row.saturating_sub(1),
        cell_is_blank: false,
        kind,
    }
}

fn parsed_input_event_to_wheel_mouse_event(
    event: &TerminalParsedInputEvent,
) -> Option<FullscreenMouseEvent> {
    if !event.key.wheel_up && !event.key.wheel_down {
        return None;
    }
    let sequence = event.keypress.sequence.as_deref()?;
    if let Some((button, column, row, _)) = parse_sgr_mouse_parts(sequence) {
        return Some(FullscreenMouseEvent {
            modifiers: mouse_modifiers_from_sgr_code(button),
            column: column.saturating_sub(1),
            row: row.saturating_sub(1),
            cell_is_blank: false,
            kind: if event.key.wheel_up {
                MouseEventKind::ScrollUp
            } else {
                MouseEventKind::ScrollDown
            },
        });
    }
    if let Some((button, column, row)) = parse_x10_mouse_parts(sequence) {
        return Some(FullscreenMouseEvent {
            modifiers: mouse_modifiers_from_sgr_code(button),
            column: column.saturating_sub(1),
            row: row.saturating_sub(1),
            cell_is_blank: false,
            kind: if event.key.wheel_up {
                MouseEventKind::ScrollUp
            } else {
                MouseEventKind::ScrollDown
            },
        });
    }
    None
}

fn mouse_button_from_sgr_code(button: u16) -> MouseButton {
    match button & 0x03 {
        1 => MouseButton::Middle,
        2 => MouseButton::Right,
        _ => MouseButton::Left,
    }
}

fn mouse_modifiers_from_sgr_code(button: u16) -> KeyModifiers {
    let mut modifiers = KeyModifiers::empty();
    if button & 0x04 != 0 {
        modifiers.insert(KeyModifiers::SHIFT);
    }
    if button & 0x08 != 0 {
        modifiers.insert(KeyModifiers::ALT);
    }
    if button & 0x10 != 0 {
        modifiers.insert(KeyModifiers::CONTROL);
    }
    modifiers
}

fn parse_x10_mouse_parts(sequence: &str) -> Option<(u16, u16, u16)> {
    let bytes = sequence.as_bytes();
    if bytes.len() != 6 || !bytes.starts_with(b"\x1b[M") {
        return None;
    }
    Some((
        bytes[3].saturating_sub(32) as u16,
        bytes[4].saturating_sub(32) as u16,
        bytes[5].saturating_sub(32) as u16,
    ))
}

pub(super) fn tokens_to_parsed_inputs(
    tokens: Vec<TerminalInputToken>,
    in_paste: &mut bool,
    paste_buffer: &mut String,
    flush: bool,
    parse_keys: bool,
) -> Vec<TerminalParsedInput> {
    let mut parsed = Vec::new();
    for token in tokens {
        match token {
            TerminalInputToken::Text(text) if *in_paste => paste_buffer.push_str(&text),
            TerminalInputToken::Text(text) if parse_keys => {
                if let Some(sequence) = resynthesize_orphan_mouse_tail(&text) {
                    push_parsed_sequence(&mut parsed, sequence, true);
                } else {
                    parsed.push(TerminalParsedInput::Key(parse_terminal_input_event(&text)));
                }
            }
            TerminalInputToken::Text(text) => {
                if let Some(sequence) = resynthesize_orphan_mouse_tail(&text) {
                    push_parsed_sequence(&mut parsed, sequence, false);
                } else {
                    parsed.push(TerminalParsedInput::Text(text));
                }
            }
            TerminalInputToken::Sequence(sequence) if sequence == BRACKETED_PASTE_START => {
                *in_paste = true;
                paste_buffer.clear();
            }
            TerminalInputToken::Sequence(sequence) if sequence == BRACKETED_PASTE_END => {
                parsed.push(TerminalParsedInput::Paste(std::mem::take(paste_buffer)));
                *in_paste = false;
            }
            TerminalInputToken::Sequence(sequence) if *in_paste => paste_buffer.push_str(&sequence),
            TerminalInputToken::Sequence(sequence) => {
                push_parsed_sequence(&mut parsed, sequence, parse_keys)
            }
        }
    }

    if flush && *in_paste && !paste_buffer.is_empty() {
        parsed.push(TerminalParsedInput::Paste(std::mem::take(paste_buffer)));
        *in_paste = false;
    }

    parsed
}

fn push_parsed_sequence(parsed: &mut Vec<TerminalParsedInput>, sequence: String, parse_keys: bool) {
    if let Some(response) = parse_terminal_response(&sequence) {
        parsed.push(TerminalParsedInput::Response(response));
    } else if let Some(mouse) = parse_sgr_mouse_sequence(&sequence) {
        parsed.push(TerminalParsedInput::Mouse(mouse));
    } else if parse_keys {
        parsed.push(TerminalParsedInput::Key(parse_terminal_input_event(
            &sequence,
        )));
    } else {
        parsed.push(TerminalParsedInput::Sequence(sequence));
    }
}

fn parse_sgr_mouse_sequence(sequence: &str) -> Option<TerminalParsedMouse> {
    let (button, column, row, action) = parse_sgr_mouse_parts(sequence)?;
    if button & 0x40 != 0 {
        return None;
    }
    Some(TerminalParsedMouse {
        button,
        action,
        column,
        row,
        sequence: sequence.to_string(),
    })
}

fn resynthesize_orphan_mouse_tail(text: &str) -> Option<String> {
    if text.starts_with("[<") {
        let sequence = format!("\x1b{text}");
        return parse_sgr_mouse_parts(&sequence).map(|_| sequence);
    }

    let mut chars = text.chars();
    if chars.next()? != '[' || chars.next()? != 'M' {
        return None;
    }
    let button = chars.next()?;
    let x = chars.next()?;
    let y = chars.next()?;
    if chars.next().is_some() || !(('\u{60}'..='\u{7f}').contains(&button)) {
        return None;
    }
    Some(format!("\x1b[M{button}{x}{y}"))
}

fn parse_sgr_mouse_parts(sequence: &str) -> Option<(u16, u16, u16, TerminalParsedMouseAction)> {
    let body = sequence.strip_prefix("\x1b[<")?;
    let terminator = body.chars().next_back()?;
    let action = match terminator {
        'M' => TerminalParsedMouseAction::Press,
        'm' => TerminalParsedMouseAction::Release,
        _ => return None,
    };
    let params = &body[..body.len() - terminator.len_utf8()];
    let mut parts = params.split(';');
    let button = parts.next()?.parse::<u16>().ok()?;
    let column = parts.next()?.parse::<u16>().ok()?;
    let row = parts.next()?.parse::<u16>().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((button, column, row, action))
}

#[derive(Clone, Copy, Debug, Default)]
struct ParsedModifierFlags {
    shift: bool,
    meta: bool,
    ctrl: bool,
    super_key: bool,
}

fn terminal_parsed_key_to_input_event(keypress: &TerminalParsedKey) -> TerminalParsedInputEvent {
    let name = keypress.name.as_deref();
    let mut key = TerminalParsedInputKey {
        up_arrow: name == Some("up"),
        down_arrow: name == Some("down"),
        left_arrow: name == Some("left"),
        right_arrow: name == Some("right"),
        page_down: name == Some("pagedown"),
        page_up: name == Some("pageup"),
        wheel_up: name == Some("wheelup"),
        wheel_down: name == Some("wheeldown"),
        home: name == Some("home"),
        end: name == Some("end"),
        return_key: name == Some("return"),
        escape: name == Some("escape"),
        ctrl: keypress.ctrl,
        shift: keypress.shift,
        fn_key: keypress.fn_key,
        tab: name == Some("tab"),
        backspace: name == Some("backspace"),
        delete: name == Some("delete"),
        meta: keypress.meta || name == Some("escape") || keypress.option,
        super_key: keypress.super_key,
    };

    let mut input = if keypress.ctrl {
        keypress.name.clone().unwrap_or_default()
    } else {
        keypress.sequence.clone().unwrap_or_default()
    };

    if keypress.ctrl && input == "space" {
        input = " ".to_string();
    }
    if keypress.code.is_some() && keypress.name.is_none() {
        input.clear();
    }
    if keypress.name.is_none() && is_orphan_sgr_mouse_tail(&input) {
        input.clear();
    }
    if input.starts_with('\x1b') {
        input = input['\x1b'.len_utf8()..].to_string();
    }

    let mut processed_as_special_sequence = false;
    if input.starts_with('[')
        && input.chars().nth(1).is_some_and(|ch| ch.is_ascii_digit())
        && input.ends_with('u')
    {
        input = input_for_special_sequence_name(name);
        processed_as_special_sequence = true;
    }

    if input.starts_with("[27;") && input.ends_with('~') {
        input = input_for_special_sequence_name(name);
        processed_as_special_sequence = true;
    }

    if input.starts_with('O')
        && input.chars().count() == 2
        && name.is_some_and(|name| name.chars().count() == 1)
    {
        input = name.unwrap_or_default().to_string();
        processed_as_special_sequence = true;
    }

    if !processed_as_special_sequence && name.is_some_and(is_non_alphanumeric_key_name) {
        input.clear();
    }

    if single_char(&input).is_some_and(|ch| ch.is_ascii_uppercase()) {
        key.shift = true;
    }

    TerminalParsedInputEvent {
        keypress: keypress.clone(),
        key,
        input,
    }
}

fn input_for_special_sequence_name(name: Option<&str>) -> String {
    match name {
        Some("space") => " ".to_string(),
        Some("escape") | None => String::new(),
        Some(name) => name.to_string(),
    }
}

fn is_orphan_sgr_mouse_tail(input: &str) -> bool {
    input.starts_with("[<") && parse_sgr_mouse_parts(&format!("\x1b{input}")).is_some()
}

fn is_non_alphanumeric_key_name(name: &str) -> bool {
    matches!(
        name,
        "up" | "down"
            | "left"
            | "right"
            | "pageup"
            | "pagedown"
            | "home"
            | "end"
            | "insert"
            | "delete"
            | "clear"
            | "tab"
            | "return"
            | "escape"
            | "backspace"
            | "wheelup"
            | "wheeldown"
            | "mouse"
    ) || is_function_key_name(name)
}

fn parse_terminal_key_sequence_impl(sequence: &str) -> TerminalParsedKey {
    if let Some((name, flags)) = parse_csi_u_key(sequence) {
        return key_with_name_and_flags(sequence, name, flags, None);
    }
    if let Some((name, flags)) = parse_modify_other_keys(sequence) {
        return key_with_name_and_flags(sequence, name, flags, None);
    }
    if let Some(name) = parse_wheel_key_name(sequence) {
        return create_nav_key(sequence, name, false);
    }

    let mut key = TerminalParsedKey {
        sequence: Some(sequence.to_string()),
        raw: Some(sequence.to_string()),
        ..Default::default()
    };

    if sequence == "\r" {
        key.raw = None;
        key.name = Some("return".to_string());
    } else if sequence == "\n" {
        key.name = Some("enter".to_string());
    } else if sequence == "\t" {
        key.name = Some("tab".to_string());
    } else if sequence == "\x08" || sequence == "\x1b\x08" {
        key.name = Some("backspace".to_string());
        key.meta = sequence.starts_with('\x1b');
    } else if sequence == "\x7f" || sequence == "\x1b\x7f" {
        key.name = Some("backspace".to_string());
        key.meta = sequence.starts_with('\x1b');
    } else if sequence == "\x1b" || sequence == "\x1b\x1b" {
        key.name = Some("escape".to_string());
        key.meta = sequence.len() == 2;
    } else if sequence == " " || sequence == "\x1b " {
        key.name = Some("space".to_string());
        key.meta = sequence.starts_with('\x1b');
    } else if sequence == "\x1f" {
        key.name = Some("_".to_string());
        key.ctrl = true;
    } else if let Some(ch) = single_char(sequence).filter(|ch| (*ch as u32) <= 0x1a) {
        let name = char::from_u32(ch as u32 + 'a' as u32 - 1).unwrap_or_default();
        key.name = Some(name.to_string());
        key.ctrl = true;
    } else if let Some(ch) = single_char(sequence).filter(|ch| ch.is_ascii_digit()) {
        let _ = ch;
        key.name = Some("number".to_string());
    } else if let Some(ch) = single_char(sequence).filter(|ch| ch.is_ascii_lowercase()) {
        key.name = Some(ch.to_string());
    } else if let Some(ch) = single_char(sequence).filter(|ch| ch.is_ascii_uppercase()) {
        key.name = Some(ch.to_ascii_lowercase().to_string());
        key.shift = true;
    } else if let Some((meta_shift, _meta_ch)) = parse_meta_alnum(sequence) {
        key.meta = true;
        key.shift = meta_shift;
    } else if let Some(parsed) = parse_function_key_sequence(sequence) {
        key = parsed;
    }

    // iTerm natural text editing mode: Option-left/right arrive as ESC b/f.
    if sequence == "\x1bb" {
        key.meta = true;
        key.name = Some("left".to_string());
    } else if sequence == "\x1bf" {
        key.meta = true;
        key.name = Some("right".to_string());
    }

    match sequence {
        "\x1b[1~" => create_nav_key(sequence, "home", false),
        "\x1b[4~" => create_nav_key(sequence, "end", false),
        "\x1b[5~" => create_nav_key(sequence, "pageup", false),
        "\x1b[6~" => create_nav_key(sequence, "pagedown", false),
        "\x1b[1;5D" => create_nav_key(sequence, "left", true),
        "\x1b[1;5C" => create_nav_key(sequence, "right", true),
        _ => {
            key.fn_key = key
                .name
                .as_deref()
                .is_some_and(|name| is_function_key_name(name));
            key
        }
    }
}

fn single_char(sequence: &str) -> Option<char> {
    let mut chars = sequence.chars();
    let ch = chars.next()?;
    chars.next().is_none().then_some(ch)
}

fn parse_csi_u_key(sequence: &str) -> Option<(Option<String>, ParsedModifierFlags)> {
    let body = sequence.strip_prefix("\x1b[")?.strip_suffix('u')?;
    if body.starts_with('?') {
        return None;
    }
    let mut parts = body.split(';');
    let codepoint = parts.next()?.parse::<u32>().ok()?;
    let modifier = parts
        .next()
        .map(|part| part.parse::<u16>().ok())
        .unwrap_or(Some(1))?;
    if parts.next().is_some() {
        return None;
    }
    Some((keycode_to_name(codepoint), decode_key_modifier(modifier)))
}

fn parse_modify_other_keys(sequence: &str) -> Option<(Option<String>, ParsedModifierFlags)> {
    let body = sequence.strip_prefix("\x1b[27;")?.strip_suffix('~')?;
    let mut parts = body.split(';');
    let modifier = parts.next()?.parse::<u16>().ok()?;
    let codepoint = parts.next()?.parse::<u32>().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((keycode_to_name(codepoint), decode_key_modifier(modifier)))
}

fn decode_key_modifier(modifier: u16) -> ParsedModifierFlags {
    let modifier = modifier.saturating_sub(1);
    ParsedModifierFlags {
        shift: modifier & 1 != 0,
        meta: modifier & 2 != 0,
        ctrl: modifier & 4 != 0,
        super_key: modifier & 8 != 0,
    }
}

fn key_with_name_and_flags(
    sequence: &str,
    name: Option<String>,
    flags: ParsedModifierFlags,
    code: Option<String>,
) -> TerminalParsedKey {
    TerminalParsedKey {
        fn_key: name.as_deref().is_some_and(is_function_key_name),
        name,
        ctrl: flags.ctrl,
        meta: flags.meta,
        shift: flags.shift,
        option: false,
        super_key: flags.super_key,
        sequence: Some(sequence.to_string()),
        raw: Some(sequence.to_string()),
        code,
        is_pasted: false,
    }
}

fn keycode_to_name(codepoint: u32) -> Option<String> {
    let name = match codepoint {
        9 => "tab",
        13 => "return",
        27 => "escape",
        32 => "space",
        127 => "backspace",
        57399 => "0",
        57400 => "1",
        57401 => "2",
        57402 => "3",
        57403 => "4",
        57404 => "5",
        57405 => "6",
        57406 => "7",
        57407 => "8",
        57408 => "9",
        57409 => ".",
        57410 => "/",
        57411 => "*",
        57412 => "-",
        57413 => "+",
        57414 => "return",
        57415 => "=",
        32..=126 => {
            return char::from_u32(codepoint).map(|ch| ch.to_ascii_lowercase().to_string());
        }
        _ => return None,
    };
    Some(name.to_string())
}

fn parse_wheel_key_name(sequence: &str) -> Option<&'static str> {
    if let Some((button, _, _, _)) = parse_sgr_mouse_parts(sequence) {
        return match button & 0x43 {
            0x40 => Some("wheelup"),
            0x41 => Some("wheeldown"),
            _ => None,
        };
    }

    let bytes = sequence.as_bytes();
    if bytes.len() == 6 && bytes.starts_with(b"\x1b[M") {
        let button = bytes[3].saturating_sub(32) as u16;
        return match button & 0x43 {
            0x40 => Some("wheelup"),
            0x41 => Some("wheeldown"),
            _ => Some("mouse"),
        };
    }

    None
}

fn parse_meta_alnum(sequence: &str) -> Option<(bool, char)> {
    let mut chars = sequence.chars();
    if chars.next()? != '\x1b' {
        return None;
    }
    let ch = chars.next()?;
    if chars.next().is_some() || !ch.is_ascii_alphanumeric() {
        return None;
    }
    Some((ch.is_ascii_uppercase(), ch))
}

struct FunctionKeyParse {
    code: String,
    modifier: u16,
    option: bool,
}

fn parse_function_key_sequence(sequence: &str) -> Option<TerminalParsedKey> {
    let parsed = parse_function_key_code(sequence)?;
    let flags = decode_key_modifier(parsed.modifier);
    let name = key_name_for_code(&parsed.code).map(str::to_string);
    let mut key = key_with_name_and_flags(sequence, name, flags, Some(parsed.code.clone()));
    key.option = parsed.option;
    if is_shift_key_code(&parsed.code) {
        key.shift = true;
    }
    if is_ctrl_key_code(&parsed.code) {
        key.ctrl = true;
    }
    key.fn_key = key.name.as_deref().is_some_and(is_function_key_name);
    Some(key)
}

fn parse_function_key_code(sequence: &str) -> Option<FunctionKeyParse> {
    let esc_count = sequence.bytes().take_while(|byte| *byte == 0x1b).count();
    if esc_count == 0 {
        return None;
    }
    let rest = &sequence[esc_count..];
    let option = esc_count >= 2;

    if rest.starts_with("[[") {
        return parse_bracket_function_body(rest, option);
    }
    if rest.starts_with('[') {
        return parse_bracket_function_body(rest, option);
    }
    if rest.starts_with('O') || rest.starts_with('N') {
        let mut chars = rest.chars();
        let prefix = chars.next()?;
        let final_ch = chars.next()?;
        if chars.next().is_none() && final_ch.is_ascii_alphabetic() {
            return Some(FunctionKeyParse {
                code: format!("{prefix}{final_ch}"),
                modifier: 1,
                option,
            });
        }
    }
    None
}

fn parse_bracket_function_body(rest: &str, option: bool) -> Option<FunctionKeyParse> {
    let final_ch = rest.chars().next_back()?;
    if !(final_ch.is_ascii_alphabetic() || matches!(final_ch, '~' | '^' | '$')) {
        return None;
    }
    let final_start = rest.len() - final_ch.len_utf8();
    let prefix_and_params = &rest[..final_start];
    let (prefix, params) = if let Some(params) = prefix_and_params.strip_prefix("[[") {
        ("[[", params)
    } else {
        ("[", prefix_and_params.strip_prefix('[')?)
    };
    if params.contains('<') {
        return None;
    }

    if final_ch.is_ascii_alphabetic() {
        let mut modifier = 1u16;
        if !params.is_empty() {
            let nums = parse_semicolon_u16(params)?;
            modifier = *nums.last().unwrap_or(&1);
        }
        return Some(FunctionKeyParse {
            code: format!("{prefix}{final_ch}"),
            modifier,
            option,
        });
    }

    let nums = parse_semicolon_u16(params)?;
    let first = nums.first().copied().unwrap_or(1);
    let modifier = nums.get(1).copied().unwrap_or(1);
    Some(FunctionKeyParse {
        code: format!("{prefix}{first}{final_ch}"),
        modifier,
        option,
    })
}

fn parse_semicolon_u16(params: &str) -> Option<Vec<u16>> {
    if params.is_empty() {
        return Some(Vec::new());
    }
    params
        .split(';')
        .map(|part| part.parse::<u16>().ok())
        .collect()
}

fn key_name_for_code(code: &str) -> Option<&'static str> {
    Some(match code {
        "OP" => "f1",
        "OQ" => "f2",
        "OR" => "f3",
        "OS" => "f4",
        "Op" => "0",
        "Oq" => "1",
        "Or" => "2",
        "Os" => "3",
        "Ot" => "4",
        "Ou" => "5",
        "Ov" => "6",
        "Ow" => "7",
        "Ox" => "8",
        "Oy" => "9",
        "Oj" => "*",
        "Ok" => "+",
        "Ol" => ",",
        "Om" => "-",
        "On" => ".",
        "Oo" => "/",
        "OM" => "return",
        "[11~" => "f1",
        "[12~" => "f2",
        "[13~" => "f3",
        "[14~" => "f4",
        "[[A" => "f1",
        "[[B" => "f2",
        "[[C" => "f3",
        "[[D" => "f4",
        "[[E" => "f5",
        "[15~" => "f5",
        "[17~" => "f6",
        "[18~" => "f7",
        "[19~" => "f8",
        "[20~" => "f9",
        "[21~" => "f10",
        "[23~" => "f11",
        "[24~" => "f12",
        "[A" | "OA" | "[a" | "Oa" => "up",
        "[B" | "OB" | "[b" | "Ob" => "down",
        "[C" | "OC" | "[c" | "Oc" => "right",
        "[D" | "OD" | "[d" | "Od" => "left",
        "[E" | "OE" | "[e" | "Oe" => "clear",
        "[F" | "OF" => "end",
        "[H" | "OH" => "home",
        "[1~" | "[7~" => "home",
        "[2~" | "[2$" | "[2^" => "insert",
        "[3~" | "[3$" | "[3^" => "delete",
        "[4~" | "[8~" => "end",
        "[5~" | "[[5~" | "[5$" | "[5^" => "pageup",
        "[6~" | "[[6~" | "[6$" | "[6^" => "pagedown",
        "[7$" | "[7^" => "home",
        "[8$" | "[8^" => "end",
        "[Z" => "tab",
        _ => return None,
    })
}

fn is_shift_key_code(code: &str) -> bool {
    matches!(
        code,
        "[a" | "[b" | "[c" | "[d" | "[e" | "[2$" | "[3$" | "[5$" | "[6$" | "[7$" | "[8$" | "[Z"
    )
}

fn is_ctrl_key_code(code: &str) -> bool {
    matches!(
        code,
        "Oa" | "Ob" | "Oc" | "Od" | "Oe" | "[2^" | "[3^" | "[5^" | "[6^" | "[7^" | "[8^"
    )
}

fn is_function_key_name(name: &str) -> bool {
    name.strip_prefix('f')
        .and_then(|suffix| suffix.parse::<u8>().ok())
        .is_some()
}

fn create_nav_key(sequence: &str, name: &'static str, ctrl: bool) -> TerminalParsedKey {
    TerminalParsedKey {
        name: Some(name.to_string()),
        ctrl,
        sequence: Some(sequence.to_string()),
        raw: Some(sequence.to_string()),
        fn_key: is_function_key_name(name),
        ..Default::default()
    }
}
