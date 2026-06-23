use crate::{
    canvas::{
        Canvas, CanvasPackedCellPools, CanvasPackedScreen, CanvasStyleTransitionCache,
        SelectionController, SelectionFocusMove, SelectionHoverOutcome, SelectionPressOutcome,
        SelectionReleaseOutcome,
    },
    element::Output,
};
use crossterm::{
    cursor,
    event::{self, Event, EventStream},
    terminal, ExecutableCommand, QueueableCommand,
};
use futures::{
    channel::{mpsc, oneshot},
    stream::{self, BoxStream, Stream, StreamExt},
};
use futures_timer::Delay;
use std::{
    collections::VecDeque,
    env,
    future::Future,
    io::{self, stdin, IsTerminal, Write},
    mem,
    pin::Pin,
    sync::{Arc, Mutex, OnceLock, Weak},
    task::{Context, Poll, Waker},
    time::{Duration, Instant},
};

// Re-exports for basic types.
pub use crossterm::event::{
    KeyCode, KeyEventKind, KeyEventState, KeyModifiers, KeyboardEnhancementFlags, MouseButton,
    MouseEventKind,
};

const STDIN_RESUME_GAP: Duration = Duration::from_secs(5);

/// Timeout used before flushing an incomplete non-paste terminal input sequence.
///
/// Mirrors CC Ink's `App.NORMAL_TIMEOUT`: a lone ESC or partial CSI is held
/// briefly so chunked key/mouse sequences can complete before being emitted as
/// an Escape-like key.
pub const TERMINAL_INPUT_NORMAL_TIMEOUT: Duration = Duration::from_millis(50);

/// Timeout used before flushing an incomplete sequence while inside bracketed paste.
///
/// Mirrors CC Ink's `App.PASTE_TIMEOUT`: paste payloads are given more time so
/// escape sequences inside the paste can be preserved as literal text instead
/// of prematurely ending the pending input state.
pub const TERMINAL_INPUT_PASTE_TIMEOUT: Duration = Duration::from_millis(500);

/// Default byte chunk size used by opt-in raw input reader adapters.
pub const TERMINAL_RAW_INPUT_DEFAULT_CHUNK_SIZE: usize = 1024;

/// Enable xterm `modifyOtherKeys` level 2 (`CSI > 4 ; 2 m`).
///
/// CC Ink writes this alongside Kitty keyboard protocol so tmux and other
/// xterm-compatible multiplexers can emit enhanced key sequences even when they
/// do not understand Kitty's keyboard stack push.
pub const TERMINAL_MODIFY_OTHER_KEYS_ENABLE: &str = "\x1b[>4;2m";

/// Reset xterm `modifyOtherKeys` to the terminal default (`CSI > 4 m`).
pub const TERMINAL_MODIFY_OTHER_KEYS_DISABLE: &str = "\x1b[>4m";

/// Terminal-side modes commonly paired with caller-owned raw input readers.
///
/// This is an opt-in planning/serialization helper for applications that bypass
/// iocraft's default crossterm event backend and feed bytes through
/// [`TerminalRawInputFrontend`] or [`TerminalRawInputFallibleEventStream`]. It
/// does not enable OS raw mode by itself; callers remain responsible for their
/// stdin/raw-mode lifecycle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TerminalRawInputModeOptions {
    /// Hide the terminal cursor while the raw-input UI is active.
    pub hide_cursor: bool,
    /// Enable bracketed paste reporting.
    pub bracketed_paste: bool,
    /// Enable terminal focus in/out reporting.
    pub focus_events: bool,
    /// Enable mouse capture/tracking.
    pub mouse_capture: bool,
    /// Push Kitty keyboard enhancement flags, if supported by the terminal.
    pub keyboard_enhancement_flags: Option<KeyboardEnhancementFlags>,
    /// Enable xterm `modifyOtherKeys` level 2 in addition to Kitty keyboard
    /// flags. This mirrors CC Ink's tmux-compatible extended-key side band, but
    /// remains explicit because callers need a parser/backend that understands
    /// xterm modifyOtherKeys sequences.
    pub xterm_modify_other_keys: bool,
}

impl Default for TerminalRawInputModeOptions {
    fn default() -> Self {
        Self {
            hide_cursor: true,
            bracketed_paste: true,
            focus_events: true,
            mouse_capture: false,
            keyboard_enhancement_flags: Some(KeyboardEnhancementFlags::REPORT_EVENT_TYPES),
            xterm_modify_other_keys: false,
        }
    }
}

/// Writes terminal-side enable sequences for a caller-owned raw input backend.
///
/// The sequence mirrors the mode setup used by iocraft's built-in terminal
/// backend: optional cursor hide, keyboard enhancement push, mouse capture,
/// focus reporting, and bracketed paste. It intentionally does not call
/// `enable_raw_mode()` and does not start reading stdin.
pub fn write_terminal_raw_input_mode_enter(
    writer: &mut (impl Write + ?Sized),
    options: TerminalRawInputModeOptions,
) -> io::Result<()> {
    if options.hide_cursor {
        writer.queue(cursor::Hide)?;
    }
    if let Some(flags) = options.keyboard_enhancement_flags {
        writer.queue(event::PushKeyboardEnhancementFlags(flags))?;
    }
    if options.xterm_modify_other_keys {
        writer.write_all(TERMINAL_MODIFY_OTHER_KEYS_ENABLE.as_bytes())?;
    }
    if options.mouse_capture {
        writer.queue(event::EnableMouseCapture)?;
    }
    if options.focus_events {
        writer.queue(event::EnableFocusChange)?;
    }
    if options.bracketed_paste {
        writer.queue(event::EnableBracketedPaste)?;
    }
    Ok(())
}

/// Writes terminal-side disable sequences for a caller-owned raw input backend.
///
/// This is the inverse of [`write_terminal_raw_input_mode_enter`]. Call it
/// before returning the terminal to the shell if your application opted into
/// these raw-input side-band modes.
pub fn write_terminal_raw_input_mode_exit(
    writer: &mut (impl Write + ?Sized),
    options: TerminalRawInputModeOptions,
) -> io::Result<()> {
    if options.bracketed_paste {
        writer.queue(event::DisableBracketedPaste)?;
    }
    if options.focus_events {
        writer.queue(event::DisableFocusChange)?;
    }
    if options.mouse_capture {
        writer.queue(event::DisableMouseCapture)?;
    }
    if options.xterm_modify_other_keys {
        writer.write_all(TERMINAL_MODIFY_OTHER_KEYS_DISABLE.as_bytes())?;
    }
    if options.keyboard_enhancement_flags.is_some() {
        writer.queue(event::PopKeyboardEnhancementFlags)?;
    }
    if options.hide_cursor {
        writer.queue(cursor::Show)?;
    }
    Ok(())
}

/// RAII owner for terminal-side modes used by caller-owned raw input backends.
///
/// CC Ink enables these terminal modes when raw input starts and disables them
/// on unmount/exit. This guard is the Rust-first equivalent for opt-in custom
/// backends: construction writes [`write_terminal_raw_input_mode_enter`], drop
/// best-effort writes [`write_terminal_raw_input_mode_exit`], and
/// [`Self::exit`] performs fallible explicit cleanup when callers need to
/// observe errors or recover the wrapped writer.
///
/// The guard does not enable OS raw mode and does not read stdin. Use
/// [`TerminalRawInputSessionGuard`] when a custom backend also wants an explicit
/// crossterm raw-mode lifecycle scope.
pub struct TerminalRawInputModeGuard<W: Write> {
    writer: Option<W>,
    options: TerminalRawInputModeOptions,
    active: bool,
}

impl<W: Write> TerminalRawInputModeGuard<W> {
    /// Enters terminal-side raw-input modes and returns a guard that will leave
    /// them on drop.
    pub fn enter(mut writer: W, options: TerminalRawInputModeOptions) -> io::Result<Self> {
        write_terminal_raw_input_mode_enter(&mut writer, options)?;
        writer.flush()?;
        Ok(Self {
            writer: Some(writer),
            options,
            active: true,
        })
    }

    /// Returns whether the guard still owns an active terminal-mode scope.
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// Returns an immutable reference to the wrapped writer.
    pub fn writer(&self) -> &W {
        self.writer.as_ref().expect("raw input mode guard writer")
    }

    /// Returns a mutable reference to the wrapped writer.
    pub fn writer_mut(&mut self) -> &mut W {
        self.writer.as_mut().expect("raw input mode guard writer")
    }

    fn exit_inner(&mut self) -> io::Result<()> {
        if self.active {
            if let Some(writer) = self.writer.as_mut() {
                write_terminal_raw_input_mode_exit(writer, self.options)?;
                writer.flush()?;
            }
            self.active = false;
        }
        Ok(())
    }

    /// Leaves terminal-side modes and returns the wrapped writer.
    ///
    /// Prefer this over relying on drop when cleanup errors matter.
    pub fn exit(mut self) -> io::Result<W> {
        self.exit_inner()?;
        Ok(self.writer.take().expect("raw input mode guard writer"))
    }
}

impl<W: Write> Drop for TerminalRawInputModeGuard<W> {
    fn drop(&mut self) {
        let _ = self.exit_inner();
    }
}

/// Opt-in lifecycle options for a caller-owned raw input backend.
///
/// The default is intentionally conservative: terminal-side bracketed paste,
/// focus, cursor, and keyboard modes are managed, but OS raw mode is **not**
/// enabled. Set [`Self::enable_os_raw_mode`] to `true` only around a backend that
/// owns stdin and can guarantee cleanup, matching the project boundary that raw
/// stdin takeover is explicit and not the default crossterm path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TerminalRawInputSessionOptions {
    /// Terminal-side modes to enter while the caller-owned backend is active.
    pub terminal_modes: TerminalRawInputModeOptions,
    /// Whether to call crossterm `enable_raw_mode()` on enter and
    /// `disable_raw_mode()` on exit/drop.
    pub enable_os_raw_mode: bool,
}

impl Default for TerminalRawInputSessionOptions {
    fn default() -> Self {
        Self {
            terminal_modes: TerminalRawInputModeOptions::default(),
            enable_os_raw_mode: false,
        }
    }
}

/// RAII owner for an explicit caller-owned raw-input session.
///
/// This combines [`TerminalRawInputModeGuard`]'s terminal-side mode cleanup with
/// an optional crossterm raw-mode scope. It still does not read stdin or replace
/// iocraft's default event backend; callers feed bytes through
/// [`TerminalRawInputFrontend`], [`TerminalRawInputEventStream`], or
/// [`TerminalRawInputFallibleEventStream`] themselves.
///
/// If enter fails after OS raw mode was enabled, the guard best-effort restores
/// terminal-side modes and disables raw mode before returning the error. Drop is
/// best-effort cleanup; call [`Self::exit`] when cleanup errors or recovering the
/// wrapped writer matter.
pub struct TerminalRawInputSessionGuard<W: Write> {
    writer: Option<W>,
    options: TerminalRawInputSessionOptions,
    terminal_modes_active: bool,
    os_raw_mode_enabled: bool,
}

impl<W: Write> TerminalRawInputSessionGuard<W> {
    /// Enters a caller-owned raw-input session.
    pub fn enter(mut writer: W, options: TerminalRawInputSessionOptions) -> io::Result<Self> {
        let mut os_raw_mode_enabled = false;
        if options.enable_os_raw_mode {
            terminal::enable_raw_mode()?;
            os_raw_mode_enabled = true;
        }

        if let Err(error) = write_terminal_raw_input_mode_enter(&mut writer, options.terminal_modes)
            .and_then(|()| writer.flush())
        {
            let _ = write_terminal_raw_input_mode_exit(&mut writer, options.terminal_modes);
            let _ = writer.flush();
            if os_raw_mode_enabled {
                let _ = terminal::disable_raw_mode();
            }
            return Err(error);
        }

        Ok(Self {
            writer: Some(writer),
            options,
            terminal_modes_active: true,
            os_raw_mode_enabled,
        })
    }

    /// Returns whether any managed session state is still active.
    pub fn is_active(&self) -> bool {
        self.terminal_modes_active || self.os_raw_mode_enabled
    }

    /// Returns whether this guard enabled crossterm OS raw mode.
    pub fn is_os_raw_mode_enabled(&self) -> bool {
        self.os_raw_mode_enabled
    }

    /// Returns the session options used by this guard.
    pub fn options(&self) -> TerminalRawInputSessionOptions {
        self.options
    }

    /// Returns an immutable reference to the wrapped writer.
    pub fn writer(&self) -> &W {
        self.writer
            .as_ref()
            .expect("raw input session guard writer")
    }

    /// Returns a mutable reference to the wrapped writer.
    pub fn writer_mut(&mut self) -> &mut W {
        self.writer
            .as_mut()
            .expect("raw input session guard writer")
    }

    fn exit_inner(&mut self) -> io::Result<()> {
        let mut first_error = None;

        if self.terminal_modes_active {
            if let Some(writer) = self.writer.as_mut() {
                if let Err(error) =
                    write_terminal_raw_input_mode_exit(writer, self.options.terminal_modes)
                        .and_then(|()| writer.flush())
                {
                    first_error = Some(error);
                }
            }
            self.terminal_modes_active = false;
        }

        if self.os_raw_mode_enabled {
            if let Err(error) = terminal::disable_raw_mode() {
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
            self.os_raw_mode_enabled = false;
        }

        if let Some(error) = first_error {
            Err(error)
        } else {
            Ok(())
        }
    }

    /// Leaves terminal-side modes, disables OS raw mode if this guard enabled it,
    /// and returns the wrapped writer.
    pub fn exit(mut self) -> io::Result<W> {
        self.exit_inner()?;
        Ok(self.writer.take().expect("raw input session guard writer"))
    }
}

impl<W: Write> Drop for TerminalRawInputSessionGuard<W> {
    fn drop(&mut self) {
        let _ = self.exit_inner();
    }
}

/// Opt-in event stream for a complete caller-owned raw-input session.
///
/// This is the highest-level raw-input building block iocraft exposes without
/// replacing its default crossterm backend: construction enters
/// [`TerminalRawInputSessionGuard`], bytes are parsed by
/// [`TerminalRawInputFallibleEventStream`], and drop/explicit [`Self::exit`]
/// restores terminal-side modes plus optional OS raw mode. The caller still owns
/// the actual reader and decides whether [`TerminalRawInputSessionOptions`] sets
/// `enable_os_raw_mode`.
pub struct TerminalRawInputSessionEventStream<W: Write> {
    guard: Option<TerminalRawInputSessionGuard<W>>,
    events: TerminalRawInputFallibleEventStream,
}

impl<W: Write> TerminalRawInputSessionEventStream<W> {
    /// Enters a raw-input session around a fallible byte source.
    pub fn new<S>(writer: W, source: S, options: TerminalRawInputSessionOptions) -> io::Result<Self>
    where
        S: Stream<Item = io::Result<Vec<u8>>> + Send + 'static,
    {
        Self::with_frontend(writer, source, TerminalRawInputFrontend::new(), options)
    }

    /// Enters a raw-input session around a fallible byte source and explicit parser frontend.
    pub fn with_frontend<S>(
        writer: W,
        source: S,
        frontend: TerminalRawInputFrontend,
        options: TerminalRawInputSessionOptions,
    ) -> io::Result<Self>
    where
        S: Stream<Item = io::Result<Vec<u8>>> + Send + 'static,
    {
        let guard = TerminalRawInputSessionGuard::enter(writer, options)?;
        Ok(Self {
            guard: Some(guard),
            events: TerminalRawInputFallibleEventStream::with_frontend(source, frontend),
        })
    }

    /// Enters a raw-input session around an async reader.
    pub fn from_reader<R>(
        writer: W,
        reader: R,
        options: TerminalRawInputSessionOptions,
    ) -> io::Result<Self>
    where
        R: futures::io::AsyncRead + Unpin + Send + 'static,
    {
        Self::new(writer, TerminalRawInputByteStream::new(reader), options)
    }

    /// Enters a raw-input session around an async reader with an explicit chunk size.
    pub fn from_reader_with_chunk_size<R>(
        writer: W,
        reader: R,
        chunk_size: usize,
        options: TerminalRawInputSessionOptions,
    ) -> io::Result<Self>
    where
        R: futures::io::AsyncRead + Unpin + Send + 'static,
    {
        Self::new(
            writer,
            TerminalRawInputByteStream::with_chunk_size(reader, chunk_size),
            options,
        )
    }

    /// Returns the wrapped terminal-mode guard.
    pub fn guard(&self) -> &TerminalRawInputSessionGuard<W> {
        self.guard.as_ref().expect("raw input session event guard")
    }

    /// Returns the wrapped terminal-mode guard mutably.
    pub fn guard_mut(&mut self) -> &mut TerminalRawInputSessionGuard<W> {
        self.guard.as_mut().expect("raw input session event guard")
    }

    /// Returns the wrapped parser frontend.
    pub fn frontend(&self) -> &TerminalRawInputFrontend {
        self.events.frontend()
    }

    /// Returns the wrapped parser frontend mutably.
    pub fn frontend_mut(&mut self) -> &mut TerminalRawInputFrontend {
        self.events.frontend_mut()
    }

    /// Leaves terminal/raw-mode state and returns the wrapped writer.
    pub fn exit(mut self) -> io::Result<W> {
        self.guard
            .take()
            .expect("raw input session event guard")
            .exit()
    }
}

impl<W> Stream for TerminalRawInputSessionEventStream<W>
where
    W: Write + Unpin,
{
    type Item = io::Result<TerminalEvent>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        Pin::new(&mut this.events).poll_next(cx)
    }
}

/// An event fired when a key is pressed.
#[non_exhaustive]
#[derive(Clone, Debug)]
pub struct KeyEvent {
    /// A code indicating the key that was pressed.
    pub code: KeyCode,

    /// The modifiers that were active when the key was pressed.
    pub modifiers: KeyModifiers,

    /// Whether the key was pressed or released.
    pub kind: KeyEventKind,
}

impl KeyEvent {
    /// Creates a new `KeyEvent`.
    pub fn new(kind: KeyEventKind, code: KeyCode) -> Self {
        Self {
            code,
            modifiers: KeyModifiers::empty(),
            kind,
        }
    }
}

/// An event fired when the mouse is moved, clicked, scrolled, etc. in fullscreen mode.
#[non_exhaustive]
#[derive(Clone, Debug)]
pub struct FullscreenMouseEvent {
    /// The modifiers that were active when the event occurred.
    pub modifiers: KeyModifiers,

    /// The column that the event occurred on.
    pub column: u16,

    /// The row that the event occurred on.
    pub row: u16,

    /// Whether the event's terminal cell is visually blank in the retained
    /// fullscreen screen buffer. This mirrors CC Ink's `ClickEvent.cellIsBlank`
    /// metadata so click handlers can ignore empty space to the right of text.
    pub cell_is_blank: bool,

    /// The kind of mouse event.
    pub kind: MouseEventKind,
}

impl FullscreenMouseEvent {
    /// Creates a new `FullscreenMouseEvent`.
    pub fn new(kind: MouseEventKind, column: u16, row: u16) -> Self {
        Self {
            modifiers: KeyModifiers::empty(),
            column,
            row,
            cell_is_blank: false,
            kind,
        }
    }

    /// Returns a copy of this mouse event with retained-buffer blank-cell
    /// metadata attached.
    pub fn with_cell_is_blank(mut self, cell_is_blank: bool) -> Self {
        self.cell_is_blank = cell_is_blank;
        self
    }
}

#[derive(Clone, Debug)]
struct EventCellSnapshot {
    width: usize,
    height: usize,
    blank: Vec<bool>,
}

impl EventCellSnapshot {
    fn from_canvas(canvas: &Canvas) -> Self {
        let width = canvas.width();
        let height = canvas.height();
        let mut blank = Vec::with_capacity(width.saturating_mul(height));
        for row in 0..height {
            for column in 0..width {
                blank.push(canvas.cell_is_blank(column, row));
            }
        }
        Self {
            width,
            height,
            blank,
        }
    }

    fn cell_is_blank(&self, column: u16, row: u16) -> bool {
        let column = column as usize;
        let row = row as usize;
        if column >= self.width || row >= self.height {
            return true;
        }
        self.blank
            .get(row * self.width + column)
            .copied()
            .unwrap_or(true)
    }
}

/// An event fired by the terminal.
#[non_exhaustive]
#[derive(Clone, Debug)]
pub enum TerminalEvent {
    /// A key event, fired when a key is pressed.
    Key(KeyEvent),
    /// A mouse event, fired when the mouse is moved, clicked, scrolled, etc. in fullscreen mode.
    FullscreenMouse(FullscreenMouseEvent),
    /// A resize event, fired when the terminal is resized.
    Resize(u16, u16),
    /// The terminal window gained focus (DECSET 1004 focus reporting).
    FocusGained,
    /// The terminal window lost focus (DECSET 1004 focus reporting).
    FocusLost,
    /// A paste event, fired when the user pastes text while bracketed paste mode is active.
    ///
    /// The entire pasted text is delivered as a single event rather than a series of key
    /// events, allowing components such as
    /// [`TextInput`](crate::components::TextInput) to process the paste in one pass (one
    /// `on_change` invocation, one render) and applications to distinguish typed input
    /// from pasted input.
    Paste(String),
    /// A terminal response parsed from a terminal query reply.
    ///
    /// Real terminal response parsing depends on lower-level input tokenization;
    /// this variant lets custom frontends or tests route parsed responses through
    /// iocraft's event system and into [`TerminalQuerier::on_event`].
    Response(TerminalResponse),
}

/// A terminal response sequence produced by a terminal query.
///
/// These responses mirror CC Ink's `TerminalResponse` union from
/// `parse-keypress.ts`. They are syntactically distinct from normal keyboard
/// input and can be parsed with [`parse_terminal_response`] when an application
/// uses lower-level input plumbing for terminal capability probes.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TerminalResponse {
    /// DECRPM: response to a DECRQM private-mode query.
    Decrpm {
        /// Queried DEC private mode number.
        mode: u32,
        /// Response status value.
        status: u32,
    },
    /// DA1: primary device attributes response.
    Da1 {
        /// Numeric DA1 parameters.
        params: Vec<u32>,
    },
    /// DA2: secondary device attributes response.
    Da2 {
        /// Numeric DA2 parameters.
        params: Vec<u32>,
    },
    /// Kitty keyboard protocol flags response.
    KittyKeyboard {
        /// Currently enabled Kitty keyboard flags.
        flags: u32,
    },
    /// DECXCPR cursor position response.
    CursorPosition {
        /// 1-based terminal row.
        row: u32,
        /// 1-based terminal column.
        col: u32,
    },
    /// OSC response, such as an OSC 10/11 dynamic color reply.
    Osc {
        /// OSC command code.
        code: u32,
        /// OSC response data payload.
        data: String,
    },
    /// XTVERSION terminal name/version response.
    Xtversion {
        /// Terminal name/version payload.
        name: String,
    },
}

impl TerminalResponse {
    /// Returns this response's DECRPM status as a typed value, when applicable.
    pub fn decrpm_status(&self) -> Option<DecrpmStatus> {
        match self {
            TerminalResponse::Decrpm { status, .. } => DecrpmStatus::from_code(*status),
            _ => None,
        }
    }
}

/// DECRPM status values returned for DECRQM private-mode queries.
///
/// This is the Rust counterpart to CC Ink's exported `DECRPM_STATUS` table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum DecrpmStatus {
    /// The queried mode is not recognized by the terminal.
    NotRecognized = 0,
    /// The queried mode is currently set.
    Set = 1,
    /// The queried mode is currently reset.
    Reset = 2,
    /// The queried mode is permanently set.
    PermanentlySet = 3,
    /// The queried mode is permanently reset.
    PermanentlyReset = 4,
}

impl DecrpmStatus {
    /// Converts a raw DECRPM status code into a typed value.
    pub fn from_code(code: u32) -> Option<Self> {
        match code {
            0 => Some(Self::NotRecognized),
            1 => Some(Self::Set),
            2 => Some(Self::Reset),
            3 => Some(Self::PermanentlySet),
            4 => Some(Self::PermanentlyReset),
            _ => None,
        }
    }

    /// Returns the raw DECRPM status code.
    pub fn code(self) -> u32 {
        self as u32
    }
}

/// Terminal multiplexer passthrough mode for clipboard escape sequences.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClipboardMultiplexer {
    /// Write the raw OSC 52 sequence.
    None,
    /// Wrap OSC 52 in tmux DCS passthrough, doubling inner ESC bytes.
    Tmux,
    /// Wrap OSC 52 in GNU screen passthrough.
    Screen,
}

impl From<ClipboardMultiplexer> for crate::ansi::MultiplexerPassthrough {
    fn from(value: ClipboardMultiplexer) -> Self {
        match value {
            ClipboardMultiplexer::None => Self::None,
            ClipboardMultiplexer::Tmux => Self::Tmux,
            ClipboardMultiplexer::Screen => Self::Screen,
        }
    }
}

/// Reason a CC Ink-style terminal diff should fall back to a full clear.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminalClearReason {
    /// The viewport changed size.
    Resize,
    /// The previous or current frame reaches native terminal scrollback.
    Offscreen,
    /// Caller-requested clear/reset outside the automatic heuristic.
    Clear,
}

/// Minimal frame geometry used by [`should_clear_terminal_screen`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TerminalFrameBounds {
    /// Rendered screen height in terminal rows.
    pub screen_height: usize,
    /// Terminal viewport width in columns.
    pub viewport_width: usize,
    /// Terminal viewport height in rows.
    pub viewport_height: usize,
}

/// Returns whether a CC Ink-style terminal diff should clear before rendering.
///
/// This mirrors `frame.ts::shouldClearScreen(...)`: resize wins first, then
/// current or previous frames whose screen height is at least the viewport
/// height are treated as offscreen/scrollback-producing and require a clear.
/// It is a mode-neutral helper for custom renderers; iocraft's built-in
/// retained-canvas renderer has additional inline/fullscreen-specific guards.
pub fn should_clear_terminal_screen(
    prev: TerminalFrameBounds,
    next: TerminalFrameBounds,
) -> Option<TerminalClearReason> {
    if next.viewport_height != prev.viewport_height || next.viewport_width != prev.viewport_width {
        return Some(TerminalClearReason::Resize);
    }

    if next.screen_height >= next.viewport_height || prev.screen_height >= prev.viewport_height {
        return Some(TerminalClearReason::Offscreen);
    }

    None
}

/// Main-screen geometry used by [`analyze_terminal_inline_diff`].
///
/// This is intentionally separate from [`TerminalFrameBounds`]: CC Ink's
/// `log-update.ts` main-screen diff has stricter cursor-reachability rules than
/// the mode-neutral `frame.ts::shouldClearScreen(...)` helper. Width shrink or
/// any width change invalidates wrap assumptions, while height growth alone does
/// not force a clear.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TerminalInlineDiffBounds {
    /// Previous rendered screen height in terminal rows.
    pub prev_screen_height: usize,
    /// Next rendered screen height in terminal rows.
    pub next_screen_height: usize,
    /// Previous terminal viewport width in columns.
    pub prev_viewport_width: usize,
    /// Previous terminal viewport height in rows.
    pub prev_viewport_height: usize,
    /// Next terminal viewport width in columns.
    pub next_viewport_width: usize,
    /// Next terminal viewport height in rows.
    pub next_viewport_height: usize,
}

/// Result of [`analyze_terminal_inline_diff`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TerminalInlineDiffAnalysis {
    /// Immediate full-clear reason, if geometry alone proves sparse diffing is unsafe.
    pub clear_reason: Option<TerminalClearReason>,
    /// Number of top rows that cannot be reached safely with relative cursor movement.
    ///
    /// If any changed cell falls within this prefix, a custom main-screen
    /// renderer should fall back to a full clear with [`TerminalClearReason::Offscreen`].
    /// This includes CC Ink's `cursorRestoreScroll` extra row when the previous
    /// frame filled or overflowed the viewport.
    pub unreachable_rows: usize,
    /// Whether the next frame is taller than the previous frame.
    pub growing: bool,
    /// Whether the next frame is shorter than the previous frame.
    pub shrinking: bool,
}

/// Analyzes CC Ink `log-update.ts` main-screen diff geometry.
///
/// This helper is **main-screen safe** and mode-neutral: it does not write to
/// the terminal, clear output, or enter fullscreen. It packages the geometry
/// guards used before sparse inline row-diffing:
///
/// - shorter viewport height or changed nonzero viewport width → `Resize`
/// - shrinking from a scrollback-producing previous frame to a frame that fits
///   the viewport → `Offscreen`
/// - clearing more rows than fit in the viewport → `Offscreen`
/// - otherwise return the top-row prefix that is unreachable; changed cells in
///   that prefix should trigger an `Offscreen` clear.
///
/// iocraft's built-in renderer applies equivalent internal guards. Custom
/// renderers that produce [`TerminalPatch`] diffs can use this to preserve CC
/// Ink's main-screen scrollback safety without adopting Claude Code policy.
pub fn analyze_terminal_inline_diff(
    bounds: TerminalInlineDiffBounds,
) -> TerminalInlineDiffAnalysis {
    let growing = bounds.next_screen_height > bounds.prev_screen_height;
    let shrinking = bounds.next_screen_height < bounds.prev_screen_height;
    let prev_had_scrollback = bounds.prev_viewport_height > 0
        && bounds.prev_screen_height > 0
        && bounds.prev_screen_height >= bounds.prev_viewport_height;

    let resize_requires_clear = bounds.next_viewport_height < bounds.prev_viewport_height
        || (bounds.prev_viewport_width != 0
            && bounds.next_viewport_width != bounds.prev_viewport_width);

    let shrink_to_fits_requires_clear = prev_had_scrollback
        && shrinking
        && bounds.next_screen_height <= bounds.prev_viewport_height;

    let shrink_clear_count = bounds
        .prev_screen_height
        .saturating_sub(bounds.next_screen_height);
    let shrink_clear_exceeds_viewport = shrinking
        && bounds.prev_viewport_height > 0
        && shrink_clear_count > bounds.prev_viewport_height;

    let clear_reason = if resize_requires_clear {
        Some(TerminalClearReason::Resize)
    } else if shrink_to_fits_requires_clear || shrink_clear_exceeds_viewport {
        Some(TerminalClearReason::Offscreen)
    } else {
        None
    };

    let cursor_restore_scroll = usize::from(prev_had_scrollback);
    let reference_height = if growing {
        bounds.prev_screen_height
    } else {
        bounds.prev_screen_height.max(bounds.next_screen_height)
    };
    let reference_viewport = if growing {
        bounds.prev_viewport_height
    } else {
        bounds.next_viewport_height
    };
    let unreachable_rows = if reference_viewport == 0 || reference_height == 0 {
        0
    } else {
        reference_height
            .saturating_sub(reference_viewport)
            .saturating_add(cursor_restore_scroll)
    };

    TerminalInlineDiffAnalysis {
        clear_reason,
        unreachable_rows,
        growing,
        shrinking,
    }
}

/// Debug metadata for a main-screen sparse diff that must fall back to clear.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalInlineDiffResetDebug {
    /// First changed row that is not safely reachable with relative cursor movement.
    pub trigger_y: usize,
    /// Previous retained-canvas text on the trigger row.
    pub prev_line: String,
    /// Next retained-canvas text on the trigger row.
    pub next_line: String,
}

/// Decision returned by [`plan_terminal_inline_canvas_diff`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalInlineCanvasDiffDecision {
    /// Geometry analysis used to make this decision.
    pub analysis: TerminalInlineDiffAnalysis,
    /// Whether a custom main-screen renderer should clear and fully repaint
    /// instead of attempting a sparse row diff.
    pub clear_reason: Option<TerminalClearReason>,
    /// Optional row-level debug information for an offscreen/unreachable change.
    pub debug: Option<TerminalInlineDiffResetDebug>,
}

/// Decides whether a main-screen retained-canvas sparse diff is safe.
///
/// This extends [`analyze_terminal_inline_diff`] with the actual canvas scan
/// from CC Ink `log-update.ts`: after geometry says sparse diffing might be
/// possible, any changed cell in the unreachable top-row prefix triggers an
/// `Offscreen` clear. The helper is mode-neutral and performs no terminal I/O;
/// it only packages the clear-vs-sparse decision and optional debug row text for
/// custom renderers.
pub fn plan_terminal_inline_canvas_diff(
    previous: &Canvas,
    next: &Canvas,
    bounds: TerminalInlineDiffBounds,
) -> TerminalInlineCanvasDiffDecision {
    let analysis = analyze_terminal_inline_diff(bounds);
    if let Some(clear_reason) = analysis.clear_reason {
        return TerminalInlineCanvasDiffDecision {
            analysis,
            clear_reason: Some(clear_reason),
            debug: None,
        };
    }

    let mut trigger_y = None;
    if analysis.unreachable_rows > 0 {
        previous.diff_each(next, |change| {
            if change.y < analysis.unreachable_rows {
                trigger_y = Some(change.y);
                true
            } else {
                false
            }
        });
    }

    let debug = trigger_y.map(|trigger_y| {
        let width = previous.width().max(next.width());
        TerminalInlineDiffResetDebug {
            trigger_y,
            prev_line: previous.get_text(0, trigger_y, width, 1),
            next_line: next.get_text(0, trigger_y, width, 1),
        }
    });

    TerminalInlineCanvasDiffDecision {
        analysis,
        clear_reason: debug.as_ref().map(|_| TerminalClearReason::Offscreen),
        debug,
    }
}

fn packed_screen_plain_row(
    screen: &CanvasPackedScreen,
    pools: &CanvasPackedCellPools,
    y: usize,
    width: usize,
) -> String {
    if y >= screen.height {
        return String::new();
    }

    let mut line = String::new();
    for x in 0..width.min(screen.width) {
        line.push_str(screen.char_in_cell(pools, x, y).unwrap_or(" "));
    }
    line.trim_end().to_string()
}

/// Decides whether a main-screen packed-screen sparse diff is safe.
///
/// This is the packed counterpart to [`plan_terminal_inline_canvas_diff`]. It
/// mirrors the CC Ink `log-update.ts` unreachable-row scan by using
/// [`CanvasPackedScreen::diff_each`] against the caller's packed buffers and by
/// reporting trimmed debug lines via [`CanvasPackedScreen::char_in_cell`]. The
/// helper remains mode-neutral: it performs no terminal I/O and does not make
/// packed screens the default renderer representation.
pub fn plan_terminal_inline_packed_canvas_diff(
    previous: &CanvasPackedScreen,
    next: &CanvasPackedScreen,
    pools: &CanvasPackedCellPools,
    bounds: TerminalInlineDiffBounds,
) -> TerminalInlineCanvasDiffDecision {
    let analysis = analyze_terminal_inline_diff(bounds);
    if let Some(clear_reason) = analysis.clear_reason {
        return TerminalInlineCanvasDiffDecision {
            analysis,
            clear_reason: Some(clear_reason),
            debug: None,
        };
    }

    let mut trigger_y = None;
    if analysis.unreachable_rows > 0 {
        previous.diff_each(next, |change| {
            if change.y < analysis.unreachable_rows {
                trigger_y = Some(change.y);
                true
            } else {
                false
            }
        });
    }

    let debug = trigger_y.map(|trigger_y| {
        let width = previous.width.max(next.width);
        TerminalInlineDiffResetDebug {
            trigger_y,
            prev_line: packed_screen_plain_row(previous, pools, trigger_y, width),
            next_line: packed_screen_plain_row(next, pools, trigger_y, width),
        }
    });

    TerminalInlineCanvasDiffDecision {
        analysis,
        clear_reason: debug.as_ref().map(|_| TerminalClearReason::Offscreen),
        debug,
    }
}

/// Patch plan for a main-screen inline retained-canvas frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalInlineCanvasFramePatchPlan {
    /// Clear-vs-sparse decision used to build this plan.
    pub decision: TerminalInlineCanvasDiffDecision,
    /// Patches to write when `decision.clear_reason` is `Some(_)`.
    ///
    /// An empty list means the sparse row diff is safe and should be produced by
    /// the caller's custom renderer. This helper deliberately does not expose a
    /// default main-screen sparse patch generator because iocraft's built-in
    /// writer keeps that path Rust-native and cursor-stateful.
    pub patches: Vec<TerminalPatch>,
}

impl TerminalInlineCanvasFramePatchPlan {
    /// Returns whether this plan requires a clear + full repaint fallback.
    pub fn requires_clear_repaint(&self) -> bool {
        self.decision.clear_reason.is_some()
    }

    /// Returns whether the caller may continue with its sparse row diff path.
    pub fn sparse_diff_safe(&self) -> bool {
        self.decision.clear_reason.is_none()
    }
}

fn canvas_ansi_without_final_newline(canvas: &Canvas) -> String {
    let mut output = Vec::new();
    canvas
        .write_ansi_without_final_newline(&mut output)
        .expect("Vec writes cannot fail");
    String::from_utf8(output).expect("canvas ANSI output is valid UTF-8")
}

/// Plans the CC Ink main-screen clear + full-repaint fallback for a canvas diff.
///
/// This combines [`plan_terminal_inline_canvas_diff`] with the full reset branch
/// from `log-update.ts`: when geometry or unreachable-row changes make sparse
/// cursor movement unsafe, the returned patches start with
/// [`TerminalPatch::ClearTerminal`] and then repaint the whole next canvas from
/// the terminal origin. When sparse diffing is safe, `patches` is empty so a
/// custom renderer can continue with its own row-diff path. The helper performs
/// no terminal I/O and does not change default renderer behavior.
pub fn plan_terminal_inline_canvas_frame_patches(
    previous: &Canvas,
    next: &Canvas,
    bounds: TerminalInlineDiffBounds,
) -> TerminalInlineCanvasFramePatchPlan {
    let decision = plan_terminal_inline_canvas_diff(previous, next, bounds);
    let patches = if decision.clear_reason.is_some() {
        vec![
            TerminalPatch::ClearTerminal,
            TerminalPatch::Stdout(canvas_ansi_without_final_newline(next)),
        ]
    } else {
        Vec::new()
    };

    TerminalInlineCanvasFramePatchPlan { decision, patches }
}

fn packed_canvas_ansi_without_final_newline(
    screen: &CanvasPackedScreen,
    pools: &CanvasPackedCellPools,
    style_cache: &mut CanvasStyleTransitionCache,
) -> String {
    if screen.height == 0 {
        return String::new();
    }

    let mut output = String::from("\x1b[0m");
    for y in 0..screen.height {
        if y > 0 {
            output.push_str("\r\n");
        }
        output.push_str(&packed_canvas_row_ansi_from_col(
            screen,
            pools,
            style_cache,
            y,
            0,
        ));
    }
    output
}

/// Plans the CC Ink main-screen clear + full-repaint fallback for a packed-screen diff.
///
/// This mirrors [`plan_terminal_inline_canvas_frame_patches`] for custom
/// renderers that already produce packed screens. When the CC Ink geometry or
/// unreachable-row scan says sparse cursor movement is unsafe, the returned
/// patch list clears the terminal and repaints the entire packed next screen
/// from the origin. When sparse diffing is safe, no patches are returned so the
/// caller can continue with its own cursor-stateful packed row diff path.
pub fn plan_terminal_inline_packed_canvas_frame_patches(
    previous: &CanvasPackedScreen,
    next: &CanvasPackedScreen,
    pools: &CanvasPackedCellPools,
    style_cache: &mut CanvasStyleTransitionCache,
    bounds: TerminalInlineDiffBounds,
) -> TerminalInlineCanvasFramePatchPlan {
    let decision = plan_terminal_inline_packed_canvas_diff(previous, next, pools, bounds);
    let patches = if decision.clear_reason.is_some() {
        vec![
            TerminalPatch::ClearTerminal,
            TerminalPatch::Stdout(packed_canvas_ansi_without_final_newline(
                next,
                pools,
                style_cache,
            )),
        ]
    } else {
        Vec::new()
    };

    TerminalInlineCanvasFramePatchPlan { decision, patches }
}

/// A terminal-output patch used by CC Ink-style diff optimizers.
///
/// iocraft's built-in terminal renderer writes retained [`Canvas`] rows
/// directly, but custom renderers and tests can use this mode-neutral patch
/// representation with [`optimize_terminal_patches`] to mirror CC Ink's
/// `optimizer.ts` rules without changing main-screen or fullscreen policy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TerminalPatch {
    /// Raw stdout payload.
    Stdout(String),
    /// Clear `count` previously-rendered rows.
    Clear {
        /// Number of rows to clear.
        count: usize,
    },
    /// Full terminal clear/reset patch.
    ClearTerminal,
    /// Hide the terminal cursor.
    CursorHide,
    /// Show the terminal cursor.
    CursorShow,
    /// Move the cursor relative to its current position.
    CursorMove {
        /// Horizontal delta; positive moves right, negative moves left.
        x: i32,
        /// Vertical delta; positive moves down, negative moves up.
        y: i32,
    },
    /// Move the cursor to an absolute 1-indexed column.
    CursorTo {
        /// Target 1-indexed terminal column.
        col: u16,
    },
    /// Emit a carriage return.
    CarriageReturn,
    /// Set the current OSC 8 hyperlink target.
    Hyperlink {
        /// Hyperlink URI.
        uri: String,
    },
    /// Pre-serialized ANSI style transition.
    StyleStr(String),
}

/// Optimizes terminal-output patches using CC Ink `optimizer.ts` rules.
///
/// For multi-patch diffs, this removes no-op stdout/clear/cursor moves,
/// merges adjacent relative cursor moves, collapses consecutive `cursorTo`
/// patches to the last target, concatenates adjacent style transition strings,
/// deduplicates consecutive hyperlinks with the same URI, and cancels adjacent
/// cursor hide/show pairs. Matching CC Ink, zero- or one-patch diffs are
/// returned unchanged. It is an opt-in optimization utility; it does not write
/// to the terminal.
pub fn optimize_terminal_patches(diff: Vec<TerminalPatch>) -> Vec<TerminalPatch> {
    if diff.len() <= 1 {
        return diff;
    }

    let mut result = Vec::with_capacity(diff.len());

    for patch in diff {
        match &patch {
            TerminalPatch::Stdout(content) if content.is_empty() => continue,
            TerminalPatch::CursorMove { x: 0, y: 0 } => continue,
            TerminalPatch::Clear { count: 0 } => continue,
            _ => {}
        }

        if let Some(last) = result.last_mut() {
            if let (
                TerminalPatch::CursorMove {
                    x: last_x,
                    y: last_y,
                },
                TerminalPatch::CursorMove { x, y },
            ) = (&mut *last, &patch)
            {
                *last_x += *x;
                *last_y += *y;
                continue;
            }

            if matches!(&*last, TerminalPatch::CursorTo { .. })
                && matches!(&patch, TerminalPatch::CursorTo { .. })
            {
                *last = patch;
                continue;
            }

            if let (TerminalPatch::StyleStr(last_str), TerminalPatch::StyleStr(str)) =
                (&mut *last, &patch)
            {
                last_str.push_str(str);
                continue;
            }

            if let (TerminalPatch::Hyperlink { uri: last_uri }, TerminalPatch::Hyperlink { uri }) =
                (&*last, &patch)
            {
                if last_uri == uri {
                    continue;
                }
            }

            if matches!(
                (&*last, &patch),
                (TerminalPatch::CursorShow, TerminalPatch::CursorHide)
                    | (TerminalPatch::CursorHide, TerminalPatch::CursorShow)
            ) {
                result.pop();
                continue;
            }
        }

        result.push(patch);
    }

    result
}

fn csi_sequence(body: impl std::fmt::Display) -> String {
    format!("\x1b[{body}")
}

fn cursor_move_sequence(x: i32, y: i32) -> String {
    let mut out = String::new();
    if x < 0 {
        out.push_str(&csi_sequence(format!("{}D", -x)));
    } else if x > 0 {
        out.push_str(&csi_sequence(format!("{x}C")));
    }
    if y < 0 {
        out.push_str(&csi_sequence(format!("{}A", -y)));
    } else if y > 0 {
        out.push_str(&csi_sequence(format!("{y}B")));
    }
    out
}

fn erase_lines_sequence(count: usize) -> String {
    if count == 0 {
        return String::new();
    }

    let mut out = String::new();
    for i in 0..count {
        out.push_str("\x1b[2K");
        if i < count - 1 {
            out.push_str("\x1b[1A");
        }
    }
    out.push_str("\x1b[G");
    out
}

fn hyperlink_patch_sequence(uri: &str) -> String {
    let mut out = Vec::new();
    if uri.is_empty() {
        crate::ansi::hyperlink_close(&mut out).expect("Vec writes cannot fail");
    } else {
        crate::ansi::hyperlink_open(&mut out, uri).expect("Vec writes cannot fail");
    }
    String::from_utf8(out).expect("hyperlink escape sequences are valid UTF-8")
}

/// Serializes terminal-output patches to ANSI, mirroring CC Ink
/// `writeDiffToTerminal(...)`.
///
/// Empty diffs serialize to an empty string. Non-empty diffs are wrapped in DEC
/// 2026 synchronized-output markers unless `skip_sync_markers` is `true`, so
/// callers can gate atomic writes on [`is_synchronized_output_supported`] or on
/// their own terminal capability probe. This is an opt-in serialization helper:
/// it does not write to the terminal or change terminal modes.
pub fn terminal_patches_to_ansi(diff: &[TerminalPatch], skip_sync_markers: bool) -> String {
    if diff.is_empty() {
        return String::new();
    }

    let mut out = String::new();
    if !skip_sync_markers {
        out.push_str("\x1b[?2026h");
    }

    for patch in diff {
        match patch {
            TerminalPatch::Stdout(content) => out.push_str(content),
            TerminalPatch::Clear { count } => out.push_str(&erase_lines_sequence(*count)),
            TerminalPatch::ClearTerminal => out.push_str(clear_terminal_sequence()),
            TerminalPatch::CursorHide => out.push_str("\x1b[?25l"),
            TerminalPatch::CursorShow => out.push_str("\x1b[?25h"),
            TerminalPatch::CursorMove { x, y } => out.push_str(&cursor_move_sequence(*x, *y)),
            TerminalPatch::CursorTo { col } => out.push_str(&csi_sequence(format!("{col}G"))),
            TerminalPatch::CarriageReturn => out.push('\r'),
            TerminalPatch::Hyperlink { uri } => out.push_str(&hyperlink_patch_sequence(uri)),
            TerminalPatch::StyleStr(str) => out.push_str(str),
        }
    }

    if !skip_sync_markers {
        out.push_str("\x1b[?2026l");
    }
    out
}

/// Screen bounds used to validate a fullscreen DECSTBM scroll hint patch.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TerminalScrollHintBounds {
    /// Previous retained screen height in rows.
    pub previous_screen_height: usize,
    /// Next retained screen height in rows.
    pub next_screen_height: usize,
}

/// Options used before emitting a fullscreen DECSTBM scroll hint patch.
///
/// CC Ink only emits the `DECSTBM + SU/SD + reset + home` fast path when it is
/// running in the alternate screen **and** the scroll+diff sequence can be made
/// atomic (`decstbmSafe`). This typed gate lets custom renderers keep the same
/// boundary without duplicating policy checks or accidentally using DECSTBM in
/// main-screen output.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TerminalScrollHintPatchOptions {
    /// Whether the caller is currently rendering in fullscreen/alternate-screen mode.
    pub fullscreen: bool,
    /// Whether the caller will write the scroll patch and following row repairs
    /// inside an atomic/synchronized terminal update.
    pub synchronized_output: bool,
}

impl TerminalScrollHintPatchOptions {
    /// Returns options for a caller that already knows it is in fullscreen and
    /// inside an atomic update scope.
    pub fn fullscreen_synchronized() -> Self {
        Self {
            fullscreen: true,
            synchronized_output: true,
        }
    }

    /// Returns whether emitting a DECSTBM scroll patch is safe under these options.
    pub fn is_decstbm_safe(self) -> bool {
        self.fullscreen && self.synchronized_output
    }
}

/// Reason a fullscreen DECSTBM scroll hint patch is skipped before validation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminalScrollHintPatchSkipReason {
    /// DECSTBM is an alternate-screen/fullscreen optimization and must not be
    /// used for main-screen scrollback-preserving renderers.
    NotFullscreen,
    /// Without atomic synchronized output, users can see the intermediate
    /// hardware-scrolled region before edge rows are repainted.
    NotSynchronized,
}

/// Result of planning a guarded fullscreen DECSTBM scroll hint patch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TerminalScrollHintPatchPlan {
    /// Emit this serialized DECSTBM patch before the sparse row diff/repair pass.
    Emit(String),
    /// Do not emit DECSTBM; fall back to a normal diff path.
    Skip(TerminalScrollHintPatchSkipReason),
}

/// Reason a fullscreen DECSTBM scroll hint cannot be serialized safely.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminalScrollHintRejection {
    /// The hint has `top > bottom`.
    InvalidRegion,
    /// The hinted region is outside either the previous or next retained screen.
    OutOfBounds,
    /// The hint has a zero scroll delta.
    ZeroDelta,
    /// The absolute delta is at least the scroll-region height.
    DeltaTooLarge,
}

/// Validates a fullscreen DECSTBM scroll hint against retained screen bounds.
///
/// This mirrors the guard used by CC Ink `log-update.ts` before emitting
/// `DECSTBM + SU/SD + reset-region + cursor-home`: the region must exist in
/// both previous and next screens, the delta must be non-zero, and the shift
/// must be smaller than the region height. Callers must still gate this helper
/// on fullscreen/alternate-screen mode and atomic-update safety; it does not
/// write to the terminal or change screen mode.
pub fn validate_terminal_scroll_hint(
    hint: crate::canvas::ScrollHint,
    bounds: TerminalScrollHintBounds,
) -> Result<crate::canvas::ScrollHint, TerminalScrollHintRejection> {
    if hint.top > hint.bottom {
        return Err(TerminalScrollHintRejection::InvalidRegion);
    }
    if hint.bottom >= bounds.previous_screen_height || hint.bottom >= bounds.next_screen_height {
        return Err(TerminalScrollHintRejection::OutOfBounds);
    }

    let region_height = hint.bottom - hint.top + 1;
    let abs_delta = hint.delta.unsigned_abs() as usize;
    if abs_delta == 0 {
        return Err(TerminalScrollHintRejection::ZeroDelta);
    }
    if abs_delta >= region_height {
        return Err(TerminalScrollHintRejection::DeltaTooLarge);
    }

    Ok(hint)
}

/// Serializes a fullscreen DECSTBM scroll hint patch.
///
/// The returned string is the same defensive sequence CC Ink emits before the
/// sparse row diff: set a 1-indexed inclusive scroll region, scroll it up (`S`)
/// or down (`T`), reset the scroll region, then home the cursor. This is a
/// fullscreen-only optimization helper for custom renderers; main-screen
/// renderers should not use DECSTBM because it mutates the terminal scroll
/// region and can visibly jump without synchronized output.
pub fn terminal_scroll_hint_to_ansi(
    hint: crate::canvas::ScrollHint,
    bounds: TerminalScrollHintBounds,
) -> Result<String, TerminalScrollHintRejection> {
    let hint = validate_terminal_scroll_hint(hint, bounds)?;
    let mut out = String::new();
    out.push_str(&format!("\x1b[{};{}r", hint.top + 1, hint.bottom + 1));
    let abs_delta = hint.delta.unsigned_abs();
    if hint.delta > 0 {
        out.push_str(&format!("\x1b[{abs_delta}S"));
    } else {
        out.push_str(&format!("\x1b[{abs_delta}T"));
    }
    out.push_str("\x1b[r\x1b[H");
    Ok(out)
}

/// Plans a fullscreen DECSTBM scroll hint patch with CC Ink-style safety gates.
///
/// This checks `fullscreen` and atomic synchronized-output safety before
/// validating geometry. That ordering matches CC Ink `log-update.ts`: when
/// `altScreen` or `decstbmSafe` is false, the renderer simply falls back to its
/// ordinary diff path and does not care whether the scroll hint would otherwise
/// fit terminal bounds.
pub fn plan_terminal_scroll_hint_patch(
    hint: crate::canvas::ScrollHint,
    bounds: TerminalScrollHintBounds,
    options: TerminalScrollHintPatchOptions,
) -> Result<TerminalScrollHintPatchPlan, TerminalScrollHintRejection> {
    if !options.fullscreen {
        return Ok(TerminalScrollHintPatchPlan::Skip(
            TerminalScrollHintPatchSkipReason::NotFullscreen,
        ));
    }
    if !options.synchronized_output {
        return Ok(TerminalScrollHintPatchPlan::Skip(
            TerminalScrollHintPatchSkipReason::NotSynchronized,
        ));
    }

    terminal_scroll_hint_to_ansi(hint, bounds).map(TerminalScrollHintPatchPlan::Emit)
}

/// Writes terminal-output patches to any writer.
///
/// This is the writer form of [`terminal_patches_to_ansi`]. It is useful for
/// custom renderers that already have a patch list and want CC Ink-style
/// serialization without depending on iocraft's built-in retained-canvas
/// terminal renderer.
pub fn write_terminal_patches(
    writer: &mut (impl Write + ?Sized),
    diff: &[TerminalPatch],
    skip_sync_markers: bool,
) -> io::Result<()> {
    let output = terminal_patches_to_ansi(diff, skip_sync_markers);
    writer.write_all(output.as_bytes())
}

/// Options for planning fullscreen/alternate-screen cursor anchor patches.
///
/// CC Ink anchors every non-empty alt-screen diff with `CSI H` and parks the
/// cursor at the terminal bottom after the diff. That self-heals out-of-band
/// cursor drift in tmux/iTerm2 without affecting main-screen scrollback. This
/// option bag exposes the same behavior for custom patch-list renderers while
/// keeping it explicitly fullscreen-only and opt-in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TerminalFullscreenDiffPatchOptions {
    /// Whether the caller is rendering in fullscreen/alternate-screen mode.
    pub fullscreen: bool,
    /// Whether the optimized diff contains any terminal writes.
    pub has_diff: bool,
    /// Whether to erase the alt-screen display before painting this diff.
    ///
    /// This mirrors CC Ink's resize path, where `CSI 2 J` is prepended inside
    /// the same synchronized output block as the repaint so stale wide-line
    /// tails disappear atomically.
    pub erase_before_paint: bool,
    /// Terminal row count used to park the cursor at `row;1H` after the diff.
    ///
    /// Use `None` when the size is unknown; the pre-diff anchor is still useful
    /// for relative diff correctness, but no post-diff park patch is emitted.
    pub terminal_rows: Option<u16>,
}

/// Fullscreen cursor anchor patches for a custom terminal diff.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TerminalFullscreenDiffPatchPlan {
    /// Patch to prepend before the caller's diff, usually `CSI H` or
    /// `CSI 2 J` + `CSI H` after resize.
    pub pre_diff_patch: Option<TerminalPatch>,
    /// Patch to append after the caller's diff to park the cursor at the bottom
    /// row and column 1.
    pub post_diff_patch: Option<TerminalPatch>,
}

impl TerminalFullscreenDiffPatchPlan {
    /// Returns whether no anchor or park patch is required.
    pub fn is_empty(&self) -> bool {
        self.pre_diff_patch.is_none() && self.post_diff_patch.is_none()
    }

    /// Prepends/appends this plan to an existing optimized diff.
    ///
    /// The caller should compute `has_diff` from the optimized diff before
    /// planning, matching CC Ink's order: optimize first, then add the
    /// fullscreen-only cursor preamble/postamble only when there is actual work.
    pub fn apply_to(&self, diff: &mut Vec<TerminalPatch>) {
        if let Some(pre) = &self.pre_diff_patch {
            diff.insert(0, pre.clone());
        }
        if let Some(post) = &self.post_diff_patch {
            diff.push(post.clone());
        }
    }
}

/// Plans CC Ink-style fullscreen cursor anchor/park patches for a terminal diff.
///
/// Returns an empty plan unless `fullscreen && has_diff`. The erase variant uses
/// `CSI 2 J` + `CSI H` rather than [`TerminalPatch::ClearTerminal`] because the
/// alt-screen resize path must erase the visible display without issuing the
/// main-screen scrollback-clear sequence. No terminal I/O is performed.
pub fn plan_terminal_fullscreen_diff_patches(
    options: TerminalFullscreenDiffPatchOptions,
) -> TerminalFullscreenDiffPatchPlan {
    if !options.fullscreen || !options.has_diff {
        return TerminalFullscreenDiffPatchPlan::default();
    }

    let pre_diff_patch = Some(TerminalPatch::Stdout(if options.erase_before_paint {
        "\x1b[2J\x1b[H".to_string()
    } else {
        "\x1b[H".to_string()
    }));
    let post_diff_patch = options
        .terminal_rows
        .filter(|row| *row > 0)
        .map(|row| TerminalPatch::Stdout(format!("\x1b[{row};1H")));

    TerminalFullscreenDiffPatchPlan {
        pre_diff_patch,
        post_diff_patch,
    }
}

/// Options for producing fullscreen/alternate-screen canvas diff patches.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TerminalFullscreenCanvasDiffOptions {
    /// Zero-indexed terminal row offset for the retained canvas origin.
    pub top_row: u16,
    /// Rewrite every retained row even when the canvases compare equal.
    ///
    /// The helper also honors `Canvas::force_full_repaint()` internally; this
    /// flag lets custom renderers request the same behavior without mutating
    /// the canvas metadata.
    pub force_full_repaint: bool,
}

fn canvas_row_ansi_from_col(canvas: &Canvas, y: usize, start_col: usize) -> String {
    let mut row = Vec::new();
    canvas
        .write_ansi_row_from_col_without_newline(y, start_col, &mut row)
        .expect("Vec writes cannot fail");
    String::from_utf8(row).expect("canvas ANSI rows are valid UTF-8")
}

/// Produces absolute fullscreen canvas diff patches for a custom renderer.
///
/// This is an opt-in patch-list counterpart to iocraft's built-in fullscreen
/// retained writer. It mirrors the CC Ink alt-screen pattern of absolute row
/// addressing from a known origin: unchanged rows are skipped, changed rows are
/// written from their first changed column through EOL, damaged rows are honored
/// through [`Canvas::row_change_start`], and rows removed by a shorter next
/// canvas are cleared with `CSI 2 K`. The function performs no terminal I/O and
/// does not add cursor anchor/park or synchronized-output wrappers; pair it with
/// [`plan_terminal_fullscreen_diff_patches`], [`optimize_terminal_patches`], and
/// [`terminal_patches_to_ansi`] as needed.
///
/// For a DECSTBM scroll fast path, shift the previous canvas baseline first
/// (for example via
/// `ScrollFastPathFrameApplication::shift_previous_canvas_for_terminal_diff`)
/// and pass the shifted baseline as `previous` so this sparse diff only emits
/// edge and repair rows.
pub fn terminal_fullscreen_canvas_diff_patches(
    previous: Option<&Canvas>,
    next: &Canvas,
    options: TerminalFullscreenCanvasDiffOptions,
) -> Vec<TerminalPatch> {
    let mut diff = Vec::new();
    let force_full_repaint = options.force_full_repaint || next.should_force_full_repaint();

    let max_height = previous
        .map(|previous| previous.height().max(next.height()))
        .unwrap_or_else(|| next.height());

    for y in 0..max_height {
        let start_col = match previous {
            None => {
                if y < next.height() {
                    Some(0)
                } else {
                    None
                }
            }
            Some(_) if force_full_repaint || y >= next.height() => Some(0),
            Some(previous) => previous.row_change_start(next, y),
        };
        let Some(start_col) = start_col else {
            continue;
        };

        let row = usize::from(options.top_row) + y + 1;
        let col = start_col + 1;
        diff.push(TerminalPatch::Stdout(format!("\x1b[{row};{col}H")));
        if y < next.height() {
            diff.push(TerminalPatch::Stdout(canvas_row_ansi_from_col(
                next, y, start_col,
            )));
        } else {
            diff.push(TerminalPatch::Stdout("\x1b[2K".to_string()));
        }
    }

    diff
}

fn packed_canvas_row_ansi_from_col(
    next: &CanvasPackedScreen,
    pools: &CanvasPackedCellPools,
    style_cache: &mut CanvasStyleTransitionCache,
    y: usize,
    start_col: usize,
) -> String {
    next.ansi_row_with_style_cache(pools, style_cache, y, start_col)
        .expect("packed canvas ANSI rows are valid UTF-8")
}

/// Produces absolute fullscreen packed-screen diff patches for custom renderers.
///
/// This is the packed counterpart to [`terminal_fullscreen_canvas_diff_patches`]
/// and CC Ink's packed `screen.diff(...)` + sparse row writer path. It uses
/// [`CanvasPackedScreen::row_change_start`] to honor damage/shrink regions,
/// serializes changed rows with [`CanvasPackedScreen::write_ansi_row_with_style_cache`],
/// and preserves the same fullscreen absolute row addressing convention. The
/// helper performs no terminal I/O, does not enter fullscreen, and keeps packed
/// screen usage opt-in for custom retained renderers and benchmarks.
pub fn terminal_fullscreen_packed_canvas_diff_patches(
    previous: Option<&CanvasPackedScreen>,
    next: &CanvasPackedScreen,
    pools: &CanvasPackedCellPools,
    style_cache: &mut CanvasStyleTransitionCache,
    options: TerminalFullscreenCanvasDiffOptions,
) -> Vec<TerminalPatch> {
    let mut diff = Vec::new();
    let max_height = previous
        .map(|previous| previous.height.max(next.height))
        .unwrap_or(next.height);

    for y in 0..max_height {
        let start_col = match previous {
            None => (y < next.height).then_some(0),
            Some(_) if options.force_full_repaint || y >= next.height => Some(0),
            Some(previous) => previous.row_change_start(next, y),
        };
        let Some(start_col) = start_col else {
            continue;
        };

        let row = usize::from(options.top_row) + y + 1;
        let col = start_col + 1;
        diff.push(TerminalPatch::Stdout(format!("\x1b[{row};{col}H")));
        if y < next.height {
            diff.push(TerminalPatch::Stdout(packed_canvas_row_ansi_from_col(
                next,
                pools,
                style_cache,
                y,
                start_col,
            )));
        } else {
            diff.push(TerminalPatch::Stdout("\x1b[2K".to_string()));
        }
    }

    diff
}

/// Options for composing a complete fullscreen canvas frame patch list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalFullscreenCanvasFramePatchOptions {
    /// Options used for absolute retained-canvas row diffing.
    pub canvas_diff: TerminalFullscreenCanvasDiffOptions,
    /// Optional fullscreen DECSTBM scroll patch to prepend to the content diff.
    ///
    /// This should already have passed [`plan_terminal_scroll_hint_patch`] or an
    /// equivalent fullscreen/atomic safety gate. It is placed before row repairs,
    /// matching CC Ink's `scrollPatch + screen.diff` ordering.
    pub scroll_patch_ansi: Option<String>,
    /// Whether the final fullscreen cursor preamble should erase the visible
    /// alt-screen before painting. This is the CC Ink resize path.
    pub erase_before_paint: bool,
    /// Terminal row count used to park the cursor after the diff.
    pub terminal_rows: Option<u16>,
    /// Whether to apply [`optimize_terminal_patches`] before adding fullscreen
    /// cursor anchor/park patches.
    ///
    /// CC Ink optimizes the content diff first, then prepends/appends the
    /// fullscreen cursor patches. Set this to `false` only when callers need to
    /// inspect the raw patch boundaries for tests or instrumentation.
    pub optimize: bool,
}

impl Default for TerminalFullscreenCanvasFramePatchOptions {
    fn default() -> Self {
        Self {
            canvas_diff: TerminalFullscreenCanvasDiffOptions::default(),
            scroll_patch_ansi: None,
            erase_before_paint: false,
            terminal_rows: None,
            optimize: true,
        }
    }
}

/// Result of [`plan_terminal_fullscreen_canvas_frame_patches`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TerminalFullscreenCanvasFramePatchPlan {
    /// Final patch list: cursor anchor, optional DECSTBM scroll patch, canvas row
    /// diff/repairs, then cursor park.
    pub patches: Vec<TerminalPatch>,
    /// Cursor pre/post plan that was applied to `patches`.
    pub cursor_plan: TerminalFullscreenDiffPatchPlan,
    /// Number of non-cursor patches after optional optimization.
    pub content_patch_count: usize,
    /// Whether a non-empty DECSTBM scroll patch was included.
    pub had_scroll_patch: bool,
}

impl TerminalFullscreenCanvasFramePatchPlan {
    /// Returns whether the plan has no terminal patches to write.
    pub fn is_empty(&self) -> bool {
        self.patches.is_empty()
    }
}

/// Composes fullscreen retained-canvas frame patches for custom renderers.
///
/// This is the opt-in bridge that packages the CC Ink fullscreen patch-list
/// sequence in Rust-native pieces:
///
/// 1. optional DECSTBM scroll patch (already safety-gated by the caller),
/// 2. absolute retained-canvas sparse row diff/repair patches,
/// 3. optional CC Ink-style optimization,
/// 4. fullscreen cursor anchor/resize-erase preamble and bottom-row park.
///
/// The function never writes to the terminal, never enters fullscreen, and does
/// not make DECSTBM decisions itself. For scroll fast paths, callers should pass
/// a previous canvas baseline that has already been shifted to mirror the
/// hardware scroll.
pub fn plan_terminal_fullscreen_canvas_frame_patches(
    previous: Option<&Canvas>,
    next: &Canvas,
    options: TerminalFullscreenCanvasFramePatchOptions,
) -> TerminalFullscreenCanvasFramePatchPlan {
    let mut content = Vec::new();
    let mut had_scroll_patch = false;

    if let Some(scroll_patch_ansi) = options.scroll_patch_ansi {
        if !scroll_patch_ansi.is_empty() {
            content.push(TerminalPatch::Stdout(scroll_patch_ansi));
            had_scroll_patch = true;
        }
    }

    content.extend(terminal_fullscreen_canvas_diff_patches(
        previous,
        next,
        options.canvas_diff,
    ));

    if options.optimize {
        content = optimize_terminal_patches(content);
    }

    let content_patch_count = content.len();
    let cursor_plan = plan_terminal_fullscreen_diff_patches(TerminalFullscreenDiffPatchOptions {
        fullscreen: true,
        has_diff: !content.is_empty(),
        erase_before_paint: options.erase_before_paint,
        terminal_rows: options.terminal_rows,
    });
    let mut patches = content;
    cursor_plan.apply_to(&mut patches);

    TerminalFullscreenCanvasFramePatchPlan {
        patches,
        cursor_plan,
        content_patch_count,
        had_scroll_patch,
    }
}

/// Composes fullscreen packed-screen frame patches for custom renderers.
///
/// This mirrors [`plan_terminal_fullscreen_canvas_frame_patches`] while keeping
/// the packed `Screen`/`CharPool`/`StylePool` path opt-in. It is useful for
/// retained renderers that already produced [`CanvasPackedScreen`] snapshots and
/// want CC Ink-style absolute fullscreen row diffs without converting back to
/// typed [`Canvas`] rows.
pub fn plan_terminal_fullscreen_packed_canvas_frame_patches(
    previous: Option<&CanvasPackedScreen>,
    next: &CanvasPackedScreen,
    pools: &CanvasPackedCellPools,
    style_cache: &mut CanvasStyleTransitionCache,
    options: TerminalFullscreenCanvasFramePatchOptions,
) -> TerminalFullscreenCanvasFramePatchPlan {
    let mut content = Vec::new();
    let mut had_scroll_patch = false;

    if let Some(scroll_patch_ansi) = options.scroll_patch_ansi {
        if !scroll_patch_ansi.is_empty() {
            content.push(TerminalPatch::Stdout(scroll_patch_ansi));
            had_scroll_patch = true;
        }
    }

    content.extend(terminal_fullscreen_packed_canvas_diff_patches(
        previous,
        next,
        pools,
        style_cache,
        options.canvas_diff,
    ));

    if options.optimize {
        content = optimize_terminal_patches(content);
    }

    let content_patch_count = content.len();
    let cursor_plan = plan_terminal_fullscreen_diff_patches(TerminalFullscreenDiffPatchOptions {
        fullscreen: true,
        has_diff: !content.is_empty(),
        erase_before_paint: options.erase_before_paint,
        terminal_rows: options.terminal_rows,
    });
    let mut patches = content;
    cursor_plan.apply_to(&mut patches);

    TerminalFullscreenCanvasFramePatchPlan {
        patches,
        cursor_plan,
        content_patch_count,
        had_scroll_patch,
    }
}

/// Result of composing a fullscreen canvas frame with a guarded DECSTBM scroll hint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalFullscreenCanvasScrollFramePatchPlan {
    /// Final frame patch plan after optional previous-baseline shifting.
    pub frame: TerminalFullscreenCanvasFramePatchPlan,
    /// Scroll-hint planning result. `None` means the caller supplied no hint;
    /// `Some(Skip(_))` means the fullscreen/atomic safety gate rejected DECSTBM
    /// and the frame fell back to an ordinary diff.
    pub scroll_hint_plan: Option<TerminalScrollHintPatchPlan>,
}

impl TerminalFullscreenCanvasScrollFramePatchPlan {
    /// Returns whether the final frame has no terminal patches to write.
    pub fn is_empty(&self) -> bool {
        self.frame.is_empty()
    }

    /// Returns whether the DECSTBM scroll patch was emitted and included in the frame.
    pub fn had_scroll_patch(&self) -> bool {
        self.frame.had_scroll_patch
    }
}

fn plan_scroll_hint_for_fullscreen_frame(
    hint: Option<crate::canvas::ScrollHint>,
    previous_height: usize,
    next_height: usize,
    options: TerminalScrollHintPatchOptions,
) -> Result<Option<TerminalScrollHintPatchPlan>, TerminalScrollHintRejection> {
    let Some(hint) = hint else {
        return Ok(None);
    };
    plan_terminal_scroll_hint_patch(
        hint,
        TerminalScrollHintBounds {
            previous_screen_height: previous_height,
            next_screen_height: next_height,
        },
        options,
    )
    .map(Some)
}

/// Composes a fullscreen retained-canvas frame and safely applies a scroll hint.
///
/// This is the direct opt-in counterpart to CC Ink `log-update.ts`'s
/// `altScreen && next.scrollHint && decstbmSafe` branch: the hint is first gated
/// by [`plan_terminal_scroll_hint_patch`]; when it emits, a previous-canvas
/// clone is shifted before row diffing so only edge/repair rows are repainted.
/// When the gate skips DECSTBM, the original previous canvas is diffed normally.
/// The function performs no terminal I/O and does not enter fullscreen.
///
/// When `hint` is `Some`, this helper owns the DECSTBM prefix: it overwrites
/// `frame_options.scroll_patch_ansi` on emit and clears it when the safety gate
/// skips. When `hint` is `None`, `frame_options` is passed through unchanged.
pub fn plan_terminal_fullscreen_canvas_scroll_frame_patches(
    previous: &Canvas,
    next: &Canvas,
    hint: Option<crate::canvas::ScrollHint>,
    scroll_options: TerminalScrollHintPatchOptions,
    mut frame_options: TerminalFullscreenCanvasFramePatchOptions,
) -> Result<TerminalFullscreenCanvasScrollFramePatchPlan, TerminalScrollHintRejection> {
    let hint_was_supplied = hint.is_some();
    let scroll_hint_plan = plan_scroll_hint_for_fullscreen_frame(
        hint,
        previous.height(),
        next.height(),
        scroll_options,
    )?;
    let mut shifted_previous = None;

    if let (Some(hint), Some(TerminalScrollHintPatchPlan::Emit(scroll_patch))) =
        (hint, scroll_hint_plan.as_ref())
    {
        let mut shifted = previous.clone();
        shifted.shift_rows(hint.top, hint.bottom, hint.delta);
        shifted_previous = Some(shifted);
        frame_options.scroll_patch_ansi = Some(scroll_patch.clone());
    } else if hint_was_supplied {
        frame_options.scroll_patch_ansi = None;
    }

    let previous_for_diff = shifted_previous.as_ref().unwrap_or(previous);
    let frame =
        plan_terminal_fullscreen_canvas_frame_patches(Some(previous_for_diff), next, frame_options);

    Ok(TerminalFullscreenCanvasScrollFramePatchPlan {
        frame,
        scroll_hint_plan,
    })
}

/// Composes a fullscreen packed-screen frame and safely applies a scroll hint.
///
/// This mirrors [`plan_terminal_fullscreen_canvas_scroll_frame_patches`] for
/// custom renderers that already use [`CanvasPackedScreen`]. A guarded emitted
/// DECSTBM hint shifts a packed previous-screen clone before sparse row diffing;
/// skipped hints fall back to the ordinary packed diff. Packed screen usage
/// remains opt-in and no terminal I/O is performed.
pub fn plan_terminal_fullscreen_packed_canvas_scroll_frame_patches(
    previous: &CanvasPackedScreen,
    next: &CanvasPackedScreen,
    pools: &CanvasPackedCellPools,
    style_cache: &mut CanvasStyleTransitionCache,
    hint: Option<crate::canvas::ScrollHint>,
    scroll_options: TerminalScrollHintPatchOptions,
    mut frame_options: TerminalFullscreenCanvasFramePatchOptions,
) -> Result<TerminalFullscreenCanvasScrollFramePatchPlan, TerminalScrollHintRejection> {
    let hint_was_supplied = hint.is_some();
    let scroll_hint_plan =
        plan_scroll_hint_for_fullscreen_frame(hint, previous.height, next.height, scroll_options)?;
    let mut shifted_previous = None;

    if let (Some(hint), Some(TerminalScrollHintPatchPlan::Emit(scroll_patch))) =
        (hint, scroll_hint_plan.as_ref())
    {
        let mut shifted = previous.clone();
        shifted.shift_rows(hint.top, hint.bottom, hint.delta);
        shifted_previous = Some(shifted);
        frame_options.scroll_patch_ansi = Some(scroll_patch.clone());
    } else if hint_was_supplied {
        frame_options.scroll_patch_ansi = None;
    }

    let previous_for_diff = shifted_previous.as_ref().unwrap_or(previous);
    let frame = plan_terminal_fullscreen_packed_canvas_frame_patches(
        Some(previous_for_diff),
        next,
        pools,
        style_cache,
        frame_options,
    );

    Ok(TerminalFullscreenCanvasScrollFramePatchPlan {
        frame,
        scroll_hint_plan,
    })
}

/// Stateful opt-in fullscreen retained-canvas frame planner.
///
/// CC Ink's `LogUpdate` owns the previous screen and mutates/shifts it before
/// diffing DECSTBM scroll frames. This Rust helper exposes the same state shape
/// without doing terminal I/O or changing modes: callers feed it successive
/// retained [`Canvas`] frames, receive patch plans, and choose when to write the
/// serialized ANSI themselves.
#[derive(Clone, Default)]
pub struct TerminalFullscreenCanvasFrameState {
    previous: Option<Canvas>,
}

impl TerminalFullscreenCanvasFrameState {
    /// Creates an empty state with no trusted previous canvas.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the trusted previous retained canvas, if any.
    pub fn previous(&self) -> Option<&Canvas> {
        self.previous.as_ref()
    }

    /// Drops the trusted previous canvas, e.g. after terminal resume or mode reset.
    pub fn reset(&mut self) {
        self.previous = None;
    }

    /// Plans a fullscreen frame diff and stores `next` as the new previous canvas.
    pub fn plan_frame(
        &mut self,
        next: &Canvas,
        options: TerminalFullscreenCanvasFramePatchOptions,
    ) -> TerminalFullscreenCanvasFramePatchPlan {
        let frame =
            plan_terminal_fullscreen_canvas_frame_patches(self.previous.as_ref(), next, options);
        self.previous = Some(next.clone());
        frame
    }

    /// Plans a fullscreen frame with an optional safety-gated DECSTBM scroll hint.
    ///
    /// If there is no trusted previous frame yet, the hint is ignored and a
    /// normal first-frame diff is produced. When a previous frame exists, this
    /// delegates to [`plan_terminal_fullscreen_canvas_scroll_frame_patches`], so
    /// emitted DECSTBM patches shift the previous baseline before sparse diffing
    /// and skipped hints fall back to an ordinary diff.
    pub fn plan_scroll_frame(
        &mut self,
        next: &Canvas,
        hint: Option<crate::canvas::ScrollHint>,
        scroll_options: TerminalScrollHintPatchOptions,
        mut frame_options: TerminalFullscreenCanvasFramePatchOptions,
    ) -> Result<TerminalFullscreenCanvasScrollFramePatchPlan, TerminalScrollHintRejection> {
        let plan = if let Some(previous) = self.previous.as_ref() {
            plan_terminal_fullscreen_canvas_scroll_frame_patches(
                previous,
                next,
                hint,
                scroll_options,
                frame_options,
            )?
        } else {
            if hint.is_some() {
                frame_options.scroll_patch_ansi = None;
            }
            TerminalFullscreenCanvasScrollFramePatchPlan {
                frame: plan_terminal_fullscreen_canvas_frame_patches(None, next, frame_options),
                scroll_hint_plan: None,
            }
        };
        self.previous = Some(next.clone());
        Ok(plan)
    }
}

/// Stateful opt-in fullscreen packed-screen frame planner.
///
/// This is the packed counterpart to [`TerminalFullscreenCanvasFrameState`]. It
/// retains the previous [`CanvasPackedScreen`] and owns a style-transition cache
/// for sparse row serialization, while the caller keeps the compatible
/// [`CanvasPackedCellPools`]. Packed IDs must remain valid for the supplied
/// pools across frames.
#[derive(Clone, Debug, Default)]
pub struct TerminalFullscreenPackedCanvasFrameState {
    previous: Option<CanvasPackedScreen>,
    style_cache: CanvasStyleTransitionCache,
}

impl TerminalFullscreenPackedCanvasFrameState {
    /// Creates an empty packed frame state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the trusted previous packed screen, if any.
    pub fn previous(&self) -> Option<&CanvasPackedScreen> {
        self.previous.as_ref()
    }

    /// Returns the internal style transition cache.
    pub fn style_cache(&self) -> &CanvasStyleTransitionCache {
        &self.style_cache
    }

    /// Returns the internal style transition cache mutably.
    pub fn style_cache_mut(&mut self) -> &mut CanvasStyleTransitionCache {
        &mut self.style_cache
    }

    /// Drops previous-screen trust and clears cached style transitions.
    pub fn reset(&mut self) {
        self.previous = None;
        self.style_cache.clear();
    }

    /// Plans a packed fullscreen frame diff and stores `next` as the new baseline.
    pub fn plan_frame(
        &mut self,
        next: &CanvasPackedScreen,
        pools: &CanvasPackedCellPools,
        options: TerminalFullscreenCanvasFramePatchOptions,
    ) -> TerminalFullscreenCanvasFramePatchPlan {
        let frame = plan_terminal_fullscreen_packed_canvas_frame_patches(
            self.previous.as_ref(),
            next,
            pools,
            &mut self.style_cache,
            options,
        );
        self.previous = Some(next.clone());
        frame
    }

    /// Plans a packed fullscreen frame with an optional safety-gated DECSTBM hint.
    pub fn plan_scroll_frame(
        &mut self,
        next: &CanvasPackedScreen,
        pools: &CanvasPackedCellPools,
        hint: Option<crate::canvas::ScrollHint>,
        scroll_options: TerminalScrollHintPatchOptions,
        mut frame_options: TerminalFullscreenCanvasFramePatchOptions,
    ) -> Result<TerminalFullscreenCanvasScrollFramePatchPlan, TerminalScrollHintRejection> {
        let plan = if let Some(previous) = self.previous.as_ref() {
            plan_terminal_fullscreen_packed_canvas_scroll_frame_patches(
                previous,
                next,
                pools,
                &mut self.style_cache,
                hint,
                scroll_options,
                frame_options,
            )?
        } else {
            if hint.is_some() {
                frame_options.scroll_patch_ansi = None;
            }
            TerminalFullscreenCanvasScrollFramePatchPlan {
                frame: plan_terminal_fullscreen_packed_canvas_frame_patches(
                    None,
                    next,
                    pools,
                    &mut self.style_cache,
                    frame_options,
                ),
                scroll_hint_plan: None,
            }
        };
        self.previous = Some(next.clone());
        Ok(plan)
    }
}

/// Outcome of routing a real fullscreen mouse event through a
/// [`SelectionController`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FullscreenSelectionEventOutcome {
    /// A left press started or changed selection state.
    Press(SelectionPressOutcome),
    /// A left drag updated selection focus/span.
    Drag,
    /// A release finished selection/click/link handling.
    Release(SelectionReleaseOutcome),
    /// No-button motion produced hover/lost-release handling.
    Hover(SelectionHoverOutcome),
    /// A non-left press reset the multi-click chain.
    NonLeftPress,
    /// A non-left release only finishes an active drag.
    NonLeftRelease {
        /// Whether an active drag was finished.
        finished_drag: bool,
    },
    /// A wheel event cleared an active selection before the caller scrolls.
    Wheel {
        /// Whether a selection existed and was cleared.
        cleared_selection: bool,
    },
    /// Event was not relevant to selection/click handling.
    Ignored,
}

/// Outcome of routing a fullscreen key event through selection behavior.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FullscreenSelectionKeyOutcome {
    /// No selection was active or the key is not a press event.
    Ignored,
    /// The key intentionally preserves selection but does not otherwise act on
    /// it, such as Shift+PageUp or Option+Arrow navigation.
    Preserved,
    /// Selection was cleared because ordinary input occurred.
    Cleared,
    /// Copy should be performed by the caller. This covers Ctrl+C on legacy
    /// terminals where Ctrl+Shift+C is indistinguishable from Ctrl+C.
    CopyRequested,
    /// Keyboard selection extension moved, or attempted to move, focus.
    FocusMoved {
        /// Semantic movement requested by the key.
        movement: SelectionFocusMove,
        /// Whether the focus endpoint actually changed.
        moved: bool,
    },
}

impl SelectionController {
    fn selection_focus_move_for_key(event: &KeyEvent) -> Option<SelectionFocusMove> {
        if !event.modifiers.contains(KeyModifiers::SHIFT)
            || event
                .modifiers
                .intersects(KeyModifiers::ALT | KeyModifiers::META)
        {
            return None;
        }
        match event.code {
            KeyCode::Left => Some(SelectionFocusMove::Left),
            KeyCode::Right => Some(SelectionFocusMove::Right),
            KeyCode::Up => Some(SelectionFocusMove::Up),
            KeyCode::Down => Some(SelectionFocusMove::Down),
            KeyCode::Home => Some(SelectionFocusMove::LineStart),
            KeyCode::End => Some(SelectionFocusMove::LineEnd),
            _ => None,
        }
    }

    fn should_clear_selection_on_key(event: &KeyEvent) -> bool {
        let is_nav = matches!(
            event.code,
            KeyCode::Left
                | KeyCode::Right
                | KeyCode::Up
                | KeyCode::Down
                | KeyCode::Home
                | KeyCode::End
                | KeyCode::PageUp
                | KeyCode::PageDown
        );
        if is_nav
            && event.modifiers.intersects(
                KeyModifiers::SHIFT | KeyModifiers::ALT | KeyModifiers::META | KeyModifiers::SUPER,
            )
        {
            return false;
        }
        true
    }

    /// Routes a decoded fullscreen key event through CC Ink-compatible
    /// selection behavior.
    ///
    /// When selection exists: Esc clears, Ctrl+C requests copy, Shift+arrow /
    /// Home / End extends focus, ordinary input clears, and modified navigation
    /// keys preserve the highlight. Callers remain responsible for performing
    /// clipboard writes when `CopyRequested` is returned.
    pub fn handle_fullscreen_key_event(
        &mut self,
        event: &KeyEvent,
        width: usize,
        height: usize,
    ) -> FullscreenSelectionKeyOutcome {
        if event.kind != KeyEventKind::Press || !self.has_selection() {
            return FullscreenSelectionKeyOutcome::Ignored;
        }

        if event.code == KeyCode::Esc {
            self.selection_mut().clear();
            return FullscreenSelectionKeyOutcome::Cleared;
        }

        if matches!(event.code, KeyCode::Char('c') | KeyCode::Char('C'))
            && event.modifiers.contains(KeyModifiers::CONTROL)
            && !event
                .modifiers
                .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT | KeyModifiers::META)
        {
            return FullscreenSelectionKeyOutcome::CopyRequested;
        }

        if let Some(movement) = Self::selection_focus_move_for_key(event) {
            let moved = self.selection_mut().move_focus_by(movement, width, height);
            return FullscreenSelectionKeyOutcome::FocusMoved { movement, moved };
        }

        if Self::should_clear_selection_on_key(event) {
            self.selection_mut().clear();
            FullscreenSelectionKeyOutcome::Cleared
        } else {
            FullscreenSelectionKeyOutcome::Preserved
        }
    }

    /// Routes a decoded fullscreen mouse event through the CC Ink-compatible
    /// selection/click/link state machine.
    ///
    /// `now_ms` is used for double/triple-click detection. `click_consumed` is
    /// only consulted for left-button releases: pass whether app-level click
    /// dispatch handled the click so hyperlink fallback can be suppressed.
    pub fn handle_fullscreen_mouse_event(
        &mut self,
        canvas: &Canvas,
        event: &FullscreenMouseEvent,
        now_ms: u64,
        click_consumed: bool,
    ) -> FullscreenSelectionEventOutcome {
        match event.kind {
            MouseEventKind::Down(event::MouseButton::Left) => {
                FullscreenSelectionEventOutcome::Press(self.handle_left_press(
                    canvas,
                    event.column as usize,
                    event.row as usize,
                    now_ms,
                    event.modifiers.contains(KeyModifiers::ALT),
                ))
            }
            MouseEventKind::Down(_) => {
                self.handle_non_left_press();
                FullscreenSelectionEventOutcome::NonLeftPress
            }
            MouseEventKind::Drag(event::MouseButton::Left) => {
                self.handle_drag(canvas, event.column as usize, event.row as usize);
                FullscreenSelectionEventOutcome::Drag
            }
            MouseEventKind::Drag(_) => FullscreenSelectionEventOutcome::Ignored,
            MouseEventKind::Up(event::MouseButton::Left) => {
                FullscreenSelectionEventOutcome::Release(self.handle_release_at(
                    canvas,
                    event.column as usize,
                    event.row as usize,
                    click_consumed,
                ))
            }
            MouseEventKind::Up(_) => FullscreenSelectionEventOutcome::NonLeftRelease {
                finished_drag: self.finish_if_dragging(),
            },
            MouseEventKind::Moved => FullscreenSelectionEventOutcome::Hover(
                self.handle_no_button_motion(event.column as usize, event.row as usize),
            ),
            MouseEventKind::ScrollUp
            | MouseEventKind::ScrollDown
            | MouseEventKind::ScrollLeft
            | MouseEventKind::ScrollRight => {
                let cleared_selection = self.has_selection();
                if cleared_selection {
                    self.selection_mut().clear();
                }
                FullscreenSelectionEventOutcome::Wheel { cleared_selection }
            }
        }
    }
}

/// Propagation state shared by every subscriber's copy of a single terminal event.
///
/// When the terminal forwards an event to its subscribers, each subscriber's queue entry
/// carries a clone of the same `Arc<SharedEventState>`. A subscriber that consumes the
/// event marks it stopped, and propagation-aware subscribers that are polled later skip
/// it. Because component hooks are polled depth-first (children before their parents'
/// hooks), this yields bubble-like semantics: the deepest interested component sees the
/// event first, and ancestors only see it if no descendant consumed it.
#[derive(Default)]
pub(crate) struct SharedEventState {
    stopped: std::sync::atomic::AtomicBool,
    default_propagation_stopped: std::sync::atomic::AtomicBool,
    default_prevented: std::sync::atomic::AtomicBool,
}

impl SharedEventState {
    pub(crate) fn stop_propagation(&self) {
        self.stopped
            .store(true, std::sync::atomic::Ordering::SeqCst);
        self.default_propagation_stopped
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    pub(crate) fn stop_component_propagation(&self) {
        self.stopped
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    pub(crate) fn is_propagation_stopped(&self) -> bool {
        self.stopped.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub(crate) fn stop_default_propagation(&self) {
        self.stop_propagation();
    }

    pub(crate) fn is_default_propagation_stopped(&self) -> bool {
        self.default_propagation_stopped
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    pub(crate) fn prevent_default(&self) {
        self.default_prevented
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    pub(crate) fn is_default_prevented(&self) -> bool {
        self.default_prevented
            .load(std::sync::atomic::Ordering::SeqCst)
    }
}

/// A terminal event paired with its shared propagation state, as delivered to
/// [`use_propagated_terminal_events`](crate::hooks::UseTerminalEvents::use_propagated_terminal_events)
/// callbacks.
pub struct PropagatedTerminalEvent {
    event: TerminalEvent,
    state: Arc<SharedEventState>,
}

impl PropagatedTerminalEvent {
    pub(crate) fn new(event: TerminalEvent, state: Arc<SharedEventState>) -> Self {
        Self { event, state }
    }

    /// The underlying terminal event.
    pub fn event(&self) -> &TerminalEvent {
        &self.event
    }

    /// Marks the event as consumed. Propagation-aware subscribers in ancestor
    /// components will not receive it. Plain
    /// [`use_terminal_events`](crate::hooks::UseTerminalEvents::use_terminal_events)
    /// subscribers are unaffected — they observe every event regardless.
    pub fn stop_propagation(&self) {
        self.state.stop_propagation();
    }

    pub(crate) fn stop_component_propagation(&self) {
        self.state.stop_component_propagation();
    }

    /// Returns `true` if a previously-polled subscriber consumed this event.
    pub fn is_propagation_stopped(&self) -> bool {
        self.state.is_propagation_stopped()
    }

    pub(crate) fn stop_default_propagation(&self) {
        self.state.stop_default_propagation();
    }

    /// Prevents the framework-level default action for this terminal event.
    ///
    /// This mirrors DOM/CC Ink `event.preventDefault()`: it does not stop
    /// propagation by itself, but default-action handlers such as
    /// [`FocusScope`](crate::components::FocusScope)'s Tab traversal observe
    /// the flag and skip their built-in behavior.
    pub fn prevent_default(&self) {
        self.state.prevent_default();
    }

    /// Returns `true` if a previously-polled subscriber called
    /// [`Self::prevent_default`].
    pub fn is_default_prevented(&self) -> bool {
        self.state.is_default_prevented()
    }
}

struct TerminalEventsInner {
    pending: VecDeque<(TerminalEvent, Arc<SharedEventState>)>,
    waker: Option<Waker>,
}

/// A stream of terminal events.
pub struct TerminalEvents {
    inner: Arc<Mutex<TerminalEventsInner>>,
}

impl TerminalEvents {
    /// Polls for the next event together with its shared propagation state.
    pub(crate) fn poll_next_shared(
        &mut self,
        cx: &mut Context,
    ) -> Poll<Option<(TerminalEvent, Arc<SharedEventState>)>> {
        let mut inner = self.inner.lock().unwrap();
        if let Some(entry) = inner.pending.pop_front() {
            Poll::Ready(Some(entry))
        } else {
            inner.waker = Some(cx.waker().clone());
            Poll::Pending
        }
    }
}

impl Stream for TerminalEvents {
    type Item = TerminalEvent;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Option<Self::Item>> {
        self.get_mut()
            .poll_next_shared(cx)
            .map(|opt| opt.map(|(event, _)| event))
    }
}

trait TerminalImpl: Write + Send {
    fn refresh_size(&mut self) {}
    fn size(&self) -> Option<(u16, u16)> {
        None
    }
    fn set_size_from_resize_event(&mut self, _width: u16, _height: u16) {}
    fn set_mouse_capture(&mut self, _enabled: bool) -> io::Result<()> {
        Ok(())
    }

    /// Re-asserts terminal modes that some emulators drop during resize.
    fn reassert_after_resize(&mut self) -> io::Result<()> {
        Ok(())
    }

    /// Re-asserts terminal modes after a long stdin silence gap.
    ///
    /// This mirrors CC Ink's `STDIN_RESUME_GAP_MS` self-heal for tmux attach,
    /// SSH reconnect, and laptop sleep/wake. It should be non-destructive: do
    /// not clear/re-enter the alternate screen here, only restore idempotent or
    /// stack-balanced modes such as extended keys and mouse tracking.
    fn reassert_after_stdin_resume(&mut self) -> io::Result<()> {
        Ok(())
    }

    /// Replaces the keyboard enhancement (kitty protocol) flags requested from the
    /// terminal. If enhancement is already active, the old flags are popped and the
    /// new ones pushed immediately; otherwise the new flags take effect the next time
    /// raw mode is enabled.
    fn set_keyboard_enhancement_flags(
        &mut self,
        _flags: event::KeyboardEnhancementFlags,
    ) -> io::Result<()> {
        Ok(())
    }

    /// Polls for a pending "resumed from suspension" signal (SIGCONT on unix). Used by
    /// [`Terminal::wait`] to wake the render loop so it can repair the display after the
    /// user foregrounds the process (e.g. Ctrl+Z followed by `fg`).
    fn poll_resumed(&mut self, _cx: &mut Context<'_>) -> Poll<()> {
        Poll::Pending
    }

    /// Returns `true` (and clears the flag) if the process was resumed from suspension
    /// since the last call.
    fn take_resumed(&mut self) -> bool {
        false
    }

    /// Re-applies terminal modes and clears cached output state after the process was
    /// resumed from suspension. The shell typically restores cooked mode while the app
    /// is stopped, so raw mode (and friends) must be re-enabled unconditionally, and the
    /// next canvas write must not assume anything previously on screen survived.
    fn reinitialize_after_resume(&mut self) -> io::Result<()> {
        Ok(())
    }

    /// Hands the terminal back to the shell and suspends the process (Ctrl+Z on
    /// Unix). Non-Unix backends or non-interactive inputs may treat this as a no-op.
    fn suspend(&mut self) -> io::Result<()> {
        Ok(())
    }

    /// Positions the physical terminal cursor. When `visible` is true (ratatui model),
    /// the cursor is shown; when false (ink model), it is positioned for IME only.
    /// Called after each canvas write.
    fn position_cursor(
        &mut self,
        _declaration: Option<crate::canvas::CursorDeclaration>,
    ) -> io::Result<()> {
        Ok(())
    }

    fn position_cursor_would_write(
        &self,
        _declaration: Option<crate::canvas::CursorDeclaration>,
    ) -> bool {
        false
    }

    fn is_raw_mode_supported(&self) -> bool {
        false
    }
    fn is_raw_mode_enabled(&self) -> bool;
    fn set_raw_mode_enabled(&mut self, _raw_mode_enabled: bool) -> io::Result<()> {
        Ok(())
    }
    fn is_fullscreen(&self) -> bool {
        false
    }
    fn set_dynamic_alternate_screen(
        &mut self,
        _request: Option<crate::context::AlternateScreenRequest>,
    ) -> io::Result<bool> {
        Ok(false)
    }
    fn set_decstbm_safe(&mut self, _safe: bool) {}
    fn clear_canvas_would_write(&self) -> bool {
        true
    }
    fn clear_canvas(&mut self) -> io::Result<()>;
    fn clear_screen(&mut self) -> io::Result<()> {
        self.clear_canvas()
    }
    fn clear_terminal(&mut self) -> io::Result<()> {
        self.clear_canvas()
    }
    fn write_canvas_would_write(&self, _prev: Option<&Canvas>, _canvas: &Canvas) -> bool {
        true
    }
    fn write_canvas(&mut self, prev: Option<&Canvas>, canvas: &Canvas) -> io::Result<()>;
    fn event_stream(&mut self) -> io::Result<BoxStream<'static, TerminalEvent>>;
    fn dest(&mut self) -> &mut dyn Write;
    fn alt(&mut self) -> &mut dyn Write;
}

/// State shared between the SIGCONT listener thread and the render loop.
#[cfg(unix)]
struct ResumeSignalShared {
    resumed: std::sync::atomic::AtomicBool,
    waker: Mutex<Option<Waker>>,
}

/// Listens for SIGCONT on a dedicated thread and records it in shared state.
///
/// signal-hook's iterator API is used so the actual signal handler only performs an
/// async-signal-safe self-pipe write; the (signal-unsafe) waker invocation happens on
/// this listener thread, in normal thread context.
#[cfg(unix)]
struct ResumeSignalListener {
    shared: Arc<ResumeSignalShared>,
    handle: signal_hook::iterator::Handle,
    thread: Option<std::thread::JoinHandle<()>>,
}

#[cfg(unix)]
impl ResumeSignalListener {
    fn new() -> io::Result<Self> {
        use std::sync::atomic::AtomicBool;

        let shared = Arc::new(ResumeSignalShared {
            resumed: AtomicBool::new(false),
            waker: Mutex::new(None),
        });
        let mut signals =
            signal_hook::iterator::Signals::new([signal_hook::consts::signal::SIGCONT])?;
        let handle = signals.handle();
        let thread_shared = shared.clone();
        let thread = std::thread::Builder::new()
            .name("iocraft-sigcont".into())
            .spawn(move || {
                for _ in &mut signals {
                    thread_shared
                        .resumed
                        .store(true, std::sync::atomic::Ordering::SeqCst);
                    if let Some(waker) = thread_shared.waker.lock().unwrap().take() {
                        waker.wake();
                    }
                }
            })?;
        Ok(Self {
            shared,
            handle,
            thread: Some(thread),
        })
    }

    fn poll_resumed(&self, cx: &mut Context<'_>) -> Poll<()> {
        use std::sync::atomic::Ordering;
        if self.shared.resumed.load(Ordering::SeqCst) {
            return Poll::Ready(());
        }
        *self.shared.waker.lock().unwrap() = Some(cx.waker().clone());
        // Re-check to close the race where the signal arrived between the first load
        // and the waker registration.
        if self.shared.resumed.load(Ordering::SeqCst) {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }

    fn take_resumed(&self) -> bool {
        self.shared
            .resumed
            .swap(false, std::sync::atomic::Ordering::SeqCst)
    }

    fn mark_resumed(&self) {
        self.shared
            .resumed
            .store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(waker) = self.shared.waker.lock().unwrap().take() {
            waker.wake();
        }
    }
}

#[cfg(unix)]
impl Drop for ResumeSignalListener {
    fn drop(&mut self) {
        self.handle.close();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn clear_canvas_inline(
    dest: &mut (impl Write + ?Sized),
    prev_canvas_height: u16,
) -> io::Result<()> {
    let lines_to_rewind = prev_canvas_height - 1;
    if lines_to_rewind == 0 {
        dest.queue(cursor::MoveToColumn(0))?
            .queue(terminal::Clear(terminal::ClearType::FromCursorDown))?;
        Ok(())
    } else {
        dest.queue(cursor::MoveToPreviousLine(lines_to_rewind as _))?
            .queue(terminal::Clear(terminal::ClearType::FromCursorDown))?;
        Ok(())
    }
}

/// Global bookkeeping for the panic hook that restores the terminal to a usable state
/// before the default hook prints the panic message and backtrace.
///
/// Without this, a panic in fullscreen mode is invisible: the message goes to the
/// alternate screen, which is discarded when the process exits and the shell restores
/// the main screen. Raw mode similarly survives the panic, leaving the user's shell
/// with no echo and broken line input.
struct PanicRestoreState {
    /// Number of live `StdTerminal`s that have modified terminal state. The hook is a
    /// no-op when this is zero (e.g. after a clean shutdown).
    live_terminals: usize,
    /// Number of live terminals in fullscreen (alternate screen) mode.
    fullscreen_terminals: usize,
}

static PANIC_RESTORE_STATE: Mutex<PanicRestoreState> = Mutex::new(PanicRestoreState {
    live_terminals: 0,
    fullscreen_terminals: 0,
});

static INSTALL_PANIC_HOOK: std::sync::Once = std::sync::Once::new();
static XTVERSION_NAME: OnceLock<Mutex<Option<String>>> = OnceLock::new();

fn xtversion_name_state() -> &'static Mutex<Option<String>> {
    XTVERSION_NAME.get_or_init(|| Mutex::new(None))
}

/// Best-effort terminal restoration, safe to call from a panic hook. Writes go directly
/// to stdout: raw mode is a tty-level state (restored via ioctl, not the stream), and
/// the escape sequences reach the same tty regardless of which stream the renderer was
/// configured to use.
fn restore_terminal_for_panic() {
    let state = PANIC_RESTORE_STATE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if state.live_terminals == 0 {
        return;
    }
    let mut stdout = io::stdout();
    let _ = terminal::disable_raw_mode();
    let _ = stdout.queue(event::DisableBracketedPaste);
    let _ = stdout.queue(event::DisableFocusChange);
    let _ = stdout.queue(event::DisableMouseCapture);
    if state.fullscreen_terminals > 0 {
        let _ = stdout.queue(terminal::LeaveAlternateScreen);
    }
    let _ = stdout.queue(cursor::Show);
    let _ = stdout.flush();
}

fn register_terminal_for_panic_restore(fullscreen: bool) {
    INSTALL_PANIC_HOOK.call_once(|| {
        let prev_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore_terminal_for_panic();
            prev_hook(info);
        }));
    });
    let mut state = PANIC_RESTORE_STATE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    state.live_terminals += 1;
    if fullscreen {
        state.fullscreen_terminals += 1;
    }
}

fn register_fullscreen_mode_for_panic_restore() {
    let mut state = PANIC_RESTORE_STATE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    state.fullscreen_terminals += 1;
}

fn unregister_fullscreen_mode_for_panic_restore() {
    let mut state = PANIC_RESTORE_STATE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    state.fullscreen_terminals = state.fullscreen_terminals.saturating_sub(1);
}

fn unregister_terminal_for_panic_restore(fullscreen: bool) {
    let mut state = PANIC_RESTORE_STATE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    state.live_terminals = state.live_terminals.saturating_sub(1);
    if fullscreen {
        state.fullscreen_terminals = state.fullscreen_terminals.saturating_sub(1);
    }
    if state.live_terminals == 0 {
        state.fullscreen_terminals = 0;
    }
}

fn is_synchronized_output_supported_with_env(
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

fn detect_terminal_from_env(mut env_lookup: impl FnMut(&str) -> Option<String>) -> Option<String> {
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

fn supports_extended_keys_with_env(mut env_lookup: impl FnMut(&str) -> Option<String>) -> bool {
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

fn has_cursor_up_viewport_yank_bug_with_env(
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

fn clear_terminal_sequence_with_env(
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

fn is_xterm_js_with_env_and_xtversion(
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

fn osc_color_query_sequence_with_env(
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

fn parse_numeric_params(params: &str) -> Option<Vec<u32>> {
    if params.is_empty() {
        return Some(Vec::new());
    }
    params
        .split(';')
        .map(|param| param.parse::<u32>().ok())
        .collect()
}

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

/// Output returned by [`TerminalRawInputFrontend`].
#[derive(Clone, Debug)]
pub struct TerminalRawInputFrontendOutput {
    /// Parsed CC Ink-style raw input items.
    pub parsed: Vec<TerminalParsedInput>,
    /// The same input converted into iocraft [`TerminalEvent`]s.
    pub events: Vec<TerminalEvent>,
    /// Timeout the frontend should arm for flushing an incomplete sequence, if any.
    pub pending_flush_timeout: Option<Duration>,
}

impl TerminalRawInputFrontendOutput {
    fn new(parsed: Vec<TerminalParsedInput>, pending_flush_timeout: Option<Duration>) -> Self {
        let events = terminal_parsed_inputs_to_events(parsed.clone());
        Self {
            parsed,
            events,
            pending_flush_timeout,
        }
    }
}

/// Opt-in raw-stdin frontend bridge.
///
/// This wraps [`TerminalInputParser`] with the state a custom raw input backend
/// usually needs: feed byte/string chunks, get both parsed CC Ink-style input
/// and iocraft events, inspect the next incomplete-sequence flush timeout, and
/// flush only when the underlying input source has no queued bytes. It does not
/// enable raw mode, read from stdin, write terminal queries, or replace
/// iocraft's default crossterm backend.
#[derive(Clone, Debug, Default)]
pub struct TerminalRawInputFrontend {
    parser: TerminalInputParser,
}

impl TerminalRawInputFrontend {
    /// Creates a stdin-oriented raw input frontend with X10 mouse parsing enabled.
    pub fn new() -> Self {
        Self {
            parser: TerminalInputParser::new(),
        }
    }

    /// Creates a raw input frontend with explicit X10 mouse parsing control.
    pub fn with_x10_mouse(x10_mouse: bool) -> Self {
        Self {
            parser: TerminalInputParser::with_x10_mouse(x10_mouse),
        }
    }

    /// Feeds a raw UTF-8 input chunk.
    pub fn feed(&mut self, input: &str) -> TerminalRawInputFrontendOutput {
        let parsed = self.parser.feed(input);
        TerminalRawInputFrontendOutput::new(parsed, self.pending_flush_timeout())
    }

    /// Feeds raw bytes after applying CC Ink's `inputToString(Buffer)` rules.
    pub fn feed_bytes(&mut self, input: &[u8]) -> TerminalRawInputFrontendOutput {
        let parsed = self.parser.feed_bytes(input);
        TerminalRawInputFrontendOutput::new(parsed, self.pending_flush_timeout())
    }

    /// Flushes any buffered incomplete input now.
    pub fn flush(&mut self) -> TerminalRawInputFrontendOutput {
        let parsed = self.parser.flush();
        TerminalRawInputFrontendOutput::new(parsed, self.pending_flush_timeout())
    }

    /// Flushes buffered input only if CC Ink's incomplete-sequence rule says to flush.
    ///
    /// Pass `true` when the underlying input source reports queued bytes; in
    /// that case the caller should re-arm [`Self::pending_flush_timeout`] rather
    /// than splitting a likely-continuing escape sequence.
    pub fn flush_if_due(
        &mut self,
        input_available: bool,
    ) -> Option<TerminalRawInputFrontendOutput> {
        self.parser
            .should_flush_incomplete(input_available)
            .then(|| self.flush())
    }

    /// Clears parser/tokenizer state.
    pub fn reset(&mut self) {
        self.parser.reset();
    }

    /// Returns the buffered incomplete sequence, if any.
    pub fn buffered(&self) -> &str {
        self.parser.buffered()
    }

    /// Returns whether an incomplete escape/control sequence is buffered.
    pub fn has_incomplete_sequence(&self) -> bool {
        self.parser.has_incomplete_sequence()
    }

    /// Returns whether the frontend is inside a bracketed paste payload.
    pub fn is_in_paste(&self) -> bool {
        self.parser.is_in_paste()
    }

    /// Returns the CC Ink-style timeout to arm before flushing incomplete input.
    pub fn pending_flush_timeout(&self) -> Option<Duration> {
        self.parser.pending_flush_timeout()
    }

    /// Returns whether an expired incomplete-sequence timeout should flush now.
    pub fn should_flush_incomplete(&self, input_available: bool) -> bool {
        self.parser.should_flush_incomplete(input_available)
    }
}

/// Opt-in byte stream adapter for caller-owned async raw input readers.
///
/// This is a small runtime-neutral bridge: it reads byte chunks from any
/// [`futures::io::AsyncRead`] and yields them as `io::Result<Vec<u8>>`. It does
/// not enable raw mode, own stdin, or change terminal state. Pair it with
/// [`TerminalRawInputFallibleEventStream`] when you want reader errors to remain
/// visible to the caller.
pub struct TerminalRawInputByteStream<R> {
    reader: R,
    buffer: Vec<u8>,
    done: bool,
}

impl<R> TerminalRawInputByteStream<R> {
    /// Creates a byte stream using [`TERMINAL_RAW_INPUT_DEFAULT_CHUNK_SIZE`].
    pub fn new(reader: R) -> Self {
        Self::with_chunk_size(reader, TERMINAL_RAW_INPUT_DEFAULT_CHUNK_SIZE)
    }

    /// Creates a byte stream with an explicit non-zero chunk size.
    pub fn with_chunk_size(reader: R, chunk_size: usize) -> Self {
        Self {
            reader,
            buffer: vec![0; chunk_size.max(1)],
            done: false,
        }
    }

    /// Returns the wrapped reader.
    pub fn into_inner(self) -> R {
        self.reader
    }
}

impl<R> Stream for TerminalRawInputByteStream<R>
where
    R: futures::io::AsyncRead + Unpin,
{
    type Item = io::Result<Vec<u8>>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if this.done {
            return Poll::Ready(None);
        }

        match Pin::new(&mut this.reader).poll_read(cx, &mut this.buffer) {
            Poll::Ready(Ok(0)) => {
                this.done = true;
                Poll::Ready(None)
            }
            Poll::Ready(Ok(len)) => Poll::Ready(Some(Ok(this.buffer[..len].to_vec()))),
            Poll::Ready(Err(error)) => {
                this.done = true;
                Poll::Ready(Some(Err(error)))
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

/// Opt-in fallible event stream adapter for caller-owned raw byte sources.
///
/// Use this when the byte producer can fail (for example an async stdin reader)
/// and the application wants to handle the error instead of silently dropping
/// it. The parsing/flush behavior matches [`TerminalRawInputEventStream`], but
/// each yielded item is wrapped in `io::Result`.
pub struct TerminalRawInputFallibleEventStream {
    source: BoxStream<'static, io::Result<Vec<u8>>>,
    frontend: TerminalRawInputFrontend,
    pending_events: VecDeque<TerminalEvent>,
    pending_error: Option<io::Error>,
    flush_delay: Option<Pin<Box<Delay>>>,
    source_done: bool,
}

impl TerminalRawInputFallibleEventStream {
    /// Creates a fallible event stream with stdin-oriented X10 mouse parsing enabled.
    pub fn new<S>(source: S) -> Self
    where
        S: Stream<Item = io::Result<Vec<u8>>> + Send + 'static,
    {
        Self::with_frontend(source, TerminalRawInputFrontend::new())
    }

    /// Creates a fallible event stream around an async reader.
    pub fn from_reader<R>(reader: R) -> Self
    where
        R: futures::io::AsyncRead + Unpin + Send + 'static,
    {
        Self::new(TerminalRawInputByteStream::new(reader))
    }

    /// Creates a fallible event stream with explicit chunk size and reader.
    pub fn from_reader_with_chunk_size<R>(reader: R, chunk_size: usize) -> Self
    where
        R: futures::io::AsyncRead + Unpin + Send + 'static,
    {
        Self::new(TerminalRawInputByteStream::with_chunk_size(
            reader, chunk_size,
        ))
    }

    /// Creates a fallible event stream using an explicit raw input frontend.
    pub fn with_frontend<S>(source: S, frontend: TerminalRawInputFrontend) -> Self
    where
        S: Stream<Item = io::Result<Vec<u8>>> + Send + 'static,
    {
        Self {
            source: source.boxed(),
            frontend,
            pending_events: VecDeque::new(),
            pending_error: None,
            flush_delay: None,
            source_done: false,
        }
    }

    /// Creates a fallible event stream around an async reader and explicit frontend.
    pub fn from_reader_with_frontend<R>(reader: R, frontend: TerminalRawInputFrontend) -> Self
    where
        R: futures::io::AsyncRead + Unpin + Send + 'static,
    {
        Self::with_frontend(TerminalRawInputByteStream::new(reader), frontend)
    }

    /// Returns the wrapped frontend.
    pub fn frontend(&self) -> &TerminalRawInputFrontend {
        &self.frontend
    }

    /// Returns the wrapped frontend mutably.
    pub fn frontend_mut(&mut self) -> &mut TerminalRawInputFrontend {
        &mut self.frontend
    }

    /// Returns the currently buffered incomplete sequence, if any.
    pub fn buffered(&self) -> &str {
        self.frontend.buffered()
    }

    /// Returns whether an incomplete sequence is buffered.
    pub fn has_incomplete_sequence(&self) -> bool {
        self.frontend.has_incomplete_sequence()
    }

    fn push_output(&mut self, output: TerminalRawInputFrontendOutput) {
        self.pending_events.extend(output.events);
        self.arm_flush_delay(output.pending_flush_timeout);
    }

    fn arm_flush_delay(&mut self, timeout: Option<Duration>) {
        self.flush_delay = timeout.map(|duration| Box::pin(Delay::new(duration)));
    }
}

impl Stream for TerminalRawInputFallibleEventStream {
    type Item = io::Result<TerminalEvent>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        loop {
            if let Some(event) = this.pending_events.pop_front() {
                return Poll::Ready(Some(Ok(event)));
            }

            if let Some(error) = this.pending_error.take() {
                return Poll::Ready(Some(Err(error)));
            }

            if this.source_done {
                if this.frontend.has_incomplete_sequence() {
                    let output = this.frontend.flush();
                    this.push_output(output);
                    continue;
                }
                return Poll::Ready(None);
            }

            match this.source.as_mut().poll_next(cx) {
                Poll::Ready(Some(Ok(bytes))) => {
                    let output = this.frontend.feed_bytes(&bytes);
                    this.push_output(output);
                    continue;
                }
                Poll::Ready(Some(Err(error))) => {
                    this.source_done = true;
                    this.pending_error = Some(error);
                    continue;
                }
                Poll::Ready(None) => {
                    this.source_done = true;
                    continue;
                }
                Poll::Pending => {}
            }

            if let Some(delay) = this.flush_delay.as_mut() {
                if delay.as_mut().poll(cx).is_ready() {
                    this.flush_delay = None;
                    if let Some(output) = this.frontend.flush_if_due(false) {
                        this.push_output(output);
                        continue;
                    }
                }
            }

            return Poll::Pending;
        }
    }
}

/// Opt-in event stream adapter for custom raw-stdin byte sources.
///
/// This is a runtime-agnostic bridge between a caller-owned byte stream and
/// iocraft [`TerminalEvent`]s. It uses [`TerminalRawInputFrontend`] internally,
/// arms CC Ink-compatible incomplete-sequence flush timers, and flushes buffered
/// input when the source ends. It does not enable raw mode, read from `stdin`,
/// write terminal queries, or replace the default crossterm backend; applications
/// must explicitly construct it around their own raw byte source.
pub struct TerminalRawInputEventStream {
    source: BoxStream<'static, Vec<u8>>,
    frontend: TerminalRawInputFrontend,
    pending_events: VecDeque<TerminalEvent>,
    flush_delay: Option<Pin<Box<Delay>>>,
    source_done: bool,
}

impl TerminalRawInputEventStream {
    /// Creates an event stream with stdin-oriented X10 mouse parsing enabled.
    pub fn new<S>(source: S) -> Self
    where
        S: Stream<Item = Vec<u8>> + Send + 'static,
    {
        Self::with_frontend(source, TerminalRawInputFrontend::new())
    }

    /// Creates an event stream using an explicit raw input frontend.
    pub fn with_frontend<S>(source: S, frontend: TerminalRawInputFrontend) -> Self
    where
        S: Stream<Item = Vec<u8>> + Send + 'static,
    {
        Self {
            source: source.boxed(),
            frontend,
            pending_events: VecDeque::new(),
            flush_delay: None,
            source_done: false,
        }
    }

    /// Returns the wrapped frontend.
    pub fn frontend(&self) -> &TerminalRawInputFrontend {
        &self.frontend
    }

    /// Returns the wrapped frontend mutably.
    pub fn frontend_mut(&mut self) -> &mut TerminalRawInputFrontend {
        &mut self.frontend
    }

    /// Returns the currently buffered incomplete sequence, if any.
    pub fn buffered(&self) -> &str {
        self.frontend.buffered()
    }

    /// Returns whether an incomplete sequence is buffered.
    pub fn has_incomplete_sequence(&self) -> bool {
        self.frontend.has_incomplete_sequence()
    }

    fn push_output(&mut self, output: TerminalRawInputFrontendOutput) {
        self.pending_events.extend(output.events);
        self.arm_flush_delay(output.pending_flush_timeout);
    }

    fn arm_flush_delay(&mut self, timeout: Option<Duration>) {
        self.flush_delay = timeout.map(|duration| Box::pin(Delay::new(duration)));
    }
}

impl Stream for TerminalRawInputEventStream {
    type Item = TerminalEvent;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        loop {
            if let Some(event) = this.pending_events.pop_front() {
                return Poll::Ready(Some(event));
            }

            if this.source_done {
                if this.frontend.has_incomplete_sequence() {
                    let output = this.frontend.flush();
                    this.push_output(output);
                    continue;
                }
                return Poll::Ready(None);
            }

            match this.source.as_mut().poll_next(cx) {
                Poll::Ready(Some(bytes)) => {
                    let output = this.frontend.feed_bytes(&bytes);
                    this.push_output(output);
                    continue;
                }
                Poll::Ready(None) => {
                    this.source_done = true;
                    continue;
                }
                Poll::Pending => {}
            }

            if let Some(delay) = this.flush_delay.as_mut() {
                if delay.as_mut().poll(cx).is_ready() {
                    this.flush_delay = None;
                    if let Some(output) = this.frontend.flush_if_due(false) {
                        this.push_output(output);
                        continue;
                    }
                }
            }

            return Poll::Pending;
        }
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

fn tokens_to_parsed_inputs(
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

fn parse_two_numeric_params(params: &str) -> Option<(u32, u32)> {
    let (first, second) = params.split_once(';')?;
    let first = first.parse::<u32>().ok()?;
    let second = second.parse::<u32>().ok()?;
    Some((first, second))
}

/// Parses a terminal query response sequence.
///
/// This mirrors CC Ink's response parsing for DECRPM, DA1/DA2, Kitty keyboard
/// flags, DECXCPR cursor position, OSC responses, and XTVERSION. Returns
/// `None` when the sequence is not a recognized terminal response and should be
/// treated as ordinary input by higher-level parsers.
pub fn parse_terminal_response(sequence: &str) -> Option<TerminalResponse> {
    if let Some(rest) = sequence.strip_prefix("\x1b[?") {
        if let Some(body) = rest.strip_suffix("$y") {
            let (mode, status) = parse_two_numeric_params(body)?;
            return Some(TerminalResponse::Decrpm { mode, status });
        }

        if let Some(body) = rest.strip_suffix('c') {
            return Some(TerminalResponse::Da1 {
                params: parse_numeric_params(body)?,
            });
        }

        if let Some(body) = rest.strip_suffix('u') {
            return Some(TerminalResponse::KittyKeyboard {
                flags: body.parse::<u32>().ok()?,
            });
        }

        if let Some(body) = rest.strip_suffix('R') {
            let (row, col) = parse_two_numeric_params(body)?;
            return Some(TerminalResponse::CursorPosition { row, col });
        }
    }

    if let Some(body) = sequence
        .strip_prefix("\x1b[>")
        .and_then(|rest| rest.strip_suffix('c'))
    {
        return Some(TerminalResponse::Da2 {
            params: parse_numeric_params(body)?,
        });
    }

    if let Some(body) = sequence.strip_prefix("\x1b]") {
        let body = if let Some(body) = body.strip_suffix("\x1b\\") {
            body
        } else {
            body.strip_suffix('\x07')?
        };
        let (code, data) = body.split_once(';')?;
        return Some(TerminalResponse::Osc {
            code: code.parse::<u32>().ok()?,
            data: data.to_string(),
        });
    }

    parse_xtversion_response(sequence).map(|name| TerminalResponse::Xtversion {
        name: name.to_string(),
    })
}

/// Incremental parser for terminal query response sequences.
///
/// This is a small Rust counterpart to CC Ink's termio tokenizer plus
/// `parseTerminalResponse(...)` path. It can be used by custom frontends that
/// read raw terminal input: feed chunks as they arrive, and the parser emits
/// recognized [`TerminalResponse`] values while buffering incomplete CSI, OSC,
/// DCS, APC, PM, and SOS string sequences across chunk boundaries.
#[derive(Clone, Debug, Default)]
pub struct TerminalResponseParser {
    buffer: String,
}

impl TerminalResponseParser {
    /// Creates an empty response parser.
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds a raw input chunk and returns any recognized terminal responses.
    ///
    /// Non-response input is ignored. Incomplete escape sequences are retained
    /// until a future call completes them.
    pub fn feed(&mut self, input: &str) -> Vec<TerminalResponse> {
        self.buffer.push_str(input);
        self.drain_responses()
    }

    /// Feeds raw input bytes after applying CC Ink's `inputToString(Buffer)` rules.
    pub fn feed_bytes(&mut self, input: &[u8]) -> Vec<TerminalResponse> {
        self.feed(&terminal_input_bytes_to_string(input))
    }

    /// Feeds a raw input chunk and wraps recognized responses as terminal events.
    pub fn feed_events(&mut self, input: &str) -> Vec<TerminalEvent> {
        self.feed(input)
            .into_iter()
            .map(TerminalEvent::Response)
            .collect()
    }

    /// Feeds raw input bytes and wraps recognized responses as terminal events.
    pub fn feed_bytes_events(&mut self, input: &[u8]) -> Vec<TerminalEvent> {
        self.feed_bytes(input)
            .into_iter()
            .map(TerminalEvent::Response)
            .collect()
    }

    /// Returns the currently buffered incomplete sequence, if any.
    pub fn buffered(&self) -> &str {
        &self.buffer
    }

    /// Clears any buffered incomplete input.
    pub fn clear(&mut self) {
        self.buffer.clear();
    }

    fn drain_responses(&mut self) -> Vec<TerminalResponse> {
        let mut responses = Vec::new();

        loop {
            let Some(start) = self
                .buffer
                .as_bytes()
                .iter()
                .position(|byte| *byte == b'\x1b')
            else {
                self.buffer.clear();
                break;
            };
            if start > 0 {
                self.buffer.drain(..start);
            }

            let bytes = self.buffer.as_bytes();
            if bytes.len() < 2 {
                break;
            }

            match bytes[1] {
                b'[' => {
                    let Some(end) = bytes.iter().enumerate().skip(2).find_map(|(index, byte)| {
                        (0x40..=0x7e).contains(byte).then_some(index + 1)
                    }) else {
                        break;
                    };
                    let sequence = self.buffer[..end].to_string();
                    self.buffer.drain(..end);
                    if let Some(response) = parse_terminal_response(&sequence) {
                        responses.push(response);
                    }
                }
                b']' | b'P' | b'_' | b'^' | b'X' => {
                    let mut end = None;
                    let mut index = 2usize;
                    while index < bytes.len() {
                        match bytes[index] {
                            b'\x07' => {
                                end = Some(index + 1);
                                break;
                            }
                            b'\x1b' if index + 1 >= bytes.len() => break,
                            b'\x1b' if bytes[index + 1] == b'\\' => {
                                end = Some(index + 2);
                                break;
                            }
                            _ => {}
                        }
                        index += 1;
                    }

                    let Some(end) = end else {
                        break;
                    };
                    let sequence = self.buffer[..end].to_string();
                    self.buffer.drain(..end);
                    if let Some(response) = parse_terminal_response(&sequence) {
                        responses.push(response);
                    }
                }
                _ => {
                    // Not a response sequence we recognize. Drop this ESC and
                    // keep scanning so mixed key/text input does not block later
                    // query replies in the same chunk.
                    self.buffer.drain(..1);
                }
            }
        }

        responses
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum TerminalQueryMatcher {
    Decrpm(u32),
    Da1,
    Da2,
    KittyKeyboard,
    CursorPosition,
    Osc(u32),
    Xtversion,
}

impl TerminalQueryMatcher {
    fn matches(&self, response: &TerminalResponse) -> bool {
        match (self, response) {
            (Self::Decrpm(expected), TerminalResponse::Decrpm { mode, .. }) => expected == mode,
            (Self::Da1, TerminalResponse::Da1 { .. }) => true,
            (Self::Da2, TerminalResponse::Da2 { .. }) => true,
            (Self::KittyKeyboard, TerminalResponse::KittyKeyboard { .. }) => true,
            (Self::CursorPosition, TerminalResponse::CursorPosition { .. }) => true,
            (Self::Osc(expected), TerminalResponse::Osc { code, .. }) => expected == code,
            (Self::Xtversion, TerminalResponse::Xtversion { .. }) => true,
            _ => false,
        }
    }
}

/// A terminal query request paired with the response kind that should satisfy it.
///
/// This is the Rust counterpart to CC Ink's `TerminalQuery<T>` shape from
/// `terminal-querier.ts`: callers write [`Self::request`] to stdout and feed
/// parsed [`TerminalResponse`] values back into [`TerminalQuerier::on_response`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalQuery {
    request: String,
    matcher: TerminalQueryMatcher,
}

impl TerminalQuery {
    /// Builds a DECRQM query for a DEC private mode.
    pub fn decrqm(mode: u32) -> Self {
        Self {
            request: decrqm_query_sequence(mode),
            matcher: TerminalQueryMatcher::Decrpm(mode),
        }
    }

    /// Builds a DA1 primary device-attributes query.
    pub fn da1() -> Self {
        Self {
            request: da1_query_sequence().to_string(),
            matcher: TerminalQueryMatcher::Da1,
        }
    }

    /// Builds a DA2 secondary device-attributes query.
    pub fn da2() -> Self {
        Self {
            request: da2_query_sequence().to_string(),
            matcher: TerminalQueryMatcher::Da2,
        }
    }

    /// Builds a Kitty keyboard flags query.
    pub fn kitty_keyboard() -> Self {
        Self {
            request: kitty_keyboard_query_sequence().to_string(),
            matcher: TerminalQueryMatcher::KittyKeyboard,
        }
    }

    /// Builds a DECXCPR cursor-position query.
    pub fn cursor_position() -> Self {
        Self {
            request: cursor_position_query_sequence().to_string(),
            matcher: TerminalQueryMatcher::CursorPosition,
        }
    }

    /// Builds an OSC dynamic color query, such as OSC 10 or OSC 11.
    pub fn osc_color(code: u32) -> Self {
        Self {
            request: osc_color_query_sequence(code),
            matcher: TerminalQueryMatcher::Osc(code),
        }
    }

    /// Builds an XTVERSION terminal name/version query.
    pub fn xtversion() -> Self {
        Self {
            request: xtversion_query_sequence().to_string(),
            matcher: TerminalQueryMatcher::Xtversion,
        }
    }

    /// Returns the escape sequence to write to stdout for this query.
    pub fn request(&self) -> &str {
        &self.request
    }
}

/// Future returned by [`TerminalQuerier::send`].
///
/// It resolves to `Some(response)` when a matching terminal response arrives,
/// or `None` when a DA1 flush sentinel proves that the terminal ignored the
/// query.
pub struct PendingTerminalQuery {
    receiver: oneshot::Receiver<Option<TerminalResponse>>,
}

impl Future for PendingTerminalQuery {
    type Output = Option<TerminalResponse>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match Pin::new(&mut self.get_mut().receiver).poll(cx) {
            Poll::Ready(Ok(response)) => Poll::Ready(response),
            Poll::Ready(Err(_)) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}

/// Future returned by [`TerminalQuerier::flush`].
pub struct PendingTerminalFlush {
    receiver: oneshot::Receiver<()>,
}

impl Future for PendingTerminalFlush {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match Pin::new(&mut self.get_mut().receiver).poll(cx) {
            Poll::Ready(_) => Poll::Ready(()),
            Poll::Pending => Poll::Pending,
        }
    }
}

enum PendingTerminalRequest {
    Query {
        matcher: TerminalQueryMatcher,
        sender: oneshot::Sender<Option<TerminalResponse>>,
    },
    Sentinel {
        sender: oneshot::Sender<()>,
    },
}

fn dispatch_terminal_query_response(
    queue: &mut VecDeque<PendingTerminalRequest>,
    response: TerminalResponse,
) {
    if let Some(index) = queue.iter().position(|pending| match pending {
        PendingTerminalRequest::Query { matcher, .. } => matcher.matches(&response),
        PendingTerminalRequest::Sentinel { .. } => false,
    }) {
        if let Some(PendingTerminalRequest::Query { sender, .. }) = queue.remove(index) {
            let _ = sender.send(Some(response));
        }
        return;
    }

    if !matches!(response, TerminalResponse::Da1 { .. }) {
        return;
    }

    let Some(sentinel_index) = queue
        .iter()
        .position(|pending| matches!(pending, PendingTerminalRequest::Sentinel { .. }))
    else {
        return;
    };

    for _ in 0..=sentinel_index {
        match queue.pop_front() {
            Some(PendingTerminalRequest::Query { sender, .. }) => {
                let _ = sender.send(None);
            }
            Some(PendingTerminalRequest::Sentinel { sender }) => {
                let _ = sender.send(());
            }
            None => break,
        }
    }
}

/// Timeout-free terminal query coordinator.
///
/// This mirrors CC Ink's `TerminalQuerier`: queries and DA1 sentinels are queued
/// in write order, responses are delivered with [`Self::on_response`], and a
/// DA1 sentinel resolves earlier unanswered queries as unsupported instead of
/// relying on wall-clock timeouts.
pub struct TerminalQuerier<W> {
    output: W,
    queue: VecDeque<PendingTerminalRequest>,
}

impl<W: Write> TerminalQuerier<W> {
    /// Creates a new terminal querier that writes requests to `output`.
    pub fn new(output: W) -> Self {
        Self {
            output,
            queue: VecDeque::new(),
        }
    }

    /// Returns an immutable reference to the wrapped output writer.
    pub fn output_ref(&self) -> &W {
        &self.output
    }

    /// Returns a mutable reference to the wrapped output writer.
    pub fn output_mut(&mut self) -> &mut W {
        &mut self.output
    }

    /// Consumes the querier and returns the wrapped output writer.
    pub fn into_output(self) -> W {
        self.output
    }

    /// Sends a query and returns a future for its response.
    ///
    /// The future resolves to `None` when a later [`Self::flush`] sentinel
    /// arrives first, matching CC Ink's no-timeout unsupported-query behavior.
    pub fn send(&mut self, query: TerminalQuery) -> io::Result<PendingTerminalQuery> {
        let (sender, receiver) = oneshot::channel();
        self.queue.push_back(PendingTerminalRequest::Query {
            matcher: query.matcher.clone(),
            sender,
        });
        self.output.write_all(query.request.as_bytes())?;
        Ok(PendingTerminalQuery { receiver })
    }

    /// Sends the DA1 sentinel and returns a future that resolves when DA1 arrives.
    ///
    /// All unanswered queries queued before this sentinel resolve to `None` when
    /// the sentinel response is observed.
    pub fn flush(&mut self) -> io::Result<PendingTerminalFlush> {
        let (sender, receiver) = oneshot::channel();
        self.queue
            .push_back(PendingTerminalRequest::Sentinel { sender });
        self.output.write_all(da1_query_sequence().as_bytes())?;
        Ok(PendingTerminalFlush { receiver })
    }

    /// Dispatches a parsed terminal response event to the queued query batch.
    ///
    /// Returns `true` when `event` was a [`TerminalEvent::Response`] and was
    /// therefore handled by this querier.
    pub fn on_event(&mut self, event: &TerminalEvent) -> bool {
        if let TerminalEvent::Response(response) = event {
            self.on_response(response.clone());
            true
        } else {
            false
        }
    }

    /// Dispatches a parsed terminal response to the queued query batch.
    ///
    /// First matching query wins. If nothing matches and the response is DA1,
    /// the first pending sentinel is completed and all earlier queries resolve
    /// as unsupported.
    pub fn on_response(&mut self, response: TerminalResponse) {
        dispatch_terminal_query_response(&mut self.queue, response);
    }
}

/// Records the terminal name returned by XTVERSION.
///
/// Like CC Ink's `setXtversionName()`, this is intentionally first-writer-wins:
/// startup probing may race with later duplicate replies, but terminal identity
/// is not expected to change during a session.
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

struct StdTerminal<'a> {
    input_is_terminal: bool,
    dest: Box<dyn Write + Send + 'a>,
    alt: Box<dyn Write + Send + 'a>,
    fullscreen: bool,
    mouse_capture: bool,
    dynamic_alternate_saved_mouse_capture: Option<bool>,
    raw_mode_enabled: bool,
    enabled_keyboard_enhancement: bool,
    keyboard_enhancement_flags: event::KeyboardEnhancementFlags,
    prev_canvas_top_row: u16,
    prev_canvas_height: u16,
    prev_size_on_write: Option<(u16, u16)>,
    size: Option<(u16, u16)>,
    /// Whether the physical cursor is currently shown (via a cursor declaration).
    cursor_visible: bool,
    /// In inline mode, how many rows above the canvas-bottom baseline the cursor was
    /// moved by the last cursor declaration. The row-diff logic in `write_canvas` and
    /// `clear_canvas` assumes the cursor sits on the canvas's last row, so this
    /// displacement must be undone (see `restore_cursor_baseline`) before either runs.
    cursor_displacement_rows: u16,
    /// Whether the last inline canvas write ended in the terminal's right-margin
    /// auto-wrap pending state. VT terminals typically delay wrapping until the next
    /// printable byte; before relative cursor movement we resolve that pending state
    /// with CR, mirroring Ink/log-update's cursor model.
    inline_pending_wrap: bool,
    /// Whether it is safe to emit a DECSTBM scroll patch before the row diff. CC
    /// only enables this optimization when the whole scroll+diff sequence is
    /// atomic (DEC 2026 synchronized update); otherwise users can see the region
    /// jump before edge rows are repainted.
    decstbm_safe: bool,
    /// The first inline diff after a fresh initial frame is treated as
    /// contaminated and repainted in full. This mirrors Ink's contaminated-frame
    /// guard: after process startup/re-entry we do not fully control what rows
    /// precede the canvas in the main screen, so establish a clean retained
    /// baseline before trusting sparse row diffs.
    inline_force_full_rewrite_next_diff: bool,
    #[cfg(unix)]
    resume_signal: Option<ResumeSignalListener>,
}

impl Write for StdTerminal<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.dest.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.dest.flush()
    }
}

impl TerminalImpl for StdTerminal<'_> {
    fn refresh_size(&mut self) {
        self.size = terminal::size().ok()
    }

    fn size(&self) -> Option<(u16, u16)> {
        self.size
    }

    fn set_size_from_resize_event(&mut self, width: u16, height: u16) {
        self.size = Some((width, height));
    }

    fn set_mouse_capture(&mut self, enabled: bool) -> io::Result<()> {
        if self.mouse_capture != enabled {
            self.mouse_capture = enabled;
            if self.raw_mode_enabled {
                if enabled {
                    self.dest.execute(event::EnableMouseCapture)?;
                } else {
                    self.dest.execute(event::DisableMouseCapture)?;
                }
            }
        }
        Ok(())
    }

    fn reassert_after_resize(&mut self) -> io::Result<()> {
        // Mirrors CC Ink's alt-screen resize self-heal: some terminals reset
        // mouse tracking on resize. Re-emit the mode without consulting the
        // cached boolean, because the whole point is to repair terminal-side
        // state that may have diverged from our retained state.
        if self.raw_mode_enabled {
            self.dest.execute(event::EnableFocusChange)?;
            if self.mouse_capture {
                self.dest.execute(event::EnableMouseCapture)?;
            }
        }
        Ok(())
    }

    fn reassert_after_stdin_resume(&mut self) -> io::Result<()> {
        // Match CC Ink's stdin-gap recovery: restore non-destructive terminal
        // modes that reconnect/sleep can drop. Kitty keyboard is stack-based,
        // so pop-before-push via reassert_keyboard_enhancement() prevents
        // accumulating protocol depth on every idle gap.
        if self.raw_mode_enabled {
            self.reassert_keyboard_enhancement()?;
            if self.mouse_capture {
                self.dest.execute(event::EnableMouseCapture)?;
            }
        }
        Ok(())
    }

    fn set_keyboard_enhancement_flags(
        &mut self,
        flags: event::KeyboardEnhancementFlags,
    ) -> io::Result<()> {
        if self.keyboard_enhancement_flags != flags {
            self.keyboard_enhancement_flags = flags;
            if self.enabled_keyboard_enhancement {
                // Swap the active flags in place.
                self.dest.execute(event::PopKeyboardEnhancementFlags)?;
                self.dest
                    .execute(event::PushKeyboardEnhancementFlags(flags))?;
            }
        }
        Ok(())
    }

    #[cfg(unix)]
    fn poll_resumed(&mut self, cx: &mut Context<'_>) -> Poll<()> {
        match &self.resume_signal {
            Some(signal) => signal.poll_resumed(cx),
            None => Poll::Pending,
        }
    }

    #[cfg(unix)]
    fn take_resumed(&mut self) -> bool {
        self.resume_signal
            .as_ref()
            .is_some_and(|signal| signal.take_resumed())
    }

    fn reinitialize_after_resume(&mut self) -> io::Result<()> {
        // The shell restored cooked mode while we were stopped, but our bookkeeping
        // still says raw mode is on. Re-apply every terminal mode unconditionally,
        // bypassing apply_raw_mode_enabled's change detection.
        if self.raw_mode_enabled {
            terminal::enable_raw_mode()?;
            self.reassert_keyboard_enhancement()?;
            if self.mouse_capture && !self.fullscreen {
                self.dest.execute(event::EnableMouseCapture)?;
            }
            self.dest.execute(event::EnableFocusChange)?;
            self.dest.execute(event::EnableBracketedPaste)?;
        }
        self.dest.queue(cursor::Hide)?;
        self.cursor_visible = false;
        self.cursor_displacement_rows = 0;
        if self.fullscreen {
            // Re-entering the alternate screen after SIGCONT/shell handoff must
            // be destructive, matching CC Ink's reenterAltScreen(): the terminal
            // may have dropped mode 1049 or preserved stale alt-buffer cells.
            // Enter alt-screen, erase it, and home the cursor so the next full
            // write starts from a known blank anchor.
            self.dest
                .queue(terminal::EnterAlternateScreen)?
                .queue(terminal::Clear(terminal::ClearType::All))?
                .queue(cursor::MoveTo(0, 0))?;
            if self.mouse_capture {
                self.dest.execute(event::EnableMouseCapture)?;
            }
        }
        // Anything we previously drew can no longer be trusted: in inline mode the
        // shell printed over our output; in fullscreen mode we just re-entered the
        // alternate screen, which starts blank. Treat the next write as a first
        // write so the canvas is fully re-rendered rather than row-diffed.
        self.prev_canvas_height = 0;
        self.prev_canvas_top_row = 0;
        self.prev_size_on_write = None;
        self.inline_pending_wrap = false;
        self.inline_force_full_rewrite_next_diff = false;
        Ok(())
    }

    fn suspend(&mut self) -> io::Result<()> {
        #[cfg(unix)]
        {
            if !self.input_is_terminal {
                return Ok(());
            }

            // Match CC Ink's Ctrl+Z handoff: leave the terminal in cooked mode,
            // show the cursor, and disable input side modes so the shell prompt
            // does not receive bracketed-paste/focus/mouse/extended-key noise
            // while this process is stopped. Keep our bookkeeping intact so the
            // SIGCONT repair path can reassert the modes that were active.
            self.restore_cursor_baseline()?;
            if self.raw_mode_enabled {
                terminal::disable_raw_mode()?;
                self.dest.execute(event::DisableBracketedPaste)?;
                self.dest.execute(event::DisableFocusChange)?;
                if self.mouse_capture {
                    self.dest.execute(event::DisableMouseCapture)?;
                }
                if self.enabled_keyboard_enhancement {
                    self.dest.execute(event::PopKeyboardEnhancementFlags)?;
                }
            }
            self.dest.queue(cursor::Show)?;
            self.dest.flush()?;

            signal_hook::low_level::raise(signal_hook::consts::signal::SIGSTOP)
                .map_err(io::Error::other)?;
            if let Some(signal) = &self.resume_signal {
                signal.mark_resumed();
            }
        }
        Ok(())
    }

    fn is_raw_mode_supported(&self) -> bool {
        self.input_is_terminal
    }

    fn is_raw_mode_enabled(&self) -> bool {
        self.raw_mode_enabled
    }

    fn set_raw_mode_enabled(&mut self, raw_mode_enabled: bool) -> io::Result<()> {
        self.apply_raw_mode_enabled(raw_mode_enabled)
    }

    fn is_fullscreen(&self) -> bool {
        self.fullscreen
    }

    fn set_dynamic_alternate_screen(
        &mut self,
        request: Option<crate::context::AlternateScreenRequest>,
    ) -> io::Result<bool> {
        match (self.fullscreen, request) {
            (false, Some(request)) => {
                self.restore_cursor_baseline()?;
                if self.cursor_visible {
                    self.dest.queue(cursor::Hide)?;
                    self.cursor_visible = false;
                }
                self.dest
                    .queue(terminal::EnterAlternateScreen)?
                    .queue(terminal::Clear(terminal::ClearType::All))?
                    .queue(cursor::MoveTo(0, 0))?;
                self.fullscreen = true;
                self.dynamic_alternate_saved_mouse_capture = Some(self.mouse_capture);
                register_fullscreen_mode_for_panic_restore();
                self.set_mouse_capture(request.mouse_tracking)?;
                self.reset_retained_output_state();
                self.dest.flush()?;
                Ok(true)
            }
            (true, Some(request)) => {
                self.set_mouse_capture(request.mouse_tracking)?;
                Ok(false)
            }
            (true, None) => {
                self.restore_cursor_baseline()?;
                let restore_mouse_capture = self
                    .dynamic_alternate_saved_mouse_capture
                    .take()
                    .unwrap_or(false);
                self.set_mouse_capture(restore_mouse_capture)?;
                self.dest.queue(terminal::LeaveAlternateScreen)?;
                self.fullscreen = false;
                unregister_fullscreen_mode_for_panic_restore();
                self.reset_retained_output_state();
                self.dest.flush()?;
                Ok(true)
            }
            (false, None) => Ok(false),
        }
    }

    fn set_decstbm_safe(&mut self, safe: bool) {
        self.decstbm_safe = safe;
    }

    fn position_cursor(
        &mut self,
        declaration: Option<crate::canvas::CursorDeclaration>,
    ) -> io::Result<()> {
        self.resolve_inline_pending_wrap()?;
        match declaration {
            Some(decl) => {
                let (mut x, mut y) = (decl.x as u16, decl.y as u16);
                if self.fullscreen {
                    if let Some((cols, rows)) = self.size {
                        x = x.min(cols.saturating_sub(1));
                        y = y.min(rows.saturating_sub(1));
                    }
                    self.dest
                        .queue(cursor::MoveTo(x, self.prev_canvas_top_row + y))?;
                } else {
                    self.restore_cursor_baseline()?;
                    let last_row = self.prev_canvas_height.saturating_sub(1);
                    let rows_up = last_row.saturating_sub(y);
                    self.move_inline_to_previous_line(rows_up)?;
                    self.move_inline_to_column(x)?;
                    self.cursor_displacement_rows = rows_up;
                }
                if decl.visible && !self.cursor_visible {
                    self.dest.queue(cursor::SetCursorStyle::SteadyBlock)?;
                    self.dest.queue(cursor::Show)?;
                    self.cursor_visible = true;
                } else if !decl.visible && self.cursor_visible {
                    self.dest.queue(cursor::Hide)?;
                    self.cursor_visible = false;
                }
            }
            None => {
                if self.cursor_visible {
                    self.dest.queue(cursor::Hide)?;
                    self.cursor_visible = false;
                }
            }
        }
        self.dest.flush()?;
        Ok(())
    }

    fn position_cursor_would_write(
        &self,
        declaration: Option<crate::canvas::CursorDeclaration>,
    ) -> bool {
        self.inline_pending_wrap
            || match declaration {
                Some(_) => true,
                None => self.cursor_visible,
            }
    }

    fn clear_canvas_would_write(&self) -> bool {
        self.inline_pending_wrap || self.cursor_displacement_rows > 0 || self.prev_canvas_height > 0
    }

    fn clear_canvas(&mut self) -> io::Result<()> {
        self.restore_cursor_baseline()?;
        if self.prev_canvas_height == 0 {
            return Ok(());
        }

        if self.fullscreen {
            self.dest
                .queue(cursor::MoveTo(0, self.prev_canvas_top_row))?
                .queue(terminal::Clear(terminal::ClearType::FromCursorDown))?;
            return Ok(());
        }

        if let Some(size) = self.size {
            if self.prev_canvas_height >= size.1 {
                // A viewport-tall inline canvas cannot be cleared reliably with
                // relative line erases: some rows may already be in native
                // scrollback. Clear the visible screen and repaint from a fresh
                // baseline, but do NOT purge scrollback. Local JSX overlays such
                // as /resume must preserve the user's mouse-wheel terminal history.
                self.dest
                    .queue(terminal::Clear(terminal::ClearType::All))?
                    .queue(cursor::MoveTo(0, 0))?;
                return Ok(());
            }
        }

        clear_canvas_inline(&mut *self.dest, self.prev_canvas_height)
    }

    fn clear_screen(&mut self) -> io::Result<()> {
        self.restore_cursor_baseline()?;
        self.dest
            .queue(terminal::Clear(terminal::ClearType::All))?
            .queue(cursor::MoveTo(0, 0))?;
        self.reset_retained_output_state();
        Ok(())
    }

    fn clear_terminal(&mut self) -> io::Result<()> {
        self.restore_cursor_baseline()?;
        self.dest.write_all(clear_terminal_sequence().as_bytes())?;
        self.reset_retained_output_state();
        Ok(())
    }

    fn write_canvas_would_write(&self, prev: Option<&Canvas>, canvas: &Canvas) -> bool {
        if self.inline_pending_wrap || self.cursor_displacement_rows > 0 {
            return true;
        }

        let Some(prev) = prev else {
            return self.fullscreen || canvas.height() > 0;
        };

        if self.fullscreen {
            if self.prev_size_on_write != self.size {
                return true;
            }
            if canvas.should_force_full_repaint() {
                return prev.height().max(canvas.height()) > 0;
            }
            if self
                .fullscreen_scroll_hint_candidate(prev, canvas)
                .is_some()
            {
                return true;
            }
            let max_height = prev.height().max(canvas.height());
            return (0..max_height).any(|y| prev.row_change_start(canvas, y).is_some());
        }

        if self.inline_force_full_rewrite_next_diff || self.inline_resize_requires_full_rewrite() {
            return self.prev_canvas_height > 0 || canvas.height() > 0;
        }
        if canvas.should_force_full_repaint() {
            return canvas.height() > self.inline_unreachable_rows_for_diff(prev.height())
                || self.prev_canvas_height > 0;
        }

        let prev_height = prev.height();
        let new_height = canvas.height();
        if self.inline_shrink_requires_full_rewrite(prev_height, new_height) {
            return self.prev_canvas_height > 0 || canvas.height() > 0;
        }

        let max_height = prev_height.max(new_height);
        (0..max_height).any(|y| {
            if prev.row_change_start(canvas, y).is_none() {
                return false;
            }
            !(y < self.inline_unreachable_rows_for_diff(prev_height) && prev.row_eq(canvas, y))
        })
    }

    fn write_canvas(&mut self, prev: Option<&Canvas>, canvas: &Canvas) -> io::Result<()> {
        self.restore_cursor_baseline()?;
        let Some(prev) = prev else {
            // No previous canvas: full write.
            if self.fullscreen {
                self.prev_canvas_top_row = 0;
                self.dest.queue(cursor::MoveTo(0, 0))?;
            }
            self.prev_canvas_height = canvas.height() as _;
            self.prev_size_on_write = self.size;
            if self.fullscreen {
                self.write_fullscreen_canvas_absolute(canvas)?;
                self.park_fullscreen_cursor_after_write(canvas.height())?;
            } else {
                self.write_inline_canvas_without_final_newline(canvas)?;
                self.inline_force_full_rewrite_next_diff = true;
            }
            return Ok(());
        };

        if self.fullscreen {
            if self.prev_size_on_write != self.size {
                // If the terminal is changing size, clear it to make sure we don't leave
                // artifacts. This is especially important when the terminal is shrinking, since
                // characters might flow outside of the visible terminal, where they can't be
                // cleared with `\033[K` and oddly may re-enter the terminal as visible characters
                // are cleared.
                self.clear_fullscreen_screen()?;
                self.prev_canvas_height = canvas.height() as _;
                self.prev_canvas_top_row = 0;
                self.prev_size_on_write = self.size;
                self.write_fullscreen_canvas_absolute(canvas)?;
                self.park_fullscreen_cursor_after_write(canvas.height())?;
                return Ok(());
            }

            let force_full_repaint = canvas.should_force_full_repaint();
            let scroll_hint = if force_full_repaint {
                None
            } else {
                self.fullscreen_scroll_hint(prev, canvas)
            };
            let shifted_prev_storage = if let Some(hint) = scroll_hint {
                self.write_fullscreen_scroll_patch(hint)?;
                let mut shifted = prev.clone();
                shifted.shift_rows(hint.top, hint.bottom, hint.delta);
                Some(shifted)
            } else {
                None
            };
            let mut wrote_anything = scroll_hint.is_some();
            let prev_for_diff = shifted_prev_storage.as_ref().unwrap_or(prev);

            // Fullscreen: absolute positioning.
            let top_row = self.prev_canvas_top_row;
            let max_height = prev_for_diff.height().max(canvas.height());
            for y in 0..max_height {
                let start_col = if force_full_repaint || y >= canvas.height() {
                    Some(0)
                } else {
                    prev_for_diff.row_change_start(canvas, y)
                };
                let Some(start_col) = start_col else {
                    continue;
                };

                wrote_anything = true;
                self.dest
                    .queue(cursor::MoveTo(start_col as u16, top_row + y as u16))?;
                if y < canvas.height() {
                    canvas.write_ansi_row_from_col_without_newline(
                        y,
                        start_col,
                        &mut *self.dest,
                    )?;
                } else {
                    self.dest
                        .queue(terminal::Clear(terminal::ClearType::CurrentLine))?;
                }
            }
            if wrote_anything {
                self.park_fullscreen_cursor_after_write(canvas.height())?;
            }
            self.prev_canvas_height = canvas.height() as _;
            return Ok(());
        }

        if self.inline_force_full_rewrite_next_diff || self.inline_resize_requires_full_rewrite() {
            self.clear_canvas()?;
            self.prev_canvas_height = canvas.height() as _;
            self.prev_size_on_write = self.size;
            self.write_inline_canvas_without_final_newline(canvas)?;
            self.inline_force_full_rewrite_next_diff = false;
            return Ok(());
        }

        if canvas.should_force_full_repaint() {
            if !self.inline_shrink_requires_full_rewrite(prev.height(), canvas.height())
                && self.write_inline_full_repaint(prev, canvas)?
            {
                self.prev_canvas_height = canvas.height() as _;
                self.prev_size_on_write = self.size;
                return Ok(());
            }

            self.clear_canvas()?;
            self.prev_canvas_height = canvas.height() as _;
            self.prev_size_on_write = self.size;
            self.write_inline_canvas_without_final_newline(canvas)?;
            self.inline_force_full_rewrite_next_diff = false;
            return Ok(());
        }
        self.prev_size_on_write = self.size;

        // Inline: row diff with relative cursor movement.
        let prev_height = prev.height();
        let new_height = canvas.height();

        if self.inline_shrink_requires_full_rewrite(prev_height, new_height) {
            self.clear_canvas()?;
            self.prev_canvas_height = canvas.height() as _;
            self.write_inline_canvas_without_final_newline(canvas)?;
            self.inline_force_full_rewrite_next_diff = false;
            return Ok(());
        }

        let max_height = prev_height.max(new_height);
        let mut current_y = prev_height.saturating_sub(1);

        for y in 0..max_height {
            let Some(mut start_col) = prev.row_change_start(canvas, y) else {
                continue;
            };
            if y >= new_height {
                start_col = 0;
            }
            // If a changed row has scrolled off the top of the reachable area,
            // we can't reach it with cursor movement — fall back to full rewrite.
            // The reachable boundary includes Ink/log-update's cursorRestoreScroll
            // guard: when the previous frame filled/overflowed the viewport, the
            // top visible row is also treated as unsafe because cursor restoration
            // can push one additional row into scrollback on real terminals.
            if y < self.inline_unreachable_rows_for_diff(prev_height) {
                if prev.row_eq(canvas, y) {
                    continue;
                }
                self.clear_canvas()?;
                self.prev_canvas_height = canvas.height() as _;
                self.write_inline_canvas_without_final_newline(canvas)?;
                self.inline_force_full_rewrite_next_diff = false;
                return Ok(());
            }
            let row_move_left_at_col0 = match y.cmp(&current_y) {
                std::cmp::Ordering::Less => {
                    self.move_inline_to_previous_line((current_y - y) as u16)?;
                    true
                }
                std::cmp::Ordering::Greater => {
                    // Lines within the previous canvas already exist in the
                    // terminal and can be reached with MoveToNextLine (CSI E).
                    // Lines beyond prev_height don't exist yet — we must emit
                    // \r\n to create them, since CSI E won't extend the
                    // scrollback when the cursor is at the bottom of the screen.
                    let last_existing_line = prev_height.saturating_sub(1).max(current_y);
                    if y <= last_existing_line {
                        self.move_inline_to_next_line((y - current_y) as u16)?;
                    } else {
                        let move_to_last = last_existing_line.saturating_sub(current_y);
                        if move_to_last > 0 {
                            self.move_inline_to_next_line(move_to_last as u16)?;
                        }
                        let new_lines = y - last_existing_line;
                        for _ in 0..new_lines {
                            self.write_inline_newline()?;
                        }
                    }
                    true
                }
                std::cmp::Ordering::Equal => false,
            };
            if start_col > 0 || !row_move_left_at_col0 {
                self.move_inline_to_column(start_col as u16)?;
            }
            current_y = y;

            if y < new_height {
                self.write_inline_canvas_row_from_col_without_newline(canvas, y, start_col)?;
            } else {
                self.dest
                    .queue(terminal::Clear(terminal::ClearType::CurrentLine))?;
            }
        }

        // Reposition cursor to last row of new canvas.
        let target_y = new_height.saturating_sub(1);
        match target_y.cmp(&current_y) {
            std::cmp::Ordering::Greater => {
                self.move_inline_to_next_line((target_y - current_y) as u16)?;
            }
            std::cmp::Ordering::Less => {
                self.move_inline_to_previous_line((current_y - target_y) as u16)?;
            }
            std::cmp::Ordering::Equal => {}
        }

        self.prev_canvas_height = new_height as _;
        Ok(())
    }

    fn event_stream(&mut self) -> io::Result<BoxStream<'static, TerminalEvent>> {
        if !self.input_is_terminal {
            return Ok(stream::pending().boxed());
        }

        self.apply_raw_mode_enabled(true)?;

        Ok(EventStream::new()
            .filter_map(|event| async move {
                match event {
                    Ok(Event::Key(event)) => Some(TerminalEvent::Key(KeyEvent {
                        code: event.code,
                        modifiers: event.modifiers,
                        kind: event.kind,
                    })),
                    Ok(Event::Mouse(event)) => {
                        Some(TerminalEvent::FullscreenMouse(FullscreenMouseEvent {
                            modifiers: event.modifiers,
                            column: event.column,
                            row: event.row,
                            cell_is_blank: false,
                            kind: event.kind,
                        }))
                    }
                    Ok(Event::Resize(width, height)) => Some(TerminalEvent::Resize(width, height)),
                    Ok(Event::FocusGained) => Some(TerminalEvent::FocusGained),
                    Ok(Event::FocusLost) => Some(TerminalEvent::FocusLost),
                    Ok(Event::Paste(text)) => Some(TerminalEvent::Paste(text)),
                    _ => None,
                }
            })
            .boxed())
    }

    fn dest(&mut self) -> &mut dyn Write {
        &mut *self.dest
    }

    fn alt(&mut self) -> &mut dyn Write {
        &mut *self.alt
    }
}

impl<'a> StdTerminal<'a> {
    fn new(
        dest: Box<dyn Write + Send + 'a>,
        alt: Box<dyn Write + Send + 'a>,
        fullscreen: bool,
        mouse_capture: bool,
    ) -> io::Result<Self> {
        let mut term = Self {
            dest,
            alt,
            input_is_terminal: stdin().is_terminal(),
            fullscreen,
            mouse_capture,
            dynamic_alternate_saved_mouse_capture: None,
            raw_mode_enabled: false,
            enabled_keyboard_enhancement: false,
            keyboard_enhancement_flags: event::KeyboardEnhancementFlags::REPORT_EVENT_TYPES,
            prev_canvas_top_row: 0,
            prev_canvas_height: 0,
            size: None,
            prev_size_on_write: None,
            cursor_visible: false,
            cursor_displacement_rows: 0,
            inline_pending_wrap: false,
            decstbm_safe: false,
            inline_force_full_rewrite_next_diff: false,
            // Best-effort: if signal registration fails, the terminal still works —
            // it just won't self-heal after suspend/resume.
            #[cfg(unix)]
            resume_signal: ResumeSignalListener::new().ok(),
        };
        term.dest.queue(cursor::Hide)?;
        if fullscreen {
            // Match Ink's <AlternateScreen> mount behavior: enter the
            // alternate buffer, erase any stale cells that a terminal may have
            // preserved, and anchor the first frame at the origin before any
            // canvas diff can run.
            term.dest
                .queue(terminal::EnterAlternateScreen)?
                .queue(terminal::Clear(terminal::ClearType::All))?
                .queue(cursor::MoveTo(0, 0))?;
        }
        register_terminal_for_panic_restore(fullscreen);
        Ok(term)
    }

    fn clear_fullscreen_screen(&mut self) -> io::Result<()> {
        self.dest
            .queue(terminal::Clear(terminal::ClearType::All))?
            .queue(cursor::MoveTo(0, 0))?;
        Ok(())
    }

    fn write_fullscreen_canvas_absolute(&mut self, canvas: &Canvas) -> io::Result<()> {
        if canvas.height() == 0 {
            return Ok(());
        }

        // CC Ink seeds alt-screen with a full-size blank previous frame so the
        // first paint uses cursor addressing instead of LF-based full-frame
        // rendering. Do the same at row granularity: absolute CUP per row never
        // scrolls the alt buffer, even when the canvas fills the viewport.
        self.dest.write_all(b"\x1b[0m")?;
        for y in 0..canvas.height() {
            self.dest
                .queue(cursor::MoveTo(0, self.prev_canvas_top_row + y as u16))?;
            canvas.write_ansi_row_without_newline(y, &mut *self.dest)?;
        }
        Ok(())
    }

    fn reset_retained_output_state(&mut self) {
        self.prev_canvas_height = 0;
        self.prev_canvas_top_row = 0;
        self.prev_size_on_write = None;
        self.cursor_displacement_rows = 0;
        self.inline_pending_wrap = false;
        self.inline_force_full_rewrite_next_diff = false;
    }

    fn park_fullscreen_cursor_after_write(&mut self, canvas_height: usize) -> io::Result<()> {
        if !self.fullscreen {
            return Ok(());
        }
        let target_row = self
            .size
            .map(|(_cols, rows)| rows.saturating_sub(1))
            .unwrap_or_else(|| {
                self.prev_canvas_top_row
                    .saturating_add(canvas_height.saturating_sub(1) as u16)
            });
        self.dest.queue(cursor::MoveTo(0, target_row))?;
        Ok(())
    }

    fn fullscreen_scroll_hint(
        &self,
        prev: &Canvas,
        canvas: &Canvas,
    ) -> Option<crate::canvas::ScrollHint> {
        if !self.decstbm_safe {
            return None;
        }
        self.fullscreen_scroll_hint_candidate(prev, canvas)
    }

    fn fullscreen_scroll_hint_candidate(
        &self,
        prev: &Canvas,
        canvas: &Canvas,
    ) -> Option<crate::canvas::ScrollHint> {
        if !self.fullscreen || self.prev_canvas_top_row != 0 {
            return None;
        }
        let hint = canvas.scroll_hint()?;
        if hint.delta == 0 || hint.top > hint.bottom {
            return None;
        }
        if hint.bottom >= prev.height() || hint.bottom >= canvas.height() {
            return None;
        }
        let region_height = hint.bottom - hint.top + 1;
        let abs_delta = hint.delta.unsigned_abs() as usize;
        if abs_delta == 0 || abs_delta >= region_height {
            return None;
        }
        Some(hint)
    }

    fn write_fullscreen_scroll_patch(&mut self, hint: crate::canvas::ScrollHint) -> io::Result<()> {
        // DECSTBM uses 1-indexed inclusive margins. SU (S) scrolls the region
        // up; SD (T) scrolls it down. Resetting the scroll region and homing the
        // cursor mirrors CC Ink's defensive sequence after the hardware scroll.
        write!(self.dest, "\x1b[{};{}r", hint.top + 1, hint.bottom + 1)?;
        let abs_delta = hint.delta.unsigned_abs();
        if hint.delta > 0 {
            write!(self.dest, "\x1b[{abs_delta}S")?;
        } else {
            write!(self.dest, "\x1b[{abs_delta}T")?;
        }
        self.dest.write_all(b"\x1b[r\x1b[H")?;
        Ok(())
    }

    fn inline_resize_requires_full_rewrite(&self) -> bool {
        if self.fullscreen {
            return false;
        }
        let (Some(prev), Some(next)) = (self.prev_size_on_write, self.size) else {
            return false;
        };

        // Mirrors Ink/log-update's resize guard: changing width invalidates
        // wrapping assumptions, and a shorter viewport can make previously
        // reachable rows disappear into scrollback. Reset the retained inline
        // canvas before diffing against the new terminal geometry.
        next.1 < prev.1 || (prev.0 != 0 && next.0 != prev.0)
    }

    fn inline_shrink_requires_full_rewrite(&self, prev_height: usize, new_height: usize) -> bool {
        if self.fullscreen || new_height >= prev_height {
            return false;
        }
        let Some((_cols, rows)) = self.size else {
            return false;
        };
        let rows = rows as usize;
        if rows == 0 {
            return false;
        }

        // Mirrors the main-screen Ink/log-update guard for shrinking from a
        // scrollback-producing frame to a frame that fits the viewport: rows
        // that should become visible again are in terminal scrollback and
        // cannot be pulled down with ordinary erase/cursor movement. Clear and
        // repaint instead of trying to patch the currently visible suffix.
        prev_height >= rows && new_height <= rows
    }

    fn inline_unreachable_rows_for_diff(&self, prev_height: usize) -> usize {
        if self.fullscreen {
            return 0;
        }
        let Some((_cols, rows)) = self.size else {
            return 0;
        };
        let rows = rows as usize;
        if rows == 0 || prev_height == 0 {
            return 0;
        }

        let viewport_y = prev_height.saturating_sub(rows);
        let cursor_restore_scroll = usize::from(prev_height >= rows);
        viewport_y + cursor_restore_scroll
    }

    fn inline_write_can_enter_pending_wrap(&self, rendered_width: usize) -> bool {
        !self.fullscreen
            && rendered_width > 0
            && self
                .size
                .is_some_and(|(cols, _)| cols > 0 && rendered_width >= cols as usize)
    }

    fn mark_inline_pending_wrap_after_full_write(&mut self, canvas: &Canvas) {
        let rendered_width = canvas
            .height()
            .checked_sub(1)
            .map(|last_row| canvas.ansi_row_rendered_width(last_row))
            .unwrap_or(0);
        self.inline_pending_wrap = self.inline_write_can_enter_pending_wrap(rendered_width);
    }

    fn mark_inline_pending_wrap_after_row_write(&mut self, canvas: &Canvas, y: usize) {
        let rendered_width = canvas.ansi_row_rendered_width(y);
        self.inline_pending_wrap = self.inline_write_can_enter_pending_wrap(rendered_width);
    }

    fn resolve_inline_pending_wrap(&mut self) -> io::Result<()> {
        if self.inline_pending_wrap {
            // Match Ink/log-update's handling of VT auto-wrap pending state: CR
            // returns to column 0 on the current row without advancing to the next
            // line, so subsequent relative movement starts from a known position.
            self.dest.write_all(b"\r")?;
            self.inline_pending_wrap = false;
        }
        Ok(())
    }

    fn move_inline_to_previous_line(&mut self, rows: u16) -> io::Result<()> {
        self.resolve_inline_pending_wrap()?;
        if rows > 0 {
            self.dest.queue(cursor::MoveToPreviousLine(rows))?;
        }
        Ok(())
    }

    fn move_inline_to_next_line(&mut self, rows: u16) -> io::Result<()> {
        self.resolve_inline_pending_wrap()?;
        if rows > 0 {
            self.dest.queue(cursor::MoveToNextLine(rows))?;
        }
        Ok(())
    }

    fn move_inline_to_column(&mut self, column: u16) -> io::Result<()> {
        self.resolve_inline_pending_wrap()?;
        self.dest.queue(cursor::MoveToColumn(column))?;
        Ok(())
    }

    fn write_inline_newline(&mut self) -> io::Result<()> {
        // CR is part of CRLF, so it resolves pending wrap and LF creates exactly
        // one new terminal row.
        self.dest.write_all(b"\r\n")?;
        self.inline_pending_wrap = false;
        Ok(())
    }

    fn write_inline_canvas_without_final_newline(&mut self, canvas: &Canvas) -> io::Result<()> {
        canvas.write_ansi_without_final_newline(&mut *self.dest)?;
        self.mark_inline_pending_wrap_after_full_write(canvas);
        Ok(())
    }

    fn write_inline_canvas_row_without_newline(
        &mut self,
        canvas: &Canvas,
        y: usize,
    ) -> io::Result<()> {
        canvas.write_ansi_row_without_newline(y, &mut *self.dest)?;
        self.mark_inline_pending_wrap_after_row_write(canvas, y);
        Ok(())
    }

    fn write_inline_canvas_row_from_col_without_newline(
        &mut self,
        canvas: &Canvas,
        y: usize,
        start_col: usize,
    ) -> io::Result<()> {
        canvas.write_ansi_row_from_col_without_newline(y, start_col, &mut *self.dest)?;
        self.mark_inline_pending_wrap_after_row_write(canvas, y);
        Ok(())
    }

    fn write_inline_full_repaint(&mut self, prev: &Canvas, canvas: &Canvas) -> io::Result<bool> {
        if self.fullscreen || prev.height() == 0 || canvas.height() == 0 {
            return Ok(false);
        }

        let prev_height = prev.height();
        let new_height = canvas.height();
        let max_height = prev_height.max(new_height);
        let unreachable_rows = self.inline_unreachable_rows_for_diff(prev_height);
        let mut current_y = prev_height.saturating_sub(1);

        for y in 0..max_height {
            if y < unreachable_rows {
                // Rows above this boundary are in scrollback or are protected by
                // Ink/log-update's cursor-restore-scroll guard. If their cells
                // changed, only a clear+rewrite can produce correct output. If
                // they are equal, skip them and still full-repaint every
                // reachable row below; this avoids duplicating tall transcripts
                // into native scrollback on ordinary layout-shift backstops.
                if !prev.row_eq(canvas, y) {
                    return Ok(false);
                }
                continue;
            }

            match y.cmp(&current_y) {
                std::cmp::Ordering::Less => {
                    self.move_inline_to_previous_line((current_y - y) as u16)?;
                }
                std::cmp::Ordering::Greater => {
                    let last_existing_line = prev_height.saturating_sub(1).max(current_y);
                    if y <= last_existing_line {
                        self.move_inline_to_next_line((y - current_y) as u16)?;
                    } else {
                        let move_to_last = last_existing_line.saturating_sub(current_y);
                        if move_to_last > 0 {
                            self.move_inline_to_next_line(move_to_last as u16)?;
                        }
                        let new_lines = y - last_existing_line;
                        for _ in 0..new_lines {
                            self.write_inline_newline()?;
                        }
                    }
                }
                std::cmp::Ordering::Equal => {
                    self.move_inline_to_column(0)?;
                }
            }
            current_y = y;

            if y < new_height {
                self.write_inline_canvas_row_without_newline(canvas, y)?;
            } else {
                self.dest
                    .queue(terminal::Clear(terminal::ClearType::CurrentLine))?;
                self.inline_pending_wrap = false;
            }
        }

        let target_y = new_height.saturating_sub(1);
        match target_y.cmp(&current_y) {
            std::cmp::Ordering::Greater => {
                self.move_inline_to_next_line((target_y - current_y) as u16)?;
            }
            std::cmp::Ordering::Less => {
                self.move_inline_to_previous_line((current_y - target_y) as u16)?;
            }
            std::cmp::Ordering::Equal => {}
        }

        Ok(true)
    }

    /// Undoes any cursor displacement left behind by a cursor declaration, returning
    /// the cursor to the canvas's last row. The inline row-diff logic relies on the
    /// cursor sitting there at the start of every write/clear.
    fn restore_cursor_baseline(&mut self) -> io::Result<()> {
        self.resolve_inline_pending_wrap()?;
        if self.cursor_displacement_rows > 0 {
            self.dest
                .queue(cursor::MoveToNextLine(self.cursor_displacement_rows))?;
            self.cursor_displacement_rows = 0;
        }
        Ok(())
    }

    fn reassert_keyboard_enhancement(&mut self) -> io::Result<()> {
        if self.enabled_keyboard_enhancement {
            // Match CC Ink's pop-before-push reassertion model for Kitty: if the
            // terminal preserved the stack, pop the old entry before pushing the
            // requested flags to avoid accumulating depth on every SIGCONT /
            // sleep-wake recovery. If the terminal reset the stack,
            // pop-on-empty is a no-op and the push restores depth 1.
            self.dest.execute(event::PopKeyboardEnhancementFlags)?;
            self.dest.execute(event::PushKeyboardEnhancementFlags(
                self.keyboard_enhancement_flags,
            ))?;
        }
        Ok(())
    }

    fn apply_raw_mode_enabled(&mut self, raw_mode_enabled: bool) -> io::Result<()> {
        if raw_mode_enabled != self.raw_mode_enabled {
            if raw_mode_enabled {
                if supports_extended_keys()
                    && terminal::supports_keyboard_enhancement().unwrap_or(false)
                {
                    self.dest.execute(event::PushKeyboardEnhancementFlags(
                        self.keyboard_enhancement_flags,
                    ))?;
                    self.enabled_keyboard_enhancement = true;
                }
                if self.mouse_capture {
                    self.dest.execute(event::EnableMouseCapture)?;
                }
                // Focus reporting lets components pause expensive work when the
                // terminal is blurred, mirroring CC Ink's TerminalFocusContext.
                self.dest.execute(event::EnableFocusChange)?;
                // Bracketed paste makes terminals deliver pasted text as a single
                // Event::Paste instead of a burst of key events. Terminals that don't
                // support it simply ignore the sequence.
                self.dest.execute(event::EnableBracketedPaste)?;
                terminal::enable_raw_mode()?;
            } else {
                terminal::disable_raw_mode()?;
                self.dest.execute(event::DisableBracketedPaste)?;
                self.dest.execute(event::DisableFocusChange)?;
                if self.mouse_capture {
                    self.dest.execute(event::DisableMouseCapture)?;
                }
                if self.enabled_keyboard_enhancement {
                    self.dest.execute(event::PopKeyboardEnhancementFlags)?;
                }
            }
            self.raw_mode_enabled = raw_mode_enabled;
        }
        Ok(())
    }
}

impl Drop for StdTerminal<'_> {
    fn drop(&mut self) {
        let _ = self.restore_cursor_baseline();
        let _ = self.apply_raw_mode_enabled(false);
        if self.fullscreen {
            let _ = self.dest.queue(terminal::LeaveAlternateScreen);
        } else if self.prev_canvas_height > 0 {
            let _ = self.dest.write_all(b"\r\n");
        }
        let _ = self.dest.queue(cursor::SetCursorStyle::DefaultUserShape);
        let _ = self.dest.execute(cursor::Show);
        unregister_terminal_for_panic_restore(self.fullscreen);
    }
}

pub(crate) struct MockTerminalOutputStream {
    inner: mpsc::UnboundedReceiver<Canvas>,
}

impl Stream for MockTerminalOutputStream {
    type Item = Canvas;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Option<Self::Item>> {
        self.inner.poll_next_unpin(cx)
    }
}

/// Used to provide the configuration for a mock terminal which can be used for testing.
///
/// This can be passed to [`ElementExt::mock_terminal_render_loop`](crate::ElementExt::mock_terminal_render_loop) for testing your dynamic components.
#[non_exhaustive]
pub struct MockTerminalConfig {
    /// The events to be emitted by the mock terminal.
    pub events: BoxStream<'static, TerminalEvent>,
    /// Whether the mock terminal should behave like a fullscreen/alternate-screen terminal.
    pub fullscreen: bool,
    /// The initial terminal size reported by the mock terminal.
    pub size: Option<(u16, u16)>,
    /// Whether the mock terminal should ignore the framework-level Ctrl+C exit.
    pub ignore_ctrl_c: bool,
    /// Whether Ctrl+Z should suspend instead of being delivered as ordinary input.
    pub suspend_on_ctrl_z: bool,
}

impl MockTerminalConfig {
    /// Creates a new `MockTerminalConfig` with the given event stream.
    pub fn with_events<T: Stream<Item = TerminalEvent> + Send + 'static>(events: T) -> Self {
        Self {
            events: events.boxed(),
            fullscreen: false,
            size: None,
            ignore_ctrl_c: false,
            suspend_on_ctrl_z: false,
        }
    }

    /// Sets whether this mock terminal behaves like a fullscreen/alternate-screen terminal.
    pub fn with_fullscreen(mut self, fullscreen: bool) -> Self {
        self.fullscreen = fullscreen;
        self
    }

    /// Sets the initial terminal size reported by this mock terminal.
    pub fn with_size(mut self, width: u16, height: u16) -> Self {
        self.size = Some((width, height));
        self
    }

    /// Sets whether the mock terminal should disable the default Ctrl+C exit.
    ///
    /// This mirrors [`RenderLoopFuture::ignore_ctrl_c`](crate::RenderLoopFuture::ignore_ctrl_c)
    /// for deterministic tests that exercise CC Ink-style `exitOnCtrlC` behavior.
    pub fn with_ignore_ctrl_c(mut self, ignore_ctrl_c: bool) -> Self {
        self.ignore_ctrl_c = ignore_ctrl_c;
        self
    }

    /// Sets whether Ctrl+Z should suspend the render loop instead of being
    /// delivered as ordinary input. This mirrors
    /// [`RenderLoopFuture::suspend_on_ctrl_z`](crate::RenderLoopFuture::suspend_on_ctrl_z)
    /// for deterministic tests.
    pub fn with_suspend_on_ctrl_z(mut self, suspend_on_ctrl_z: bool) -> Self {
        self.suspend_on_ctrl_z = suspend_on_ctrl_z;
        self
    }
}

impl Default for MockTerminalConfig {
    fn default() -> Self {
        Self {
            events: stream::pending().boxed(),
            fullscreen: false,
            size: None,
            ignore_ctrl_c: false,
            suspend_on_ctrl_z: false,
        }
    }
}

struct MockTerminal {
    config: MockTerminalConfig,
    output: mpsc::UnboundedSender<Canvas>,
    dummy_dest: io::Sink,
    dummy_alt: io::Sink,
    size: Option<(u16, u16)>,
    fullscreen: bool,
    raw_mode_enabled: bool,
    resumed: bool,
}

impl MockTerminal {
    fn new(config: MockTerminalConfig) -> (Self, MockTerminalOutputStream) {
        let (output_tx, output_rx) = mpsc::unbounded();
        let output = MockTerminalOutputStream { inner: output_rx };
        let size = config.size;
        let fullscreen = config.fullscreen;
        (
            Self {
                config,
                output: output_tx,
                dummy_dest: io::sink(),
                dummy_alt: io::sink(),
                size,
                fullscreen,
                raw_mode_enabled: false,
                resumed: false,
            },
            output,
        )
    }
}

impl Write for MockTerminal {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl TerminalImpl for MockTerminal {
    fn size(&self) -> Option<(u16, u16)> {
        self.size
    }

    fn set_size_from_resize_event(&mut self, width: u16, height: u16) {
        self.size = Some((width, height));
    }

    fn is_raw_mode_supported(&self) -> bool {
        true
    }

    fn take_resumed(&mut self) -> bool {
        std::mem::take(&mut self.resumed)
    }

    fn suspend(&mut self) -> io::Result<()> {
        self.resumed = true;
        Ok(())
    }

    fn is_raw_mode_enabled(&self) -> bool {
        self.raw_mode_enabled
    }

    fn set_raw_mode_enabled(&mut self, raw_mode_enabled: bool) -> io::Result<()> {
        self.raw_mode_enabled = raw_mode_enabled;
        Ok(())
    }

    fn is_fullscreen(&self) -> bool {
        self.fullscreen
    }

    fn set_dynamic_alternate_screen(
        &mut self,
        request: Option<crate::context::AlternateScreenRequest>,
    ) -> io::Result<bool> {
        let next = request.is_some();
        let changed = self.fullscreen != next;
        self.fullscreen = next;
        Ok(changed)
    }

    fn clear_canvas(&mut self) -> io::Result<()> {
        Ok(())
    }

    fn clear_screen(&mut self) -> io::Result<()> {
        Ok(())
    }

    fn clear_terminal(&mut self) -> io::Result<()> {
        Ok(())
    }

    fn write_canvas(&mut self, _prev: Option<&Canvas>, canvas: &Canvas) -> io::Result<()> {
        let _ = self.output.unbounded_send(canvas.clone());
        Ok(())
    }

    fn event_stream(&mut self) -> io::Result<BoxStream<'static, TerminalEvent>> {
        self.raw_mode_enabled = true;
        let mut events = stream::pending().boxed();
        mem::swap(&mut events, &mut self.config.events);
        Ok(events.chain(stream::pending()).boxed())
    }

    fn dest(&mut self) -> &mut dyn Write {
        &mut self.dummy_dest
    }

    fn alt(&mut self) -> &mut dyn Write {
        &mut self.dummy_alt
    }
}

pub(crate) struct Terminal<'a> {
    inner: Box<dyn TerminalImpl + 'a>,
    output: Output,
    base_fullscreen: bool,
    event_stream: Option<BoxStream<'static, TerminalEvent>>,
    subscribers: Vec<Weak<Mutex<TerminalEventsInner>>>,
    event_cell_snapshot: Option<EventCellSnapshot>,
    terminal_focus_state: Option<bool>,
    last_stdin_event_at: Option<Instant>,
    query_queue: VecDeque<PendingTerminalRequest>,
    pending_ctrl_c: Vec<Arc<SharedEventState>>,
    received_ctrl_c: bool,
    ignore_ctrl_c: bool,
    suspend_on_ctrl_z: bool,
    synchronized_update_depth: usize,
    synchronized_update_started: bool,
    synchronized_update_supported: bool,
}

impl<'a> Terminal<'a> {
    pub fn new(
        stdout: Box<dyn Write + Send + 'a>,
        stderr: Box<dyn Write + Send + 'a>,
        output: Output,
        fullscreen: bool,
        mouse_capture: bool,
    ) -> io::Result<Self> {
        // dest is the render destination, alt is the other stream
        let (dest, alt) = match output {
            Output::Stdout => (stdout, stderr),
            Output::Stderr => (stderr, stdout),
        };
        Ok(Self {
            inner: Box::new(StdTerminal::new(dest, alt, fullscreen, mouse_capture)?),
            output,
            base_fullscreen: fullscreen,
            event_stream: None,
            subscribers: Vec::new(),
            event_cell_snapshot: None,
            terminal_focus_state: None,
            last_stdin_event_at: Some(Instant::now()),
            query_queue: VecDeque::new(),
            pending_ctrl_c: Vec::new(),
            received_ctrl_c: false,
            ignore_ctrl_c: false,
            suspend_on_ctrl_z: false,
            synchronized_update_depth: 0,
            synchronized_update_started: false,
            synchronized_update_supported: is_synchronized_output_supported(),
        })
    }

    pub fn enable_mouse_capture(&mut self) -> io::Result<()> {
        self.inner.set_mouse_capture(true)
    }

    pub fn disable_mouse_capture(&mut self) -> io::Result<()> {
        self.inner.set_mouse_capture(false)
    }

    pub fn set_keyboard_enhancement_flags(
        &mut self,
        flags: event::KeyboardEnhancementFlags,
    ) -> io::Result<()> {
        self.inner.set_keyboard_enhancement_flags(flags)
    }

    pub fn ignore_ctrl_c(&mut self) {
        self.ignore_ctrl_c = true;
    }

    pub fn suspend_on_ctrl_z(&mut self) {
        self.suspend_on_ctrl_z = true;
    }

    pub fn exit_on_ctrl_c(&self) -> bool {
        !self.ignore_ctrl_c
    }

    pub fn is_raw_mode_supported(&self) -> bool {
        self.inner.is_raw_mode_supported()
    }

    pub fn is_raw_mode_enabled(&self) -> bool {
        self.inner.is_raw_mode_enabled()
    }

    pub fn set_raw_mode_enabled(&mut self, raw_mode_enabled: bool) -> io::Result<()> {
        self.inner.set_raw_mode_enabled(raw_mode_enabled)
    }

    pub fn is_fullscreen(&self) -> bool {
        self.inner.is_fullscreen()
    }

    pub fn set_dynamic_alternate_screen(
        &mut self,
        request: Option<crate::context::AlternateScreenRequest>,
    ) -> io::Result<bool> {
        if self.base_fullscreen {
            return Ok(false);
        }
        self.inner.set_dynamic_alternate_screen(request)
    }

    pub fn refresh_size(&mut self) {
        self.inner.refresh_size()
    }

    pub fn size(&self) -> Option<(u16, u16)> {
        self.inner.size()
    }

    pub(crate) fn terminal_focus_state(&self) -> Option<bool> {
        self.terminal_focus_state
    }

    pub fn clear_canvas(&mut self) -> io::Result<()> {
        if self.inner.clear_canvas_would_write() {
            self.ensure_synchronized_update_started()?;
        }
        self.inner.clear_canvas()
    }

    pub fn clear_screen(&mut self) -> io::Result<()> {
        self.ensure_synchronized_update_started()?;
        self.inner.clear_screen()
    }

    pub fn clear_terminal(&mut self) -> io::Result<()> {
        self.ensure_synchronized_update_started()?;
        self.inner.clear_terminal()
    }

    pub fn write_canvas(&mut self, prev: Option<&Canvas>, canvas: &Canvas) -> io::Result<()> {
        if self.inner.write_canvas_would_write(prev, canvas) {
            self.ensure_synchronized_update_started()?;
        }
        let decstbm_safe = self.synchronized_update_supported && self.synchronized_update_started;
        self.inner.set_decstbm_safe(decstbm_safe);
        self.inner.write_canvas(prev, canvas)
    }

    /// Updates the retained screen-buffer metadata used to annotate mouse events
    /// with CC Ink-style blank-cell information.
    pub fn set_event_cell_snapshot(&mut self, canvas: &Canvas) {
        self.event_cell_snapshot = Some(EventCellSnapshot::from_canvas(canvas));
    }

    fn annotate_terminal_event(
        mut event: TerminalEvent,
        snapshot: Option<&EventCellSnapshot>,
    ) -> TerminalEvent {
        if let TerminalEvent::FullscreenMouse(mouse) = &mut event {
            mouse.cell_is_blank = snapshot
                .map(|snapshot| snapshot.cell_is_blank(mouse.column, mouse.row))
                .unwrap_or(false);
        }
        event
    }

    /// Positions the physical cursor per the declaration's visibility flag.
    /// See [`Canvas::declare_cursor`].
    pub fn position_cursor(
        &mut self,
        declaration: Option<crate::canvas::CursorDeclaration>,
    ) -> io::Result<()> {
        if self.inner.position_cursor_would_write(declaration) {
            self.ensure_synchronized_update_started()?;
        }
        self.inner.position_cursor(declaration)
    }

    /// Writes a raw OSC 52 clipboard sequence to the terminal.
    ///
    /// This is intentionally outside synchronized-update framing: it is not a
    /// screen repaint and mirrors CC Ink's selection-copy path, which writes the
    /// clipboard escape sequence directly to stdout after extracting selected
    /// text. Clipboard transport fallbacks such as tmux buffers or native tools
    /// remain application-level policy.
    pub fn set_clipboard(&mut self, text: &str) -> io::Result<()> {
        crate::ansi::osc52_clipboard(self.inner.dest(), text)
    }

    /// Writes an OSC 52 clipboard sequence using an explicit multiplexer
    /// passthrough wrapper. This mirrors CC Ink's tmux/screen passthrough
    /// helpers; native clipboard and tmux-buffer loading remain app policy.
    pub fn set_clipboard_with_multiplexer(
        &mut self,
        text: &str,
        multiplexer: ClipboardMultiplexer,
    ) -> io::Result<()> {
        if multiplexer == ClipboardMultiplexer::None {
            self.set_clipboard(text)
        } else {
            crate::ansi::osc52_clipboard_for_multiplexer(
                self.inner.dest(),
                text,
                multiplexer.into(),
            )
        }
    }

    /// Writes a raw non-visual terminal control sequence directly to stdout.
    ///
    /// This intentionally bypasses synchronized-update framing and retained
    /// canvas clearing, matching CC Ink's TerminalWriteProvider side channel for
    /// OSC notifications, terminal progress, and BEL-like controls.
    pub fn write_control_sequence(&mut self, sequence: &str) -> io::Result<()> {
        self.inner.dest().write_all(sequence.as_bytes())
    }

    /// Sends a terminal query on the render output side band and returns a
    /// future for its response.
    ///
    /// Pair this with [`Self::flush_terminal_queries`] to bound unsupported
    /// queries without timeouts. Parsed [`TerminalEvent::Response`] events are
    /// routed automatically while the render loop waits, or can be delivered
    /// manually with [`Self::on_terminal_response`] in custom integrations.
    ///
    /// Sending a query starts the terminal event stream so backends that
    /// surface [`TerminalEvent::Response`] do not require a separate keyboard
    /// input subscriber. Custom raw-stdin frontends can also feed responses
    /// manually with [`Self::on_terminal_response`].
    pub fn send_terminal_query(
        &mut self,
        query: TerminalQuery,
    ) -> io::Result<PendingTerminalQuery> {
        self.start_event_stream()?;
        let (sender, receiver) = oneshot::channel();
        self.query_queue.push_back(PendingTerminalRequest::Query {
            matcher: query.matcher.clone(),
            sender,
        });
        self.write_control_sequence(query.request())?;
        Ok(PendingTerminalQuery { receiver })
    }

    /// Sends the DA1 flush sentinel for pending terminal queries.
    ///
    /// This also starts the terminal event stream so backends that surface
    /// [`TerminalEvent::Response`] can resolve the sentinel round trip without
    /// a separate input subscriber.
    pub fn flush_terminal_queries(&mut self) -> io::Result<PendingTerminalFlush> {
        self.start_event_stream()?;
        let (sender, receiver) = oneshot::channel();
        self.query_queue
            .push_back(PendingTerminalRequest::Sentinel { sender });
        self.write_control_sequence(da1_query_sequence())?;
        Ok(PendingTerminalFlush { receiver })
    }

    /// Dispatches a parsed terminal response to this terminal's query queue.
    #[allow(dead_code)]
    pub fn on_terminal_response(&mut self, response: TerminalResponse) {
        if let TerminalResponse::Xtversion { name } = &response {
            set_xtversion_name(name.clone());
        }
        dispatch_terminal_query_response(&mut self.query_queue, response);
    }

    pub fn received_ctrl_c(&self) -> bool {
        self.received_ctrl_c
    }

    /// Applies the default Ctrl+C behavior after component hooks have had a chance
    /// to consume the event via propagation. A pending Ctrl+C exits only if none of
    /// the component callbacks called `stop_propagation()`.
    pub fn resolve_pending_ctrl_c(&mut self) {
        if self.ignore_ctrl_c || self.received_ctrl_c {
            self.pending_ctrl_c.clear();
            return;
        }
        if self
            .pending_ctrl_c
            .drain(..)
            .any(|state| !state.is_propagation_stopped())
        {
            self.received_ctrl_c = true;
        }
    }

    /// Returns `true` (and clears the flag) if the process was resumed from suspension
    /// (SIGCONT) since the last call. The render loop uses this to trigger a full
    /// terminal reinitialization and redraw.
    pub fn take_resumed(&mut self) -> bool {
        self.inner.take_resumed()
    }

    /// Re-applies terminal modes and resets cached output state after a resume from
    /// suspension. See [`TerminalImpl::reinitialize_after_resume`].
    pub fn reinitialize_after_resume(&mut self) -> io::Result<()> {
        self.inner.reinitialize_after_resume()
    }

    /// Returns a mutable reference to the stdout handle.
    pub fn stdout(&mut self) -> &mut dyn Write {
        let _ = self.ensure_synchronized_update_started();
        match self.output {
            Output::Stdout => self.inner.dest(),
            Output::Stderr => self.inner.alt(),
        }
    }

    /// Returns a mutable reference to the stderr handle.
    pub fn stderr(&mut self) -> &mut dyn Write {
        let _ = self.ensure_synchronized_update_started();
        match self.output {
            Output::Stdout => self.inner.alt(),
            Output::Stderr => self.inner.dest(),
        }
    }

    /// Returns a mutable reference to the render output handle (stdout or stderr based on output setting).
    pub fn render_output(&mut self) -> &mut dyn Write {
        let _ = self.ensure_synchronized_update_started();
        self.inner.dest()
    }

    fn ensure_synchronized_update_started(&mut self) -> io::Result<()> {
        if self.synchronized_update_supported
            && self.synchronized_update_depth > 0
            && !self.synchronized_update_started
        {
            self.inner
                .dest()
                .execute(terminal::BeginSynchronizedUpdate)?;
            self.synchronized_update_started = true;
        }
        Ok(())
    }

    fn end_synchronized_update(&mut self) {
        self.synchronized_update_depth = self.synchronized_update_depth.saturating_sub(1);
        if self.synchronized_update_depth == 0 && self.synchronized_update_started {
            let _ = self.inner.dest().execute(terminal::EndSynchronizedUpdate);
            self.synchronized_update_started = false;
        }
    }

    /// Wraps a series of terminal updates in a synchronized update block, making sure to end the
    /// synchronized update even if there is an error or panic.
    pub fn synchronized_update<F>(&mut self, f: F) -> io::Result<()>
    where
        F: FnOnce(&mut Self) -> io::Result<()>,
    {
        let t = SynchronizedUpdate::begin(self);
        f(t.inner)
    }

    pub fn start_event_stream(&mut self) -> io::Result<()> {
        if self.event_stream.is_none() {
            self.event_stream = Some(self.inner.event_stream()?);
        }
        Ok(())
    }

    pub async fn wait(&mut self) {
        use futures::future::{poll_fn, select, Either};

        let event_cell_snapshot = self.event_cell_snapshot.clone();
        match &mut self.event_stream {
            Some(event_stream) => loop {
                // Race the next terminal event against the resume (SIGCONT) signal so
                // the render loop wakes immediately when the process is foregrounded,
                // even with no pending input.
                let inner = &mut self.inner;
                let next =
                    match select(event_stream.next(), poll_fn(|cx| inner.poll_resumed(cx))).await {
                        Either::Left((next, _)) => next,
                        Either::Right(((), _)) => return,
                    };
                let Some(event) = next else {
                    return;
                };
                let now = Instant::now();
                if self
                    .last_stdin_event_at
                    .is_some_and(|last| now.duration_since(last) > STDIN_RESUME_GAP)
                {
                    let _ = self.inner.reassert_after_stdin_resume();
                }
                self.last_stdin_event_at = Some(now);
                let is_ctrl_c = matches!(
                    event,
                    TerminalEvent::Key(KeyEvent {
                        code: KeyCode::Char('c'),
                        kind: KeyEventKind::Press,
                        modifiers,
                        ..
                    }) if modifiers.contains(KeyModifiers::CONTROL)
                        && !modifiers.intersects(KeyModifiers::ALT | KeyModifiers::SUPER)
                );
                let is_ctrl_z = matches!(
                    event,
                    TerminalEvent::Key(KeyEvent {
                        code: KeyCode::Char(ch),
                        kind: KeyEventKind::Press,
                        modifiers,
                        ..
                    }) if ch.eq_ignore_ascii_case(&'z')
                        && modifiers.contains(KeyModifiers::CONTROL)
                        && !modifiers.intersects(KeyModifiers::ALT | KeyModifiers::SUPER)
                );
                if self.suspend_on_ctrl_z && is_ctrl_z {
                    // Opt-in CC Ink/Claude Code Ctrl+Z behavior: hand the
                    // terminal back to the shell, stop the process on Unix, and
                    // let the SIGCONT repair path repaint from a clean slate
                    // after `fg`. Generic iocraft apps receive Ctrl+Z as normal
                    // input unless they explicitly enable this policy.
                    let _ = self.inner.suspend();
                    return;
                }
                match &event {
                    TerminalEvent::FocusGained => self.terminal_focus_state = Some(true),
                    TerminalEvent::FocusLost => self.terminal_focus_state = Some(false),
                    TerminalEvent::Response(response) => {
                        if let TerminalResponse::Xtversion { name } = response {
                            set_xtversion_name(name.clone());
                        }
                        dispatch_terminal_query_response(&mut self.query_queue, response.clone());
                    }
                    _ => {}
                }
                let is_resize = matches!(event, TerminalEvent::Resize(..));
                if let TerminalEvent::Resize(width, height) = &event {
                    let next_size = (*width, *height);
                    if self.inner.size() == Some(next_size) {
                        // Match CC Ink's resize handler: same-dimension resize
                        // events are terminal-settling noise. Do not wake the
                        // render loop or reset/reassert modes redundantly.
                        continue;
                    }
                    self.inner.set_size_from_resize_event(*width, *height);
                    let _ = self.inner.reassert_after_resize();
                }
                let event = Self::annotate_terminal_event(event, event_cell_snapshot.as_ref());

                // Dispatch to all subscribers first — Ctrl+C is a normal event.
                let shared_state = Arc::new(SharedEventState::default());
                let mut delivered = false;
                self.subscribers.retain(|subscriber| {
                    if let Some(subscriber) = subscriber.upgrade() {
                        delivered = true;
                        let mut subscriber = subscriber.lock().unwrap();
                        subscriber
                            .pending
                            .push_back((event.clone(), shared_state.clone()));
                        if let Some(waker) = subscriber.waker.take() {
                            waker.wake();
                        }
                        true
                    } else {
                        false
                    }
                });

                if is_ctrl_c && !self.ignore_ctrl_c {
                    if delivered {
                        // Defer default exit until the render loop has polled component
                        // hooks, giving propagation-aware listeners a chance to consume it.
                        self.pending_ctrl_c.push(shared_state);
                    } else {
                        // No component can consume it, so preserve the default behavior.
                        self.received_ctrl_c = true;
                    }
                }
                if self.received_ctrl_c || is_resize {
                    return;
                }
            },
            None => {
                let inner = &mut self.inner;
                poll_fn(|cx| inner.poll_resumed(cx)).await;
            }
        }
    }

    pub fn events(&mut self) -> io::Result<TerminalEvents> {
        self.start_event_stream()?;
        let inner = Arc::new(Mutex::new(TerminalEventsInner {
            pending: VecDeque::new(),
            waker: None,
        }));
        self.subscribers.push(Arc::downgrade(&inner));
        Ok(TerminalEvents { inner })
    }
}

impl Terminal<'static> {
    pub fn mock(config: MockTerminalConfig) -> (Self, MockTerminalOutputStream) {
        let ignore_ctrl_c = config.ignore_ctrl_c;
        let suspend_on_ctrl_z = config.suspend_on_ctrl_z;
        let (term, output_stream) = MockTerminal::new(config);
        let base_fullscreen = term.fullscreen;
        (
            Self {
                inner: Box::new(term),
                output: Output::Stdout,
                base_fullscreen,
                event_stream: None,
                subscribers: Vec::new(),
                event_cell_snapshot: None,
                terminal_focus_state: None,
                last_stdin_event_at: Some(Instant::now()),
                query_queue: VecDeque::new(),
                pending_ctrl_c: Vec::new(),
                received_ctrl_c: false,
                ignore_ctrl_c,
                suspend_on_ctrl_z,
                synchronized_update_depth: 0,
                synchronized_update_started: false,
                synchronized_update_supported: true,
            },
            output_stream,
        )
    }
}

impl Write for Terminal<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.ensure_synchronized_update_started()?;
        self.inner.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// Synchronized update terminal guard.
/// Enters synchronized update on creation, exits when dropped.
pub(crate) struct SynchronizedUpdate<'a, 'b> {
    inner: &'a mut Terminal<'b>,
}

impl<'a, 'b> SynchronizedUpdate<'a, 'b> {
    pub fn begin(terminal: &'a mut Terminal<'b>) -> Self {
        terminal.synchronized_update_depth += 1;
        Self { inner: terminal }
    }
}

impl Drop for SynchronizedUpdate<'_, '_> {
    fn drop(&mut self) {
        self.inner.end_synchronized_update();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prelude::*;
    use crossterm::QueueableCommand;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    };

    #[derive(Clone, Default)]
    struct TestWriter {
        buf: Arc<Mutex<Vec<u8>>>,
    }

    impl Write for TestWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.buf.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn test_terminal_set_clipboard_writes_raw_osc52_without_sync_update() {
        let stdout = TestWriter::default();
        let stderr = TestWriter::default();
        let mut terminal = Terminal::new(
            Box::new(stdout.clone()),
            Box::new(stderr),
            Output::Stdout,
            false,
            false,
        )
        .unwrap();
        stdout.buf.lock().unwrap().clear(); // discard setup cursor-hide bytes

        terminal.set_clipboard("copy").unwrap();

        let output = String::from_utf8(stdout.buf.lock().unwrap().clone()).unwrap();
        assert_eq!(output, "\x1b]52;c;Y29weQ==\x07");
        assert!(
            !output.contains("\x1b[?2026"),
            "clipboard writes are not screen diffs and must not start synchronized update"
        );
    }

    #[test]
    fn test_terminal_control_sequence_writes_raw_without_sync_update() {
        let stdout = TestWriter::default();
        let stderr = TestWriter::default();
        let mut terminal = Terminal::new(
            Box::new(stdout.clone()),
            Box::new(stderr),
            Output::Stdout,
            false,
            false,
        )
        .unwrap();
        stdout.buf.lock().unwrap().clear(); // discard setup cursor-hide bytes

        terminal.write_control_sequence("\x1b]9;4;3;\x07").unwrap();

        let output = String::from_utf8(stdout.buf.lock().unwrap().clone()).unwrap();
        assert_eq!(output, "\x1b]9;4;3;\x07");
        assert!(
            !output.contains("\x1b[?2026"),
            "terminal notifications/progress are side-band controls, not screen diffs"
        );
    }

    fn selection_key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
        }
    }

    #[test]
    fn test_selection_controller_routes_shift_nav_key_to_focus_move() {
        let mut controller = SelectionController::new();
        controller.selection_mut().start(1, 1);
        controller.selection_mut().update(0, 1);

        let outcome = controller.handle_fullscreen_key_event(
            &selection_key(KeyCode::Left, KeyModifiers::SHIFT),
            4,
            3,
        );

        assert_eq!(
            outcome,
            FullscreenSelectionKeyOutcome::FocusMoved {
                movement: SelectionFocusMove::Left,
                moved: true,
            }
        );
        assert_eq!(
            controller.selection().focus(),
            Some(crate::canvas::SelectionPoint { col: 3, row: 0 })
        );
    }

    #[test]
    fn test_selection_controller_key_event_copy_clear_and_preserve_paths() {
        let mut controller = SelectionController::new();
        controller.selection_mut().start(0, 0);
        controller.selection_mut().update(1, 0);

        assert_eq!(
            controller.handle_fullscreen_key_event(
                &selection_key(KeyCode::Char('c'), KeyModifiers::CONTROL),
                8,
                1,
            ),
            FullscreenSelectionKeyOutcome::CopyRequested
        );
        assert!(
            controller.has_selection(),
            "copy request should leave clear policy to caller"
        );

        assert_eq!(
            controller.handle_fullscreen_key_event(
                &selection_key(KeyCode::PageUp, KeyModifiers::SHIFT),
                8,
                1,
            ),
            FullscreenSelectionKeyOutcome::Preserved
        );
        assert!(controller.has_selection());

        assert_eq!(
            controller.handle_fullscreen_key_event(
                &selection_key(KeyCode::Right, KeyModifiers::empty()),
                8,
                1,
            ),
            FullscreenSelectionKeyOutcome::Cleared
        );
        assert!(!controller.has_selection());
    }

    #[test]
    fn test_selection_controller_key_event_escape_and_release_handling() {
        let mut controller = SelectionController::new();
        controller.selection_mut().start(0, 0);
        controller.selection_mut().update(1, 0);
        let release = KeyEvent {
            code: KeyCode::Esc,
            modifiers: KeyModifiers::empty(),
            kind: KeyEventKind::Release,
        };
        assert_eq!(
            controller.handle_fullscreen_key_event(&release, 8, 1),
            FullscreenSelectionKeyOutcome::Ignored
        );
        assert!(controller.has_selection());

        assert_eq!(
            controller.handle_fullscreen_key_event(
                &selection_key(KeyCode::Esc, KeyModifiers::empty()),
                8,
                1,
            ),
            FullscreenSelectionKeyOutcome::Cleared
        );
        assert!(!controller.has_selection());
    }

    #[test]
    fn test_selection_controller_routes_fullscreen_mouse_press_drag_release() {
        let mut canvas = Canvas::new(8, 1);
        canvas
            .subview_mut(0, 0, 0, 0, 8, 1)
            .set_text(0, 0, "abcdef", CanvasTextStyle::default());
        let mut controller = SelectionController::new();

        let press = FullscreenMouseEvent {
            modifiers: KeyModifiers::ALT,
            column: 1,
            row: 0,
            cell_is_blank: false,
            kind: MouseEventKind::Down(event::MouseButton::Left),
        };
        let outcome = controller.handle_fullscreen_mouse_event(&canvas, &press, 1_000, false);
        assert!(matches!(
            outcome,
            FullscreenSelectionEventOutcome::Press(SelectionPressOutcome {
                kind: crate::canvas::SelectionMousePressKind::Single,
                finished_previous_drag: false,
                cancel_pending_hyperlink: false,
            })
        ));
        assert!(controller.selection().last_press_had_alt());

        let drag = FullscreenMouseEvent::new(MouseEventKind::Drag(event::MouseButton::Left), 3, 0);
        assert_eq!(
            controller.handle_fullscreen_mouse_event(&canvas, &drag, 1_010, false),
            FullscreenSelectionEventOutcome::Drag
        );
        assert_eq!(controller.selected_text(&canvas), "bcd");

        let release = FullscreenMouseEvent::new(MouseEventKind::Up(event::MouseButton::Left), 3, 0);
        let outcome = controller.handle_fullscreen_mouse_event(&canvas, &release, 1_020, false);
        assert!(matches!(
            outcome,
            FullscreenSelectionEventOutcome::Release(SelectionReleaseOutcome {
                was_dragging: true,
                click: None,
                hyperlink: None,
            })
        ));
        assert!(controller.has_selection());
        assert!(!controller.selection().is_dragging());
    }

    #[test]
    fn test_selection_controller_routes_fullscreen_mouse_double_click_and_wheel() {
        let mut canvas = Canvas::new(16, 1);
        canvas.subview_mut(0, 0, 0, 0, 16, 1).set_text(
            0,
            0,
            "one two three",
            CanvasTextStyle::default(),
        );
        let mut controller = SelectionController::new();
        let down = FullscreenMouseEvent::new(MouseEventKind::Down(event::MouseButton::Left), 4, 0);

        controller.handle_fullscreen_mouse_event(&canvas, &down, 1_000, false);
        let outcome = controller.handle_fullscreen_mouse_event(&canvas, &down, 1_200, false);
        assert!(matches!(
            outcome,
            FullscreenSelectionEventOutcome::Press(SelectionPressOutcome {
                kind: crate::canvas::SelectionMousePressKind::Double,
                finished_previous_drag: true,
                cancel_pending_hyperlink: true,
            })
        ));
        assert_eq!(controller.selected_text(&canvas), "two");

        let wheel = FullscreenMouseEvent::new(MouseEventKind::ScrollDown, 4, 0);
        assert_eq!(
            controller.handle_fullscreen_mouse_event(&canvas, &wheel, 1_250, false),
            FullscreenSelectionEventOutcome::Wheel {
                cleared_selection: true,
            }
        );
        assert!(!controller.has_selection());
    }

    #[test]
    fn test_selection_controller_routes_no_button_motion_lost_release_and_hover_dedupe() {
        let mut canvas = Canvas::new(8, 1);
        canvas
            .subview_mut(0, 0, 0, 0, 8, 1)
            .set_text(0, 0, "abcdef", CanvasTextStyle::default());
        let mut controller = SelectionController::new();
        let down = FullscreenMouseEvent::new(MouseEventKind::Down(event::MouseButton::Left), 1, 0);
        let drag = FullscreenMouseEvent::new(MouseEventKind::Drag(event::MouseButton::Left), 3, 0);
        controller.handle_fullscreen_mouse_event(&canvas, &down, 1_000, false);
        controller.handle_fullscreen_mouse_event(&canvas, &drag, 1_010, false);

        let moved = FullscreenMouseEvent::new(MouseEventKind::Moved, 3, 0);
        let outcome = controller.handle_fullscreen_mouse_event(&canvas, &moved, 1_020, false);
        assert!(matches!(
            outcome,
            FullscreenSelectionEventOutcome::Hover(SelectionHoverOutcome {
                finished_drag: true,
                hover: Some(crate::canvas::SelectionPoint { col: 3, row: 0 }),
            })
        ));
        let repeat = controller.handle_fullscreen_mouse_event(&canvas, &moved, 1_030, false);
        assert!(matches!(
            repeat,
            FullscreenSelectionEventOutcome::Hover(SelectionHoverOutcome {
                finished_drag: false,
                hover: None,
            })
        ));
    }

    #[test]
    fn test_terminal_set_clipboard_with_multiplexer_wraps_osc52() {
        let stdout = TestWriter::default();
        let stderr = TestWriter::default();
        let mut terminal = Terminal::new(
            Box::new(stdout.clone()),
            Box::new(stderr),
            Output::Stdout,
            false,
            false,
        )
        .unwrap();
        stdout.buf.lock().unwrap().clear();

        terminal
            .set_clipboard_with_multiplexer("copy", ClipboardMultiplexer::Tmux)
            .unwrap();

        let output = String::from_utf8(stdout.buf.lock().unwrap().clone()).unwrap();
        assert_eq!(output, "\x1bPtmux;\x1b\x1b]52;c;Y29weQ==\x07\x1b\\");
    }

    struct ResizeReassertTerminal {
        events: Option<BoxStream<'static, TerminalEvent>>,
        reasserts: Arc<AtomicUsize>,
        size: Option<(u16, u16)>,
        dest: io::Sink,
    }

    impl Write for ResizeReassertTerminal {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl TerminalImpl for ResizeReassertTerminal {
        fn size(&self) -> Option<(u16, u16)> {
            self.size
        }

        fn set_size_from_resize_event(&mut self, width: u16, height: u16) {
            self.size = Some((width, height));
        }

        fn reassert_after_resize(&mut self) -> io::Result<()> {
            self.reasserts.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        fn reassert_after_stdin_resume(&mut self) -> io::Result<()> {
            self.reasserts.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        fn is_raw_mode_enabled(&self) -> bool {
            false
        }

        fn clear_canvas(&mut self) -> io::Result<()> {
            Ok(())
        }

        fn write_canvas(&mut self, _prev: Option<&Canvas>, _canvas: &Canvas) -> io::Result<()> {
            Ok(())
        }

        fn event_stream(&mut self) -> io::Result<BoxStream<'static, TerminalEvent>> {
            Ok(self
                .events
                .take()
                .unwrap_or_else(|| stream::pending().boxed()))
        }

        fn dest(&mut self) -> &mut dyn Write {
            &mut self.dest
        }

        fn alt(&mut self) -> &mut dyn Write {
            &mut self.dest
        }
    }

    fn new_test_writer() -> (TestWriter, Arc<Mutex<Vec<u8>>>) {
        let writer = TestWriter::default();
        let buf = writer.buf.clone();
        (writer, buf)
    }

    #[test]
    fn test_std_terminal() {
        // There's unfortunately not much here we can really test, but we'll do our best.
        // TODO: Is there a library we can use to emulate terminal input/output?
        let mut terminal = Terminal::new(
            Box::new(std::io::stdout()),
            Box::new(std::io::stderr()),
            Output::Stdout,
            false,
            true,
        )
        .unwrap();
        assert!(!terminal.is_raw_mode_enabled());
        assert!(!terminal.received_ctrl_c());
        assert!(!terminal.is_raw_mode_enabled());
        let canvas = Canvas::new(10, 1);
        terminal.write_canvas(None, &canvas).unwrap();
    }

    fn render_canvas_to_vt(canvas: &Canvas, cols: usize, rows: usize) -> avt::Vt {
        render_canvases_to_vt(&[canvas], cols, rows)
    }

    fn render_canvases_to_vt(canvases: &[&Canvas], cols: usize, rows: usize) -> avt::Vt {
        let mut buf = Vec::new();
        for (i, canvas) in canvases.iter().enumerate() {
            if i > 0 {
                super::clear_canvas_inline(&mut buf, canvases[i - 1].height() as _).unwrap();
            }
            canvas.write_ansi_without_final_newline(&mut buf).unwrap();
        }
        let mut vt = avt::Vt::new(cols, rows);
        vt.feed_str(&String::from_utf8(buf).unwrap());
        vt
    }

    #[test]
    fn test_inline_rewrite_single_line_cursor() {
        let mut canvas = Canvas::new(10, 1);
        canvas
            .subview_mut(0, 0, 0, 0, 10, 1)
            .set_text(0, 0, "hello", CanvasTextStyle::default());

        let vt = render_canvas_to_vt(&canvas, 10, 5);

        assert_eq!(vt.line(0).text(), "hello     ");
        assert_eq!(vt.cursor().row, 0, "cursor should stay on the first row");

        // clear and rerender with new content
        let mut canvas2 = Canvas::new(10, 1);
        canvas2
            .subview_mut(0, 0, 0, 0, 10, 1)
            .set_text(0, 0, "world", CanvasTextStyle::default());

        let vt = render_canvases_to_vt(&[&canvas, &canvas2], 10, 5);

        assert_eq!(vt.line(0).text(), "world     ");
        assert_eq!(vt.cursor().row, 0);
    }

    #[test]
    fn test_inline_rewrite_multi_line_cursor() {
        let mut canvas = Canvas::new(10, 3);
        canvas
            .subview_mut(0, 0, 0, 0, 10, 3)
            .set_text(0, 0, "line1", CanvasTextStyle::default());
        canvas
            .subview_mut(0, 0, 0, 0, 10, 3)
            .set_text(0, 2, "line3", CanvasTextStyle::default());

        let vt = render_canvas_to_vt(&canvas, 10, 5);

        assert_eq!(vt.line(0).text(), "line1     ");
        assert_eq!(vt.line(1).text(), "          ");
        assert_eq!(vt.line(2).text(), "line3     ");
        assert_eq!(
            vt.cursor().row,
            2,
            "cursor should be on the last content row"
        );

        // clear and rerender with fewer lines
        let mut canvas2 = Canvas::new(10, 2);
        canvas2
            .subview_mut(0, 0, 0, 0, 10, 2)
            .set_text(0, 0, "new1", CanvasTextStyle::default());
        canvas2
            .subview_mut(0, 0, 0, 0, 10, 2)
            .set_text(0, 1, "new2", CanvasTextStyle::default());

        let vt = render_canvases_to_vt(&[&canvas, &canvas2], 10, 5);

        assert_eq!(vt.line(0).text(), "new1      ");
        assert_eq!(vt.line(1).text(), "new2      ");
        assert_eq!(
            vt.line(2).text(),
            "          ",
            "old line 3 should be cleared"
        );
        assert_eq!(vt.cursor().row, 1);
    }

    #[test]
    fn test_inline_rewrite_no_extra_blank_line() {
        let mut canvas = Canvas::new(10, 2);
        canvas
            .subview_mut(0, 0, 0, 0, 10, 2)
            .set_text(0, 0, "first", CanvasTextStyle::default());
        canvas
            .subview_mut(0, 0, 0, 0, 10, 2)
            .set_text(0, 1, "second", CanvasTextStyle::default());

        let vt = render_canvas_to_vt(&canvas, 10, 5);

        assert_eq!(vt.line(0).text(), "first     ");
        assert_eq!(vt.line(1).text(), "second    ");
        assert_eq!(vt.cursor().row, 1, "cursor stays on last content row");

        // clear and rerender
        let mut canvas2 = Canvas::new(10, 2);
        canvas2
            .subview_mut(0, 0, 0, 0, 10, 2)
            .set_text(0, 0, "third", CanvasTextStyle::default());
        canvas2
            .subview_mut(0, 0, 0, 0, 10, 2)
            .set_text(0, 1, "fourth", CanvasTextStyle::default());

        let vt = render_canvases_to_vt(&[&canvas, &canvas2], 10, 5);

        assert_eq!(vt.line(0).text(), "third     ");
        assert_eq!(vt.line(1).text(), "fourth    ");
        assert_eq!(vt.cursor().row, 1);
    }

    #[test]
    fn test_fullscreen_diff_preserves_origin() {
        let mut prev = Canvas::new(10, 2);
        prev.subview_mut(0, 0, 0, 0, 10, 2)
            .set_text(0, 0, "first", CanvasTextStyle::default());
        prev.subview_mut(0, 0, 0, 0, 10, 2)
            .set_text(0, 1, "second", CanvasTextStyle::default());

        let mut next = Canvas::new(10, 2);
        next.subview_mut(0, 0, 0, 0, 10, 2)
            .set_text(0, 0, "first", CanvasTextStyle::default());
        next.subview_mut(0, 0, 0, 0, 10, 2)
            .set_text(0, 1, "changed", CanvasTextStyle::default());

        let (dest, diff_buf) = new_test_writer();
        let mut term = new_fullscreen_term(dest, 1, prev.height() as _);
        term.write_canvas(Some(&prev), &next).unwrap();

        let mut setup = Vec::new();
        write!(setup, "log\r\n").unwrap();
        prev.write_ansi_without_final_newline(&mut setup).unwrap();
        setup.extend_from_slice(&diff_buf.lock().unwrap());

        let mut vt = avt::Vt::new(10, 5);
        vt.feed_str(&String::from_utf8(setup).unwrap());

        assert_eq!(vt.line(0).text(), "log       ");
        assert_eq!(vt.line(1).text(), "first     ");
        assert_eq!(vt.line(2).text(), "changed   ");
        assert_eq!(
            vt.cursor().row,
            2,
            "cursor should stay on the canvas bottom when terminal size is unknown"
        );
    }

    #[test]
    fn test_fullscreen_decstbm_scroll_hint_shifts_prev_before_diff() {
        let mut prev = Canvas::new(10, 5);
        for (y, label) in ["top", "one", "two", "three", "bottom"].iter().enumerate() {
            prev.subview_mut(0, 0, 0, 0, 10, 5).set_text(
                0,
                y as isize,
                label,
                CanvasTextStyle::default(),
            );
        }

        let mut next = Canvas::new(10, 5);
        for (y, label) in ["top", "two", "three", "new", "bottom"].iter().enumerate() {
            next.subview_mut(0, 0, 0, 0, 10, 5).set_text(
                0,
                y as isize,
                label,
                CanvasTextStyle::default(),
            );
        }
        next.set_scroll_hint(crate::canvas::ScrollHint {
            top: 1,
            bottom: 3,
            delta: 1,
        });

        let (dest, diff_buf) = new_test_writer();
        let mut term = new_fullscreen_term(dest, 0, prev.height() as _);
        term.size = Some((10, 5));
        term.prev_size_on_write = Some((10, 5));
        term.decstbm_safe = true;
        term.write_canvas(Some(&prev), &next).unwrap();

        let diff = diff_buf.lock().unwrap().clone();
        let diff_str = String::from_utf8_lossy(&diff);
        assert!(
            diff_str.contains("\x1b[2;4r\x1b[1S\x1b[r\x1b[H"),
            "expected DECSTBM + SU scroll patch; got {diff_str:?}"
        );
        assert!(diff_str.contains("new"), "edge row should be repainted");
        assert!(
            !diff_str.contains("two") && !diff_str.contains("three"),
            "stable shifted rows should not be rewritten after virtual prev shift: {diff_str:?}"
        );

        let mut setup = Vec::new();
        prev.write_ansi_without_final_newline(&mut setup).unwrap();
        setup.extend_from_slice(&diff);
        let mut vt = avt::Vt::new(10, 5);
        vt.feed_str(&String::from_utf8(setup).unwrap());
        assert_eq!(vt.line(0).text(), "top       ");
        assert_eq!(vt.line(1).text(), "two       ");
        assert_eq!(vt.line(2).text(), "three     ");
        assert_eq!(vt.line(3).text(), "new       ");
        assert_eq!(vt.line(4).text(), "bottom    ");
    }

    #[test]
    fn test_fullscreen_damage_preserves_decstbm_scroll_hint() {
        let mut prev = Canvas::new(10, 5);
        for (y, label) in ["top", "one", "two", "three", "bottom"].iter().enumerate() {
            prev.subview_mut(0, 0, 0, 0, 10, 5).set_text(
                0,
                y as isize,
                label,
                CanvasTextStyle::default(),
            );
        }

        let mut next = Canvas::new(10, 5);
        for (y, label) in ["top", "two", "three", "new", "bottom"].iter().enumerate() {
            next.subview_mut(0, 0, 0, 0, 10, 5).set_text(
                0,
                y as isize,
                label,
                CanvasTextStyle::default(),
            );
        }
        next.set_scroll_hint(crate::canvas::ScrollHint {
            top: 1,
            bottom: 3,
            delta: 1,
        });
        next.mark_damage(crate::canvas::DamageRegion {
            x: 0,
            y: 0,
            width: 10,
            height: 5,
        });

        let (dest, diff_buf) = new_test_writer();
        let mut term = new_fullscreen_term(dest, 0, prev.height() as _);
        term.size = Some((10, 5));
        term.prev_size_on_write = Some((10, 5));
        term.decstbm_safe = true;
        term.write_canvas(Some(&prev), &next).unwrap();

        let diff = String::from_utf8_lossy(&diff_buf.lock().unwrap().clone()).to_string();
        assert!(
            diff.contains("\x1b[2;4r\x1b[1S\x1b[r\x1b[H"),
            "damage should not disable atomic DECSTBM scroll optimization: {diff:?}"
        );
        assert!(
            diff.contains("two") && diff.contains("three") && diff.contains("new"),
            "full damage should repaint the scrolled region after the hardware scroll: {diff:?}"
        );
    }

    #[test]
    fn test_fullscreen_decstbm_scroll_hint_requires_atomic_update() {
        let mut prev = Canvas::new(10, 3);
        prev.subview_mut(0, 0, 0, 0, 10, 3)
            .set_text(0, 0, "one", CanvasTextStyle::default());
        prev.subview_mut(0, 0, 0, 0, 10, 3)
            .set_text(0, 1, "two", CanvasTextStyle::default());
        prev.subview_mut(0, 0, 0, 0, 10, 3)
            .set_text(0, 2, "three", CanvasTextStyle::default());

        let mut next = Canvas::new(10, 3);
        next.subview_mut(0, 0, 0, 0, 10, 3)
            .set_text(0, 0, "two", CanvasTextStyle::default());
        next.subview_mut(0, 0, 0, 0, 10, 3)
            .set_text(0, 1, "three", CanvasTextStyle::default());
        next.subview_mut(0, 0, 0, 0, 10, 3)
            .set_text(0, 2, "four", CanvasTextStyle::default());
        next.set_scroll_hint(crate::canvas::ScrollHint {
            top: 0,
            bottom: 2,
            delta: 1,
        });

        let (dest, diff_buf) = new_test_writer();
        let mut term = new_fullscreen_term(dest, 0, prev.height() as _);
        term.size = Some((10, 3));
        term.prev_size_on_write = Some((10, 3));
        term.decstbm_safe = false;
        term.write_canvas(Some(&prev), &next).unwrap();

        let diff_str = String::from_utf8_lossy(&diff_buf.lock().unwrap().clone()).to_string();
        assert!(
            !diff_str.contains("\x1b[1;3r") && !diff_str.contains("\x1b[1S"),
            "DECSTBM must be skipped without synchronized-output atomicity: {diff_str:?}"
        );
        assert!(
            diff_str.contains("two") && diff_str.contains("hree") && diff_str.contains("four"),
            "without hardware scroll, changed rows should be patched with sparse row diffs: {diff_str:?}"
        );

        let mut setup = Vec::new();
        prev.write_ansi_without_final_newline(&mut setup).unwrap();
        setup.extend_from_slice(diff_str.as_bytes());
        let mut vt = avt::Vt::new(10, 3);
        vt.feed_str(&String::from_utf8(setup).unwrap());
        assert_eq!(vt.line(0).text(), "two       ");
        assert_eq!(vt.line(1).text(), "three     ");
        assert_eq!(vt.line(2).text(), "four      ");
    }

    #[test]
    fn test_fullscreen_initial_write_parks_cursor_at_terminal_bottom_when_size_known() {
        let mut canvas = Canvas::new(10, 2);
        canvas
            .subview_mut(0, 0, 0, 0, 10, 2)
            .set_text(0, 0, "first", CanvasTextStyle::default());
        canvas
            .subview_mut(0, 0, 0, 0, 10, 2)
            .set_text(0, 1, "second", CanvasTextStyle::default());

        let (dest, buf) = new_test_writer();
        let mut term = new_fullscreen_term(dest, 0, 0);
        term.size = Some((10, 5));
        term.write_canvas(None, &canvas).unwrap();

        let output = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
        assert!(
            !output.contains("\r\n"),
            "fullscreen initial paint should use absolute row writes, not LF-based rendering that can scroll alt-screen: {output:?}"
        );
        let mut vt = avt::Vt::new(10, 5);
        vt.feed_str(&output);

        assert_eq!(vt.line(0).text(), "first     ");
        assert_eq!(vt.line(1).text(), "second    ");
        assert_eq!(
            vt.cursor().row,
            4,
            "fullscreen cursor should be parked at terminal bottom"
        );
    }

    #[test]
    fn test_fullscreen_resize_full_clears_and_reanchors() {
        let style = CanvasTextStyle::default();
        let mut prev = Canvas::new(10, 2);
        prev.subview_mut(0, 0, 0, 0, 10, 2)
            .set_text(0, 0, "old0", style);
        prev.subview_mut(0, 0, 0, 0, 10, 2)
            .set_text(0, 1, "old1", style);

        let mut next = Canvas::new(10, 2);
        next.subview_mut(0, 0, 0, 0, 10, 2)
            .set_text(0, 0, "new0", style);
        next.subview_mut(0, 0, 0, 0, 10, 2)
            .set_text(0, 1, "new1", style);

        let (dest, buf) = new_test_writer();
        let mut term = new_fullscreen_term(dest, 1, prev.height() as _);
        term.prev_size_on_write = Some((12, 4));
        term.size = Some((10, 4));
        term.write_canvas(Some(&prev), &next).unwrap();
        assert_eq!(term.prev_canvas_top_row, 0);

        let diff = buf.lock().unwrap().clone();
        let diff_str = String::from_utf8_lossy(&diff);
        assert!(
            diff_str.contains("\x1b[2J"),
            "fullscreen resize should erase the whole screen before repaint: {diff_str:?}"
        );
        assert!(
            !diff_str.contains("\r\n"),
            "fullscreen resize repaint should use absolute row writes, not LF-based rendering that can scroll alt-screen: {diff_str:?}"
        );

        let mut setup = Vec::new();
        write!(setup, "LOG\r\n").unwrap();
        prev.write_ansi_without_final_newline(&mut setup).unwrap();
        setup.extend_from_slice(&diff);

        let mut vt = avt::Vt::new(10, 4);
        vt.feed_str(&String::from_utf8(setup).unwrap());
        assert_eq!(vt.line(0).text(), "new0      ");
        assert_eq!(vt.line(1).text(), "new1      ");
        assert_eq!(vt.cursor().row, 3, "cursor should park at terminal bottom");
    }

    #[test]
    fn test_fullscreen_clear_preserves_output_above() {
        let mut canvas = Canvas::new(10, 2);
        canvas
            .subview_mut(0, 0, 0, 0, 10, 2)
            .set_text(0, 0, "first", CanvasTextStyle::default());
        canvas
            .subview_mut(0, 0, 0, 0, 10, 2)
            .set_text(0, 1, "second", CanvasTextStyle::default());

        let (dest, clear_buf) = new_test_writer();
        let mut term = new_fullscreen_term(dest, 1, canvas.height() as _);
        term.clear_canvas().unwrap();

        let mut setup = Vec::new();
        write!(setup, "log\r\n").unwrap();
        canvas.write_ansi_without_final_newline(&mut setup).unwrap();
        write!(setup, "\r\ntail").unwrap();
        setup.queue(cursor::MoveTo(0, 0)).unwrap();
        setup.extend_from_slice(&clear_buf.lock().unwrap());

        let mut vt = avt::Vt::new(10, 5);
        vt.feed_str(&String::from_utf8(setup).unwrap());

        assert_eq!(vt.line(0).text(), "log       ");
        assert_eq!(vt.line(1).text(), "          ");
        assert_eq!(vt.line(2).text(), "          ");
        assert_eq!(vt.line(3).text(), "          ");
    }

    fn new_fullscreen_term(
        dest: TestWriter,
        prev_canvas_top_row: u16,
        prev_canvas_height: u16,
    ) -> StdTerminal<'static> {
        StdTerminal {
            input_is_terminal: false,
            dest: Box::new(dest),
            alt: Box::new(io::sink()),
            fullscreen: true,
            mouse_capture: false,
            dynamic_alternate_saved_mouse_capture: None,
            raw_mode_enabled: false,
            enabled_keyboard_enhancement: false,
            keyboard_enhancement_flags: event::KeyboardEnhancementFlags::REPORT_EVENT_TYPES,
            prev_canvas_top_row,
            prev_canvas_height,
            size: None,
            prev_size_on_write: None,
            cursor_visible: false,
            cursor_displacement_rows: 0,
            inline_pending_wrap: false,
            decstbm_safe: false,
            inline_force_full_rewrite_next_diff: false,
            #[cfg(unix)]
            resume_signal: None,
        }
    }

    fn new_inline_term(dest: TestWriter, prev_canvas_height: u16) -> StdTerminal<'static> {
        new_inline_term_with_size(dest, prev_canvas_height, (10, 10))
    }

    fn new_inline_term_with_size(
        dest: TestWriter,
        prev_canvas_height: u16,
        term_size: (u16, u16),
    ) -> StdTerminal<'static> {
        StdTerminal {
            input_is_terminal: false,
            dest: Box::new(dest),
            alt: Box::new(io::sink()),
            fullscreen: false,
            mouse_capture: false,
            dynamic_alternate_saved_mouse_capture: None,
            raw_mode_enabled: false,
            enabled_keyboard_enhancement: false,
            keyboard_enhancement_flags: event::KeyboardEnhancementFlags::REPORT_EVENT_TYPES,
            prev_canvas_top_row: 0,
            prev_canvas_height,
            size: Some(term_size),
            prev_size_on_write: None,
            cursor_visible: false,
            cursor_displacement_rows: 0,
            inline_pending_wrap: false,
            decstbm_safe: false,
            inline_force_full_rewrite_next_diff: false,
            #[cfg(unix)]
            resume_signal: None,
        }
    }

    /// Run an inline diff (prev → next) and return the raw diff bytes plus
    /// an `avt::Vt` showing the final visible state.
    fn inline_diff_vt(prev: &Canvas, next: &Canvas, term_size: (u16, u16)) -> (Vec<u8>, avt::Vt) {
        let (dest, diff_buf) = new_test_writer();
        let mut term = new_inline_term_with_size(dest, prev.height() as _, term_size);
        term.write_canvas(Some(prev), next).unwrap();

        let diff = diff_buf.lock().unwrap().clone();
        let mut setup = Vec::new();
        prev.write_ansi_without_final_newline(&mut setup).unwrap();
        setup.extend_from_slice(&diff);

        let mut vt = avt::Vt::new(term_size.0 as _, term_size.1 as _);
        vt.feed_str(&String::from_utf8(setup).unwrap());
        (diff, vt)
    }

    #[test]
    fn test_inline_pending_wrap_resolved_before_next_frame_relative_move() {
        let style = CanvasTextStyle::default();
        let mut prev = Canvas::new(10, 2);
        prev.subview_mut(0, 0, 0, 0, 10, 2)
            .set_text(0, 0, "aaaaaaaaaa", style);
        prev.subview_mut(0, 0, 0, 0, 10, 2)
            .set_text(0, 1, "bbbbbbbbbb", style);

        let mut next = prev.clone();
        next.subview_mut(0, 0, 0, 0, 10, 2)
            .set_text(0, 0, "cccccccccc", style);

        let (dest, buf) = new_test_writer();
        let mut term = new_inline_term_with_size(dest, 0, (10, 5));
        term.write_canvas(None, &prev).unwrap();
        assert!(term.inline_pending_wrap);

        let first_frame = buf.lock().unwrap().clone();
        buf.lock().unwrap().clear();
        term.write_canvas(Some(&prev), &next).unwrap();

        let diff = buf.lock().unwrap().clone();
        let diff_str = String::from_utf8_lossy(&diff);
        assert!(
            diff_str.starts_with("\r\x1b[1F"),
            "pending wrap should be resolved with CR before moving up: {diff_str:?}"
        );

        let mut vt = avt::Vt::new(10, 5);
        let mut all = first_frame;
        all.extend_from_slice(&diff);
        vt.feed_str(&String::from_utf8(all).unwrap());
        assert_eq!(vt.line(0).text(), "cccccccccc");
        assert_eq!(vt.line(1).text(), "bbbbbbbbbb");
        assert_eq!(vt.cursor().row, 1);
    }

    #[test]
    fn test_inline_pending_wrap_resolved_between_changed_rows() {
        let style = CanvasTextStyle::default();
        let mut prev = Canvas::new(10, 2);
        prev.subview_mut(0, 0, 0, 0, 10, 2)
            .set_text(0, 0, "aaaaaaaaaa", style);
        prev.subview_mut(0, 0, 0, 0, 10, 2)
            .set_text(0, 1, "bbb", style);

        let mut next = Canvas::new(10, 2);
        next.subview_mut(0, 0, 0, 0, 10, 2)
            .set_text(0, 0, "cccccccccc", style);
        next.subview_mut(0, 0, 0, 0, 10, 2)
            .set_text(0, 1, "ddd", style);

        let (diff, vt) = inline_diff_vt(&prev, &next, (10, 5));
        let diff_str = String::from_utf8_lossy(&diff);
        assert!(
            diff_str.contains("\r\x1b[1E"),
            "pending wrap should be resolved with CR before moving down: {diff_str:?}"
        );
        assert_eq!(vt.line(0).text(), "cccccccccc");
        assert_eq!(vt.line(1).text(), "ddd       ");
        assert_eq!(vt.cursor().row, 1);
    }

    #[test]
    fn test_inline_pending_wrap_tracks_actual_rendered_width() {
        let style = CanvasTextStyle::default();
        let (dest, buf) = new_test_writer();
        let mut term = new_inline_term_with_size(dest, 0, (10, 5));

        let mut short = Canvas::new(10, 2);
        short
            .subview_mut(0, 0, 0, 0, 10, 2)
            .set_text(0, 0, "aaaaaaaaaa", style);
        short
            .subview_mut(0, 0, 0, 0, 10, 2)
            .set_text(0, 1, "bbb", style);
        term.write_canvas(None, &short).unwrap();
        assert!(
            !term.inline_pending_wrap,
            "full-width canvas should not imply pending-wrap when the actual last row is short"
        );

        let mut full = short.clone();
        full.subview_mut(0, 0, 0, 0, 10, 2)
            .set_text(0, 1, "cccccccccc", style);
        buf.lock().unwrap().clear();
        term.write_canvas(Some(&short), &full).unwrap();
        assert!(
            term.inline_pending_wrap,
            "a row write that reaches the right margin should enter pending-wrap"
        );
    }

    /// Inline-mode cursor positioning: moving the cursor up to the declared row must be
    /// undone (baseline restore) before the next canvas write, so the row-diff logic's
    /// "cursor sits on the last row" assumption holds.
    #[test]
    fn test_clear_canvas_viewport_tall_inline_preserves_scrollback() {
        let (dest, buf) = new_test_writer();
        let mut term = new_inline_term_with_size(dest, 5, (10, 5));

        term.clear_canvas().unwrap();

        let output = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
        assert!(
            output.contains("\x1b[2J"),
            "viewport-tall inline clear should erase visible screen: {output:?}"
        );
        assert!(
            !output.contains("\x1b[3J"),
            "clearing a main-screen live canvas must preserve native scrollback: {output:?}"
        );
        assert!(
            output.contains("\x1b[1;1H"),
            "viewport-tall inline clear should return cursor home: {output:?}"
        );
    }

    #[test]
    fn test_clear_screen_resets_inline_state_without_purging_scrollback() {
        let (dest, buf) = new_test_writer();
        let mut term = new_inline_term(dest, 3);
        term.cursor_displacement_rows = 1;
        term.inline_pending_wrap = true;
        term.inline_force_full_rewrite_next_diff = true;

        term.clear_screen().unwrap();

        let output = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
        assert!(
            output.contains("\x1b[2J"),
            "visible-screen clear should erase the screen: {output:?}"
        );
        assert!(
            !output.contains("\x1b[3J"),
            "visible-screen clear must preserve native terminal scrollback: {output:?}"
        );
        assert!(
            output.contains("\x1b[1;1H"),
            "visible-screen clear should return cursor home: {output:?}"
        );
        assert_eq!(term.prev_canvas_height, 0);
        assert_eq!(term.prev_canvas_top_row, 0);
        assert_eq!(term.prev_size_on_write, None);
        assert_eq!(term.cursor_displacement_rows, 0);
        assert!(!term.inline_pending_wrap);
        assert!(!term.inline_force_full_rewrite_next_diff);
    }

    #[test]
    fn test_clear_terminal_full_resets_inline_state() {
        let (dest, buf) = new_test_writer();
        let mut term = new_inline_term(dest, 3);
        term.cursor_displacement_rows = 1;
        term.inline_pending_wrap = true;
        term.inline_force_full_rewrite_next_diff = true;

        term.clear_terminal().unwrap();

        let output = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
        assert!(
            output.contains("\x1b[2J"),
            "full terminal clear should erase visible screen: {output:?}"
        );
        assert!(
            output.contains("\x1b[3J"),
            "full terminal clear should purge scrollback: {output:?}"
        );
        assert!(
            output.ends_with("\x1b[2J\x1b[3J\x1b[H"),
            "full terminal clear should use the CC Ink modern clear sequence: {output:?}"
        );
        assert!(
            !output.contains("\x1b[?1049h") && !output.contains("\x1b[?1049l"),
            "main-screen clear must not enter/leave alternate-screen fullscreen mode: {output:?}"
        );
        assert_eq!(term.prev_canvas_height, 0);
        assert_eq!(term.prev_canvas_top_row, 0);
        assert_eq!(term.prev_size_on_write, None);
        assert_eq!(term.cursor_displacement_rows, 0);
        assert!(!term.inline_pending_wrap);
        assert!(!term.inline_force_full_rewrite_next_diff);
    }

    #[test]
    fn test_position_cursor_inline_displacement_and_baseline_restore() {
        let (dest, buf) = new_test_writer();
        let mut term = new_inline_term(dest, 3); // canvas height 3, last row = 2

        use crate::canvas::CursorDeclaration;

        // ink model: visible=false, physical cursor stays hidden — only positioned for IME.
        term.position_cursor(Some(CursorDeclaration {
            x: 4,
            y: 0,
            visible: false,
        }))
        .unwrap();
        assert_eq!(term.cursor_displacement_rows, 2);
        assert!(!term.cursor_visible);
        let output = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
        assert!(
            output.contains("\x1b[2F"),
            "expected MoveToPreviousLine(2): {output:?}"
        );
        assert!(
            output.contains("\x1b[5G"),
            "expected MoveToColumn(4) (1-based CSI G): {output:?}"
        );
        assert!(
            !output.contains("\x1b[?25h"),
            "physical cursor should NOT be shown: {output:?}"
        );

        // position_cursor(None) is a no-op when cursor was never shown.
        buf.lock().unwrap().clear();
        term.position_cursor(None).unwrap();
        assert!(!term.cursor_visible);

        // The next baseline restore moves back down by the displacement.
        buf.lock().unwrap().clear();
        term.restore_cursor_baseline().unwrap();
        assert_eq!(term.cursor_displacement_rows, 0);
        term.dest.flush().unwrap();
        let output = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
        assert!(
            output.contains("\x1b[2E"),
            "expected MoveToNextLine(2): {output:?}"
        );
    }

    /// Fullscreen-mode cursor positioning uses absolute coordinates and leaves no
    /// displacement to restore.
    #[test]
    fn test_position_cursor_fullscreen_absolute() {
        use crate::canvas::CursorDeclaration;
        let (dest, buf) = new_test_writer();
        let mut term = new_fullscreen_term(dest, 5, 3); // top row 5
        term.position_cursor(Some(CursorDeclaration {
            x: 2,
            y: 1,
            visible: false,
        }))
        .unwrap();
        assert_eq!(term.cursor_displacement_rows, 0);
        let output = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
        // MoveTo is 1-based in CSI: row 5+1+1=7, col 2+1=3.
        assert!(
            output.contains("\x1b[7;3H"),
            "expected absolute MoveTo: {output:?}"
        );
    }

    #[test]
    fn test_position_cursor_fullscreen_clamps_to_terminal_size() {
        use crate::canvas::CursorDeclaration;
        let (dest, buf) = new_test_writer();
        let mut term = new_fullscreen_term(dest, 0, 3);
        term.size = Some((5, 4));

        term.position_cursor(Some(CursorDeclaration {
            x: 99,
            y: 99,
            visible: false,
        }))
        .unwrap();

        let output = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
        assert!(
            output.contains("\x1b[4;5H"),
            "fullscreen cursor declarations should be clamped to terminal bounds: {output:?}"
        );
    }

    fn synchronized_output_env(pairs: &[(&str, &str)]) -> bool {
        is_synchronized_output_supported_with_env(|key| {
            pairs
                .iter()
                .find_map(|(k, v)| (*k == key).then(|| (*v).to_string()))
        })
    }

    #[test]
    fn test_synchronized_output_supported_matches_cc_gate() {
        assert!(synchronized_output_env(&[("TERM_PROGRAM", "iTerm.app")]));
        assert!(synchronized_output_env(&[("TERM_PROGRAM", "ghostty")]));
        assert!(synchronized_output_env(&[("TERM", "xterm-kitty")]));
        assert!(synchronized_output_env(&[("TERM", "foot-extra")]));
        assert!(synchronized_output_env(&[("WT_SESSION", "abc")]));
        assert!(synchronized_output_env(&[("VTE_VERSION", "6800")]));
        assert!(!synchronized_output_env(&[("VTE_VERSION", "6700")]));
        assert!(!synchronized_output_env(&[(
            "TMUX",
            "/tmp/tmux-1/default,1,0"
        )]));
    }

    fn extended_keys_env(pairs: &[(&str, &str)]) -> bool {
        supports_extended_keys_with_env(|key| {
            pairs
                .iter()
                .find_map(|(k, v)| (*k == key).then(|| (*v).to_string()))
        })
    }

    #[test]
    fn test_supports_extended_keys_matches_cc_allowlist() {
        assert!(extended_keys_env(&[("TERM_PROGRAM", "iTerm.app")]));
        assert!(extended_keys_env(&[("TERM", "xterm-kitty")]));
        assert!(extended_keys_env(&[("TERM", "xterm-ghostty")]));
        assert!(extended_keys_env(&[("TMUX", "/tmp/tmux-1/default,1,0")]));
        assert!(extended_keys_env(&[("WT_SESSION", "1")]));
        assert!(!extended_keys_env(&[("TERM_PROGRAM", "vscode")]));
        assert!(!extended_keys_env(&[("TERM_PROGRAM", "alacritty")]));
    }

    fn cursor_yank_env(is_windows: bool, pairs: &[(&str, &str)]) -> bool {
        has_cursor_up_viewport_yank_bug_with_env(is_windows, |key| {
            pairs
                .iter()
                .find_map(|(k, v)| (*k == key).then(|| (*v).to_string()))
        })
    }

    #[test]
    fn test_has_cursor_up_viewport_yank_bug_matches_cc_gate() {
        assert!(cursor_yank_env(true, &[]));
        assert!(cursor_yank_env(false, &[("WT_SESSION", "abc")]));
        assert!(!cursor_yank_env(false, &[]));
    }

    fn clear_sequence_env(is_windows: bool, pairs: &[(&str, &str)]) -> &'static str {
        clear_terminal_sequence_with_env(is_windows, |key| {
            pairs
                .iter()
                .find_map(|(k, v)| (*k == key).then(|| (*v).to_string()))
        })
    }

    #[test]
    fn test_clear_terminal_sequence_matches_cc_windows_gate() {
        let modern = "\x1b[2J\x1b[3J\x1b[H";
        let legacy_windows = "\x1b[2J\x1b[0f";

        assert_eq!(clear_sequence_env(false, &[]), modern);
        assert_eq!(clear_sequence_env(true, &[("WT_SESSION", "abc")]), modern);
        assert_eq!(
            clear_sequence_env(
                true,
                &[
                    ("TERM_PROGRAM", "vscode"),
                    ("TERM_PROGRAM_VERSION", "1.0.0")
                ]
            ),
            modern
        );
        assert_eq!(
            clear_sequence_env(true, &[("TERM_PROGRAM", "mintty")]),
            modern
        );
        assert_eq!(clear_sequence_env(true, &[("MSYSTEM", "MINGW64")]), modern);
        assert_eq!(clear_sequence_env(true, &[]), legacy_windows);
        assert_eq!(
            clear_sequence_env(true, &[("TERM_PROGRAM", "vscode")]),
            legacy_windows
        );
    }

    fn is_xterm_js_env(pairs: &[(&str, &str)], xtversion: Option<&str>) -> bool {
        is_xterm_js_with_env_and_xtversion(
            |key| {
                pairs
                    .iter()
                    .find_map(|(k, v)| (*k == key).then(|| (*v).to_string()))
            },
            xtversion,
        )
    }

    #[test]
    fn test_xtversion_detection_matches_cc_xterm_js_fallback() {
        assert_eq!(xtversion_query_sequence(), "\x1b[>0q");
        assert_eq!(
            parse_xtversion_response("\x1bP>|xterm.js(5.5.0)\x1b\\"),
            Some("xterm.js(5.5.0)")
        );
        assert_eq!(
            parse_xtversion_response("\x1bP>|ghostty 1.2.0\x07"),
            Some("ghostty 1.2.0")
        );
        assert_eq!(parse_xtversion_response("\x1b[?1;2c"), None);

        assert!(is_xterm_js_env(&[("TERM_PROGRAM", "vscode")], None));
        assert!(is_xterm_js_env(&[], Some("xterm.js(5.5.0)")));
        assert!(!is_xterm_js_env(&[], Some("ghostty 1.2.0")));
        assert!(!is_xterm_js_env(&[], None));
    }

    #[test]
    fn test_terminal_input_tokenizer_matches_cc_ink_boundaries() {
        use TerminalInputToken::{Sequence, Text};

        let mut tokenizer = TerminalInputTokenizer::new();
        assert_eq!(tokenizer.feed("hi中\x1b["), vec![Text("hi中".to_string())]);
        assert_eq!(tokenizer.buffered(), "\x1b[");
        assert_eq!(
            tokenizer.feed("A!"),
            vec![Sequence("\x1b[A".to_string()), Text("!".to_string())]
        );
        assert_eq!(tokenizer.buffered(), "");

        assert!(tokenizer.feed("\x1b]11;rgb:0000").is_empty());
        assert_eq!(tokenizer.buffered(), "\x1b]11;rgb:0000");
        assert_eq!(
            tokenizer.feed("/0000/0000\x1b\\x"),
            vec![
                Sequence("\x1b]11;rgb:0000/0000/0000\x1b\\".to_string()),
                Text("x".to_string()),
            ]
        );

        assert!(tokenizer.feed("\x1b[?").is_empty());
        assert_eq!(tokenizer.flush(), vec![Sequence("\x1b[?".to_string())]);
        assert_eq!(tokenizer.buffered(), "");

        let mut legacy_mouse = TerminalInputTokenizer::with_x10_mouse(true);
        assert_eq!(
            legacy_mouse.feed("\x1b[Mabc"),
            vec![Sequence("\x1b[Mabc".to_string())]
        );

        let mut csi_delete_lines = TerminalInputTokenizer::new();
        assert_eq!(
            csi_delete_lines.feed("\x1b[Mabc"),
            vec![Sequence("\x1b[M".to_string()), Text("abc".to_string())]
        );
    }

    #[test]
    fn test_terminal_input_tokenizer_feeds_response_parser_like_cc_ink() {
        let mut tokenizer = TerminalInputTokenizer::new();
        assert_eq!(
            tokenizer.feed_parsed("typed\x1b[?2026;1$y\x1bP>|xterm.js(5.5.0)\x1b\\"),
            vec![
                TerminalParsedInput::Text("typed".to_string()),
                TerminalParsedInput::Response(TerminalResponse::Decrpm {
                    mode: 2026,
                    status: DecrpmStatus::Set.code(),
                }),
                TerminalParsedInput::Response(TerminalResponse::Xtversion {
                    name: "xterm.js(5.5.0)".to_string(),
                }),
            ]
        );

        let mut tokenizer = TerminalInputTokenizer::new();
        assert!(tokenizer.feed_parsed("\x1b[?").is_empty());
        assert_eq!(
            tokenizer.flush_parsed(),
            vec![TerminalParsedInput::Sequence("\x1b[?".to_string())]
        );
    }

    #[test]
    fn test_terminal_input_tokenizer_groups_bracketed_paste_like_cc_ink() {
        let mut tokenizer = TerminalInputTokenizer::new();
        assert_eq!(
            tokenizer.feed_parsed("before\x1b[200~hello\x1b[31m\x1b[201~after"),
            vec![
                TerminalParsedInput::Text("before".to_string()),
                TerminalParsedInput::Paste("hello\x1b[31m".to_string()),
                TerminalParsedInput::Text("after".to_string()),
            ]
        );

        let mut chunked = TerminalInputTokenizer::new();
        assert!(chunked.feed_parsed("\x1b[200~hel").is_empty());
        assert_eq!(
            chunked.feed_parsed("lo\x1b[201~"),
            vec![TerminalParsedInput::Paste("hello".to_string())]
        );

        let mut empty = TerminalInputTokenizer::new();
        assert_eq!(
            empty.feed_parsed("\x1b[200~\x1b[201~"),
            vec![TerminalParsedInput::Paste(String::new())]
        );

        let mut flushed = TerminalInputTokenizer::new();
        assert!(flushed.feed_parsed("\x1b[200~partial").is_empty());
        assert_eq!(
            flushed.flush_parsed(),
            vec![TerminalParsedInput::Paste("partial".to_string())]
        );
    }

    #[test]
    fn test_terminal_input_tokenizer_parses_sgr_mouse_like_cc_ink() {
        let mut tokenizer = TerminalInputTokenizer::new();
        assert_eq!(
            tokenizer.feed_parsed("\x1b[<0;12;3M\x1b[<32;12;3m"),
            vec![
                TerminalParsedInput::Mouse(TerminalParsedMouse {
                    button: 0,
                    action: TerminalParsedMouseAction::Press,
                    column: 12,
                    row: 3,
                    sequence: "\x1b[<0;12;3M".to_string(),
                }),
                TerminalParsedInput::Mouse(TerminalParsedMouse {
                    button: 32,
                    action: TerminalParsedMouseAction::Release,
                    column: 12,
                    row: 3,
                    sequence: "\x1b[<32;12;3m".to_string(),
                }),
            ]
        );

        let mut wheel = TerminalInputTokenizer::new();
        assert_eq!(
            wheel.feed_parsed("\x1b[<64;12;3M"),
            vec![TerminalParsedInput::Sequence("\x1b[<64;12;3M".to_string())],
            "wheel events stay as key-parser input like CC Ink"
        );

        let mut orphaned = TerminalInputTokenizer::new();
        assert_eq!(
            orphaned.feed_parsed("[<0;12;3M"),
            vec![TerminalParsedInput::Mouse(TerminalParsedMouse {
                button: 0,
                action: TerminalParsedMouseAction::Press,
                column: 12,
                row: 3,
                sequence: "\x1b[<0;12;3M".to_string(),
            })],
            "orphaned mouse tails are re-synthesized instead of leaking into text"
        );
    }

    #[test]
    fn test_parse_terminal_key_sequence_matches_cc_ink_parse_keypress() {
        let shift_enter = parse_terminal_key_sequence("\x1b[13;2u");
        assert_eq!(shift_enter.name.as_deref(), Some("return"));
        assert!(shift_enter.shift);
        assert!(!shift_enter.ctrl);

        let alt_b = parse_terminal_key_sequence("\x1b[27;3;98~");
        assert_eq!(alt_b.name.as_deref(), Some("b"));
        assert!(alt_b.meta);
        assert!(!alt_b.shift);

        let ctrl_space = parse_terminal_key_sequence("\x1b[32;5u");
        assert_eq!(ctrl_space.name.as_deref(), Some("space"));
        assert!(ctrl_space.ctrl);

        let ctrl_left = parse_terminal_key_sequence("\x1b[1;5D");
        assert_eq!(ctrl_left.name.as_deref(), Some("left"));
        assert_eq!(ctrl_left.code, None);
        assert!(ctrl_left.ctrl);

        let upper = parse_terminal_key_sequence("A");
        assert_eq!(upper.name.as_deref(), Some("a"));
        assert!(upper.shift);

        let ctrl_c = parse_terminal_key_sequence("\x03");
        assert_eq!(ctrl_c.name.as_deref(), Some("c"));
        assert!(ctrl_c.ctrl);

        let return_key = parse_terminal_key_sequence("\r");
        assert_eq!(return_key.name.as_deref(), Some("return"));
        assert_eq!(return_key.raw, None);

        let natural_left = parse_terminal_key_sequence("\x1bb");
        assert_eq!(natural_left.name.as_deref(), Some("left"));
        assert!(natural_left.meta);

        let sgr_wheel = parse_terminal_key_sequence("\x1b[<64;12;3M");
        assert_eq!(sgr_wheel.name.as_deref(), Some("wheelup"));

        let x10_wheel = parse_terminal_key_sequence("\x1b[M`!!");
        assert_eq!(x10_wheel.name.as_deref(), Some("wheelup"));

        let unmapped = parse_terminal_key_sequence("\x1b[25~");
        assert_eq!(unmapped.name, None);
        assert_eq!(unmapped.code.as_deref(), Some("[25~"));
    }

    #[test]
    fn test_parse_terminal_input_event_matches_cc_ink_input_event() {
        let enter = parse_terminal_input_event("\r");
        assert!(enter.key.return_key);
        assert_eq!(enter.input, "");

        let ctrl_space = parse_terminal_input_event("\x1b[32;5u");
        assert!(ctrl_space.key.ctrl);
        assert_eq!(ctrl_space.input, " ");

        let shift_enter = parse_terminal_input_event("\x1b[13;2u");
        assert!(shift_enter.key.return_key);
        assert!(shift_enter.key.shift);
        assert_eq!(shift_enter.input, "return");

        let alt_b = parse_terminal_input_event("\x1b[27;3;98~");
        assert!(alt_b.key.meta);
        assert_eq!(alt_b.input, "b");

        let keypad_zero = parse_terminal_input_event("\x1bOp");
        assert_eq!(keypad_zero.input, "0");

        let arrow = parse_terminal_input_event("\x1b[D");
        assert!(arrow.key.left_arrow);
        assert_eq!(arrow.input, "");

        let upper = parse_terminal_input_event("A");
        assert!(upper.key.shift);
        assert_eq!(upper.input, "A");

        let wheel = parse_terminal_input_event("\x1b[<64;12;3M");
        assert!(wheel.key.wheel_up);
        assert_eq!(wheel.input, "");

        let orphaned_mouse_tail = parse_terminal_input_event("[<64;12;3M");
        assert_eq!(orphaned_mouse_tail.input, "");

        let unmapped = parse_terminal_input_event("\x1b[25~");
        assert_eq!(unmapped.input, "");
    }

    #[test]
    fn test_terminal_input_parser_exposes_cc_ink_flush_timing_state() {
        assert_eq!(TERMINAL_INPUT_NORMAL_TIMEOUT, Duration::from_millis(50));
        assert_eq!(TERMINAL_INPUT_PASTE_TIMEOUT, Duration::from_millis(500));

        let mut tokenizer = TerminalInputTokenizer::new();
        assert_eq!(tokenizer.pending_flush_timeout(), None);
        assert!(tokenizer.feed("\x1b[").is_empty());
        assert!(tokenizer.has_incomplete_sequence());
        assert!(!tokenizer.is_in_paste());
        assert_eq!(
            tokenizer.pending_flush_timeout(),
            Some(TERMINAL_INPUT_NORMAL_TIMEOUT)
        );
        assert!(tokenizer.should_flush_incomplete(false));
        assert!(!tokenizer.should_flush_incomplete(true));

        let mut parser = TerminalInputParser::new();
        assert!(parser.feed("\x1b[200~payload\x1b[").is_empty());
        assert!(parser.is_in_paste());
        assert!(parser.has_incomplete_sequence());
        assert_eq!(
            parser.pending_flush_timeout(),
            Some(TERMINAL_INPUT_PASTE_TIMEOUT)
        );
        assert!(!parser.should_flush_incomplete(true));
        assert!(parser.should_flush_incomplete(false));
        assert_eq!(
            parser.flush(),
            vec![TerminalParsedInput::Paste("payload\x1b[".to_string())]
        );
        assert!(!parser.is_in_paste());
        assert_eq!(parser.pending_flush_timeout(), None);
    }

    #[test]
    fn test_terminal_input_bytes_match_cc_ink_input_to_string() {
        assert_eq!(terminal_input_bytes_to_string(&[0xe1]), "\x1ba");
        assert_eq!(terminal_input_bytes_to_string("é".as_bytes()), "é");

        let mut tokenizer = TerminalInputTokenizer::new();
        assert_eq!(
            tokenizer.feed_bytes(&[0xe1]),
            vec![TerminalInputToken::Sequence("\x1ba".to_string())]
        );

        let mut parser = TerminalInputParser::new();
        let parsed = parser.feed_bytes(&[0xe1]);
        assert!(matches!(
            parsed.as_slice(),
            [TerminalParsedInput::Key(event)]
                if event.input == "a" && event.key.meta && event.keypress.meta
        ));

        let mut parser = TerminalInputParser::new();
        let events = parser.feed_bytes_events(&[0xe1]);
        assert!(matches!(
            events.as_slice(),
            [TerminalEvent::Key(KeyEvent {
                code: KeyCode::Char('a'),
                modifiers,
                kind: KeyEventKind::Press,
            })] if modifiers.contains(KeyModifiers::ALT)
        ));

        let mut parser = TerminalInputParser::new();
        assert_eq!(
            parser.feed_bytes(b"\x1b[200~pasted\x1b[201~"),
            vec![TerminalParsedInput::Paste("pasted".to_string())]
        );

        let mut responses = TerminalResponseParser::new();
        assert_eq!(
            responses.feed_bytes(b"\x1bP>|xterm.js(5.5.0)\x1b\\"),
            vec![TerminalResponse::Xtversion {
                name: "xterm.js(5.5.0)".to_string()
            }]
        );
    }

    #[test]
    fn test_terminal_input_parser_matches_cc_ink_parse_multiple_keypresses() {
        let mut parser = TerminalInputParser::new();
        let parsed = parser
            .feed("a\x1b[A\x1b[?2026;1$y\x1b[200~hi\x1b[31m\x1b[201~\x1b[<0;12;3M\x1b[<64;12;3M");

        assert!(matches!(
            &parsed[0],
            TerminalParsedInput::Key(event)
                if event.input == "a" && event.keypress.name.as_deref() == Some("a")
        ));
        assert!(matches!(
            &parsed[1],
            TerminalParsedInput::Key(event) if event.key.up_arrow && event.input.is_empty()
        ));
        assert!(matches!(
            &parsed[2],
            TerminalParsedInput::Response(TerminalResponse::Decrpm { mode: 2026, status })
                if *status == DecrpmStatus::Set.code()
        ));
        assert_eq!(
            parsed[3],
            TerminalParsedInput::Paste("hi\x1b[31m".to_string())
        );
        assert!(matches!(
            &parsed[4],
            TerminalParsedInput::Mouse(mouse)
                if mouse.button == 0 && mouse.column == 12 && mouse.row == 3
        ));
        assert!(matches!(
            &parsed[5],
            TerminalParsedInput::Key(event) if event.key.wheel_up && event.input.is_empty()
        ));

        let mut orphaned = TerminalInputParser::new();
        let parsed = [orphaned.feed("[<64;12;3M"), orphaned.feed("[M`!!")].concat();
        assert!(matches!(
            &parsed[0],
            TerminalParsedInput::Key(event) if event.key.wheel_up && event.input.is_empty()
        ));
        assert!(matches!(
            &parsed[1],
            TerminalParsedInput::Key(event) if event.key.wheel_up && event.input.is_empty()
        ));

        let mut incomplete = TerminalInputParser::new();
        assert!(incomplete.feed("\x1b[?").is_empty());
        assert!(matches!(
            incomplete.flush().as_slice(),
            [TerminalParsedInput::Key(event)] if event.input == "[?"
        ));
    }

    #[test]
    fn test_terminal_input_parser_event_bridge_feeds_iocraft_events() {
        let mut parser = TerminalInputParser::new();
        let events = parser.feed_events(
            "ab\x1b[A\x1b[?2026;1$y\x1b[200~paste\x1b[201~\x1b[<4;12;3M\x1b[<64;12;3M",
        );

        assert!(matches!(
            &events[0],
            TerminalEvent::Key(KeyEvent {
                code: KeyCode::Char('a'),
                modifiers,
                kind: KeyEventKind::Press,
            }) if modifiers.is_empty()
        ));
        assert!(matches!(
            &events[1],
            TerminalEvent::Key(KeyEvent {
                code: KeyCode::Char('b'),
                kind: KeyEventKind::Press,
                ..
            })
        ));
        assert!(matches!(
            &events[2],
            TerminalEvent::Key(KeyEvent {
                code: KeyCode::Up,
                kind: KeyEventKind::Press,
                ..
            })
        ));
        assert!(matches!(
            &events[3],
            TerminalEvent::Response(TerminalResponse::Decrpm { mode: 2026, status })
                if *status == DecrpmStatus::Set.code()
        ));
        assert!(matches!(&events[4], TerminalEvent::Paste(text) if text == "paste"));
        assert!(matches!(
            &events[5],
            TerminalEvent::FullscreenMouse(FullscreenMouseEvent {
                modifiers,
                column: 11,
                row: 2,
                kind: MouseEventKind::Down(MouseButton::Left),
                ..
            }) if modifiers.contains(KeyModifiers::SHIFT)
        ));
        assert!(matches!(
            &events[6],
            TerminalEvent::FullscreenMouse(FullscreenMouseEvent {
                column: 11,
                row: 2,
                kind: MouseEventKind::ScrollUp,
                ..
            })
        ));

        assert!(parser.flush_events().is_empty());

        let text_events = terminal_parsed_input_to_events(TerminalParsedInput::Key(
            parse_terminal_input_event("typed"),
        ));
        assert_eq!(text_events.len(), 5);
        assert!(matches!(
            &text_events[4],
            TerminalEvent::Key(KeyEvent {
                code: KeyCode::Char('d'),
                ..
            })
        ));

        let escape_events = terminal_parsed_input_to_events(TerminalParsedInput::Key(
            parse_terminal_input_event("\x1b"),
        ));
        assert!(matches!(
            escape_events.as_slice(),
            [TerminalEvent::Key(KeyEvent {
                code: KeyCode::Esc,
                modifiers,
                ..
            })] if modifiers.is_empty()
        ));
    }

    #[test]
    fn test_terminal_raw_input_mode_sequences_are_opt_in_and_reversible() {
        let options = TerminalRawInputModeOptions {
            hide_cursor: true,
            bracketed_paste: true,
            focus_events: true,
            mouse_capture: false,
            keyboard_enhancement_flags: None,
            xterm_modify_other_keys: false,
        };

        let mut enter = Vec::new();
        write_terminal_raw_input_mode_enter(&mut enter, options).unwrap();
        let enter = String::from_utf8(enter).unwrap();
        assert!(
            enter.contains("\x1b[?25l"),
            "enter should hide cursor: {enter:?}"
        );
        assert!(
            enter.contains("\x1b[?1004h"),
            "enter should enable focus events: {enter:?}"
        );
        assert!(
            enter.contains("\x1b[?2004h"),
            "enter should enable bracketed paste: {enter:?}"
        );
        assert!(
            !enter.contains("\x1b[?1000h"),
            "mouse capture is opt-in: {enter:?}"
        );
        assert!(
            !enter.contains(TERMINAL_MODIFY_OTHER_KEYS_ENABLE),
            "modifyOtherKeys is opt-in: {enter:?}"
        );

        let mut exit = Vec::new();
        write_terminal_raw_input_mode_exit(&mut exit, options).unwrap();
        let exit = String::from_utf8(exit).unwrap();
        assert!(
            exit.contains("\x1b[?2004l"),
            "exit should disable bracketed paste: {exit:?}"
        );
        assert!(
            exit.contains("\x1b[?1004l"),
            "exit should disable focus events: {exit:?}"
        );
        assert!(
            exit.contains("\x1b[?25h"),
            "exit should show cursor: {exit:?}"
        );
    }

    #[test]
    fn test_terminal_raw_input_mode_can_enable_tmux_compatible_extended_keys() {
        let options = TerminalRawInputModeOptions {
            hide_cursor: false,
            bracketed_paste: false,
            focus_events: false,
            mouse_capture: false,
            keyboard_enhancement_flags: Some(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES),
            xterm_modify_other_keys: true,
        };

        let mut enter = Vec::new();
        write_terminal_raw_input_mode_enter(&mut enter, options).unwrap();
        let enter = String::from_utf8(enter).unwrap();
        assert!(
            enter.contains("\x1b[>1u"),
            "enter should push Kitty disambiguate flag: {enter:?}"
        );
        assert!(
            enter.contains(TERMINAL_MODIFY_OTHER_KEYS_ENABLE),
            "enter should enable xterm modifyOtherKeys: {enter:?}"
        );

        let mut exit = Vec::new();
        write_terminal_raw_input_mode_exit(&mut exit, options).unwrap();
        let exit = String::from_utf8(exit).unwrap();
        assert!(
            exit.contains(TERMINAL_MODIFY_OTHER_KEYS_DISABLE),
            "exit should reset xterm modifyOtherKeys: {exit:?}"
        );
        assert!(
            exit.contains("\x1b[<1u"),
            "exit should pop Kitty keyboard flags: {exit:?}"
        );
    }

    #[test]
    fn test_terminal_raw_input_mode_guard_cleans_up_on_drop_and_explicit_exit() {
        let options = TerminalRawInputModeOptions {
            hide_cursor: true,
            bracketed_paste: true,
            focus_events: false,
            mouse_capture: false,
            keyboard_enhancement_flags: None,
            xterm_modify_other_keys: true,
        };

        let mut dropped = Vec::new();
        {
            let mut guard = TerminalRawInputModeGuard::enter(&mut dropped, options).unwrap();
            assert!(guard.is_active());
            guard.writer_mut().write_all(b"payload").unwrap();
        }
        let dropped = String::from_utf8(dropped).unwrap();
        let payload = dropped.find("payload").unwrap_or_else(|| {
            panic!("expected payload between enter/exit sequences: {dropped:?}")
        });
        assert!(
            dropped[..payload].contains("\x1b[?25l"),
            "drop guard should write enter sequence before payload: {dropped:?}"
        );
        assert!(
            dropped[..payload].contains(TERMINAL_MODIFY_OTHER_KEYS_ENABLE),
            "drop guard should enable modifyOtherKeys before payload: {dropped:?}"
        );
        assert!(
            dropped[payload..].contains(TERMINAL_MODIFY_OTHER_KEYS_DISABLE),
            "drop guard should reset modifyOtherKeys after payload: {dropped:?}"
        );
        assert!(
            dropped.ends_with("\x1b[?25h"),
            "drop guard should restore cursor visibility last: {dropped:?}"
        );

        let options = TerminalRawInputModeOptions {
            hide_cursor: false,
            bracketed_paste: false,
            focus_events: true,
            mouse_capture: false,
            keyboard_enhancement_flags: None,
            xterm_modify_other_keys: false,
        };
        let mut guard = TerminalRawInputModeGuard::enter(Vec::new(), options).unwrap();
        guard.writer_mut().write_all(b"explicit").unwrap();
        let explicit = String::from_utf8(guard.exit().unwrap()).unwrap();
        assert!(
            explicit.contains("\x1b[?1004h"),
            "explicit guard should write enter focus sequence: {explicit:?}"
        );
        assert!(
            explicit.contains("explicit"),
            "payload should be preserved: {explicit:?}"
        );
        assert!(
            explicit.contains("\x1b[?1004l"),
            "explicit guard should write exit focus sequence: {explicit:?}"
        );
    }

    #[test]
    fn test_terminal_raw_input_session_guard_keeps_os_raw_mode_opt_in() {
        let terminal_modes = TerminalRawInputModeOptions {
            hide_cursor: true,
            bracketed_paste: true,
            focus_events: true,
            mouse_capture: false,
            keyboard_enhancement_flags: None,
            xterm_modify_other_keys: true,
        };
        let options = TerminalRawInputSessionOptions {
            terminal_modes,
            enable_os_raw_mode: false,
        };

        let mut guard = TerminalRawInputSessionGuard::enter(Vec::new(), options).unwrap();
        assert!(guard.is_active());
        assert!(!guard.is_os_raw_mode_enabled());
        assert_eq!(guard.options(), options);
        guard.writer_mut().write_all(b"session-payload").unwrap();

        let output = String::from_utf8(guard.exit().unwrap()).unwrap();
        let payload = output
            .find("session-payload")
            .unwrap_or_else(|| panic!("expected payload between enter/exit sequences: {output:?}"));
        assert!(
            output[..payload].contains("\x1b[?25l"),
            "session should hide the cursor before payload: {output:?}"
        );
        assert!(
            output[..payload].contains(TERMINAL_MODIFY_OTHER_KEYS_ENABLE),
            "session should enable explicit modifyOtherKeys before payload: {output:?}"
        );
        assert!(
            output[payload..].contains(TERMINAL_MODIFY_OTHER_KEYS_DISABLE),
            "session should reset explicit modifyOtherKeys after payload: {output:?}"
        );
        assert!(
            output.ends_with("\x1b[?25h"),
            "session should restore cursor visibility last: {output:?}"
        );

        let mut dropped = Vec::new();
        {
            let mut guard = TerminalRawInputSessionGuard::enter(&mut dropped, options).unwrap();
            guard.writer_mut().write_all(b"drop-session").unwrap();
        }
        let dropped = String::from_utf8(dropped).unwrap();
        let payload = dropped.find("drop-session").unwrap_or_else(|| {
            panic!("expected drop payload between enter/exit sequences: {dropped:?}")
        });
        assert!(
            dropped[..payload].contains(TERMINAL_MODIFY_OTHER_KEYS_ENABLE),
            "drop session should enable modifyOtherKeys before payload: {dropped:?}"
        );
        assert!(
            dropped[payload..].contains(TERMINAL_MODIFY_OTHER_KEYS_DISABLE),
            "drop session should reset modifyOtherKeys after payload: {dropped:?}"
        );
    }

    #[test]
    fn test_terminal_raw_input_session_event_stream_scopes_modes_and_reader() {
        let terminal_modes = TerminalRawInputModeOptions {
            hide_cursor: true,
            bracketed_paste: true,
            focus_events: false,
            mouse_capture: false,
            keyboard_enhancement_flags: None,
            xterm_modify_other_keys: true,
        };
        let options = TerminalRawInputSessionOptions {
            terminal_modes,
            enable_os_raw_mode: false,
        };
        let reader = futures::io::Cursor::new(b"ab\x1b[A".to_vec());

        let mut session = TerminalRawInputSessionEventStream::from_reader_with_chunk_size(
            Vec::new(),
            reader,
            2,
            options,
        )
        .unwrap();
        assert!(session.guard().is_active());
        assert!(!session.guard().is_os_raw_mode_enabled());
        session
            .guard_mut()
            .writer_mut()
            .write_all(b"session-ui")
            .unwrap();

        let events = smol::block_on(async {
            let mut events = Vec::new();
            while let Some(event) = session.next().await {
                events.push(event.unwrap());
            }
            events
        });
        assert_eq!(events.len(), 3);
        assert!(matches!(
            &events[0],
            TerminalEvent::Key(KeyEvent {
                code: KeyCode::Char('a'),
                kind: KeyEventKind::Press,
                ..
            })
        ));
        assert!(matches!(
            &events[1],
            TerminalEvent::Key(KeyEvent {
                code: KeyCode::Char('b'),
                kind: KeyEventKind::Press,
                ..
            })
        ));
        assert!(matches!(
            &events[2],
            TerminalEvent::Key(KeyEvent {
                code: KeyCode::Up,
                kind: KeyEventKind::Press,
                ..
            })
        ));

        let output = String::from_utf8(session.exit().unwrap()).unwrap();
        let payload = output.find("session-ui").unwrap_or_else(|| {
            panic!("expected session payload between enter/exit sequences: {output:?}")
        });
        assert!(
            output[..payload].contains(TERMINAL_MODIFY_OTHER_KEYS_ENABLE),
            "session stream should enter modes before payload: {output:?}"
        );
        assert!(
            output[payload..].contains(TERMINAL_MODIFY_OTHER_KEYS_DISABLE),
            "session stream should exit modes after parsing reader: {output:?}"
        );
    }

    #[test]
    fn test_terminal_raw_input_frontend_bridges_bytes_events_and_flush_timing() {
        let mut frontend = TerminalRawInputFrontend::new();

        let output = frontend.feed_bytes(b"a\x1b[?2026;1$y\x1b[200~paste\x1b[201~\x1b[<64;12;3M");
        assert_eq!(output.pending_flush_timeout, None);
        assert!(matches!(
            output.parsed.as_slice(),
            [
                TerminalParsedInput::Key(event),
                TerminalParsedInput::Response(TerminalResponse::Decrpm { mode: 2026, status }),
                TerminalParsedInput::Paste(text),
                TerminalParsedInput::Key(wheel),
            ] if event.input == "a"
                && *status == DecrpmStatus::Set.code()
                && text == "paste"
                && wheel.key.wheel_up
        ));
        assert!(matches!(
            &output.events[0],
            TerminalEvent::Key(KeyEvent {
                code: KeyCode::Char('a'),
                kind: KeyEventKind::Press,
                ..
            })
        ));
        assert!(matches!(
            &output.events[1],
            TerminalEvent::Response(TerminalResponse::Decrpm { mode: 2026, .. })
        ));
        assert!(matches!(&output.events[2], TerminalEvent::Paste(text) if text == "paste"));
        assert!(matches!(
            &output.events[3],
            TerminalEvent::FullscreenMouse(FullscreenMouseEvent {
                kind: MouseEventKind::ScrollUp,
                column: 11,
                row: 2,
                ..
            })
        ));

        let output = frontend.feed("\x1b[");
        assert!(output.parsed.is_empty());
        assert_eq!(
            output.pending_flush_timeout,
            Some(TERMINAL_INPUT_NORMAL_TIMEOUT)
        );
        assert!(frontend.has_incomplete_sequence());
        assert!(frontend.flush_if_due(true).is_none());
        let flushed = frontend
            .flush_if_due(false)
            .expect("expired timer with no queued input should flush");
        assert_eq!(flushed.pending_flush_timeout, None);
        assert!(matches!(
            flushed.parsed.as_slice(),
            [TerminalParsedInput::Key(event)] if event.input == "[?" || event.input == "["
        ));
    }

    #[test]
    fn test_terminal_raw_input_event_stream_adapts_byte_chunks_and_flushes_on_end() {
        let source = stream::iter(vec![
            b"a\x1b[".to_vec(),
            b"A\x1b[200~paste".to_vec(),
            b"\x1b[201~".to_vec(),
            b"\x1b".to_vec(),
        ]);
        let events: Vec<_> = smol::block_on(TerminalRawInputEventStream::new(source).collect());

        assert!(matches!(
            &events[0],
            TerminalEvent::Key(KeyEvent {
                code: KeyCode::Char('a'),
                kind: KeyEventKind::Press,
                ..
            })
        ));
        assert!(matches!(
            &events[1],
            TerminalEvent::Key(KeyEvent {
                code: KeyCode::Up,
                kind: KeyEventKind::Press,
                ..
            })
        ));
        assert!(matches!(&events[2], TerminalEvent::Paste(text) if text == "paste"));
        assert!(matches!(
            events.last(),
            Some(TerminalEvent::Key(KeyEvent {
                code: KeyCode::Esc,
                kind: KeyEventKind::Press,
                ..
            }))
        ));
    }

    #[test]
    fn test_terminal_raw_input_fallible_reader_stream_adapts_async_read() {
        let reader = futures::io::Cursor::new(b"ab\x1b[A".to_vec());
        let events = smol::block_on(
            TerminalRawInputFallibleEventStream::from_reader_with_chunk_size(reader, 2)
                .collect::<Vec<_>>(),
        );
        assert_eq!(events.len(), 3);
        assert!(matches!(
            events[0].as_ref().unwrap(),
            TerminalEvent::Key(KeyEvent {
                code: KeyCode::Char('a'),
                kind: KeyEventKind::Press,
                ..
            })
        ));
        assert!(matches!(
            events[1].as_ref().unwrap(),
            TerminalEvent::Key(KeyEvent {
                code: KeyCode::Char('b'),
                kind: KeyEventKind::Press,
                ..
            })
        ));
        assert!(matches!(
            events[2].as_ref().unwrap(),
            TerminalEvent::Key(KeyEvent {
                code: KeyCode::Up,
                kind: KeyEventKind::Press,
                ..
            })
        ));

        let chunks = smol::block_on(
            TerminalRawInputByteStream::with_chunk_size(
                futures::io::Cursor::new(b"abcdef".to_vec()),
                3,
            )
            .collect::<Vec<_>>(),
        );
        assert_eq!(
            chunks
                .into_iter()
                .map(|chunk| chunk.unwrap())
                .collect::<Vec<_>>(),
            vec![b"abc".to_vec(), b"def".to_vec()]
        );
    }

    #[test]
    fn test_terminal_query_sequences_and_response_parser_match_cc_ink() {
        assert_eq!(decrqm_query_sequence(2026), "\x1b[?2026$p");
        assert_eq!(da1_query_sequence(), "\x1b[c");
        assert_eq!(da2_query_sequence(), "\x1b[>c");
        assert_eq!(kitty_keyboard_query_sequence(), "\x1b[?u");
        assert_eq!(cursor_position_query_sequence(), "\x1b[?6n");
        assert_eq!(
            osc_color_query_sequence_with_env(11, |_| None),
            "\x1b]11;?\x07"
        );
        assert_eq!(
            osc_color_query_sequence_with_env(11, |key| {
                (key == "TERM").then(|| "xterm-kitty".to_string())
            }),
            "\x1b]11;?\x1b\\"
        );

        let decrpm = parse_terminal_response("\x1b[?2026;1$y").unwrap();
        assert_eq!(
            decrpm,
            TerminalResponse::Decrpm {
                mode: 2026,
                status: DecrpmStatus::Set.code(),
            }
        );
        assert_eq!(decrpm.decrpm_status(), Some(DecrpmStatus::Set));
        assert_eq!(DecrpmStatus::from_code(99), None);
        assert_eq!(
            parse_terminal_response("\x1b[?1;2c"),
            Some(TerminalResponse::Da1 { params: vec![1, 2] })
        );
        assert_eq!(
            parse_terminal_response("\x1b[>0;95;0c"),
            Some(TerminalResponse::Da2 {
                params: vec![0, 95, 0]
            })
        );
        assert_eq!(
            parse_terminal_response("\x1b[?7u"),
            Some(TerminalResponse::KittyKeyboard { flags: 7 })
        );
        assert_eq!(
            parse_terminal_response("\x1b[?24;80R"),
            Some(TerminalResponse::CursorPosition { row: 24, col: 80 })
        );
        assert_eq!(
            parse_terminal_response("\x1b]11;rgb:0000/0000/0000\x1b\\"),
            Some(TerminalResponse::Osc {
                code: 11,
                data: "rgb:0000/0000/0000".to_string()
            })
        );
        assert_eq!(
            parse_terminal_response("\x1bP>|xterm.js(5.5.0)\x1b\\"),
            Some(TerminalResponse::Xtversion {
                name: "xterm.js(5.5.0)".to_string()
            })
        );
        assert_eq!(parse_terminal_response("\x1b[1;2R"), None);
    }

    #[test]
    fn test_terminal_response_parser_buffers_chunked_responses() {
        let mut parser = TerminalResponseParser::new();
        assert!(parser.feed("typed text \x1bP>|xterm").is_empty());
        assert_eq!(parser.buffered(), "\x1bP>|xterm");
        assert_eq!(
            parser.feed(".js(5.5.0)\x1b\\ trailing"),
            vec![TerminalResponse::Xtversion {
                name: "xterm.js(5.5.0)".to_string()
            }]
        );
        assert_eq!(parser.buffered(), "");
    }

    #[test]
    fn test_terminal_response_parser_scans_mixed_csi_osc_and_ignores_keys() {
        let mut parser = TerminalResponseParser::new();
        let events = parser
            .feed_events("\x1b[A\x1b[?2026;1$yplain\x1b]11;rgb:0000/0000/0000\x07\x1b[?24;80R");
        assert_eq!(events.len(), 3);
        assert!(matches!(
            &events[0],
            TerminalEvent::Response(TerminalResponse::Decrpm {
                mode: 2026,
                status: 1
            })
        ));
        assert!(matches!(
            &events[1],
            TerminalEvent::Response(TerminalResponse::Osc { code: 11, data })
                if data == "rgb:0000/0000/0000"
        ));
        assert!(matches!(
            &events[2],
            TerminalEvent::Response(TerminalResponse::CursorPosition { row: 24, col: 80 })
        ));
    }

    #[test]
    fn test_terminal_response_parser_buffers_incomplete_osc_st() {
        let mut parser = TerminalResponseParser::new();
        assert!(parser.feed("\x1b]11;rgb:ffff").is_empty());
        assert_eq!(parser.buffered(), "\x1b]11;rgb:ffff");
        assert_eq!(
            parser.feed("/eeee/dddd\x1b\\"),
            vec![TerminalResponse::Osc {
                code: 11,
                data: "rgb:ffff/eeee/dddd".to_string()
            }]
        );
    }

    #[test]
    fn test_terminal_response_parser_skips_non_response_string_sequences() {
        let mut parser = TerminalResponseParser::new();
        assert_eq!(
            parser.feed("\x1b^ignore \x1b[?2026;1$y\x1b\\\x1b[?2026;2$y"),
            vec![TerminalResponse::Decrpm {
                mode: 2026,
                status: 2,
            }]
        );
    }

    #[test]
    fn test_terminal_querier_matches_cc_flush_barrier_semantics() {
        let mut querier = TerminalQuerier::new(Vec::new());
        let decrpm = querier.send(TerminalQuery::decrqm(2026)).unwrap();
        let flush = querier.flush().unwrap();
        assert_eq!(
            String::from_utf8(querier.output_ref().clone()).unwrap(),
            "\x1b[?2026$p\x1b[c"
        );

        let response = TerminalResponse::Decrpm {
            mode: 2026,
            status: 1,
        };
        assert!(querier.on_event(&TerminalEvent::Response(response.clone())));
        assert_eq!(futures::executor::block_on(decrpm), Some(response));

        querier.on_response(TerminalResponse::Da1 { params: vec![1, 2] });
        futures::executor::block_on(flush);

        let unsupported = querier.send(TerminalQuery::decrqm(2027)).unwrap();
        let flush = querier.flush().unwrap();
        querier.on_response(TerminalResponse::Da1 { params: vec![1] });
        assert_eq!(futures::executor::block_on(unsupported), None);
        futures::executor::block_on(flush);
    }

    #[test]
    fn test_terminal_querier_explicit_da1_query_precedes_flush_sentinel() {
        let mut querier = TerminalQuerier::new(Vec::new());
        let da1 = querier.send(TerminalQuery::da1()).unwrap();
        let flush = querier.flush().unwrap();

        let first = TerminalResponse::Da1 { params: vec![1] };
        querier.on_response(first.clone());
        assert_eq!(futures::executor::block_on(da1), Some(first));

        querier.on_response(TerminalResponse::Da1 { params: vec![2] });
        futures::executor::block_on(flush);
    }

    #[test]
    fn test_terminal_query_methods_write_side_band_and_resolve() {
        let (dest, buf) = new_test_writer();
        let mut term = Terminal::new(
            Box::new(dest),
            Box::new(io::sink()),
            Output::Stdout,
            false,
            false,
        )
        .unwrap();

        // Discard startup cursor-hide output; terminal queries should write raw
        // side-band control sequences without entering synchronized update.
        buf.lock().unwrap().clear();

        let pending = term
            .send_terminal_query(TerminalQuery::xtversion())
            .unwrap();
        let flush = term.flush_terminal_queries().unwrap();
        let output = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
        assert_eq!(output, "\x1b[>0q\x1b[c");
        assert!(
            !output.contains("\x1b[?2026"),
            "terminal queries are non-visual side-band controls: {output:?}"
        );

        let response = TerminalResponse::Xtversion {
            name: "xterm.js(5.5.0)".to_string(),
        };
        term.on_terminal_response(response.clone());
        assert_eq!(futures::executor::block_on(pending), Some(response));

        term.on_terminal_response(TerminalResponse::Da1 { params: vec![1] });
        futures::executor::block_on(flush);
    }

    #[test]
    fn test_terminal_query_starts_event_stream_without_input_subscriber() {
        let (mut term, _output) = Terminal::mock(MockTerminalConfig::default());
        assert!(term.is_raw_mode_supported());
        assert!(!term.is_raw_mode_enabled());

        let pending = term.send_terminal_query(TerminalQuery::da1()).unwrap();

        assert!(
            term.is_raw_mode_enabled(),
            "query response routing should activate the backend event stream even without use_input/use_terminal_events"
        );
        term.on_terminal_response(TerminalResponse::Da1 { params: vec![1, 2] });
        assert_eq!(
            futures::executor::block_on(pending),
            Some(TerminalResponse::Da1 { params: vec![1, 2] })
        );
    }

    #[test]
    fn test_should_clear_terminal_screen_matches_cc_ink_frame_helper() {
        let base = TerminalFrameBounds {
            screen_height: 3,
            viewport_width: 80,
            viewport_height: 10,
        };

        assert_eq!(should_clear_terminal_screen(base, base), None);
        assert_eq!(
            should_clear_terminal_screen(
                base,
                TerminalFrameBounds {
                    viewport_width: 100,
                    ..base
                }
            ),
            Some(TerminalClearReason::Resize)
        );
        assert_eq!(
            should_clear_terminal_screen(
                base,
                TerminalFrameBounds {
                    viewport_height: 8,
                    ..base
                }
            ),
            Some(TerminalClearReason::Resize)
        );
        assert_eq!(
            should_clear_terminal_screen(
                base,
                TerminalFrameBounds {
                    screen_height: 10,
                    ..base
                }
            ),
            Some(TerminalClearReason::Offscreen)
        );
        assert_eq!(
            should_clear_terminal_screen(
                TerminalFrameBounds {
                    screen_height: 10,
                    ..base
                },
                base
            ),
            Some(TerminalClearReason::Offscreen)
        );
    }

    #[test]
    fn test_analyze_terminal_inline_diff_matches_cc_log_update_geometry_guards() {
        let base = TerminalInlineDiffBounds {
            prev_screen_height: 8,
            next_screen_height: 8,
            prev_viewport_width: 80,
            prev_viewport_height: 10,
            next_viewport_width: 80,
            next_viewport_height: 10,
        };

        assert_eq!(
            analyze_terminal_inline_diff(base),
            TerminalInlineDiffAnalysis {
                clear_reason: None,
                unreachable_rows: 0,
                growing: false,
                shrinking: false,
            }
        );
        assert_eq!(
            analyze_terminal_inline_diff(TerminalInlineDiffBounds {
                next_viewport_height: 8,
                ..base
            })
            .clear_reason,
            Some(TerminalClearReason::Resize)
        );
        assert_eq!(
            analyze_terminal_inline_diff(TerminalInlineDiffBounds {
                next_viewport_width: 100,
                ..base
            })
            .clear_reason,
            Some(TerminalClearReason::Resize)
        );
        assert_eq!(
            analyze_terminal_inline_diff(TerminalInlineDiffBounds {
                next_viewport_height: 12,
                ..base
            })
            .clear_reason,
            None,
            "main-screen log-update does not clear solely because the viewport grew taller"
        );

        let shrink_to_fits = analyze_terminal_inline_diff(TerminalInlineDiffBounds {
            prev_screen_height: 12,
            next_screen_height: 10,
            ..base
        });
        assert_eq!(
            shrink_to_fits.clear_reason,
            Some(TerminalClearReason::Offscreen)
        );
        assert_eq!(shrink_to_fits.unreachable_rows, 3);
        assert!(shrink_to_fits.shrinking);

        let steady_overflow = analyze_terminal_inline_diff(TerminalInlineDiffBounds {
            prev_screen_height: 12,
            next_screen_height: 12,
            ..base
        });
        assert_eq!(steady_overflow.clear_reason, None);
        assert_eq!(
            steady_overflow.unreachable_rows, 3,
            "custom renderers should clear if sparse diffs touch this top prefix"
        );

        let growing_after_full_viewport = analyze_terminal_inline_diff(TerminalInlineDiffBounds {
            prev_screen_height: 10,
            next_screen_height: 11,
            ..base
        });
        assert_eq!(growing_after_full_viewport.clear_reason, None);
        assert_eq!(
            growing_after_full_viewport.unreachable_rows, 1,
            "cursorRestoreScroll contributes one unreachable top row"
        );
        assert!(growing_after_full_viewport.growing);

        let shrink_clear_too_large = analyze_terminal_inline_diff(TerminalInlineDiffBounds {
            prev_screen_height: 100,
            next_screen_height: 70,
            prev_viewport_height: 20,
            next_viewport_height: 20,
            ..base
        });
        assert_eq!(
            shrink_clear_too_large.clear_reason,
            Some(TerminalClearReason::Offscreen)
        );
    }

    #[test]
    fn test_plan_terminal_inline_canvas_diff_scans_unreachable_rows_like_cc_log_update() {
        let bounds = TerminalInlineDiffBounds {
            prev_screen_height: 12,
            next_screen_height: 12,
            prev_viewport_width: 80,
            prev_viewport_height: 10,
            next_viewport_width: 80,
            next_viewport_height: 10,
        };

        let mut previous = Canvas::new(12, 12);
        previous.subview_mut(0, 0, 0, 0, 12, 12).set_text(
            0,
            0,
            "old top",
            CanvasTextStyle::default(),
        );
        previous.clear_damage();

        let mut next = Canvas::new(12, 12);
        next.subview_mut(0, 0, 0, 0, 12, 12)
            .set_text(0, 0, "new top", CanvasTextStyle::default());
        next.clear_damage();

        let decision = plan_terminal_inline_canvas_diff(&previous, &next, bounds);
        assert_eq!(decision.analysis.unreachable_rows, 3);
        assert_eq!(decision.clear_reason, Some(TerminalClearReason::Offscreen));
        assert_eq!(
            decision.debug,
            Some(TerminalInlineDiffResetDebug {
                trigger_y: 0,
                prev_line: "old top".to_string(),
                next_line: "new top".to_string(),
            })
        );

        let mut reachable_next = previous.clone();
        reachable_next.subview_mut(0, 0, 0, 0, 12, 12).set_text(
            0,
            5,
            "safe row",
            CanvasTextStyle::default(),
        );
        reachable_next.clear_damage();
        let reachable = plan_terminal_inline_canvas_diff(&previous, &reachable_next, bounds);
        assert_eq!(reachable.clear_reason, None);
        assert_eq!(reachable.debug, None);

        let resize = plan_terminal_inline_canvas_diff(
            &previous,
            &next,
            TerminalInlineDiffBounds {
                next_viewport_width: 100,
                ..bounds
            },
        );
        assert_eq!(resize.clear_reason, Some(TerminalClearReason::Resize));
        assert_eq!(resize.debug, None);
    }

    #[test]
    fn test_plan_terminal_inline_packed_canvas_diff_scans_unreachable_rows_like_cc_log_update() {
        let bounds = TerminalInlineDiffBounds {
            prev_screen_height: 12,
            next_screen_height: 12,
            prev_viewport_width: 80,
            prev_viewport_height: 10,
            next_viewport_width: 80,
            next_viewport_height: 10,
        };
        let mut pools = CanvasPackedCellPools::new();
        let mut line_cache = CanvasPackedLineCache::new();
        let style = CanvasResolvedStyle::default();

        let mut previous = CanvasPackedScreen::new(12, 12);
        previous.write_line_with_cache(&mut pools, &mut line_cache, 0, 0, "old top", style, None);
        previous.clear_damage();

        let mut next = CanvasPackedScreen::new(12, 12);
        next.write_line_with_cache(&mut pools, &mut line_cache, 0, 0, "new top", style, None);

        let decision = plan_terminal_inline_packed_canvas_diff(&previous, &next, &pools, bounds);
        assert_eq!(decision.analysis.unreachable_rows, 3);
        assert_eq!(decision.clear_reason, Some(TerminalClearReason::Offscreen));
        assert_eq!(
            decision.debug,
            Some(TerminalInlineDiffResetDebug {
                trigger_y: 0,
                prev_line: "old top".to_string(),
                next_line: "new top".to_string(),
            })
        );

        let mut reachable_next = previous.clone();
        reachable_next.write_line_with_cache(
            &mut pools,
            &mut line_cache,
            0,
            5,
            "safe row",
            style,
            None,
        );
        let reachable =
            plan_terminal_inline_packed_canvas_diff(&previous, &reachable_next, &pools, bounds);
        assert_eq!(reachable.clear_reason, None);
        assert_eq!(reachable.debug, None);
    }

    #[test]
    fn test_plan_terminal_inline_packed_canvas_frame_patches_emits_full_reset_fallback() {
        let bounds = TerminalInlineDiffBounds {
            prev_screen_height: 12,
            next_screen_height: 12,
            prev_viewport_width: 80,
            prev_viewport_height: 10,
            next_viewport_width: 80,
            next_viewport_height: 10,
        };
        let mut pools = CanvasPackedCellPools::new();
        let mut line_cache = CanvasPackedLineCache::new();
        let mut style_cache = CanvasStyleTransitionCache::new();
        let style = CanvasResolvedStyle::default();

        let mut previous = CanvasPackedScreen::new(12, 12);
        previous.write_line_with_cache(&mut pools, &mut line_cache, 0, 0, "old top", style, None);
        previous.clear_damage();

        let mut next = CanvasPackedScreen::new(12, 12);
        next.write_line_with_cache(&mut pools, &mut line_cache, 0, 0, "new top", style, None);

        let plan = plan_terminal_inline_packed_canvas_frame_patches(
            &previous,
            &next,
            &pools,
            &mut style_cache,
            bounds,
        );
        assert!(plan.requires_clear_repaint());
        assert!(!plan.sparse_diff_safe());
        assert_eq!(
            plan.decision.clear_reason,
            Some(TerminalClearReason::Offscreen)
        );
        assert_eq!(plan.patches.first(), Some(&TerminalPatch::ClearTerminal));
        assert!(matches!(
            plan.patches.get(1),
            Some(TerminalPatch::Stdout(output)) if output.contains("new") && output.contains("top")
        ));

        let mut reachable_next = previous.clone();
        reachable_next.write_line_with_cache(
            &mut pools,
            &mut line_cache,
            0,
            5,
            "safe row",
            style,
            None,
        );
        let safe = plan_terminal_inline_packed_canvas_frame_patches(
            &previous,
            &reachable_next,
            &pools,
            &mut style_cache,
            bounds,
        );
        assert!(safe.sparse_diff_safe());
        assert!(!safe.requires_clear_repaint());
        assert!(safe.patches.is_empty());
    }

    #[test]
    fn test_plan_terminal_inline_canvas_frame_patches_emits_full_reset_fallback() {
        let bounds = TerminalInlineDiffBounds {
            prev_screen_height: 12,
            next_screen_height: 12,
            prev_viewport_width: 80,
            prev_viewport_height: 10,
            next_viewport_width: 80,
            next_viewport_height: 10,
        };

        let mut previous = Canvas::new(12, 12);
        previous.subview_mut(0, 0, 0, 0, 12, 12).set_text(
            0,
            0,
            "old top",
            CanvasTextStyle::default(),
        );
        previous.clear_damage();

        let mut next = Canvas::new(12, 12);
        next.subview_mut(0, 0, 0, 0, 12, 12)
            .set_text(0, 0, "new top", CanvasTextStyle::default());
        next.clear_damage();

        let plan = plan_terminal_inline_canvas_frame_patches(&previous, &next, bounds);
        assert!(plan.requires_clear_repaint());
        assert!(!plan.sparse_diff_safe());
        assert_eq!(
            plan.decision.clear_reason,
            Some(TerminalClearReason::Offscreen)
        );
        assert_eq!(plan.patches.first(), Some(&TerminalPatch::ClearTerminal));
        assert!(matches!(
            plan.patches.get(1),
            Some(TerminalPatch::Stdout(output)) if output.contains("new top")
        ));

        let mut reachable_next = previous.clone();
        reachable_next.subview_mut(0, 0, 0, 0, 12, 12).set_text(
            0,
            5,
            "safe row",
            CanvasTextStyle::default(),
        );
        reachable_next.clear_damage();
        let safe = plan_terminal_inline_canvas_frame_patches(&previous, &reachable_next, bounds);
        assert!(safe.sparse_diff_safe());
        assert!(!safe.requires_clear_repaint());
        assert!(safe.patches.is_empty());
    }

    #[test]
    fn test_optimize_terminal_patches_matches_cc_ink_optimizer_rules() {
        let optimized = optimize_terminal_patches(vec![
            TerminalPatch::Stdout(String::new()),
            TerminalPatch::CursorMove { x: 0, y: 0 },
            TerminalPatch::Clear { count: 0 },
            TerminalPatch::CursorMove { x: 2, y: -1 },
            TerminalPatch::CursorMove { x: -1, y: 3 },
            TerminalPatch::CursorTo { col: 4 },
            TerminalPatch::CursorTo { col: 8 },
            TerminalPatch::StyleStr("\x1b[31m".to_string()),
            TerminalPatch::StyleStr("\x1b[1m".to_string()),
            TerminalPatch::Hyperlink {
                uri: "https://example.test".to_string(),
            },
            TerminalPatch::Hyperlink {
                uri: "https://example.test".to_string(),
            },
            TerminalPatch::CursorHide,
            TerminalPatch::CursorShow,
            TerminalPatch::Stdout("text".to_string()),
            TerminalPatch::Clear { count: 2 },
        ]);

        assert_eq!(
            optimized,
            vec![
                TerminalPatch::CursorMove { x: 1, y: 2 },
                TerminalPatch::CursorTo { col: 8 },
                TerminalPatch::StyleStr("\x1b[31m\x1b[1m".to_string()),
                TerminalPatch::Hyperlink {
                    uri: "https://example.test".to_string(),
                },
                TerminalPatch::Stdout("text".to_string()),
                TerminalPatch::Clear { count: 2 },
            ]
        );
    }

    #[test]
    fn test_optimize_terminal_patches_keeps_cursor_move_sum_zero_like_cc_ink() {
        let optimized = optimize_terminal_patches(vec![
            TerminalPatch::CursorMove { x: 1, y: 0 },
            TerminalPatch::CursorMove { x: -1, y: 0 },
        ]);

        assert_eq!(optimized, vec![TerminalPatch::CursorMove { x: 0, y: 0 }]);
    }

    #[test]
    fn test_terminal_scroll_hint_to_ansi_matches_cc_ink_decstbm_patch_guard() {
        let bounds = TerminalScrollHintBounds {
            previous_screen_height: 10,
            next_screen_height: 10,
        };
        assert_eq!(
            terminal_scroll_hint_to_ansi(
                crate::canvas::ScrollHint {
                    top: 2,
                    bottom: 6,
                    delta: 2,
                },
                bounds,
            ),
            Ok("\x1b[3;7r\x1b[2S\x1b[r\x1b[H".to_string())
        );
        assert_eq!(
            terminal_scroll_hint_to_ansi(
                crate::canvas::ScrollHint {
                    top: 2,
                    bottom: 6,
                    delta: -3,
                },
                bounds,
            ),
            Ok("\x1b[3;7r\x1b[3T\x1b[r\x1b[H".to_string())
        );
        assert_eq!(
            terminal_scroll_hint_to_ansi(
                crate::canvas::ScrollHint {
                    top: 7,
                    bottom: 6,
                    delta: 1,
                },
                bounds,
            ),
            Err(TerminalScrollHintRejection::InvalidRegion)
        );
        assert_eq!(
            terminal_scroll_hint_to_ansi(
                crate::canvas::ScrollHint {
                    top: 2,
                    bottom: 10,
                    delta: 1,
                },
                bounds,
            ),
            Err(TerminalScrollHintRejection::OutOfBounds)
        );
        assert_eq!(
            terminal_scroll_hint_to_ansi(
                crate::canvas::ScrollHint {
                    top: 2,
                    bottom: 6,
                    delta: 0,
                },
                bounds,
            ),
            Err(TerminalScrollHintRejection::ZeroDelta)
        );
        assert_eq!(
            terminal_scroll_hint_to_ansi(
                crate::canvas::ScrollHint {
                    top: 2,
                    bottom: 6,
                    delta: 5,
                },
                bounds,
            ),
            Err(TerminalScrollHintRejection::DeltaTooLarge)
        );
    }

    #[test]
    fn test_plan_terminal_scroll_hint_patch_matches_cc_ink_decstbm_safe_gate() {
        let hint = crate::canvas::ScrollHint {
            top: 2,
            bottom: 6,
            delta: 2,
        };
        let bounds = TerminalScrollHintBounds {
            previous_screen_height: 10,
            next_screen_height: 10,
        };

        assert_eq!(
            plan_terminal_scroll_hint_patch(
                hint,
                bounds,
                TerminalScrollHintPatchOptions {
                    fullscreen: false,
                    synchronized_output: true,
                },
            ),
            Ok(TerminalScrollHintPatchPlan::Skip(
                TerminalScrollHintPatchSkipReason::NotFullscreen,
            )),
        );
        assert_eq!(
            plan_terminal_scroll_hint_patch(
                hint,
                bounds,
                TerminalScrollHintPatchOptions {
                    fullscreen: true,
                    synchronized_output: false,
                },
            ),
            Ok(TerminalScrollHintPatchPlan::Skip(
                TerminalScrollHintPatchSkipReason::NotSynchronized,
            )),
        );
        assert_eq!(
            plan_terminal_scroll_hint_patch(
                hint,
                TerminalScrollHintBounds {
                    previous_screen_height: 0,
                    next_screen_height: 0,
                },
                TerminalScrollHintPatchOptions::default(),
            ),
            Ok(TerminalScrollHintPatchPlan::Skip(
                TerminalScrollHintPatchSkipReason::NotFullscreen,
            )),
            "unsafe modes skip before validating geometry, like CC Ink's altScreen/decstbmSafe gate"
        );
        assert_eq!(
            plan_terminal_scroll_hint_patch(
                hint,
                bounds,
                TerminalScrollHintPatchOptions::fullscreen_synchronized(),
            ),
            Ok(TerminalScrollHintPatchPlan::Emit(
                "\x1b[3;7r\x1b[2S\x1b[r\x1b[H".to_string(),
            )),
        );
        assert!(TerminalScrollHintPatchOptions::fullscreen_synchronized().is_decstbm_safe());
    }

    #[test]
    fn test_terminal_patches_to_ansi_matches_cc_ink_write_diff_to_terminal() {
        let diff = vec![
            TerminalPatch::CursorHide,
            TerminalPatch::CursorMove { x: -2, y: 3 },
            TerminalPatch::CursorTo { col: 5 },
            TerminalPatch::CarriageReturn,
            TerminalPatch::Clear { count: 2 },
            TerminalPatch::Hyperlink {
                uri: "https://example.com".to_string(),
            },
            TerminalPatch::Stdout("hi".to_string()),
            TerminalPatch::Hyperlink { uri: String::new() },
            TerminalPatch::StyleStr("\x1b[0m".to_string()),
            TerminalPatch::CursorShow,
        ];

        let body = concat!(
            "\x1b[?25l",
            "\x1b[2D\x1b[3B",
            "\x1b[5G",
            "\r",
            "\x1b[2K\x1b[1A\x1b[2K\x1b[G",
            "\x1b]8;id=ags5vy;https://example.com\x1b\\",
            "hi",
            "\x1b]8;;\x1b\\",
            "\x1b[0m",
            "\x1b[?25h",
        );
        assert_eq!(
            terminal_patches_to_ansi(&diff, false),
            format!("\x1b[?2026h{body}\x1b[?2026l")
        );
        assert_eq!(terminal_patches_to_ansi(&diff, true), body);
        assert_eq!(terminal_patches_to_ansi(&[], false), "");
    }

    #[test]
    fn test_write_terminal_patches_writes_serialized_diff() {
        let diff = [TerminalPatch::Stdout("patch".to_string())];
        let mut output = Vec::new();

        write_terminal_patches(&mut output, &diff, true).unwrap();

        assert_eq!(String::from_utf8(output).unwrap(), "patch");
    }

    #[test]
    fn test_plan_terminal_fullscreen_diff_patches_matches_cc_alt_screen_anchor_and_park() {
        let mut diff = vec![TerminalPatch::Stdout("body".to_string())];
        let plan = plan_terminal_fullscreen_diff_patches(TerminalFullscreenDiffPatchOptions {
            fullscreen: true,
            has_diff: !diff.is_empty(),
            erase_before_paint: false,
            terminal_rows: Some(24),
        });

        assert_eq!(
            plan.pre_diff_patch,
            Some(TerminalPatch::Stdout("\x1b[H".to_string()))
        );
        assert_eq!(
            plan.post_diff_patch,
            Some(TerminalPatch::Stdout("\x1b[24;1H".to_string()))
        );
        plan.apply_to(&mut diff);
        assert_eq!(
            terminal_patches_to_ansi(&diff, true),
            "\x1b[Hbody\x1b[24;1H"
        );

        let erase = plan_terminal_fullscreen_diff_patches(TerminalFullscreenDiffPatchOptions {
            fullscreen: true,
            has_diff: true,
            erase_before_paint: true,
            terminal_rows: Some(10),
        });
        assert_eq!(
            erase.pre_diff_patch,
            Some(TerminalPatch::Stdout("\x1b[2J\x1b[H".to_string()))
        );
        assert_eq!(
            erase.post_diff_patch,
            Some(TerminalPatch::Stdout("\x1b[10;1H".to_string()))
        );

        assert!(
            plan_terminal_fullscreen_diff_patches(TerminalFullscreenDiffPatchOptions {
                fullscreen: false,
                has_diff: true,
                erase_before_paint: true,
                terminal_rows: Some(24),
            })
            .is_empty()
        );
        assert!(
            plan_terminal_fullscreen_diff_patches(TerminalFullscreenDiffPatchOptions {
                fullscreen: true,
                has_diff: false,
                erase_before_paint: true,
                terminal_rows: Some(24),
            })
            .is_empty()
        );

        let no_size = plan_terminal_fullscreen_diff_patches(TerminalFullscreenDiffPatchOptions {
            fullscreen: true,
            has_diff: true,
            erase_before_paint: false,
            terminal_rows: None,
        });
        assert_eq!(
            no_size.pre_diff_patch,
            Some(TerminalPatch::Stdout("\x1b[H".to_string()))
        );
        assert_eq!(no_size.post_diff_patch, None);
    }

    #[test]
    fn test_terminal_fullscreen_canvas_diff_patches_use_absolute_rows_and_sparse_prefixes() {
        let mut prev = Canvas::new(6, 2);
        prev.subview_mut(0, 0, 0, 0, 6, 2)
            .set_text(0, 0, "abcdef", CanvasTextStyle::default());
        prev.subview_mut(0, 0, 0, 0, 6, 2)
            .set_text(0, 1, "stable", CanvasTextStyle::default());
        prev.clear_damage();

        let mut next = Canvas::new(6, 1);
        next.subview_mut(0, 0, 0, 0, 6, 1)
            .set_text(0, 0, "abXYef", CanvasTextStyle::default());
        next.clear_damage();

        let diff = terminal_fullscreen_canvas_diff_patches(
            Some(&prev),
            &next,
            TerminalFullscreenCanvasDiffOptions {
                top_row: 1,
                force_full_repaint: false,
            },
        );
        let ansi = terminal_patches_to_ansi(&diff, true);

        assert!(
            ansi.starts_with("\x1b[2;3HXYef"),
            "changed row should be addressed from first changed column: {ansi:?}"
        );
        assert!(
            ansi.contains("\x1b[3;1H\x1b[2K"),
            "removed rows should be cleared in-place: {ansi:?}"
        );
        assert!(
            !ansi.contains("stable"),
            "unchanged removed content must not be rewritten: {ansi:?}"
        );

        let full = terminal_fullscreen_canvas_diff_patches(
            None,
            &next,
            TerminalFullscreenCanvasDiffOptions {
                top_row: 2,
                force_full_repaint: false,
            },
        );
        let full_ansi = terminal_patches_to_ansi(&full, true);
        assert!(
            full_ansi.starts_with("\x1b[3;1HabXYef"),
            "first fullscreen write should address canvas origin absolutely: {full_ansi:?}"
        );
    }

    #[test]
    fn test_terminal_fullscreen_packed_canvas_diff_patches_use_sparse_packed_rows() {
        let mut pools = CanvasPackedCellPools::new();
        let mut line_cache = CanvasPackedLineCache::new();
        let style = CanvasResolvedStyle::default();
        let mut prev = CanvasPackedScreen::new(6, 2);
        prev.write_line_with_cache(&mut pools, &mut line_cache, 0, 0, "abcdef", style, None);
        prev.write_line_with_cache(&mut pools, &mut line_cache, 0, 1, "stable", style, None);
        prev.clear_damage();

        let mut next = CanvasPackedScreen::new(6, 1);
        next.write_line_with_cache(&mut pools, &mut line_cache, 0, 0, "abXYef", style, None);
        next.clear_damage();

        let mut style_cache = CanvasStyleTransitionCache::new();
        let diff = terminal_fullscreen_packed_canvas_diff_patches(
            Some(&prev),
            &next,
            &pools,
            &mut style_cache,
            TerminalFullscreenCanvasDiffOptions {
                top_row: 1,
                force_full_repaint: false,
            },
        );
        let ansi = terminal_patches_to_ansi(&diff, true);

        assert!(
            ansi.starts_with("\x1b[2;3HXYef"),
            "packed changed row should be addressed from first changed column: {ansi:?}"
        );
        assert!(
            ansi.contains("\x1b[3;1H\x1b[2K"),
            "packed removed rows should be cleared in-place: {ansi:?}"
        );
        assert!(
            !ansi.contains("stable"),
            "unchanged removed packed content must not be rewritten: {ansi:?}"
        );

        let full = terminal_fullscreen_packed_canvas_diff_patches(
            None,
            &next,
            &pools,
            &mut style_cache,
            TerminalFullscreenCanvasDiffOptions {
                top_row: 2,
                force_full_repaint: false,
            },
        );
        let full_ansi = terminal_patches_to_ansi(&full, true);
        assert!(
            full_ansi.starts_with("\x1b[3;1HabXYef"),
            "first packed fullscreen write should address origin absolutely: {full_ansi:?}"
        );
    }

    #[test]
    fn test_plan_terminal_fullscreen_packed_canvas_frame_patches_orders_scroll_diff_and_cursor() {
        let mut pools = CanvasPackedCellPools::new();
        let mut line_cache = CanvasPackedLineCache::new();
        let style = CanvasResolvedStyle::default();
        let mut prev = CanvasPackedScreen::new(6, 2);
        prev.write_line_with_cache(&mut pools, &mut line_cache, 0, 0, "old", style, None);
        prev.clear_damage();

        let mut next = CanvasPackedScreen::new(6, 2);
        next.write_line_with_cache(&mut pools, &mut line_cache, 0, 0, "new", style, None);
        next.clear_damage();

        let mut style_cache = CanvasStyleTransitionCache::new();
        let plan = plan_terminal_fullscreen_packed_canvas_frame_patches(
            Some(&prev),
            &next,
            &pools,
            &mut style_cache,
            TerminalFullscreenCanvasFramePatchOptions {
                canvas_diff: TerminalFullscreenCanvasDiffOptions {
                    top_row: 0,
                    force_full_repaint: false,
                },
                scroll_patch_ansi: Some("\x1b[1;2r\x1b[1S\x1b[r\x1b[H".to_string()),
                erase_before_paint: false,
                terminal_rows: Some(24),
                optimize: true,
            },
        );
        let ansi = terminal_patches_to_ansi(&plan.patches, true);

        assert!(plan.had_scroll_patch);
        assert_eq!(plan.content_patch_count, 3);
        assert!(
            ansi.starts_with("\x1b[H\x1b[1;2r\x1b[1S\x1b[r\x1b[H\x1b[1;1Hnew"),
            "cursor anchor must precede DECSTBM and packed row repairs: {ansi:?}"
        );
        assert!(
            ansi.ends_with("\x1b[24;1H"),
            "packed fullscreen patch plan should park the cursor after content diff: {ansi:?}"
        );
    }

    #[test]
    fn test_plan_terminal_fullscreen_canvas_frame_patches_orders_scroll_diff_and_cursor_like_cc() {
        let mut prev = Canvas::new(6, 2);
        prev.subview_mut(0, 0, 0, 0, 6, 2)
            .set_text(0, 0, "old", CanvasTextStyle::default());
        prev.clear_damage();

        let mut next = Canvas::new(6, 2);
        next.subview_mut(0, 0, 0, 0, 6, 2)
            .set_text(0, 0, "new", CanvasTextStyle::default());
        next.clear_damage();

        let plan = plan_terminal_fullscreen_canvas_frame_patches(
            Some(&prev),
            &next,
            TerminalFullscreenCanvasFramePatchOptions {
                canvas_diff: TerminalFullscreenCanvasDiffOptions {
                    top_row: 0,
                    force_full_repaint: false,
                },
                scroll_patch_ansi: Some("\x1b[1;2r\x1b[1S\x1b[r\x1b[H".to_string()),
                erase_before_paint: false,
                terminal_rows: Some(24),
                optimize: true,
            },
        );
        let ansi = terminal_patches_to_ansi(&plan.patches, true);

        assert!(plan.had_scroll_patch);
        assert_eq!(plan.content_patch_count, 3);
        assert!(
            ansi.starts_with("\x1b[H\x1b[1;2r\x1b[1S\x1b[r\x1b[H\x1b[1;1Hnew"),
            "cursor anchor must precede DECSTBM and row repairs: {ansi:?}"
        );
        assert!(
            ansi.ends_with("\x1b[24;1H"),
            "fullscreen patch plan should park the cursor after content diff: {ansi:?}"
        );

        let empty = plan_terminal_fullscreen_canvas_frame_patches(
            Some(&next),
            &next,
            TerminalFullscreenCanvasFramePatchOptions::default(),
        );
        assert!(empty.is_empty());
        assert_eq!(empty.content_patch_count, 0);
    }

    #[test]
    fn test_terminal_fullscreen_canvas_frame_state_tracks_previous_and_scroll_hint() {
        let mut state = TerminalFullscreenCanvasFrameState::new();
        let mut first = Canvas::new(6, 3);
        first
            .subview_mut(0, 0, 0, 0, 6, 3)
            .set_text(0, 0, "aaa", CanvasTextStyle::default());
        first
            .subview_mut(0, 0, 0, 0, 6, 3)
            .set_text(0, 1, "bbb", CanvasTextStyle::default());
        first
            .subview_mut(0, 0, 0, 0, 6, 3)
            .set_text(0, 2, "ccc", CanvasTextStyle::default());
        first.clear_damage();

        let initial = state.plan_frame(
            &first,
            TerminalFullscreenCanvasFramePatchOptions {
                terminal_rows: Some(24),
                ..TerminalFullscreenCanvasFramePatchOptions::default()
            },
        );
        assert!(!initial.is_empty());
        assert_eq!(state.previous().unwrap().get_text(0, 0, 6, 1), "aaa");

        let identical = state.plan_frame(
            &first,
            TerminalFullscreenCanvasFramePatchOptions {
                terminal_rows: Some(24),
                ..TerminalFullscreenCanvasFramePatchOptions::default()
            },
        );
        assert!(
            identical.is_empty(),
            "stateful identical frame should be a no-op"
        );

        let mut next = Canvas::new(6, 3);
        next.subview_mut(0, 0, 0, 0, 6, 3)
            .set_text(0, 0, "bbb", CanvasTextStyle::default());
        next.subview_mut(0, 0, 0, 0, 6, 3)
            .set_text(0, 1, "ccc", CanvasTextStyle::default());
        next.subview_mut(0, 0, 0, 0, 6, 3)
            .set_text(0, 2, "ddd", CanvasTextStyle::default());
        next.clear_damage();

        let scroll = state
            .plan_scroll_frame(
                &next,
                Some(crate::canvas::ScrollHint {
                    top: 0,
                    bottom: 2,
                    delta: 1,
                }),
                TerminalScrollHintPatchOptions::fullscreen_synchronized(),
                TerminalFullscreenCanvasFramePatchOptions {
                    terminal_rows: Some(24),
                    ..TerminalFullscreenCanvasFramePatchOptions::default()
                },
            )
            .unwrap();
        let ansi = terminal_patches_to_ansi(&scroll.frame.patches, true);
        assert!(scroll.had_scroll_patch());
        assert!(
            ansi.contains("ddd"),
            "incoming edge row should be repainted: {ansi:?}"
        );
        assert_eq!(state.previous().unwrap().get_text(0, 2, 6, 1), "ddd");

        state.reset();
        let first_again = state
            .plan_scroll_frame(
                &next,
                Some(crate::canvas::ScrollHint {
                    top: 0,
                    bottom: 2,
                    delta: 1,
                }),
                TerminalScrollHintPatchOptions::fullscreen_synchronized(),
                TerminalFullscreenCanvasFramePatchOptions::default(),
            )
            .unwrap();
        assert_eq!(first_again.scroll_hint_plan, None);
        assert!(!first_again.had_scroll_patch());
    }

    #[test]
    fn test_terminal_fullscreen_packed_canvas_frame_state_tracks_previous_and_scroll_hint() {
        let mut pools = CanvasPackedCellPools::new();
        let mut line_cache = CanvasPackedLineCache::new();
        let style = CanvasResolvedStyle::default();
        let mut state = TerminalFullscreenPackedCanvasFrameState::new();
        let mut first = CanvasPackedScreen::new(6, 3);
        first.write_line_with_cache(&mut pools, &mut line_cache, 0, 0, "aaa", style, None);
        first.write_line_with_cache(&mut pools, &mut line_cache, 0, 1, "bbb", style, None);
        first.write_line_with_cache(&mut pools, &mut line_cache, 0, 2, "ccc", style, None);
        first.clear_damage();

        let initial = state.plan_frame(
            &first,
            &pools,
            TerminalFullscreenCanvasFramePatchOptions {
                terminal_rows: Some(24),
                ..TerminalFullscreenCanvasFramePatchOptions::default()
            },
        );
        assert!(!initial.is_empty());
        assert_eq!(
            state.previous().unwrap().char_in_cell(&pools, 0, 0),
            Some("a")
        );

        let identical = state.plan_frame(
            &first,
            &pools,
            TerminalFullscreenCanvasFramePatchOptions {
                terminal_rows: Some(24),
                ..TerminalFullscreenCanvasFramePatchOptions::default()
            },
        );
        assert!(identical.is_empty());

        let mut next = CanvasPackedScreen::new(6, 3);
        next.write_line_with_cache(&mut pools, &mut line_cache, 0, 0, "bbb", style, None);
        next.write_line_with_cache(&mut pools, &mut line_cache, 0, 1, "ccc", style, None);
        next.write_line_with_cache(&mut pools, &mut line_cache, 0, 2, "ddd", style, None);
        next.clear_damage();

        let scroll = state
            .plan_scroll_frame(
                &next,
                &pools,
                Some(crate::canvas::ScrollHint {
                    top: 0,
                    bottom: 2,
                    delta: 1,
                }),
                TerminalScrollHintPatchOptions::fullscreen_synchronized(),
                TerminalFullscreenCanvasFramePatchOptions {
                    terminal_rows: Some(24),
                    ..TerminalFullscreenCanvasFramePatchOptions::default()
                },
            )
            .unwrap();
        let ansi = terminal_patches_to_ansi(&scroll.frame.patches, true);
        assert!(scroll.had_scroll_patch());
        assert!(
            ansi.contains("ddd"),
            "packed incoming edge should repaint: {ansi:?}"
        );
        assert_eq!(
            state.previous().unwrap().char_in_cell(&pools, 0, 2),
            Some("d")
        );
    }

    #[test]
    fn test_plan_terminal_fullscreen_canvas_scroll_frame_patches_shifts_previous_like_cc_decstbm() {
        let mut prev = Canvas::new(6, 3);
        prev.subview_mut(0, 0, 0, 0, 6, 3)
            .set_text(0, 0, "aaa", CanvasTextStyle::default());
        prev.subview_mut(0, 0, 0, 0, 6, 3)
            .set_text(0, 1, "bbb", CanvasTextStyle::default());
        prev.subview_mut(0, 0, 0, 0, 6, 3)
            .set_text(0, 2, "ccc", CanvasTextStyle::default());
        prev.clear_damage();

        let mut next = Canvas::new(6, 3);
        next.subview_mut(0, 0, 0, 0, 6, 3)
            .set_text(0, 0, "bbb", CanvasTextStyle::default());
        next.subview_mut(0, 0, 0, 0, 6, 3)
            .set_text(0, 1, "ccc", CanvasTextStyle::default());
        next.subview_mut(0, 0, 0, 0, 6, 3)
            .set_text(0, 2, "ddd", CanvasTextStyle::default());
        next.clear_damage();

        let hint = crate::canvas::ScrollHint {
            top: 0,
            bottom: 2,
            delta: 1,
        };
        let plan = plan_terminal_fullscreen_canvas_scroll_frame_patches(
            &prev,
            &next,
            Some(hint),
            TerminalScrollHintPatchOptions::fullscreen_synchronized(),
            TerminalFullscreenCanvasFramePatchOptions {
                canvas_diff: TerminalFullscreenCanvasDiffOptions {
                    top_row: 0,
                    force_full_repaint: false,
                },
                scroll_patch_ansi: None,
                erase_before_paint: false,
                terminal_rows: Some(24),
                optimize: true,
            },
        )
        .unwrap();
        let ansi = terminal_patches_to_ansi(&plan.frame.patches, true);

        assert_eq!(
            plan.scroll_hint_plan,
            Some(TerminalScrollHintPatchPlan::Emit(
                "\x1b[1;3r\x1b[1S\x1b[r\x1b[H".to_string()
            ))
        );
        assert!(plan.had_scroll_patch());
        assert_eq!(plan.frame.content_patch_count, 3);
        assert!(
            ansi.starts_with("\x1b[H\x1b[1;3r\x1b[1S\x1b[r\x1b[H\x1b[3;1Hddd"),
            "scroll frame should shift previous baseline and repaint only incoming edge rows: {ansi:?}"
        );
        assert!(
            !ansi.contains("\x1b[1;1Hbbb") && !ansi.contains("\x1b[2;1Hccc"),
            "shifted baseline should avoid rewriting stable scrolled rows: {ansi:?}"
        );

        let skipped = plan_terminal_fullscreen_canvas_scroll_frame_patches(
            &prev,
            &next,
            Some(hint),
            TerminalScrollHintPatchOptions {
                fullscreen: false,
                synchronized_output: true,
            },
            TerminalFullscreenCanvasFramePatchOptions {
                scroll_patch_ansi: Some("unsafe-scroll-patch".to_string()),
                ..TerminalFullscreenCanvasFramePatchOptions::default()
            },
        )
        .unwrap();
        assert_eq!(
            skipped.scroll_hint_plan,
            Some(TerminalScrollHintPatchPlan::Skip(
                TerminalScrollHintPatchSkipReason::NotFullscreen
            ))
        );
        assert!(!skipped.had_scroll_patch());
        assert!(
            !terminal_patches_to_ansi(&skipped.frame.patches, true).contains("unsafe-scroll-patch"),
            "safety-gated helper must drop caller-provided scroll prefixes when a supplied hint is skipped"
        );
    }

    #[test]
    fn test_plan_terminal_fullscreen_packed_canvas_scroll_frame_patches_shifts_previous_like_cc_decstbm(
    ) {
        let mut pools = CanvasPackedCellPools::new();
        let mut line_cache = CanvasPackedLineCache::new();
        let style = CanvasResolvedStyle::default();
        let mut prev = CanvasPackedScreen::new(6, 3);
        prev.write_line_with_cache(&mut pools, &mut line_cache, 0, 0, "aaa", style, None);
        prev.write_line_with_cache(&mut pools, &mut line_cache, 0, 1, "bbb", style, None);
        prev.write_line_with_cache(&mut pools, &mut line_cache, 0, 2, "ccc", style, None);
        prev.clear_damage();

        let mut next = CanvasPackedScreen::new(6, 3);
        next.write_line_with_cache(&mut pools, &mut line_cache, 0, 0, "bbb", style, None);
        next.write_line_with_cache(&mut pools, &mut line_cache, 0, 1, "ccc", style, None);
        next.write_line_with_cache(&mut pools, &mut line_cache, 0, 2, "ddd", style, None);
        next.clear_damage();

        let hint = crate::canvas::ScrollHint {
            top: 0,
            bottom: 2,
            delta: 1,
        };
        let mut style_cache = CanvasStyleTransitionCache::new();
        let plan = plan_terminal_fullscreen_packed_canvas_scroll_frame_patches(
            &prev,
            &next,
            &pools,
            &mut style_cache,
            Some(hint),
            TerminalScrollHintPatchOptions::fullscreen_synchronized(),
            TerminalFullscreenCanvasFramePatchOptions {
                canvas_diff: TerminalFullscreenCanvasDiffOptions {
                    top_row: 0,
                    force_full_repaint: false,
                },
                scroll_patch_ansi: None,
                erase_before_paint: false,
                terminal_rows: Some(24),
                optimize: true,
            },
        )
        .unwrap();
        let ansi = terminal_patches_to_ansi(&plan.frame.patches, true);

        assert!(matches!(
            plan.scroll_hint_plan,
            Some(TerminalScrollHintPatchPlan::Emit(ref patch))
                if patch == "\x1b[1;3r\x1b[1S\x1b[r\x1b[H"
        ));
        assert!(plan.had_scroll_patch());
        assert_eq!(plan.frame.content_patch_count, 3);
        assert!(
            ansi.starts_with("\x1b[H\x1b[1;3r\x1b[1S\x1b[r\x1b[H\x1b[3;1Hddd"),
            "packed scroll frame should shift previous baseline before row diff: {ansi:?}"
        );
        assert!(
            !ansi.contains("\x1b[1;1Hbbb") && !ansi.contains("\x1b[2;1Hccc"),
            "packed shifted baseline should avoid rewriting stable scrolled rows: {ansi:?}"
        );
    }

    /// Changing keyboard enhancement flags while enhancement is active must swap the
    /// flags in place (pop + push); while inactive it must only update the stored value.
    #[test]
    fn test_set_keyboard_enhancement_flags() {
        let (dest, buf) = new_test_writer();
        let mut term = new_inline_term(dest, 0);

        // Inactive: no escape output, but the new flags are stored.
        let flags = event::KeyboardEnhancementFlags::REPORT_EVENT_TYPES
            | event::KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES;
        term.set_keyboard_enhancement_flags(flags).unwrap();
        assert_eq!(term.keyboard_enhancement_flags, flags);
        assert!(buf.lock().unwrap().is_empty(), "no output while inactive");

        // Active: swapping flags emits pop + push.
        term.enabled_keyboard_enhancement = true;
        let new_flags = event::KeyboardEnhancementFlags::REPORT_EVENT_TYPES;
        term.set_keyboard_enhancement_flags(new_flags).unwrap();
        let output = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
        assert!(
            output.contains("\x1b[<1u"),
            "expected pop sequence: {output:?}"
        );
        assert!(
            output.contains("\x1b[>2u"),
            "expected push of REPORT_EVENT_TYPES: {output:?}"
        );

        // Setting identical flags is a no-op.
        buf.lock().unwrap().clear();
        term.set_keyboard_enhancement_flags(new_flags).unwrap();
        assert!(buf.lock().unwrap().is_empty(), "same flags must be a no-op");
    }

    #[test]
    fn test_keyboard_enhancement_reassertion_pops_before_push() {
        let (dest, buf) = new_test_writer();
        let mut term = new_inline_term(dest, 0);
        term.enabled_keyboard_enhancement = true;
        term.keyboard_enhancement_flags = event::KeyboardEnhancementFlags::REPORT_EVENT_TYPES;

        term.reassert_keyboard_enhancement().unwrap();

        let output = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
        let pop = output
            .find("\x1b[<1u")
            .unwrap_or_else(|| panic!("expected pop sequence before reassert push: {output:?}"));
        let push = output
            .find("\x1b[>2u")
            .unwrap_or_else(|| panic!("expected push sequence after reassert pop: {output:?}"));
        assert!(
            pop < push,
            "keyboard reassertion should pop before push to keep Kitty stack depth balanced: {output:?}"
        );
    }

    #[test]
    fn test_stdin_resume_reasserts_keyboard_and_mouse_without_focus_clear() {
        let (dest, buf) = new_test_writer();
        let mut term = new_fullscreen_term(dest, 0, 0);
        term.raw_mode_enabled = true;
        term.enabled_keyboard_enhancement = true;
        term.mouse_capture = true;

        term.reassert_after_stdin_resume().unwrap();

        let output = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
        let pop = output.find("\x1b[<1u").unwrap_or_else(|| {
            panic!("expected keyboard pop before stdin-gap reassert: {output:?}")
        });
        let push = output.find("\x1b[>2u").unwrap_or_else(|| {
            panic!("expected keyboard push after stdin-gap reassert: {output:?}")
        });
        assert!(
            pop < push,
            "stdin-gap reassert should keep Kitty stack depth balanced: {output:?}"
        );
        assert!(
            output.contains("\x1b[?1006h"),
            "stdin-gap reassert should restore mouse tracking: {output:?}"
        );
        assert!(
            !output.contains("\x1b[?1004h"),
            "stdin-gap reassert mirrors CC Ink and avoids extra focus-reporting writes: {output:?}"
        );
    }

    #[test]
    fn test_resize_reasserts_mouse_capture_when_raw_mode_enabled() {
        let (dest, buf) = new_test_writer();
        let mut term = new_fullscreen_term(dest, 0, 0);
        term.raw_mode_enabled = true;
        term.mouse_capture = true;

        term.reassert_after_resize().unwrap();
        let output = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
        assert!(
            output.contains("\x1b[?1004h"),
            "resize self-heal should re-enable focus reporting: {output:?}"
        );
        assert!(
            output.contains("\x1b[?1006h"),
            "resize self-heal should re-enable SGR mouse tracking: {output:?}"
        );

        buf.lock().unwrap().clear();
        term.mouse_capture = false;
        term.reassert_after_resize().unwrap();
        let output = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
        assert!(
            output.contains("\x1b[?1004h"),
            "resize self-heal should still re-enable focus reporting: {output:?}"
        );
        assert!(
            !output.contains("\x1b[?1006h"),
            "resize self-heal should not emit mouse tracking when capture is disabled: {output:?}"
        );
    }

    #[test]
    fn test_resize_event_triggers_terminal_mode_reassertion() {
        let reasserts = Arc::new(AtomicUsize::new(0));
        let inner = ResizeReassertTerminal {
            events: Some(stream::iter(vec![TerminalEvent::Resize(120, 40)]).boxed()),
            reasserts: reasserts.clone(),
            size: None,
            dest: io::sink(),
        };
        let mut term = Terminal {
            inner: Box::new(inner),
            output: Output::Stdout,
            base_fullscreen: false,
            event_stream: None,
            subscribers: Vec::new(),
            event_cell_snapshot: None,
            terminal_focus_state: None,
            last_stdin_event_at: Some(Instant::now()),
            query_queue: VecDeque::new(),
            pending_ctrl_c: Vec::new(),
            received_ctrl_c: false,
            ignore_ctrl_c: false,
            suspend_on_ctrl_z: false,
            synchronized_update_depth: 0,
            synchronized_update_started: false,
            synchronized_update_supported: false,
        };

        term.start_event_stream().unwrap();
        smol::block_on(term.wait());

        assert_eq!(term.size(), Some((120, 40)));
        assert_eq!(
            reasserts.load(Ordering::SeqCst),
            1,
            "resize events should re-assert terminal modes before returning to render"
        );
    }

    #[test]
    fn test_stdin_gap_reasserts_terminal_modes_like_cc_ink() {
        let reasserts = Arc::new(AtomicUsize::new(0));
        let inner = ResizeReassertTerminal {
            events: Some(
                stream::iter(vec![TerminalEvent::Key(KeyEvent::new(
                    KeyEventKind::Press,
                    KeyCode::Char('a'),
                ))])
                .boxed(),
            ),
            reasserts: reasserts.clone(),
            size: None,
            dest: io::sink(),
        };
        let mut term = Terminal {
            inner: Box::new(inner),
            output: Output::Stdout,
            base_fullscreen: false,
            event_stream: None,
            subscribers: Vec::new(),
            event_cell_snapshot: None,
            terminal_focus_state: None,
            last_stdin_event_at: Some(Instant::now() - STDIN_RESUME_GAP - Duration::from_secs(1)),
            query_queue: VecDeque::new(),
            pending_ctrl_c: Vec::new(),
            received_ctrl_c: false,
            ignore_ctrl_c: false,
            suspend_on_ctrl_z: false,
            synchronized_update_depth: 0,
            synchronized_update_started: false,
            synchronized_update_supported: false,
        };

        term.start_event_stream().unwrap();
        smol::block_on(term.wait());

        assert_eq!(
            reasserts.load(Ordering::SeqCst),
            1,
            "stdin silence longer than CC Ink's resume gap should re-assert terminal modes"
        );
    }

    #[test]
    fn test_same_size_resize_event_is_ignored() {
        let reasserts = Arc::new(AtomicUsize::new(0));
        let inner = ResizeReassertTerminal {
            events: Some(
                stream::iter(vec![
                    TerminalEvent::Resize(80, 24),
                    TerminalEvent::Resize(120, 40),
                ])
                .boxed(),
            ),
            reasserts: reasserts.clone(),
            size: Some((80, 24)),
            dest: io::sink(),
        };
        let mut term = Terminal {
            inner: Box::new(inner),
            output: Output::Stdout,
            base_fullscreen: false,
            event_stream: None,
            subscribers: Vec::new(),
            event_cell_snapshot: None,
            terminal_focus_state: None,
            last_stdin_event_at: Some(Instant::now()),
            query_queue: VecDeque::new(),
            pending_ctrl_c: Vec::new(),
            received_ctrl_c: false,
            ignore_ctrl_c: false,
            suspend_on_ctrl_z: false,
            synchronized_update_depth: 0,
            synchronized_update_started: false,
            synchronized_update_supported: false,
        };

        term.start_event_stream().unwrap();
        smol::block_on(term.wait());

        assert_eq!(term.size(), Some((120, 40)));
        assert_eq!(
            reasserts.load(Ordering::SeqCst),
            1,
            "same-dimension resize events should not reassert modes or wake render"
        );
    }

    #[test]
    fn test_synchronized_update_is_lazy() {
        let (dest, buf) = new_test_writer();
        let mut term = Terminal::new(
            Box::new(dest),
            Box::new(io::sink()),
            Output::Stdout,
            false,
            false,
        )
        .unwrap();
        term.synchronized_update_supported = true;

        // StdTerminal::new hides the cursor; discard setup bytes.
        buf.lock().unwrap().clear();

        term.synchronized_update(|_| Ok(())).unwrap();
        assert!(
            buf.lock().unwrap().is_empty(),
            "empty synchronized updates must not emit BSU/ESU"
        );

        term.synchronized_update(|term| {
            term.write_all(b"hello")?;
            Ok(())
        })
        .unwrap();
        let output = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
        assert!(
            output.starts_with("\x1b[?2026h"),
            "first real write should begin synchronized update: {output:?}"
        );
        assert!(
            output.contains("hello"),
            "payload should be written: {output:?}"
        );
        assert!(
            output.ends_with("\x1b[?2026l"),
            "drop should end synchronized update after writes: {output:?}"
        );
    }

    #[test]
    fn test_synchronized_update_stays_lazy_for_noop_cursor_positioning() {
        let (dest, buf) = new_test_writer();
        let mut term = Terminal::new(
            Box::new(dest),
            Box::new(io::sink()),
            Output::Stdout,
            false,
            false,
        )
        .unwrap();
        term.synchronized_update_supported = true;

        // StdTerminal::new hides the cursor; discard setup bytes.
        buf.lock().unwrap().clear();

        term.synchronized_update(|term| term.position_cursor(None))
            .unwrap();
        assert!(
            buf.lock().unwrap().is_empty(),
            "no-op cursor positioning must preserve the empty synchronized-update fast path"
        );
    }

    #[test]
    fn test_synchronized_update_stays_lazy_for_noop_clear_canvas() {
        let (dest, buf) = new_test_writer();
        let mut term = Terminal::new(
            Box::new(dest),
            Box::new(io::sink()),
            Output::Stdout,
            false,
            false,
        )
        .unwrap();
        term.synchronized_update_supported = true;

        // StdTerminal::new hides the cursor; discard setup bytes.
        buf.lock().unwrap().clear();

        term.synchronized_update(|term| term.clear_canvas())
            .unwrap();
        assert!(
            buf.lock().unwrap().is_empty(),
            "clear_canvas with no retained output must preserve the empty synchronized-update fast path"
        );
    }

    #[test]
    fn test_synchronized_update_wraps_clear_canvas_when_it_writes() {
        let (dest, buf) = new_test_writer();
        let inner = new_inline_term(dest, 1);
        let mut term = Terminal {
            inner: Box::new(inner),
            output: Output::Stdout,
            base_fullscreen: false,
            event_stream: None,
            subscribers: Vec::new(),
            event_cell_snapshot: None,
            terminal_focus_state: None,
            last_stdin_event_at: Some(Instant::now()),
            query_queue: VecDeque::new(),
            pending_ctrl_c: Vec::new(),
            received_ctrl_c: false,
            ignore_ctrl_c: false,
            suspend_on_ctrl_z: false,
            synchronized_update_depth: 0,
            synchronized_update_started: false,
            synchronized_update_supported: true,
        };

        term.synchronized_update(|term| term.clear_canvas())
            .unwrap();
        let output = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
        assert!(
            output.starts_with("\x1b[?2026h"),
            "real clear_canvas should start synchronized update: {output:?}"
        );
        assert!(
            output.contains("\x1b[J"),
            "real clear_canvas payload should be present: {output:?}"
        );
        assert!(
            output.ends_with("\x1b[?2026l"),
            "real clear_canvas should end synchronized update: {output:?}"
        );
    }

    #[test]
    fn test_synchronized_update_stays_lazy_for_identical_canvas_write() {
        let mut canvas = Canvas::new(10, 1);
        canvas
            .subview_mut(0, 0, 0, 0, 10, 1)
            .set_text(0, 0, "hello", CanvasTextStyle::default());

        let (dest, buf) = new_test_writer();
        let mut inner = new_inline_term_with_size(dest, canvas.height() as _, (10, 5));
        inner.prev_size_on_write = inner.size;
        let mut term = Terminal {
            inner: Box::new(inner),
            output: Output::Stdout,
            base_fullscreen: false,
            event_stream: None,
            subscribers: Vec::new(),
            event_cell_snapshot: None,
            terminal_focus_state: None,
            last_stdin_event_at: Some(Instant::now()),
            query_queue: VecDeque::new(),
            pending_ctrl_c: Vec::new(),
            received_ctrl_c: false,
            ignore_ctrl_c: false,
            suspend_on_ctrl_z: false,
            synchronized_update_depth: 0,
            synchronized_update_started: false,
            synchronized_update_supported: true,
        };

        term.synchronized_update(|term| term.write_canvas(Some(&canvas), &canvas))
            .unwrap();
        assert!(
            buf.lock().unwrap().is_empty(),
            "identical canvas writes must preserve the empty synchronized-update fast path"
        );
    }

    #[test]
    fn test_synchronized_update_wraps_current_damage_canvas_write() {
        let mut prev = Canvas::new(10, 1);
        prev.subview_mut(0, 0, 0, 0, 10, 1)
            .set_text(0, 0, "hello", CanvasTextStyle::default());
        let mut next = prev.clone();
        next.mark_damage(crate::canvas::DamageRegion {
            x: 0,
            y: 0,
            width: 10,
            height: 1,
        });

        let (dest, buf) = new_test_writer();
        let mut inner = new_inline_term_with_size(dest, prev.height() as _, (10, 5));
        inner.prev_size_on_write = inner.size;
        let mut term = Terminal {
            inner: Box::new(inner),
            output: Output::Stdout,
            base_fullscreen: false,
            event_stream: None,
            subscribers: Vec::new(),
            event_cell_snapshot: None,
            terminal_focus_state: None,
            last_stdin_event_at: Some(Instant::now()),
            query_queue: VecDeque::new(),
            pending_ctrl_c: Vec::new(),
            received_ctrl_c: false,
            ignore_ctrl_c: false,
            suspend_on_ctrl_z: false,
            synchronized_update_depth: 0,
            synchronized_update_started: false,
            synchronized_update_supported: true,
        };

        term.synchronized_update(|term| term.write_canvas(Some(&prev), &next))
            .unwrap();

        let output = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
        assert!(
            output.starts_with("\x1b[?2026h"),
            "current damage should start synchronized update: {output:?}"
        );
        assert!(
            output.contains("hello"),
            "damaged identical row should be repainted: {output:?}"
        );
        assert!(
            output.ends_with("\x1b[?2026l"),
            "current damage should end synchronized update: {output:?}"
        );
    }

    #[test]
    fn test_synchronized_update_wraps_previous_damage_canvas_write() {
        let mut prev = Canvas::new(10, 1);
        prev.subview_mut(0, 0, 0, 0, 10, 1)
            .set_text(0, 0, "hello", CanvasTextStyle::default());
        prev.mark_damage(crate::canvas::DamageRegion {
            x: 0,
            y: 0,
            width: 10,
            height: 1,
        });
        let mut next = prev.clone();
        next.clear_damage();

        let (dest, buf) = new_test_writer();
        let mut inner = new_inline_term_with_size(dest, prev.height() as _, (10, 5));
        inner.prev_size_on_write = inner.size;
        let mut term = Terminal {
            inner: Box::new(inner),
            output: Output::Stdout,
            base_fullscreen: false,
            event_stream: None,
            subscribers: Vec::new(),
            event_cell_snapshot: None,
            terminal_focus_state: None,
            last_stdin_event_at: Some(Instant::now()),
            query_queue: VecDeque::new(),
            pending_ctrl_c: Vec::new(),
            received_ctrl_c: false,
            ignore_ctrl_c: false,
            suspend_on_ctrl_z: false,
            synchronized_update_depth: 0,
            synchronized_update_started: false,
            synchronized_update_supported: true,
        };

        term.synchronized_update(|term| term.write_canvas(Some(&prev), &next))
            .unwrap();

        let output = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
        assert!(
            output.starts_with("\x1b[?2026h"),
            "previous damage should start synchronized update: {output:?}"
        );
        assert!(
            output.contains("hello"),
            "previous damaged identical row should be repainted: {output:?}"
        );
        assert!(
            output.ends_with("\x1b[?2026l"),
            "previous damage should end synchronized update: {output:?}"
        );
    }

    #[test]
    fn test_synchronized_update_stays_lazy_for_identical_fullscreen_canvas_write() {
        let mut canvas = Canvas::new(10, 2);
        canvas
            .subview_mut(0, 0, 0, 0, 10, 2)
            .set_text(0, 0, "hello", CanvasTextStyle::default());
        canvas
            .subview_mut(0, 0, 0, 0, 10, 2)
            .set_text(0, 1, "world", CanvasTextStyle::default());

        let (dest, buf) = new_test_writer();
        let mut inner = new_fullscreen_term(dest, 0, canvas.height() as _);
        inner.size = Some((10, 2));
        inner.prev_size_on_write = Some((10, 2));
        let mut term = Terminal {
            inner: Box::new(inner),
            output: Output::Stdout,
            base_fullscreen: false,
            event_stream: None,
            subscribers: Vec::new(),
            event_cell_snapshot: None,
            terminal_focus_state: None,
            last_stdin_event_at: Some(Instant::now()),
            query_queue: VecDeque::new(),
            pending_ctrl_c: Vec::new(),
            received_ctrl_c: false,
            ignore_ctrl_c: false,
            suspend_on_ctrl_z: false,
            synchronized_update_depth: 0,
            synchronized_update_started: false,
            synchronized_update_supported: true,
        };

        term.synchronized_update(|term| term.write_canvas(Some(&canvas), &canvas))
            .unwrap();
        assert!(
            buf.lock().unwrap().is_empty(),
            "identical fullscreen canvas writes must preserve the empty synchronized-update fast path"
        );
    }

    #[test]
    fn test_synchronized_update_wraps_cursor_positioning_when_it_writes() {
        use crate::canvas::CursorDeclaration;
        let (dest, buf) = new_test_writer();
        let mut term = Terminal::new(
            Box::new(dest),
            Box::new(io::sink()),
            Output::Stdout,
            false,
            false,
        )
        .unwrap();
        term.synchronized_update_supported = true;

        // StdTerminal::new hides the cursor; discard setup bytes.
        buf.lock().unwrap().clear();

        term.synchronized_update(|term| {
            term.position_cursor(Some(CursorDeclaration {
                x: 1,
                y: 0,
                visible: false,
            }))
        })
        .unwrap();
        let output = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
        assert!(
            output.starts_with("\x1b[?2026h"),
            "cursor movement should start synchronized update: {output:?}"
        );
        assert!(
            output.contains("\x1b[2G"),
            "cursor movement payload should be present: {output:?}"
        );
        assert!(
            output.ends_with("\x1b[?2026l"),
            "cursor movement should end synchronized update: {output:?}"
        );
    }

    #[test]
    fn test_synchronized_update_wraps_pending_wrap_resolution() {
        let (dest, buf) = new_test_writer();
        let mut inner = new_inline_term(dest, 1);
        inner.inline_pending_wrap = true;
        let mut term = Terminal {
            inner: Box::new(inner),
            output: Output::Stdout,
            base_fullscreen: false,
            event_stream: None,
            subscribers: Vec::new(),
            event_cell_snapshot: None,
            terminal_focus_state: None,
            last_stdin_event_at: Some(Instant::now()),
            query_queue: VecDeque::new(),
            pending_ctrl_c: Vec::new(),
            received_ctrl_c: false,
            ignore_ctrl_c: false,
            suspend_on_ctrl_z: false,
            synchronized_update_depth: 0,
            synchronized_update_started: false,
            synchronized_update_supported: true,
        };

        term.synchronized_update(|term| term.position_cursor(None))
            .unwrap();

        let output = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
        assert!(
            output.starts_with("\x1b[?2026h"),
            "pending-wrap CR should start synchronized update: {output:?}"
        );
        assert!(
            output.contains('\r'),
            "pending-wrap resolution should emit CR: {output:?}"
        );
        assert!(
            output.ends_with("\x1b[?2026l"),
            "pending-wrap CR should end synchronized update: {output:?}"
        );
    }

    #[test]
    fn test_synchronized_update_skips_markers_when_unsupported() {
        let (dest, buf) = new_test_writer();
        let mut term = Terminal::new(
            Box::new(dest),
            Box::new(io::sink()),
            Output::Stdout,
            false,
            false,
        )
        .unwrap();
        term.synchronized_update_supported = false;

        // StdTerminal::new hides the cursor; discard setup bytes.
        buf.lock().unwrap().clear();

        term.synchronized_update(|term| {
            term.write_all(b"hello")?;
            Ok(())
        })
        .unwrap();
        let output = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
        assert_eq!(
            output, "hello",
            "unsupported terminals should receive payload without BSU/ESU"
        );
    }

    /// The panic-restore registry must track live terminals and reset the fullscreen
    /// count as fullscreen terminals are dropped, so the hook does not leave the
    /// alternate screen when only inline terminals remain alive.
    #[test]
    fn test_panic_restore_registration_counts() {
        // Snapshot the baseline: other tests may have live terminals.
        let (baseline_live, baseline_fullscreen) = {
            let state = PANIC_RESTORE_STATE
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            (state.live_terminals, state.fullscreen_terminals)
        };

        register_terminal_for_panic_restore(true);
        register_terminal_for_panic_restore(false);
        {
            let state = PANIC_RESTORE_STATE
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            assert_eq!(state.live_terminals, baseline_live + 2);
            assert_eq!(state.fullscreen_terminals, baseline_fullscreen + 1);
        }

        unregister_terminal_for_panic_restore(true);
        {
            let state = PANIC_RESTORE_STATE
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            assert_eq!(state.live_terminals, baseline_live + 1);
            assert_eq!(state.fullscreen_terminals, baseline_fullscreen);
        }

        unregister_terminal_for_panic_restore(false);
        if baseline_live == 0 {
            let state = PANIC_RESTORE_STATE
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            assert_eq!(state.live_terminals, 0);
            assert_eq!(state.fullscreen_terminals, 0);
        }
    }

    /// After a resume from suspension, the terminal must forget everything it knew
    /// about previously written output, so the next write_canvas behaves like a
    /// first write (full render at the current cursor position) instead of a row
    /// diff against content that the shell has since overwritten.
    #[test]
    fn test_fullscreen_terminal_enters_alt_screen_clears_and_homes() {
        let (dest, buf) = new_test_writer();
        let _term = Terminal::new(
            Box::new(dest),
            Box::new(io::sink()),
            Output::Stdout,
            true,
            false,
        )
        .unwrap();

        let output = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
        assert!(
            output.contains("\x1b[?1049h"),
            "fullscreen startup should enter alternate screen: {output:?}"
        );
        assert!(
            output.contains("\x1b[2J"),
            "fullscreen startup should clear stale alternate-screen cells: {output:?}"
        );
        assert!(
            output.contains("\x1b[1;1H"),
            "fullscreen startup should home the first frame anchor: {output:?}"
        );
    }

    #[test]
    fn test_dynamic_alternate_screen_enters_clears_homes_and_exits() {
        let (dest, buf) = new_test_writer();
        let mut term = Terminal::new(
            Box::new(dest),
            Box::new(io::sink()),
            Output::Stdout,
            false,
            false,
        )
        .unwrap();

        buf.lock().unwrap().clear();
        let changed = term
            .set_dynamic_alternate_screen(Some(crate::context::AlternateScreenRequest {
                mouse_tracking: false,
            }))
            .unwrap();
        assert!(changed);
        assert!(term.is_fullscreen());
        let output = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
        assert!(
            output.contains("\x1b[?1049h"),
            "dynamic AlternateScreen should enter alt screen: {output:?}"
        );
        assert!(
            output.contains("\x1b[2J"),
            "dynamic AlternateScreen should clear stale alt-screen cells: {output:?}"
        );
        assert!(
            output.contains("\x1b[1;1H"),
            "dynamic AlternateScreen should home the first frame anchor: {output:?}"
        );

        buf.lock().unwrap().clear();
        let changed = term.set_dynamic_alternate_screen(None).unwrap();
        assert!(changed);
        assert!(!term.is_fullscreen());
        let output = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
        assert!(
            output.contains("\x1b[?1049l"),
            "unmounting AlternateScreen should leave alt screen: {output:?}"
        );
    }

    #[test]
    fn test_reinitialize_after_resume_resets_output_state() {
        let (dest, _buf) = new_test_writer();
        let mut term = new_inline_term(dest, 5);
        term.prev_size_on_write = Some((10, 10));
        term.prev_canvas_top_row = 3;

        term.reinitialize_after_resume().unwrap();

        assert_eq!(term.prev_canvas_height, 0);
        assert_eq!(term.prev_canvas_top_row, 0);
        assert_eq!(term.prev_size_on_write, None);
    }

    #[test]
    fn test_fullscreen_reinitialize_after_resume_reenters_clears_and_homes() {
        let (dest, buf) = new_test_writer();
        let mut term = new_fullscreen_term(dest, 7, 4);
        term.prev_size_on_write = Some((80, 24));
        term.inline_pending_wrap = true;
        term.mouse_capture = true;

        term.reinitialize_after_resume().unwrap();

        let output = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
        assert!(
            output.contains("\x1b[?1049h"),
            "fullscreen resume should re-enter alternate screen: {output:?}"
        );
        assert!(
            output.contains("\x1b[2J"),
            "fullscreen resume should erase stale alternate-screen cells: {output:?}"
        );
        assert!(
            output.contains("\x1b[1;1H"),
            "fullscreen resume should home the cursor before repaint: {output:?}"
        );
        let home = output
            .find("\x1b[1;1H")
            .unwrap_or_else(|| panic!("expected cursor home: {output:?}"));
        let mouse = output
            .rfind("\x1b[?1006h")
            .unwrap_or_else(|| panic!("expected SGR mouse tracking reassertion: {output:?}"));
        assert!(
            mouse > home,
            "fullscreen resume should re-enable mouse tracking after alt-screen re-entry/home: {output:?}"
        );
        assert_eq!(term.prev_canvas_height, 0);
        assert_eq!(term.prev_canvas_top_row, 0);
        assert_eq!(term.prev_size_on_write, None);
        assert!(!term.inline_pending_wrap);
    }

    #[test]
    fn test_inline_diff_unchanged_row_skipped() {
        let mut prev = Canvas::new(10, 2);
        prev.subview_mut(0, 0, 0, 0, 10, 2)
            .set_text(0, 0, "first", CanvasTextStyle::default());
        prev.subview_mut(0, 0, 0, 0, 10, 2)
            .set_text(0, 1, "second", CanvasTextStyle::default());

        let mut next = Canvas::new(10, 2);
        next.subview_mut(0, 0, 0, 0, 10, 2)
            .set_text(0, 0, "first", CanvasTextStyle::default());
        next.subview_mut(0, 0, 0, 0, 10, 2)
            .set_text(0, 1, "changed", CanvasTextStyle::default());

        let (dest, diff_buf) = new_test_writer();
        let mut term = new_inline_term(dest, prev.height() as _);
        term.write_canvas(Some(&prev), &next).unwrap();

        // Build vt: render prev, then apply diff output.
        let mut setup = Vec::new();
        prev.write_ansi_without_final_newline(&mut setup).unwrap();
        setup.extend_from_slice(&diff_buf.lock().unwrap());

        let mut vt = avt::Vt::new(10, 5);
        vt.feed_str(&String::from_utf8(setup).unwrap());

        assert_eq!(vt.line(0).text(), "first     ");
        assert_eq!(vt.line(1).text(), "changed   ");
        assert_eq!(vt.cursor().row, 1);
    }

    #[test]
    fn test_inline_diff_rewrites_from_first_changed_column() {
        let style = CanvasTextStyle::default();
        let mut prev = Canvas::new(12, 1);
        prev.subview_mut(0, 0, 0, 0, 12, 1)
            .set_text(0, 0, "prefix-old", style);
        let mut next = Canvas::new(12, 1);
        next.subview_mut(0, 0, 0, 0, 12, 1)
            .set_text(0, 0, "prefix-new", style);

        let (diff, vt) = inline_diff_vt(&prev, &next, (12, 5));
        let diff = String::from_utf8(diff).unwrap();
        assert!(
            diff.contains("\x1b[8Gnew"),
            "inline diff should jump to the first changed column: {diff:?}"
        );
        assert!(
            !diff.contains("prefix-"),
            "inline diff should not repaint the unchanged prefix: {diff:?}"
        );
        assert_eq!(vt.line(0).text(), "prefix-new  ");
    }

    #[test]
    fn test_inline_first_diff_after_initial_write_full_rewrites_once() {
        let style = CanvasTextStyle::default();
        let mut first = Canvas::new(10, 2);
        first
            .subview_mut(0, 0, 0, 0, 10, 2)
            .set_text(0, 0, "same", style);
        first
            .subview_mut(0, 0, 0, 0, 10, 2)
            .set_text(0, 1, "first", style);

        let mut second = Canvas::new(10, 2);
        second
            .subview_mut(0, 0, 0, 0, 10, 2)
            .set_text(0, 0, "same", style);
        second
            .subview_mut(0, 0, 0, 0, 10, 2)
            .set_text(0, 1, "second", style);

        let (dest, diff_buf) = new_test_writer();
        let mut term = new_inline_term_with_size(dest, 0, (10, 6));
        term.write_canvas(None, &first).unwrap();
        assert!(
            term.inline_force_full_rewrite_next_diff,
            "first inline frame should mark the next diff as contaminated"
        );

        diff_buf.lock().unwrap().clear();
        term.write_canvas(Some(&first), &second).unwrap();
        let first_diff = String::from_utf8(diff_buf.lock().unwrap().clone()).unwrap();
        assert!(
            first_diff.contains("\x1b[J"),
            "contaminated first diff should clear the previous inline canvas: {first_diff:?}"
        );
        assert!(
            first_diff.contains("same") && first_diff.contains("second"),
            "contaminated first diff should rewrite the whole canvas, not just changed rows: {first_diff:?}"
        );
        assert!(
            !term.inline_force_full_rewrite_next_diff,
            "contaminated-frame guard should be one-shot"
        );

        let mut third = Canvas::new(10, 2);
        third
            .subview_mut(0, 0, 0, 0, 10, 2)
            .set_text(0, 0, "same", style);
        third
            .subview_mut(0, 0, 0, 0, 10, 2)
            .set_text(0, 1, "third", style);

        diff_buf.lock().unwrap().clear();
        term.write_canvas(Some(&second), &third).unwrap();
        let second_diff = String::from_utf8(diff_buf.lock().unwrap().clone()).unwrap();
        assert!(
            !second_diff.contains("\x1b[J"),
            "subsequent clean diff should return to sparse row updates: {second_diff:?}"
        );
        assert!(second_diff.contains("third"));
        assert!(
            !second_diff.contains("same"),
            "unchanged rows should be skipped after the one-shot contaminated rewrite: {second_diff:?}"
        );
    }

    #[test]
    fn test_inline_diff_shrinking() {
        let mut prev = Canvas::new(10, 3);
        prev.subview_mut(0, 0, 0, 0, 10, 3)
            .set_text(0, 0, "aaa", CanvasTextStyle::default());
        prev.subview_mut(0, 0, 0, 0, 10, 3)
            .set_text(0, 1, "bbb", CanvasTextStyle::default());
        prev.subview_mut(0, 0, 0, 0, 10, 3)
            .set_text(0, 2, "ccc", CanvasTextStyle::default());

        let mut next = Canvas::new(10, 2);
        next.subview_mut(0, 0, 0, 0, 10, 2)
            .set_text(0, 0, "aaa", CanvasTextStyle::default());
        next.subview_mut(0, 0, 0, 0, 10, 2)
            .set_text(0, 1, "ddd", CanvasTextStyle::default());

        let (dest, diff_buf) = new_test_writer();
        let mut term = new_inline_term(dest, prev.height() as _);
        term.write_canvas(Some(&prev), &next).unwrap();

        let mut setup = Vec::new();
        prev.write_ansi_without_final_newline(&mut setup).unwrap();
        setup.extend_from_slice(&diff_buf.lock().unwrap());

        let mut vt = avt::Vt::new(10, 5);
        vt.feed_str(&String::from_utf8(setup).unwrap());

        assert_eq!(vt.line(0).text(), "aaa       ");
        assert_eq!(vt.line(1).text(), "ddd       ");
        assert_eq!(
            vt.line(2).text(),
            "          ",
            "old row 2 should be cleared"
        );
        assert_eq!(vt.cursor().row, 1, "cursor on last row of new canvas");
    }

    #[test]
    fn test_inline_diff_growing() {
        let mut prev = Canvas::new(10, 2);
        prev.subview_mut(0, 0, 0, 0, 10, 2)
            .set_text(0, 0, "aaa", CanvasTextStyle::default());
        prev.subview_mut(0, 0, 0, 0, 10, 2)
            .set_text(0, 1, "bbb", CanvasTextStyle::default());

        let mut next = Canvas::new(10, 3);
        next.subview_mut(0, 0, 0, 0, 10, 3)
            .set_text(0, 0, "aaa", CanvasTextStyle::default());
        next.subview_mut(0, 0, 0, 0, 10, 3)
            .set_text(0, 1, "bbb", CanvasTextStyle::default());
        next.subview_mut(0, 0, 0, 0, 10, 3)
            .set_text(0, 2, "ccc", CanvasTextStyle::default());

        let (dest, diff_buf) = new_test_writer();
        let mut term = new_inline_term(dest, prev.height() as _);
        term.write_canvas(Some(&prev), &next).unwrap();

        let mut setup = Vec::new();
        prev.write_ansi_without_final_newline(&mut setup).unwrap();
        setup.extend_from_slice(&diff_buf.lock().unwrap());

        let mut vt = avt::Vt::new(10, 5);
        vt.feed_str(&String::from_utf8(setup).unwrap());

        assert_eq!(vt.line(0).text(), "aaa       ");
        assert_eq!(vt.line(1).text(), "bbb       ");
        assert_eq!(vt.line(2).text(), "ccc       ");
        assert_eq!(vt.cursor().row, 2, "cursor on last row of new canvas");
    }

    #[test]
    fn test_inline_diff_non_adjacent_rows_forward() {
        // Two non-adjacent rows change within the existing canvas. The diff
        // visits row 1 first (moving the cursor up from row 4), then row 3
        // (moving forward but still within the old canvas). This exercises the
        // Greater branch when y < prev_height.
        let style = CanvasTextStyle::default();

        let mut prev = Canvas::new(10, 5);
        for i in 0..5 {
            prev.subview_mut(0, 0, 0, 0, 10, 5)
                .set_text(0, i, &format!("row{i}"), style);
        }

        let mut next = Canvas::new(10, 5);
        for i in 0..5 {
            next.subview_mut(0, 0, 0, 0, 10, 5)
                .set_text(0, i, &format!("row{i}"), style);
        }
        // Use same-length replacements to avoid masking the bug with
        // trailing-cell issues in write_ansi_row_without_newline.
        next.subview_mut(0, 0, 0, 0, 10, 5)
            .set_text(0, 1, "AAA1", style);
        next.subview_mut(0, 0, 0, 0, 10, 5)
            .set_text(0, 3, "BBB3", style);

        let (_diff, vt) = inline_diff_vt(&prev, &next, (10, 10));

        assert_eq!(vt.line(0).text(), "row0      ");
        assert_eq!(vt.line(1).text(), "AAA1      ");
        assert_eq!(vt.line(2).text(), "row2      ");
        assert_eq!(vt.line(3).text(), "BBB3      ");
        assert_eq!(vt.line(4).text(), "row4      ");
    }

    #[test]
    fn test_inline_diff_growing_at_bottom_of_screen() {
        // Simulate the canvas being at the bottom of the terminal so that
        // growing from 1 row to 2 requires scrolling. MoveToNextLine (CSI E)
        // won't create new lines at the screen bottom — only \r\n will.
        let style = CanvasTextStyle::default();

        let mut prev = Canvas::new(10, 1);
        prev.subview_mut(0, 0, 0, 0, 10, 1)
            .set_text(0, 0, "hello", style);

        let mut next = Canvas::new(10, 2);
        next.subview_mut(0, 0, 0, 0, 10, 2)
            .set_text(0, 0, "hello", style);
        next.subview_mut(0, 0, 0, 0, 10, 2)
            .set_text(0, 1, "world", style);

        let (dest, diff_buf) = new_test_writer();
        let mut term = new_inline_term(dest, prev.height() as _);
        term.write_canvas(Some(&prev), &next).unwrap();

        // Fill the VT so the canvas starts on the last row, then apply the diff.
        let mut setup = Vec::new();
        let vt_rows = 5;
        for i in 0..vt_rows - 1 {
            write!(setup, "line{i}\r\n").unwrap();
        }
        prev.write_ansi_without_final_newline(&mut setup).unwrap();
        setup.extend_from_slice(&diff_buf.lock().unwrap());

        let mut vt = avt::Vt::new(10, vt_rows);
        vt.feed_str(&String::from_utf8(setup).unwrap());

        // The VT should have scrolled: line0 is gone, canvas occupies last 2 rows.
        assert_eq!(vt.line(vt_rows - 2).text(), "hello     ");
        assert_eq!(vt.line(vt_rows - 1).text(), "world     ");
        assert_eq!(
            vt.cursor().row,
            vt_rows - 1,
            "cursor on last row of new canvas"
        );
    }

    #[test]
    fn test_inline_diff_identical_canvas_is_noop() {
        let mut canvas = Canvas::new(10, 2);
        canvas
            .subview_mut(0, 0, 0, 0, 10, 2)
            .set_text(0, 0, "hello", CanvasTextStyle::default());
        canvas
            .subview_mut(0, 0, 0, 0, 10, 2)
            .set_text(0, 1, "world", CanvasTextStyle::default());

        let (dest, diff_buf) = new_test_writer();
        let mut term = new_inline_term(dest, canvas.height() as _);
        term.prev_size_on_write = term.size;
        term.write_canvas(Some(&canvas), &canvas).unwrap();

        assert!(
            diff_buf.lock().unwrap().is_empty(),
            "identical canvas should produce no output"
        );
    }

    #[test]
    fn test_inline_prev_damage_region_rewrites_identical_reachable_row() {
        let mut prev = Canvas::new(10, 3);
        for (y, text) in ["one", "two", "three"].iter().enumerate() {
            prev.subview_mut(0, 0, 0, 0, 10, 3).set_text(
                0,
                y as isize,
                text,
                CanvasTextStyle::default(),
            );
        }
        prev.mark_damage(crate::canvas::DamageRegion {
            x: 0,
            y: 1,
            width: 10,
            height: 1,
        });

        let next = prev.clone();
        let mut next_without_damage = next.clone();
        next_without_damage.clear_damage();

        let (dest, diff_buf) = new_test_writer();
        let mut term = new_inline_term_with_size(dest, prev.height() as _, (10, 5));
        term.prev_size_on_write = term.size;
        term.write_canvas(Some(&prev), &next_without_damage)
            .unwrap();

        let diff = String::from_utf8_lossy(&diff_buf.lock().unwrap().clone()).to_string();
        assert!(
            diff.contains("two"),
            "previous-frame damage should be unioned into the next diff: {diff:?}"
        );
        assert!(
            !diff.contains("one") && !diff.contains("three"),
            "undamaged rows should remain skipped: {diff:?}"
        );
    }

    #[test]
    fn test_inline_damage_region_rewrites_identical_reachable_row() {
        let mut prev = Canvas::new(10, 3);
        for (y, text) in ["one", "two", "three"].iter().enumerate() {
            prev.subview_mut(0, 0, 0, 0, 10, 3).set_text(
                0,
                y as isize,
                text,
                CanvasTextStyle::default(),
            );
        }

        let mut next = prev.clone();
        next.mark_damage(crate::canvas::DamageRegion {
            x: 0,
            y: 1,
            width: 10,
            height: 1,
        });

        let (dest, diff_buf) = new_test_writer();
        let mut term = new_inline_term_with_size(dest, prev.height() as _, (10, 5));
        term.prev_size_on_write = term.size;
        term.write_canvas(Some(&prev), &next).unwrap();

        let diff = String::from_utf8_lossy(&diff_buf.lock().unwrap().clone()).to_string();
        assert!(
            diff.contains("two"),
            "damaged identical row should be repainted: {diff:?}"
        );
        assert!(
            !diff.contains("one") && !diff.contains("three"),
            "undamaged identical rows should remain skipped: {diff:?}"
        );
    }

    #[test]
    fn test_inline_force_full_repaint_rewrites_identical_canvas() {
        let mut prev = Canvas::new(10, 2);
        prev.subview_mut(0, 0, 0, 0, 10, 2)
            .set_text(0, 0, "hello", CanvasTextStyle::default());
        prev.subview_mut(0, 0, 0, 0, 10, 2)
            .set_text(0, 1, "world", CanvasTextStyle::default());

        let mut next = prev.clone();
        next.force_full_repaint();

        let (dest, diff_buf) = new_test_writer();
        let mut term = new_inline_term_with_size(dest, prev.height() as _, (10, 5));
        term.prev_size_on_write = term.size;
        term.write_canvas(Some(&prev), &next).unwrap();

        let diff = String::from_utf8_lossy(&diff_buf.lock().unwrap().clone()).to_string();
        assert!(
            !diff.contains("\x1b[J"),
            "reachable forced repaint should rewrite in place without clearing: {diff:?}"
        );
        assert!(
            diff.contains("hello") && diff.contains("world"),
            "forced full repaint should rewrite unchanged rows: {diff:?}"
        );

        let mut setup = Vec::new();
        prev.write_ansi_without_final_newline(&mut setup).unwrap();
        setup.extend_from_slice(&diff_buf.lock().unwrap());
        let mut vt = avt::Vt::new(10, 5);
        vt.feed_str(&String::from_utf8(setup).unwrap());
        assert_eq!(vt.line(0).text(), "hello     ");
        assert_eq!(vt.line(1).text(), "world     ");
        assert_eq!(vt.cursor().row, 1);
    }

    #[test]
    fn test_inline_force_full_repaint_rewrites_reachable_rows_without_scrollback_clear() {
        let mut prev = Canvas::new(10, 5);
        for (y, text) in ["one", "two", "three", "four", "five"].iter().enumerate() {
            prev.subview_mut(0, 0, 0, 0, 10, 5).set_text(
                0,
                y as isize,
                text,
                CanvasTextStyle::default(),
            );
        }

        let mut next = prev.clone();
        next.force_full_repaint();

        let (dest, diff_buf) = new_test_writer();
        let mut term = new_inline_term_with_size(dest, prev.height() as _, (10, 5));
        term.prev_size_on_write = term.size;
        term.write_canvas(Some(&prev), &next).unwrap();

        let diff = String::from_utf8_lossy(&diff_buf.lock().unwrap().clone()).to_string();
        assert!(
            !diff.contains("\x1b[2J") && !diff.contains("\x1b[3J"),
            "forced repaint should not clear or purge native scrollback when protected rows are unchanged: {diff:?}"
        );
        assert!(
            !diff.contains("one"),
            "the cursor-restore-scroll protected top row should be skipped rather than duplicated into scrollback: {diff:?}"
        );
        assert!(
            diff.contains("two") && diff.contains("five"),
            "reachable rows should still be repainted: {diff:?}"
        );
    }

    #[test]
    fn test_inline_force_full_repaint_growing_rewrites_without_clear() {
        let mut prev = Canvas::new(10, 2);
        prev.subview_mut(0, 0, 0, 0, 10, 2)
            .set_text(0, 0, "hello", CanvasTextStyle::default());
        prev.subview_mut(0, 0, 0, 0, 10, 2)
            .set_text(0, 1, "world", CanvasTextStyle::default());

        let mut next = Canvas::new(10, 3);
        next.subview_mut(0, 0, 0, 0, 10, 3)
            .set_text(0, 0, "hello", CanvasTextStyle::default());
        next.subview_mut(0, 0, 0, 0, 10, 3)
            .set_text(0, 1, "world", CanvasTextStyle::default());
        next.subview_mut(0, 0, 0, 0, 10, 3)
            .set_text(0, 2, "again", CanvasTextStyle::default());
        next.force_full_repaint();

        let (dest, diff_buf) = new_test_writer();
        let mut term = new_inline_term_with_size(dest, prev.height() as _, (10, 5));
        term.prev_size_on_write = term.size;
        term.write_canvas(Some(&prev), &next).unwrap();

        let diff = String::from_utf8_lossy(&diff_buf.lock().unwrap().clone()).to_string();
        assert!(
            !diff.contains("\x1b[J"),
            "growing forced repaint should append/rewrite in place without clearing: {diff:?}"
        );
        assert!(diff.contains("hello") && diff.contains("world") && diff.contains("again"));

        let mut setup = Vec::new();
        prev.write_ansi_without_final_newline(&mut setup).unwrap();
        setup.extend_from_slice(&diff_buf.lock().unwrap());
        let mut vt = avt::Vt::new(10, 5);
        vt.feed_str(&String::from_utf8(setup).unwrap());
        assert_eq!(vt.line(0).text(), "hello     ");
        assert_eq!(vt.line(1).text(), "world     ");
        assert_eq!(vt.line(2).text(), "again     ");
        assert_eq!(vt.cursor().row, 2);
    }

    #[test]
    fn test_inline_resize_forces_full_rewrite_even_when_canvas_identical() {
        let mut canvas = Canvas::new(10, 2);
        canvas
            .subview_mut(0, 0, 0, 0, 10, 2)
            .set_text(0, 0, "hello", CanvasTextStyle::default());
        canvas
            .subview_mut(0, 0, 0, 0, 10, 2)
            .set_text(0, 1, "world", CanvasTextStyle::default());

        let (dest, diff_buf) = new_test_writer();
        let mut term = new_inline_term_with_size(dest, canvas.height() as _, (10, 5));
        term.prev_size_on_write = Some((12, 5));
        term.write_canvas(Some(&canvas), &canvas).unwrap();

        let diff = String::from_utf8_lossy(&diff_buf.lock().unwrap().clone()).to_string();
        assert!(
            diff.contains("\x1b[J"),
            "resize reset should clear the old inline canvas: {diff:?}"
        );
        assert!(
            diff.contains("hello") && diff.contains("world"),
            "resize reset should repaint the full canvas: {diff:?}"
        );
    }

    #[test]
    fn test_fullscreen_diff_identical_canvas_is_noop() {
        let mut canvas = Canvas::new(10, 2);
        canvas
            .subview_mut(0, 0, 0, 0, 10, 2)
            .set_text(0, 0, "hello", CanvasTextStyle::default());
        canvas
            .subview_mut(0, 0, 0, 0, 10, 2)
            .set_text(0, 1, "world", CanvasTextStyle::default());

        let (dest, diff_buf) = new_test_writer();
        let mut term = new_fullscreen_term(dest, 0, canvas.height() as _);
        term.write_canvas(Some(&canvas), &canvas).unwrap();

        let buf = diff_buf.lock().unwrap();
        assert!(
            buf.is_empty(),
            "identical fullscreen canvas should preserve the empty-diff zero-write fast path"
        );
    }

    #[test]
    fn test_fullscreen_prev_damage_region_rewrites_identical_rows() {
        let mut prev = Canvas::new(10, 3);
        for (y, text) in ["one", "two", "three"].iter().enumerate() {
            prev.subview_mut(0, 0, 0, 0, 10, 3).set_text(
                0,
                y as isize,
                text,
                CanvasTextStyle::default(),
            );
        }
        prev.mark_damage(crate::canvas::DamageRegion {
            x: 0,
            y: 1,
            width: 10,
            height: 1,
        });

        let mut next = prev.clone();
        next.clear_damage();

        let (dest, diff_buf) = new_test_writer();
        let mut term = new_fullscreen_term(dest, 0, prev.height() as _);
        term.write_canvas(Some(&prev), &next).unwrap();

        let diff = String::from_utf8_lossy(&diff_buf.lock().unwrap().clone()).to_string();
        assert!(
            diff.contains("two"),
            "previous-frame damage should be unioned into fullscreen diff: {diff:?}"
        );
        assert!(
            !diff.contains("one") && !diff.contains("three"),
            "undamaged fullscreen rows should remain skipped: {diff:?}"
        );
    }

    #[test]
    fn test_fullscreen_damage_region_rewrites_identical_rows() {
        let mut prev = Canvas::new(10, 3);
        for (y, text) in ["one", "two", "three"].iter().enumerate() {
            prev.subview_mut(0, 0, 0, 0, 10, 3).set_text(
                0,
                y as isize,
                text,
                CanvasTextStyle::default(),
            );
        }

        let mut next = prev.clone();
        next.mark_damage(crate::canvas::DamageRegion {
            x: 0,
            y: 1,
            width: 10,
            height: 1,
        });

        let (dest, diff_buf) = new_test_writer();
        let mut term = new_fullscreen_term(dest, 0, prev.height() as _);
        term.write_canvas(Some(&prev), &next).unwrap();

        let diff = String::from_utf8_lossy(&diff_buf.lock().unwrap().clone()).to_string();
        assert!(
            diff.contains("two"),
            "damaged identical fullscreen row should be repainted: {diff:?}"
        );
        assert!(
            !diff.contains("one") && !diff.contains("three"),
            "undamaged identical fullscreen rows should remain skipped: {diff:?}"
        );
    }

    #[test]
    fn test_fullscreen_force_full_repaint_rewrites_identical_rows() {
        let mut prev = Canvas::new(10, 2);
        prev.subview_mut(0, 0, 0, 0, 10, 2)
            .set_text(0, 0, "hello", CanvasTextStyle::default());
        prev.subview_mut(0, 0, 0, 0, 10, 2)
            .set_text(0, 1, "world", CanvasTextStyle::default());

        let mut next = prev.clone();
        next.force_full_repaint();

        let (dest, diff_buf) = new_test_writer();
        let mut term = new_fullscreen_term(dest, 0, prev.height() as _);
        term.write_canvas(Some(&prev), &next).unwrap();

        let diff = String::from_utf8_lossy(&diff_buf.lock().unwrap().clone()).to_string();
        assert!(
            diff.contains("hello") && diff.contains("world"),
            "forced full repaint should rewrite unchanged fullscreen rows: {diff:?}"
        );
    }

    #[test]
    fn test_inline_diff_styled_text_preserved() {
        let bold_style = CanvasTextStyle {
            weight: Weight::Bold,
            color: Some(Color::Red),
            ..Default::default()
        };

        let mut prev = Canvas::new(10, 2);
        prev.subview_mut(0, 0, 0, 0, 10, 2)
            .set_text(0, 0, "hello", bold_style);
        prev.subview_mut(0, 0, 0, 0, 10, 2)
            .set_text(0, 1, "old", CanvasTextStyle::default());

        let mut next = Canvas::new(10, 2);
        next.subview_mut(0, 0, 0, 0, 10, 2)
            .set_text(0, 0, "hello", bold_style);
        next.subview_mut(0, 0, 0, 0, 10, 2)
            .set_text(0, 1, "new", bold_style);

        let (dest, diff_buf) = new_test_writer();
        let mut term = new_inline_term(dest, prev.height() as _);
        term.write_canvas(Some(&prev), &next).unwrap();

        let mut setup = Vec::new();
        prev.write_ansi_without_final_newline(&mut setup).unwrap();
        setup.extend_from_slice(&diff_buf.lock().unwrap());

        let mut vt = avt::Vt::new(10, 5);
        vt.feed_str(&String::from_utf8(setup).unwrap());

        // Row 0 unchanged: bold red "hello"
        let row0 = vt.line(0);
        assert_eq!(row0.text(), "hello     ");
        assert!(row0.cells()[0].pen().is_bold());
        assert!(row0.cells()[0].pen().foreground().is_some());

        // Row 1 updated: bold red "new"
        let row1 = vt.line(1);
        assert_eq!(row1.text(), "new       ");
        assert!(row1.cells()[0].pen().is_bold());
        assert!(row1.cells()[0].pen().foreground().is_some());
    }

    #[test]
    fn test_fullscreen_diff_styled_text_preserved() {
        let underline_style = CanvasTextStyle {
            underline: true,
            color: Some(Color::Green),
            ..Default::default()
        };

        let mut prev = Canvas::new(10, 2);
        prev.subview_mut(0, 0, 0, 0, 10, 2)
            .set_text(0, 0, "keep", underline_style);
        prev.subview_mut(0, 0, 0, 0, 10, 2)
            .set_text(0, 1, "old", CanvasTextStyle::default());

        let mut next = Canvas::new(10, 2);
        next.subview_mut(0, 0, 0, 0, 10, 2)
            .set_text(0, 0, "keep", underline_style);
        next.subview_mut(0, 0, 0, 0, 10, 2)
            .set_text(0, 1, "new", underline_style);

        let (dest, diff_buf) = new_test_writer();
        let mut term = new_fullscreen_term(dest, 0, prev.height() as _);
        term.write_canvas(Some(&prev), &next).unwrap();

        let mut setup = Vec::new();
        prev.write_ansi_without_final_newline(&mut setup).unwrap();
        setup.extend_from_slice(&diff_buf.lock().unwrap());

        let mut vt = avt::Vt::new(10, 5);
        vt.feed_str(&String::from_utf8(setup).unwrap());

        // Row 0 unchanged
        let row0 = vt.line(0);
        assert_eq!(row0.text(), "keep      ");
        assert!(row0.cells()[0].pen().is_underline());

        // Row 1 updated with underline green
        let row1 = vt.line(1);
        assert_eq!(row1.text(), "new       ");
        assert!(row1.cells()[0].pen().is_underline());
        assert!(row1.cells()[0].pen().foreground().is_some());
    }

    #[test]
    fn test_inline_diff_at_terminal_height_boundary() {
        // Canvas height == terminal height uses the normal diff path when only
        // visible rows changed (no off-screen changes trigger a fallback).
        let mut prev = Canvas::new(10, 5);
        prev.subview_mut(0, 0, 0, 0, 10, 5)
            .set_text(0, 0, "aaa", CanvasTextStyle::default());
        prev.subview_mut(0, 0, 0, 0, 10, 5)
            .set_text(0, 4, "bbb", CanvasTextStyle::default());

        let mut next = Canvas::new(10, 5);
        next.subview_mut(0, 0, 0, 0, 10, 5)
            .set_text(0, 0, "aaa", CanvasTextStyle::default());
        next.subview_mut(0, 0, 0, 0, 10, 5)
            .set_text(0, 4, "ccc", CanvasTextStyle::default());

        let (_diff, vt) = inline_diff_vt(&prev, &next, (10, 5));

        assert_eq!(vt.line(0).text(), "aaa       ");
        assert_eq!(vt.line(4).text(), "ccc       ");
    }

    #[test]
    fn test_inline_diff_tall_canvas_visible_change() {
        // Canvas (8 rows) taller than terminal (5 rows). Only the last row
        // changes, which is in the visible area — the normal diff path should
        // handle it without a full clear+rewrite.
        let style = CanvasTextStyle::default();

        let mut prev = Canvas::new(10, 8);
        for i in 0..8 {
            prev.subview_mut(0, 0, 0, 0, 10, 8)
                .set_text(0, i, &format!("row{i}"), style);
        }

        let mut next = Canvas::new(10, 8);
        for i in 0..7 {
            next.subview_mut(0, 0, 0, 0, 10, 8)
                .set_text(0, i, &format!("row{i}"), style);
        }
        next.subview_mut(0, 0, 0, 0, 10, 8)
            .set_text(0, 7, "CHANGED", style);

        let (diff, vt) = inline_diff_vt(&prev, &next, (10, 5));

        // Should NOT contain a full clear (ClearAll = ESC[2J)
        let diff_str = String::from_utf8_lossy(&diff);
        assert!(
            !diff_str.contains("\x1b[2J"),
            "expected row-level diff, not full clear; got: {diff_str:?}"
        );

        // The bottom 5 rows of the 8-row canvas are visible in the terminal.
        assert_eq!(vt.line(0).text(), "row3      ");
        assert_eq!(vt.line(4).text(), "CHANGED   ");
    }

    #[test]
    fn test_inline_diff_full_viewport_top_row_change_full_rewrite() {
        // Mirrors Ink/log-update's cursorRestoreScroll guard. When the previous
        // frame exactly filled the viewport, a cursor restore can push the top
        // row into scrollback; changing that row should therefore reset instead
        // of relying on a relative move to the viewport top.
        let style = CanvasTextStyle::default();
        let mut prev = Canvas::new(10, 5);
        for i in 0..5 {
            prev.subview_mut(0, 0, 0, 0, 10, 5)
                .set_text(0, i, &format!("row{i}"), style);
        }

        let mut next = prev.clone();
        next.subview_mut(0, 0, 0, 0, 10, 5)
            .set_text(0, 0, "TOP", style);

        let (diff, vt) = inline_diff_vt(&prev, &next, (10, 5));
        let diff_str = String::from_utf8_lossy(&diff);
        assert!(
            diff_str.contains("\x1b[2J"),
            "expected full reset for top row after full-viewport frame: {diff_str:?}"
        );
        assert_eq!(vt.line(0).text(), "TOP0      ");
        assert_eq!(vt.line(4).text(), "row4      ");
    }

    #[test]
    fn test_inline_diff_tall_canvas_top_visible_change_full_rewrite() {
        // For an overflowing previous frame, Ink/log-update treats viewportY+1
        // rows as unreachable: the top visible row may have been pushed into
        // scrollback by cursor restoration. A change at row 3 for an 8-row
        // canvas in a 5-row viewport should therefore force a full reset.
        let style = CanvasTextStyle::default();

        let mut prev = Canvas::new(10, 8);
        for i in 0..8 {
            prev.subview_mut(0, 0, 0, 0, 10, 8)
                .set_text(0, i, &format!("row{i}"), style);
        }

        let mut next = prev.clone();
        next.subview_mut(0, 0, 0, 0, 10, 8)
            .set_text(0, 3, "TOPVIS", style);

        let (diff, vt) = inline_diff_vt(&prev, &next, (10, 5));
        let diff_str = String::from_utf8_lossy(&diff);
        assert!(
            diff_str.contains("\x1b[2J"),
            "expected full reset for cursorRestoreScroll top visible row: {diff_str:?}"
        );
        assert_eq!(vt.line(0).text(), "TOPVIS    ");
        assert_eq!(vt.line(4).text(), "row7      ");
    }

    #[test]
    fn test_inline_diff_tall_canvas_offscreen_change() {
        // Canvas (8 rows) taller than terminal (5 rows). A row above the
        // visible area changes — this must trigger the full-rewrite fallback
        // since we can't cursor to an off-screen row.
        let style = CanvasTextStyle::default();

        let mut prev = Canvas::new(10, 8);
        for i in 0..8 {
            prev.subview_mut(0, 0, 0, 0, 10, 8)
                .set_text(0, i, &format!("row{i}"), style);
        }

        let mut next = Canvas::new(10, 8);
        for i in 0..8 {
            next.subview_mut(0, 0, 0, 0, 10, 8)
                .set_text(0, i, &format!("row{i}"), style);
        }
        // Change row 1, which is above the visible area (visible_start = 8-5 = 3).
        next.subview_mut(0, 0, 0, 0, 10, 8)
            .set_text(0, 1, "OFFSCR", style);

        let (diff, vt) = inline_diff_vt(&prev, &next, (10, 5));

        // Should contain a full clear (ClearAll = ESC[2J, because
        // prev_canvas_height >= term_height triggers the heavy clear path).
        let diff_str = String::from_utf8_lossy(&diff);
        assert!(
            diff_str.contains("\x1b[2J"),
            "expected full clear fallback; got: {diff_str:?}"
        );

        // After full rewrite, the bottom 5 rows of the new canvas are visible.
        assert_eq!(vt.line(0).text(), "row3      ");
        assert_eq!(vt.line(4).text(), "row7      ");
    }

    #[test]
    fn test_inline_diff_shrinking_from_scrollback_to_viewport_full_rewrite() {
        // Mirrors Ink/log-update's shrink->fits guard: when the previous
        // inline canvas overflowed the terminal, the visible terminal contains
        // the suffix of that canvas. Shrinking to a viewport-sized canvas should
        // reveal row0 again, but cursor/erase operations cannot pull rows out of
        // scrollback. Force a clear+repaint instead.
        let style = CanvasTextStyle::default();

        let mut prev = Canvas::new(10, 8);
        for i in 0..8 {
            prev.subview_mut(0, 0, 0, 0, 10, 8)
                .set_text(0, i, &format!("old{i}"), style);
        }

        let mut next = Canvas::new(10, 5);
        for i in 0..5 {
            next.subview_mut(0, 0, 0, 0, 10, 5)
                .set_text(0, i, &format!("new{i}"), style);
        }

        let (diff, vt) = inline_diff_vt(&prev, &next, (10, 5));
        let diff_str = String::from_utf8_lossy(&diff);
        assert!(
            diff_str.contains("\x1b[2J"),
            "expected full clear fallback for shrink->fits; got: {diff_str:?}"
        );
        assert_eq!(vt.line(0).text(), "new0      ");
        assert_eq!(vt.line(4).text(), "new4      ");
        assert_eq!(vt.cursor().row, 4);
    }

    #[test]
    fn test_inline_diff_sequential_updates() {
        let style = CanvasTextStyle::default();

        let mut c1 = Canvas::new(10, 2);
        c1.subview_mut(0, 0, 0, 0, 10, 2)
            .set_text(0, 0, "aaa", style);
        c1.subview_mut(0, 0, 0, 0, 10, 2)
            .set_text(0, 1, "bbb", style);

        let mut c2 = Canvas::new(10, 2);
        c2.subview_mut(0, 0, 0, 0, 10, 2)
            .set_text(0, 0, "aaa", style);
        c2.subview_mut(0, 0, 0, 0, 10, 2)
            .set_text(0, 1, "ccc", style);

        let mut c3 = Canvas::new(10, 3);
        c3.subview_mut(0, 0, 0, 0, 10, 3)
            .set_text(0, 0, "xxx", style);
        c3.subview_mut(0, 0, 0, 0, 10, 3)
            .set_text(0, 1, "ccc", style);
        c3.subview_mut(0, 0, 0, 0, 10, 3)
            .set_text(0, 2, "ddd", style);

        let (dest, buf) = new_test_writer();
        let mut term = new_inline_term(dest, c1.height() as _);

        // First diff: c1 -> c2
        term.write_canvas(Some(&c1), &c2).unwrap();
        // Second diff: c2 -> c3
        term.write_canvas(Some(&c2), &c3).unwrap();

        let mut setup = Vec::new();
        c1.write_ansi_without_final_newline(&mut setup).unwrap();
        setup.extend_from_slice(&buf.lock().unwrap());

        let mut vt = avt::Vt::new(10, 6);
        vt.feed_str(&String::from_utf8(setup).unwrap());

        assert_eq!(vt.line(0).text(), "xxx       ");
        assert_eq!(vt.line(1).text(), "ccc       ");
        assert_eq!(vt.line(2).text(), "ddd       ");
        assert_eq!(vt.cursor().row, 2, "cursor on last row of final canvas");
    }

    #[test]
    fn test_fullscreen_diff_sequential_updates() {
        let style = CanvasTextStyle::default();

        let mut c1 = Canvas::new(10, 2);
        c1.subview_mut(0, 0, 0, 0, 10, 2)
            .set_text(0, 0, "aaa", style);
        c1.subview_mut(0, 0, 0, 0, 10, 2)
            .set_text(0, 1, "bbb", style);

        let mut c2 = Canvas::new(10, 2);
        c2.subview_mut(0, 0, 0, 0, 10, 2)
            .set_text(0, 0, "aaa", style);
        c2.subview_mut(0, 0, 0, 0, 10, 2)
            .set_text(0, 1, "ccc", style);

        let mut c3 = Canvas::new(10, 3);
        c3.subview_mut(0, 0, 0, 0, 10, 3)
            .set_text(0, 0, "xxx", style);
        c3.subview_mut(0, 0, 0, 0, 10, 3)
            .set_text(0, 1, "ccc", style);
        c3.subview_mut(0, 0, 0, 0, 10, 3)
            .set_text(0, 2, "ddd", style);

        let (dest, buf) = new_test_writer();
        let mut term = new_fullscreen_term(dest, 0, c1.height() as _);

        term.write_canvas(Some(&c1), &c2).unwrap();
        term.write_canvas(Some(&c2), &c3).unwrap();

        let mut setup = Vec::new();
        c1.write_ansi_without_final_newline(&mut setup).unwrap();
        setup.extend_from_slice(&buf.lock().unwrap());

        let mut vt = avt::Vt::new(10, 6);
        vt.feed_str(&String::from_utf8(setup).unwrap());

        assert_eq!(vt.line(0).text(), "xxx       ");
        assert_eq!(vt.line(1).text(), "ccc       ");
        assert_eq!(vt.line(2).text(), "ddd       ");
        assert_eq!(vt.cursor().row, 2, "cursor on last row of final canvas");
    }

    #[test]
    fn test_fullscreen_diff_rewrites_from_first_changed_column() {
        let style = CanvasTextStyle::default();
        let mut prev = Canvas::new(12, 1);
        prev.subview_mut(0, 0, 0, 0, 12, 1)
            .set_text(0, 0, "prefix-old", style);
        let mut next = Canvas::new(12, 1);
        next.subview_mut(0, 0, 0, 0, 12, 1)
            .set_text(0, 0, "prefix-new", style);

        let (dest, buf) = new_test_writer();
        let mut term = new_fullscreen_term(dest, 0, prev.height() as _);
        term.size = Some((12, 5));
        term.prev_size_on_write = Some((12, 5));
        term.write_canvas(Some(&prev), &next).unwrap();

        let diff = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
        assert!(
            diff.contains("\x1b[1;8Hnew"),
            "fullscreen diff should jump to the first changed column: {diff:?}"
        );
        assert!(
            !diff.contains("prefix-"),
            "fullscreen diff should not repaint the unchanged prefix: {diff:?}"
        );

        let mut setup = Vec::new();
        prev.write_ansi_without_final_newline(&mut setup).unwrap();
        setup.extend_from_slice(diff.as_bytes());
        let mut vt = avt::Vt::new(12, 5);
        vt.feed_str(&String::from_utf8(setup).unwrap());
        assert_eq!(vt.line(0).text(), "prefix-new  ");
    }

    #[test]
    fn test_borrowed_writers() {
        let mut stdout_buf: Vec<u8> = Vec::new();
        let mut stderr_buf: Vec<u8> = Vec::new();

        {
            let mut terminal = Terminal::new(
                Box::new(&mut stdout_buf),
                Box::new(&mut stderr_buf),
                Output::Stdout,
                false,
                true,
            )
            .unwrap();
            let canvas = Canvas::new(10, 1);
            terminal.write_canvas(None, &canvas).unwrap();
        }

        assert!(!stdout_buf.is_empty());
    }

    /// Helper: build a pair of 10×5 canvases (4 content rows + 1 footer) that
    /// differ only in a single cell's background color on `changed_row`,
    /// simulating a mouse-highlight overlay.
    fn make_fullscreen_diff_canvases(changed_row: usize) -> (Canvas, Canvas) {
        let style = CanvasTextStyle::default();
        let width = 10;
        let height = 5;

        let build = |highlight: bool| {
            let mut c = Canvas::new(width, height);
            let mut sv = c.subview_mut(0, 0, 0, 0, width, height);
            for y in 0..4u32 {
                sv.set_text(0, y as isize, &format!("row{y}"), style);
            }
            sv.set_text(0, 4, "FOOTER", style);
            sv.set_background_color(0, 4, width, 1, Color::Green);
            if highlight {
                sv.set_background_color(0, changed_row as isize, 1, 1, Color::Yellow);
            }
            c
        };

        (build(false), build(true))
    }

    /// Verify that with `prev_canvas_top_row = 0` the fullscreen row-level
    /// diff writes each changed row to its correct terminal position.
    ///
    /// Uses a layout with numbered content rows and a distinct footer, where
    /// a single cell changes between frames (as a mouse-highlight overlay
    /// would cause).
    #[test]
    fn test_fullscreen_diff_zero_top_row_renders_correctly() {
        let (prev, next) = make_fullscreen_diff_canvases(2);
        let width = prev.width();
        let height = prev.height();

        let (dest, buf) = new_test_writer();
        let mut term = new_fullscreen_term(dest, 0, height as _);
        term.write_canvas(Some(&prev), &next).unwrap();

        // Replay: write prev canvas as the baseline already on screen,
        // then apply the diff on top.
        let mut setup = Vec::new();
        prev.write_ansi_without_final_newline(&mut setup).unwrap();
        setup.extend_from_slice(&buf.lock().unwrap());

        let mut vt = avt::Vt::new(width, height + 2);
        vt.feed_str(&String::from_utf8(setup).unwrap());

        assert_eq!(vt.line(0).text(), "row0      ");
        assert_eq!(vt.line(1).text(), "row1      ");
        assert_eq!(vt.line(2).text(), "row2      ");
        assert_eq!(vt.line(3).text(), "row3      ");
        assert_eq!(
            vt.line(4).text(),
            "FOOTER    ",
            "every row must appear at its correct terminal position"
        );
    }

    /// Counterpart: with a non-zero `prev_canvas_top_row`, every changed row Y
    /// is written to terminal line `top_row + Y` instead of line Y.  Unchanged
    /// rows are skipped by `row_eq`, so the corruption is never self-correcting.
    ///
    /// This demonstrates why `prev_canvas_top_row` must be anchored at 0 in
    /// fullscreen mode — any stale cursor position causes the entire diff to
    /// be offset.
    #[test]
    fn test_fullscreen_diff_nonzero_top_row_offsets_changed_rows() {
        let (prev, next) = make_fullscreen_diff_canvases(1);
        let width = prev.width();
        let height = prev.height();

        // With top_row = 2 (simulating a stale cursor value), the diff for
        // changed row 1 writes to terminal position 2+1 = 3 instead of 1.
        let (dest, buf) = new_test_writer();
        let mut term = new_fullscreen_term(dest, 2, height as _);
        term.write_canvas(Some(&prev), &next).unwrap();

        let mut setup = Vec::new();
        prev.write_ansi_without_final_newline(&mut setup).unwrap();
        setup.extend_from_slice(&buf.lock().unwrap());

        let mut vt = avt::Vt::new(width, height + 4);
        vt.feed_str(&String::from_utf8(setup).unwrap());

        // Row 1's diff landed at terminal line 3 (offset 2+1) instead of 1,
        // overwriting row 3's original content.
        assert_eq!(
            vt.line(3).text(),
            "row1      ",
            "row 1's diff landed at terminal line 3 (offset 2+1) instead of line 1"
        );
    }

    /// Regression test: exercises the full initial-write → diff pipeline.
    ///
    /// `write_canvas(None, …)` must set `prev_canvas_top_row` to 0.  This path
    /// runs both on the very first frame and whenever `clear_terminal_output()`
    /// triggers a full rewrite on a subsequent frame — in either case the
    /// cursor may not be at (0, 0), so the code must reset it explicitly.
    ///
    /// Without the fix, the old code called `cursor::position()` which returns
    /// a stale value inside `BeginSynchronizedUpdate` on real terminals, and
    /// fails outright in non-TTY test environments (timeout → panic).
    #[test]
    fn test_fullscreen_initial_write_sets_zero_top_row() {
        let (initial, next) = make_fullscreen_diff_canvases(2);
        let width = initial.width();
        let height = initial.height();

        let (dest, buf) = new_test_writer();
        let mut term = new_fullscreen_term(dest, 99, 0); // start with intentionally wrong value

        // The initial write must set prev_canvas_top_row = 0 (the fix).
        // Without the fix, this panics due to cursor::position() timeout.
        term.write_canvas(None, &initial).unwrap();
        assert_eq!(
            term.prev_canvas_top_row, 0,
            "initial fullscreen write must anchor prev_canvas_top_row at 0"
        );

        // Subsequent diff should render correctly with top_row = 0.
        term.write_canvas(Some(&initial), &next).unwrap();

        let mut vt = avt::Vt::new(width, height + 2);
        vt.feed_str(&String::from_utf8(buf.lock().unwrap().clone()).unwrap());

        assert_eq!(vt.line(0).text(), "row0      ");
        assert_eq!(vt.line(1).text(), "row1      ");
        assert_eq!(vt.line(2).text(), "row2      ");
        assert_eq!(vt.line(3).text(), "row3      ");
        assert_eq!(vt.line(4).text(), "FOOTER    ");
    }
}
