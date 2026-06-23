use super::*;

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
