use super::*;

/// A raw terminal input token split at escape-sequence boundaries.
///
/// This mirrors CC Ink's `termio/tokenize.ts`: semantic interpretation is left
/// to higher layers, but CSI/OSC/DCS/SS3 boundaries are preserved so terminal
/// query responses can be separated from ordinary text/key input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TerminalInputToken {
    /// Ordinary text bytes decoded as UTF-8.
    Text(String),
    /// A complete or flushed escape/control sequence beginning with ESC.
    Sequence(String),
}

/// Bracketed paste start marker (`CSI 200 ~`) emitted by terminals when DEC
/// mode 2004 is enabled.
pub const BRACKETED_PASTE_START: &str = "\x1b[200~";

/// Bracketed paste end marker (`CSI 201 ~`) emitted by terminals when DEC mode
/// 2004 is enabled.
pub const BRACKETED_PASTE_END: &str = "\x1b[201~";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TerminalInputTokenizerState {
    Ground,
    Escape,
    EscapeIntermediate,
    Csi,
    Ss3,
    Osc,
    Dcs,
    Apc,
}

/// Streaming tokenizer for raw terminal input.
///
/// Use this when building a custom frontend or stdin reader that needs to split
/// raw terminal input into plain text and escape sequences before routing known
/// query replies through [`parse_terminal_response`] or
/// [`TerminalResponseParser`]. It is mode-neutral: it does not enable raw mode,
/// query the terminal, or write any escape sequences.
#[derive(Clone, Debug)]
pub struct TerminalInputTokenizer {
    state: TerminalInputTokenizerState,
    buffer: String,
    x10_mouse: bool,
    in_paste: bool,
    paste_buffer: String,
}

impl Default for TerminalInputTokenizer {
    fn default() -> Self {
        Self::new()
    }
}

impl TerminalInputTokenizer {
    /// Creates a tokenizer with X10 mouse payload handling disabled.
    pub fn new() -> Self {
        Self {
            state: TerminalInputTokenizerState::Ground,
            buffer: String::new(),
            x10_mouse: false,
            in_paste: false,
            paste_buffer: String::new(),
        }
    }

    /// Creates a tokenizer, optionally treating `CSI M` as an X10 mouse event.
    ///
    /// Enable `x10_mouse` only for stdin streams where legacy mouse reporting is
    /// possible. As in CC Ink, `CSI M` is also the ANSI Delete Lines command in
    /// output streams, so blindly consuming three payload bytes there would be
    /// incorrect.
    pub fn with_x10_mouse(x10_mouse: bool) -> Self {
        Self {
            x10_mouse,
            ..Self::new()
        }
    }

    /// Feeds an input chunk and returns any complete tokens.
    pub fn feed(&mut self, input: &str) -> Vec<TerminalInputToken> {
        self.tokenize(input, false)
    }

    /// Feeds raw input bytes after applying CC Ink's `inputToString(Buffer)` rules.
    pub fn feed_bytes(&mut self, input: &[u8]) -> Vec<TerminalInputToken> {
        self.feed(&terminal_input_bytes_to_string(input))
    }

    /// Feeds an input chunk and parses recognized terminal query responses and
    /// bracketed paste payloads.
    pub fn feed_parsed(&mut self, input: &str) -> Vec<TerminalParsedInput> {
        let tokens = self.feed(input);
        tokens_to_parsed_inputs(
            tokens,
            &mut self.in_paste,
            &mut self.paste_buffer,
            false,
            false,
        )
    }

    /// Feeds raw input bytes and parses responses/paste/mouse using CC Ink's
    /// `inputToString(Buffer)` rules.
    pub fn feed_parsed_bytes(&mut self, input: &[u8]) -> Vec<TerminalParsedInput> {
        self.feed_parsed(&terminal_input_bytes_to_string(input))
    }

    /// Flushes any buffered incomplete escape sequence as a [`TerminalInputToken::Sequence`].
    pub fn flush(&mut self) -> Vec<TerminalInputToken> {
        self.tokenize("", true)
    }

    /// Flushes buffered input and parses recognized terminal query responses.
    ///
    /// If a bracketed paste is unterminated, a non-empty buffered paste payload
    /// is emitted as [`TerminalParsedInput::Paste`], matching CC Ink's flush
    /// behavior.
    pub fn flush_parsed(&mut self) -> Vec<TerminalParsedInput> {
        let tokens = self.flush();
        tokens_to_parsed_inputs(
            tokens,
            &mut self.in_paste,
            &mut self.paste_buffer,
            true,
            false,
        )
    }

