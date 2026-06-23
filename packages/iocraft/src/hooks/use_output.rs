use super::SelectionContext;
use crate::{
    Canvas, ClipboardMultiplexer, ComponentUpdater, Hook, Hooks, SelectionController,
    SelectionState,
};
use core::{
    pin::Pin,
    task::{Context, Poll, Waker},
};
use crossterm::{cursor, QueueableCommand};
use std::{
    borrow::Cow,
    sync::{Arc, Mutex},
};
use unicode_width::UnicodeWidthChar;

mod private {
    pub trait Sealed {}
    impl Sealed for crate::Hooks<'_, '_> {}
}

/// `UseOutput` is a hook that allows you to write to stdout and stderr from a component. The
/// output will be appended to stdout or stderr, above the rendered component output.
///
/// Both `print` and `println` methods are available for writing output with or without newlines.
///
/// # Example
///
/// ```
/// # use iocraft::prelude::*;
/// # use std::time::Duration;
/// #[component]
/// fn Example(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
///     let (stdout, stderr) = hooks.use_output();
///
///     hooks.use_future(async move {
///         stdout.println("Hello from iocraft to stdout!");
///         stderr.println("  And hello to stderr too!");
///
///         stdout.print("Working...");
///         for _ in 0..5 {
///             smol::Timer::after(Duration::from_secs(1)).await;
///             stdout.print(".");
///         }
///         stdout.println("\nDone!");
///     });
///
///     element! {
///         View(border_style: BorderStyle::Round, border_color: Color::Green) {
///             Text(content: "Hello, use_output!")
///         }
///     }
/// }
/// ```
pub trait UseOutput: private::Sealed {
    /// Gets handles which can be used to write to stdout and stderr.
    fn use_output(&mut self) -> (StdoutHandle, StderrHandle);
}

impl UseOutput for Hooks<'_, '_> {
    fn use_output(&mut self) -> (StdoutHandle, StderrHandle) {
        let output = self.use_hook(UseOutputImpl::default);
        (output.use_stdout(), output.use_stderr())
    }
}

enum Message {
    Stdout(String),
    StdoutNoNewline(String),
    StdoutClipboard(String, ClipboardMultiplexer),
    StdoutControl(String),
    Stderr(String),
    StderrNoNewline(String),
}

impl Message {
    fn affects_visible_output(&self) -> bool {
        !matches!(
            self,
            Message::StdoutClipboard(..) | Message::StdoutControl(_)
        )
    }
}

#[derive(Default)]
struct UseOutputState {
    queue: Vec<Message>,
    waker: Option<Waker>,
    appended_newline: Option<u16>,
}

fn normalize_terminal_newlines(text: &str) -> Cow<'_, str> {
    let mut prev = '\0';
    let mut normalized = None::<String>;
    for (idx, ch) in text.char_indices() {
        if ch == '\n' && prev != '\r' {
            let out = normalized.get_or_insert_with(|| {
                let mut s = String::with_capacity(text.len() + 1);
                s.push_str(&text[..idx]);
                s
            });
            out.push('\r');
            out.push('\n');
        } else if let Some(out) = normalized.as_mut() {
            out.push(ch);
        }
        prev = ch;
    }
    normalized.map(Cow::Owned).unwrap_or(Cow::Borrowed(text))
}

fn skip_ansi_string_control(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    let mut saw_escape = false;
    for ch in chars.by_ref() {
        if ch == '\u{7}' {
            break;
        }
        if saw_escape && ch == '\\' {
            break;
        }
        saw_escape = ch == '\u{1b}';
    }
}

fn skip_ansi_escape(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    let Some(kind) = chars.next() else {
        return;
    };
    match kind {
        '[' => {
            for ch in chars.by_ref() {
                let code = ch as u32;
                if (0x40..=0x7e).contains(&code) {
                    break;
                }
            }
        }
        ']' | 'P' | '_' | '^' | 'X' => skip_ansi_string_control(chars),
        _ => {}
    }
}

fn advance_printable_width(col: u16, char_width: usize, terminal_width: Option<usize>) -> u16 {
    if char_width == 0 {
        return col;
    }

    let Some(terminal_width) = terminal_width.filter(|w| *w > 0) else {
        return (col as usize)
            .saturating_add(char_width)
            .min(u16::MAX as usize) as u16;
    };

    // `col == terminal_width` represents the VT pending-wrap state after a
    // printable ended exactly at the right margin. The next printable first
    // wraps to the following row before drawing at column 0.
    let start_col = if col as usize >= terminal_width {
        0
    } else {
        col as usize
    };
    let advanced = start_col.saturating_add(char_width);
    if advanced <= terminal_width {
        // Preserve `terminal_width` as the pending-wrap marker.
        advanced as u16
    } else {
        (advanced % terminal_width) as u16
    }
}

fn advance_terminal_column(col: u16, text: &str, terminal_width: Option<usize>) -> u16 {
    let mut col = col;
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\u{1b}' => skip_ansi_escape(&mut chars),
            '\r' | '\n' => col = 0,
            '\t' => {
                let next_tab = ((col as usize / 8) + 1) * 8;
                let delta = next_tab.saturating_sub(col as usize);
                col = advance_printable_width(col, delta, terminal_width);
            }
            '\u{8}' => col = col.saturating_sub(1),
            _ if ch.is_control() => {}
            _ => {
                col = advance_printable_width(col, ch.width().unwrap_or(0), terminal_width);
            }
        }
    }
    col
}

impl UseOutputState {
    fn exec(&mut self, updater: &mut ComponentUpdater) {
        if self.queue.is_empty() {
            return;
        }

        // Check if we have a terminal - if not, messages stay queued
        if updater.terminal_mut().is_none() {
            return;
        }

        let has_visible_output = self.queue.iter().any(Message::affects_visible_output);
        if has_visible_output {
            updater.clear_terminal_output();
        }
        let terminal = updater.terminal_mut().unwrap();
        let terminal_width = terminal.size().map(|(width, _)| width as usize);

        if has_visible_output {
            if let Some(col) = self.appended_newline {
                let _ = terminal
                    .render_output()
                    .queue(cursor::MoveUp(1))
                    .and_then(|w| w.queue(cursor::MoveRight(col)));
            }
            // Flush render output to ensure escape sequences are sent before any
            // cross-stream writes (e.g., stdout messages when rendering to stderr).
            let _ = terminal.render_output().flush();
        }

        // Track the virtual cursor column ourselves instead of querying the
        // terminal. Crossterm's cursor::position() races with EventStream's
        // raw-mode stdin reader and can fail or steal bytes; when that happens
        // no extra newline is emitted and the next canvas frame is painted on
        // the same row as the progress output (visible tearing in kitty).
        let mut output_col = self.appended_newline.unwrap_or(0);

        for msg in self.queue.drain(..) {
            match msg {
                Message::Stdout(msg) => {
                    let mut formatted = normalize_terminal_newlines(&msg).into_owned();
                    formatted.push_str("\r\n");
                    let _ = terminal.stdout().write_all(formatted.as_bytes());
                    output_col = 0;
                }
                Message::StdoutNoNewline(msg) => {
                    let formatted = normalize_terminal_newlines(&msg);
                    let _ = terminal.stdout().write_all(formatted.as_bytes());
                    output_col = advance_terminal_column(output_col, &msg, terminal_width);
                }
                Message::StdoutClipboard(msg, multiplexer) => {
                    let _ = terminal.set_clipboard_with_multiplexer(&msg, multiplexer);
                }
                Message::StdoutControl(sequence) => {
                    let _ = terminal.write_control_sequence(&sequence);
                }
                Message::Stderr(msg) => {
                    let mut formatted = normalize_terminal_newlines(&msg).into_owned();
                    formatted.push_str("\r\n");
                    let _ = terminal.stderr().write_all(formatted.as_bytes());
                    output_col = 0;
                }
                Message::StderrNoNewline(msg) => {
                    let formatted = normalize_terminal_newlines(&msg);
                    let _ = terminal.stderr().write_all(formatted.as_bytes());
                    output_col = advance_terminal_column(output_col, &msg, terminal_width);
                }
            }
        }

        if has_visible_output {
            if output_col > 0 {
                self.appended_newline = match terminal_width {
                    Some(width) if output_col as usize >= width => None,
                    _ => Some(output_col),
                };
                let _ = terminal.render_output().write_all(b"\r\n");
            } else {
                self.appended_newline = None;
            }
        }
    }
}