    /// Clears buffered input and returns to the ground state.
    pub fn reset(&mut self) {
        self.state = TerminalInputTokenizerState::Ground;
        self.buffer.clear();
        self.in_paste = false;
        self.paste_buffer.clear();
    }

    /// Returns the currently buffered incomplete sequence, if any.
    pub fn buffered(&self) -> &str {
        &self.buffer
    }

    /// Returns whether an incomplete escape/control sequence is buffered.
    pub fn has_incomplete_sequence(&self) -> bool {
        !self.buffer.is_empty()
    }

    /// Returns whether parsed input is currently inside bracketed paste mode.
    pub fn is_in_paste(&self) -> bool {
        self.in_paste
    }

    /// Returns the CC Ink-style timeout a custom frontend should use before
    /// calling [`Self::flush`] / [`Self::flush_parsed`], if a sequence is incomplete.
    pub fn pending_flush_timeout(&self) -> Option<Duration> {
        self.has_incomplete_sequence().then_some(if self.in_paste {
            TERMINAL_INPUT_PASTE_TIMEOUT
        } else {
            TERMINAL_INPUT_NORMAL_TIMEOUT
        })
    }

    /// Returns whether an expired incomplete-sequence timer should actually flush.
    ///
    /// This mirrors CC Ink `App.flushIncomplete()`: if the stream already has
    /// queued bytes (`stdin.readableLength > 0` in Node), re-arm the timer
    /// instead of flushing so delayed mouse/CSI continuations are not split.
    pub fn should_flush_incomplete(&self, input_available: bool) -> bool {
        self.has_incomplete_sequence() && !input_available
    }

    fn tokenize(&mut self, input: &str, flush: bool) -> Vec<TerminalInputToken> {
        let mut data = String::new();
        if !self.buffer.is_empty() {
            data.push_str(&self.buffer);
            self.buffer.clear();
        }
        data.push_str(input);

        let chars = data.char_indices().collect::<Vec<_>>();
        let mut tokens = Vec::new();
        let mut state = self.state;
        let mut idx = 0usize;
        let mut text_start = 0usize;
        let mut seq_start = 0usize;
        let mut seq_start_idx = 0usize;

        while idx < chars.len() {
            let byte = chars[idx].0;
            let ch = chars[idx].1;
            let code = ch as u32;

            match state {
                TerminalInputTokenizerState::Ground => {
                    if code == 0x1b {
                        push_text_token(&mut tokens, &data, &mut text_start, byte);
                        seq_start = byte;
                        seq_start_idx = idx;
                        state = TerminalInputTokenizerState::Escape;
                        idx += 1;
                    } else {
                        idx += 1;
                    }
                }
                TerminalInputTokenizerState::Escape => {
                    if ch == '[' {
                        state = TerminalInputTokenizerState::Csi;
                        idx += 1;
                    } else if ch == ']' {
                        state = TerminalInputTokenizerState::Osc;
                        idx += 1;
                    } else if ch == 'P' {
                        state = TerminalInputTokenizerState::Dcs;
                        idx += 1;
                    } else if matches!(ch, '_' | '^' | 'X') {
                        state = TerminalInputTokenizerState::Apc;
                        idx += 1;
                    } else if ch == 'O' {
                        state = TerminalInputTokenizerState::Ss3;
                        idx += 1;
                    } else if is_csi_intermediate(code) {
                        state = TerminalInputTokenizerState::EscapeIntermediate;
                        idx += 1;
                    } else if is_esc_final(code) {
                        idx += 1;
                        let end = char_end(&chars, idx, data.len());
                        push_sequence_token(&mut tokens, &data, seq_start, end);
                        state = TerminalInputTokenizerState::Ground;
                        text_start = end;
                    } else if code == 0x1b {
                        push_sequence_token(&mut tokens, &data, seq_start, byte);
                        seq_start = byte;
                        seq_start_idx = idx;
                        state = TerminalInputTokenizerState::Escape;
                        idx += 1;
                        text_start = byte;
                    } else {
                        state = TerminalInputTokenizerState::Ground;
                        text_start = seq_start;
                    }
                }
                TerminalInputTokenizerState::EscapeIntermediate => {
                    if is_csi_intermediate(code) {
                        idx += 1;
                    } else if is_esc_final(code) {
                        idx += 1;
                        let end = char_end(&chars, idx, data.len());
                        push_sequence_token(&mut tokens, &data, seq_start, end);
                        state = TerminalInputTokenizerState::Ground;
                        text_start = end;
                    } else {
                        state = TerminalInputTokenizerState::Ground;
                        text_start = seq_start;
                    }
                }
                TerminalInputTokenizerState::Csi => {
                    if self.x10_mouse
                        && ch == 'M'
                        && idx.saturating_sub(seq_start_idx) == 2
                        && x10_payload_slot_is_available(&chars, idx + 1)
                        && x10_payload_slot_is_available(&chars, idx + 2)
                        && x10_payload_slot_is_available(&chars, idx + 3)
                    {
                        if idx + 4 <= chars.len() {
                            idx += 4;
                            let end = char_end(&chars, idx, data.len());
                            push_sequence_token(&mut tokens, &data, seq_start, end);
                            state = TerminalInputTokenizerState::Ground;
                            text_start = end;
                        } else {
                            idx = chars.len();
                        }
                    } else if is_csi_final(code) {
                        idx += 1;
                        let end = char_end(&chars, idx, data.len());
                        push_sequence_token(&mut tokens, &data, seq_start, end);
                        state = TerminalInputTokenizerState::Ground;
                        text_start = end;
                    } else if is_csi_param(code) || is_csi_intermediate(code) {
                        idx += 1;
                    } else {
                        state = TerminalInputTokenizerState::Ground;
                        text_start = seq_start;
                    }
                }
                TerminalInputTokenizerState::Ss3 => {
                    if (0x40..=0x7e).contains(&code) {
                        idx += 1;
                        let end = char_end(&chars, idx, data.len());
                        push_sequence_token(&mut tokens, &data, seq_start, end);
                        state = TerminalInputTokenizerState::Ground;
                        text_start = end;
                    } else {
                        state = TerminalInputTokenizerState::Ground;
                        text_start = seq_start;
                    }
                }
                TerminalInputTokenizerState::Osc
                | TerminalInputTokenizerState::Dcs
                | TerminalInputTokenizerState::Apc => {
                    if code == 0x07 {
                        idx += 1;
                        let end = char_end(&chars, idx, data.len());
                        push_sequence_token(&mut tokens, &data, seq_start, end);
                        state = TerminalInputTokenizerState::Ground;
                        text_start = end;
                    } else if code == 0x1b
                        && chars.get(idx + 1).is_some_and(|(_, next)| *next == '\\')
                    {
                        idx += 2;
                        let end = char_end(&chars, idx, data.len());
                        push_sequence_token(&mut tokens, &data, seq_start, end);
                        state = TerminalInputTokenizerState::Ground;
                        text_start = end;
                    } else {
                        idx += 1;
                    }
                }
            }
        }

        if state == TerminalInputTokenizerState::Ground {
            push_text_token(&mut tokens, &data, &mut text_start, data.len());
        } else if flush {
            if seq_start < data.len() {
                push_sequence_token(&mut tokens, &data, seq_start, data.len());
            }
            state = TerminalInputTokenizerState::Ground;
        } else if seq_start < data.len() {
            self.buffer.push_str(&data[seq_start..]);
        }

        self.state = state;
        tokens
    }
}

fn char_end(chars: &[(usize, char)], idx: usize, data_len: usize) -> usize {
    chars.get(idx).map(|(byte, _)| *byte).unwrap_or(data_len)
}

fn push_text_token(
    tokens: &mut Vec<TerminalInputToken>,
    data: &str,
    text_start: &mut usize,
    end: usize,
) {
    if end > *text_start {
        let text = &data[*text_start..end];
        if !text.is_empty() {
            tokens.push(TerminalInputToken::Text(text.to_string()));
        }
    }
    *text_start = end;
}

fn push_sequence_token(tokens: &mut Vec<TerminalInputToken>, data: &str, start: usize, end: usize) {
    if end > start {
        tokens.push(TerminalInputToken::Sequence(data[start..end].to_string()));
    }
}

fn is_esc_final(code: u32) -> bool {
    (0x30..=0x7e).contains(&code)
}

fn is_csi_param(code: u32) -> bool {
    (0x30..=0x3f).contains(&code)
}

fn is_csi_intermediate(code: u32) -> bool {
    (0x20..=0x2f).contains(&code)
}

fn is_csi_final(code: u32) -> bool {
    (0x40..=0x7e).contains(&code)
}

fn x10_payload_slot_is_available(chars: &[(usize, char)], idx: usize) -> bool {
    chars.get(idx).is_none_or(|(_, ch)| (*ch as u32) >= 0x20)
}