/// A handle to write to stdout, obtained from [`UseOutput::use_output`].
#[derive(Clone)]
pub struct StdoutHandle {
    state: Arc<Mutex<UseOutputState>>,
}

impl StdoutHandle {
    /// Queues a message to be written asynchronously to stdout, above the rendered component
    /// output.
    pub fn println<S: ToString>(&self, msg: S) {
        let mut state = self.state.lock().unwrap();
        state.queue.push(Message::Stdout(msg.to_string()));
        if let Some(waker) = state.waker.take() {
            waker.wake();
        }
    }

    /// Queues a message to be written asynchronously to stdout without a newline, above the
    /// rendered component output.
    pub fn print<S: ToString>(&self, msg: S) {
        let msg = msg.to_string();
        if msg.is_empty() {
            return;
        }
        let mut state = self.state.lock().unwrap();
        state.queue.push(Message::StdoutNoNewline(msg));
        if let Some(waker) = state.waker.take() {
            waker.wake();
        }
    }

    /// Queues a raw terminal control sequence to stdout.
    ///
    /// Unlike [`StdoutHandle::print`], this is non-visual output: it does not
    /// clear or reposition the retained render canvas. It is intended for OSC
    /// notifications, terminal progress, and other side-band terminal controls.
    pub fn write_control_sequence<S: ToString>(&self, sequence: S) {
        let sequence = sequence.to_string();
        if sequence.is_empty() {
            return;
        }
        let mut state = self.state.lock().unwrap();
        state.queue.push(Message::StdoutControl(sequence));
        if let Some(waker) = state.waker.take() {
            waker.wake();
        }
    }

    /// Queues an OSC 52 clipboard write to stdout.
    ///
    /// Unlike [`StdoutHandle::print`], this is a non-visual terminal control
    /// sequence: it does not clear or reposition the retained render canvas.
    /// This makes it suitable for fullscreen text-selection copy behavior.
    pub fn set_clipboard<S: ToString>(&self, text: S) {
        self.set_clipboard_with_multiplexer(text, ClipboardMultiplexer::None);
    }

    /// Queues an OSC 52 clipboard write to stdout using an explicit
    /// multiplexer passthrough wrapper.
    pub fn set_clipboard_with_multiplexer<S: ToString>(
        &self,
        text: S,
        multiplexer: ClipboardMultiplexer,
    ) {
        let mut state = self.state.lock().unwrap();
        state
            .queue
            .push(Message::StdoutClipboard(text.to_string(), multiplexer));
        if let Some(waker) = state.waker.take() {
            waker.wake();
        }
    }

    /// Copies a fullscreen [`SelectionState`] from a [`Canvas`] to the terminal
    /// clipboard without clearing the highlight. Returns the selected text.
    pub fn copy_selection_no_clear(&self, selection: &SelectionState, canvas: &Canvas) -> String {
        if !selection.has_selection() {
            return String::new();
        }
        let text = selection.selected_text(canvas);
        if !text.is_empty() {
            self.set_clipboard(&text);
        }
        text
    }

    /// Copies a fullscreen [`SelectionState`] from a [`Canvas`] to the terminal
    /// clipboard and clears the selection. Returns the selected text.
    pub fn copy_selection(&self, selection: &mut SelectionState, canvas: &Canvas) -> String {
        if !selection.has_selection() {
            return String::new();
        }
        let text = self.copy_selection_no_clear(selection, canvas);
        selection.clear();
        text
    }

    /// Copies from an app-level [`SelectionContext`] without clearing the
    /// highlight. Returns the selected text.
    ///
    /// This is the public app-level counterpart to CC Ink's
    /// `useSelection().copySelectionNoClear()`: the selection owner stays in a
    /// shared context while clipboard transport remains on `StdoutHandle`.
    pub fn copy_selection_context_no_clear(
        &self,
        selection: &SelectionContext,
        canvas: &Canvas,
    ) -> String {
        let text = selection.copy_selection_no_clear_text(canvas);
        if !text.is_empty() {
            self.set_clipboard(&text);
        }
        text
    }

    /// Copies from an app-level [`SelectionContext`] and clears the highlight.
    /// Returns the selected text.
    pub fn copy_selection_context(&self, selection: &SelectionContext, canvas: &Canvas) -> String {
        let text = selection.copy_selection_text(canvas);
        if !text.is_empty() {
            self.set_clipboard(&text);
        }
        text
    }

    /// Runs CC Ink-style copy-on-select for an app-level [`SelectionContext`].
    pub fn copy_on_select_context(
        &self,
        selection: &SelectionContext,
        canvas: &Canvas,
    ) -> Option<String> {
        self.copy_on_select_context_with_multiplexer(selection, canvas, ClipboardMultiplexer::None)
    }

    /// Runs app-level copy-on-select with an explicit multiplexer passthrough wrapper.
    pub fn copy_on_select_context_with_multiplexer(
        &self,
        selection: &SelectionContext,
        canvas: &Canvas,
        multiplexer: ClipboardMultiplexer,
    ) -> Option<String> {
        let text = selection.copy_on_select_text(canvas)?;
        self.set_clipboard_with_multiplexer(&text, multiplexer);
        Some(text)
    }

    /// Runs CC Ink-style copy-on-select for a [`SelectionController`].
    ///
    /// When a selection has just settled, this queues an OSC 52 clipboard write
    /// and returns the copied text. Repeated calls for the same settled
    /// selection return `None` until a new drag/selection resets the controller's
    /// copy-on-select guard.
    pub fn copy_on_select(
        &self,
        selection: &mut SelectionController,
        canvas: &Canvas,
    ) -> Option<String> {
        self.copy_on_select_with_multiplexer(selection, canvas, ClipboardMultiplexer::None)
    }

    /// Runs copy-on-select with an explicit multiplexer passthrough wrapper.
    pub fn copy_on_select_with_multiplexer(
        &self,
        selection: &mut SelectionController,
        canvas: &Canvas,
        multiplexer: ClipboardMultiplexer,
    ) -> Option<String> {
        let text = selection.copy_on_select_text(canvas)?;
        self.set_clipboard_with_multiplexer(&text, multiplexer);
        Some(text)
    }
}

/// A handle to write to stderr, obtained from [`UseOutput::use_output`].
#[derive(Clone)]
pub struct StderrHandle {
    state: Arc<Mutex<UseOutputState>>,
}

impl StderrHandle {
    /// Queues a message to be written asynchronously to stderr, above the rendered component
    /// output.
    pub fn println<S: ToString>(&self, msg: S) {
        let mut state = self.state.lock().unwrap();
        state.queue.push(Message::Stderr(msg.to_string()));
        if let Some(waker) = state.waker.take() {
            waker.wake();
        }
    }

    /// Queues a message to be written asynchronously to stderr without a newline, above the
    /// rendered component output.
    pub fn print<S: ToString>(&self, msg: S) {
        let msg = msg.to_string();
        if msg.is_empty() {
            return;
        }
        let mut state = self.state.lock().unwrap();
        state.queue.push(Message::StderrNoNewline(msg));
        if let Some(waker) = state.waker.take() {
            waker.wake();
        }
    }
}

#[derive(Default)]
struct UseOutputImpl {
    state: Arc<Mutex<UseOutputState>>,
}

impl Hook for UseOutputImpl {
    fn poll_change(self: Pin<&mut Self>, cx: &mut Context) -> Poll<()> {
        let mut state = self.state.lock().unwrap();
        if state.queue.is_empty() {
            state.waker = Some(cx.waker().clone());
            Poll::Pending
        } else {
            Poll::Ready(())
        }
    }

    fn post_component_update(&mut self, updater: &mut ComponentUpdater) {
        let mut state = self.state.lock().unwrap();
        state.exec(updater);
    }
}

impl UseOutputImpl {
    pub fn use_stdout(&mut self) -> StdoutHandle {
        StdoutHandle {
            state: self.state.clone(),
        }
    }

    pub fn use_stderr(&mut self) -> StderrHandle {
        StderrHandle {
            state: self.state.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prelude::*;
    use futures::task::noop_waker;
    use macro_rules_attribute::apply;
    use smol_macros::test;

    #[test]
    fn test_normalize_terminal_newlines_preserves_carriage_return_alignment() {
        assert!(matches!(
            normalize_terminal_newlines("plain"),
            Cow::Borrowed("plain")
        ));
        assert_eq!(normalize_terminal_newlines("a\nb").as_ref(), "a\r\nb");
        assert_eq!(normalize_terminal_newlines("a\r\nb").as_ref(), "a\r\nb");
        assert_eq!(normalize_terminal_newlines("\nDone!").as_ref(), "\r\nDone!");
    }

    #[test]
    fn test_advance_terminal_column_tracks_print_continuation_without_dsr() {
        assert_eq!(advance_terminal_column(0, "Working...", None), 10);
        assert_eq!(advance_terminal_column(10, ".", None), 11);
        assert_eq!(advance_terminal_column(11, "\nDone!", None), 5);
        assert_eq!(advance_terminal_column(0, "中\t", None), 8);
    }

    #[test]
    fn test_advance_terminal_column_wraps_with_terminal_width() {
        assert_eq!(advance_terminal_column(0, "abcdef", Some(10)), 6);
        assert_eq!(advance_terminal_column(6, "abcdef", Some(10)), 2);
        assert_eq!(advance_terminal_column(0, "1234567890", Some(10)), 10);
        assert_eq!(advance_terminal_column(10, ".", Some(10)), 1);
        assert_eq!(advance_terminal_column(0, "12345\nabc", Some(10)), 3);
    }

    #[test]
    fn test_advance_terminal_column_ignores_ansi_controls() {
        assert_eq!(advance_terminal_column(0, "\x1b[31mred\x1b[0m", None), 3);
        assert_eq!(
            advance_terminal_column(
                0,
                "\x1b]8;;https://example.com\x1b\\link\x1b]8;;\x1b\\",
                None,
            ),
            4
        );
        assert_eq!(advance_terminal_column(3, "\x08!", None), 3);
    }

    #[test]
    fn test_use_output_polling() {
        let mut use_output = UseOutputImpl::default();
        assert_eq!(
            Pin::new(&mut use_output)
                .poll_change(&mut core::task::Context::from_waker(&noop_waker())),
            Poll::Pending
        );

        // Empty no-newline prints are true no-ops: they must not wake the
        // render loop or clear the retained live canvas. This guards against
        // using `print("")` as a repaint workaround, which would otherwise call
        // clear_terminal_output() during UseOutputState::exec().
        let mut no_op_output = UseOutputImpl::default();
        let no_op_stdout = no_op_output.use_stdout();
        let no_op_stderr = no_op_output.use_stderr();
        no_op_stdout.print("");
        no_op_stderr.print("");
        assert_eq!(
            Pin::new(&mut no_op_output)
                .poll_change(&mut core::task::Context::from_waker(&noop_waker())),
            Poll::Pending
        );

        let mut disabled_context_output = UseOutputImpl::default();
        let disabled_context_stdout = disabled_context_output.use_stdout();
        let disabled_context_canvas = Canvas::new(4, 1);
        assert_eq!(
            disabled_context_stdout.copy_selection_context_no_clear(
                &SelectionContext::disabled(),
                &disabled_context_canvas,
            ),
            ""
        );
        assert_eq!(
            Pin::new(&mut disabled_context_output)
                .poll_change(&mut core::task::Context::from_waker(&noop_waker())),
            Poll::Pending
        );

        let mut whitespace_output = UseOutputImpl::default();
        let whitespace_stdout = whitespace_output.use_stdout();
        let mut whitespace_canvas = Canvas::new(8, 1);
        whitespace_canvas.subview_mut(0, 0, 0, 0, 8, 1).set_text(
            0,
            0,
            "a   b",
            CanvasTextStyle::default(),
        );
        let mut whitespace_controller = SelectionController::new();
        whitespace_controller.selection_mut().start(1, 0);
        whitespace_controller.selection_mut().update(3, 0);
        whitespace_controller.selection_mut().finish();
        assert_eq!(
            whitespace_stdout.copy_on_select(&mut whitespace_controller, &whitespace_canvas),
            None
        );
        assert_eq!(
            Pin::new(&mut whitespace_output)
                .poll_change(&mut core::task::Context::from_waker(&noop_waker())),
            Poll::Pending,
            "whitespace-only copy-on-select should not queue an OSC 52 clipboard write"
        );

        let stdout = use_output.use_stdout();
        stdout.set_clipboard("copy");
        assert_eq!(
            Pin::new(&mut use_output)
                .poll_change(&mut core::task::Context::from_waker(&noop_waker())),
            Poll::Ready(())
        );

        let mut canvas = Canvas::new(8, 1);
        canvas
            .subview_mut(0, 0, 0, 0, 8, 1)
            .set_text(0, 0, "copy", CanvasTextStyle::default());
        let mut selection = SelectionState::new();
        selection.start(1, 0);
        selection.update(3, 0);
        assert_eq!(stdout.copy_selection_no_clear(&selection, &canvas), "opy");
        assert!(selection.has_selection());
        assert_eq!(stdout.copy_selection(&mut selection, &canvas), "opy");
        assert!(!selection.has_selection());

        let mut controller = SelectionController::new();
        controller.selection_mut().start(1, 0);
        controller.selection_mut().update(3, 0);
        controller.selection_mut().finish();
        assert_eq!(
            stdout.copy_on_select(&mut controller, &canvas).as_deref(),
            Some("opy")
        );
        assert_eq!(stdout.copy_on_select(&mut controller, &canvas), None);
        assert_eq!(
            Pin::new(&mut use_output)
                .poll_change(&mut core::task::Context::from_waker(&noop_waker())),
            Poll::Ready(())
        );

        stdout.println("Hello, world!");
        assert_eq!(
            Pin::new(&mut use_output)
                .poll_change(&mut core::task::Context::from_waker(&noop_waker())),
            Poll::Ready(())
        );

        let stderr = use_output.use_stderr();
        stderr.println("Hello, error!");
        assert_eq!(
            Pin::new(&mut use_output)
                .poll_change(&mut core::task::Context::from_waker(&noop_waker())),
            Poll::Ready(())
        );

        // Test print methods
        stdout.print("Hello, ");
        stdout.print("world!");
        stderr.print("Error: ");
        stderr.print("test");
        stderr.print("Warning: ");
        stderr.print("print test");
        assert_eq!(
            Pin::new(&mut use_output)
                .poll_change(&mut core::task::Context::from_waker(&noop_waker())),
            Poll::Ready(())
        );
    }

    #[component]
    fn MyComponent(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let (stdout, stderr) = hooks.use_output();
        stdout.println("Hello, world!");
        stderr.println("Hello, error!");
        stdout.print("Testing ");
        stdout.print("print ");
        stdout.println("method!");
        stderr.print("Error: ");
        stderr.println("test");
        stderr.print("Warning: ");
        stderr.println("print test");
        system.exit();
        element!(View)
    }

    #[apply(test!)]
    async fn test_use_output() {
        element!(MyComponent).render_loop().await.unwrap();
    }
}
