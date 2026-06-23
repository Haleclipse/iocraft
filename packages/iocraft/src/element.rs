use crate::{
    any_key::AnyKey,
    component::{Component, ComponentHelper, ComponentHelperExt},
    mock_terminal_render_loop, mock_terminal_render_loop_with_profile,
    props::AnyProps,
    render, terminal_render_loop, Canvas, FrameProfileCallback, MockTerminalConfig,
    RenderFrameProfile, Terminal, TextMatchPosition,
};
use crossterm::terminal;
use futures::Stream;
use std::{
    fmt::Debug,
    future::Future,
    hash::Hash,
    io::{self, stderr, stdout, IsTerminal, LineWriter, Write},
    pin::Pin,
    sync::Arc,
};

/// Used by the `element!` macro to extend a collection with elements.
#[doc(hidden)]
pub trait ExtendWithElements<T>: Sized {
    fn extend_with_elements<E: Extend<T>>(self, dest: &mut E);
}

impl<'a, T, U> ExtendWithElements<T> for Element<'a, U>
where
    U: ElementType + 'a,
    T: From<Element<'a, U>>,
{
    fn extend_with_elements<E: Extend<T>>(self, dest: &mut E) {
        dest.extend([self.into()]);
    }
}

impl<'a> ExtendWithElements<AnyElement<'a>> for AnyElement<'a> {
    fn extend_with_elements<E: Extend<AnyElement<'a>>>(self, dest: &mut E) {
        dest.extend([self]);
    }
}

impl<T, U, I> ExtendWithElements<T> for I
where
    I: IntoIterator<Item = U>,
    U: Into<T>,
{
    fn extend_with_elements<E: Extend<T>>(self, dest: &mut E) {
        dest.extend(self.into_iter().map(|e| e.into()));
    }
}

/// Used by the `element!` macro to extend a collection with elements.
#[doc(hidden)]
pub fn extend_with_elements<T, U, E>(dest: &mut T, elements: U)
where
    T: Extend<E>,
    U: ExtendWithElements<E>,
{
    elements.extend_with_elements(dest);
}

/// Used to identify an element within the scope of its parent. This is used to minimize the number
/// of times components are destroyed and recreated from render-to-render.
#[derive(Clone, Hash, PartialEq, Eq, Debug)]
pub struct ElementKey(Arc<Box<dyn AnyKey + Send + Sync>>);

impl ElementKey {
    /// Constructs a new key.
    pub fn new<K: Debug + Hash + Eq + Send + Sync + 'static>(key: K) -> Self {
        Self(Arc::new(Box::new(key)))
    }
}

/// An element is a description of an uninstantiated component, including its key and properties.
#[derive(Clone)]
pub struct Element<'a, T: ElementType + 'a> {
    /// The key of the element.
    pub key: ElementKey,
    /// The properties of the element.
    pub props: T::Props<'a>,
}

/// A trait implemented by all element types to define the properties that can be passed to them.
///
/// This trait is automatically implemented for all types that implement [`Component`].
pub trait ElementType {
    /// The type of the properties that can be passed to the element.
    type Props<'a>
    where
        Self: 'a;
}

/// A type-erased element that can be created from any [`Element`].
pub struct AnyElement<'a> {
    key: ElementKey,
    props: AnyProps<'a>,
    helper: Box<dyn ComponentHelperExt>,
}

impl<'a, T> Element<'a, T>
where
    T: Component + 'a,
{
    /// Converts the element into an [`AnyElement`].
    pub fn into_any(self) -> AnyElement<'a> {
        self.into()
    }
}

impl<'a, T> From<Element<'a, T>> for AnyElement<'a>
where
    T: Component + 'a,
{
    fn from(e: Element<'a, T>) -> Self {
        Self {
            key: e.key,
            props: AnyProps::owned(e.props),
            helper: ComponentHelper::<T>::boxed(),
        }
    }
}

impl<'a, 'b: 'a, T> From<&'a mut Element<'b, T>> for AnyElement<'a>
where
    T: Component,
{
    fn from(e: &'a mut Element<'b, T>) -> Self {
        Self {
            key: e.key.clone(),
            props: AnyProps::borrowed(&mut e.props),
            helper: ComponentHelper::<T>::boxed(),
        }
    }
}

impl<'a, 'b: 'a> From<&'a mut AnyElement<'b>> for AnyElement<'b> {
    fn from(e: &'a mut AnyElement<'b>) -> Self {
        Self {
            key: e.key.clone(),
            props: e.props.borrow(),
            helper: e.helper.copy(),
        }
    }
}

mod private {
    use super::*;

    pub trait Sealed {}
    impl Sealed for AnyElement<'_> {}
    impl Sealed for &mut AnyElement<'_> {}
    impl<T> Sealed for Element<'_, T> where T: Component {}
    impl<T> Sealed for &mut Element<'_, T> where T: Component {}
}

/// A trait implemented by all element types, providing methods for common operations on them.
/// Result of rendering an element into an isolated retained screen buffer.
///
/// This is the Rust counterpart to the CC Ink fork's `render-to-screen.ts`:
/// callers can render a subtree off-terminal at a fixed width, inspect its
/// natural height, and scan the retained [`Canvas`] for exact terminal-cell
/// match coordinates. It is mode-neutral and does not enter fullscreen or write
/// to the terminal.
#[derive(Clone)]
pub struct RenderedScreen {
    /// Rendered retained canvas.
    pub canvas: Canvas,
    /// Natural rendered height in rows.
    pub height: usize,
}

impl RenderedScreen {
    /// Scans this screen for a query, returning terminal-cell match positions.
    pub fn scan_positions(&self, query: &str) -> Vec<TextMatchPosition> {
        self.canvas.scan_text_positions(query)
    }
}

/// Common operations available on iocraft elements.
pub trait ElementExt: private::Sealed + Sized {
    /// Returns the key of the element.
    fn key(&self) -> &ElementKey;

    #[doc(hidden)]
    fn props_mut(&mut self) -> AnyProps<'_>;

    #[doc(hidden)]
    fn helper(&self) -> Box<dyn ComponentHelperExt>;

    /// Renders the element into a canvas.
    fn render(&mut self, max_width: Option<usize>) -> Canvas;

    /// Renders the element into an isolated retained screen buffer at a fixed width.
    ///
    /// This mirrors CC Ink's `renderToScreen(el, width)` helper used for
    /// side-rendered search/highlight calculations. Unlike [`Self::render_loop`]
    /// or [`Self::fullscreen`], it is a pure off-terminal render: no terminal
    /// modes are changed, no events are wired, and fullscreen-only capabilities
    /// such as mouse selection are inactive.
    fn render_to_screen(&mut self, width: usize) -> RenderedScreen {
        let mut canvas = self.render(Some(width));
        let height = canvas.height();
        if height == 0 {
            // CC Ink's render-to-screen.ts returns the natural Yoga height but
            // allocates at least one screen row so downstream scanners have a
            // valid retained buffer even for empty elements.
            canvas = Canvas::new(width, 1);
        }
        RenderedScreen { canvas, height }
    }

    /// Renders the element into a string.
    ///
    /// Note that unlike [`std::fmt::Display`] and [`std::string::ToString`], this method requires
    /// the element to be mutable, as it's possible for the properties of the element to change
    /// during rendering.
    fn to_string(&mut self) -> String {
        self.render(None).to_string()
    }

    /// Renders the element and prints it to stdout.
    fn print(&mut self) {
        self.write_to_is_terminal(stdout()).unwrap();
    }

    /// Renders the element and prints it to stderr.
    fn eprint(&mut self) {
        self.write_to_is_terminal(stderr()).unwrap();
    }

    /// Renders the element and writes it to the given writer.
    fn write<W: Write>(&mut self, w: W) -> io::Result<()> {
        let canvas = self.render(None);
        canvas.write(w)
    }

    /// Renders the element and writes it to the given raw file descriptor. If the file descriptor
    /// is a TTY, the canvas will be rendered based on its size, with ANSI escape codes.
    #[cfg(unix)]
    fn write_to_raw_fd<F: Write + std::os::fd::AsRawFd>(&mut self, fd: F) -> io::Result<()> {
        use crossterm::tty::IsTty;
        if fd.is_tty() {
            let (width, _) = terminal::size()?;
            let canvas = self.render(Some(width as _));
            canvas.write_ansi(fd)
        } else {
            self.write(fd)
        }
    }

    /// Renders the element and writes it to the given writer also implementing
    /// [`IsTerminal`](std::io::IsTerminal). If the writer is a terminal, the canvas will be
    /// rendered based on its size, with ANSI escape codes.
    fn write_to_is_terminal<W: Write + IsTerminal>(&mut self, w: W) -> io::Result<()> {
        if w.is_terminal() {
            let (width, _) = terminal::size()?;
            let canvas = self.render(Some(width as _));
            canvas.write_ansi(w)
        } else {
            self.write(w)
        }
    }

    /// Returns a future which renders the element in a loop, allowing it to be dynamic and
    /// interactive.
    ///
    /// This method should only be used when stdio is a TTY terminal. If for example, stdout is a
    /// file, this will probably not produce the desired result. You can determine whether stdout
    /// is a terminal with [`IsTerminal`](std::io::IsTerminal).
    ///
    /// The behavior of the render loop can be configured via the methods on the returned future
    /// before awaiting it.
    fn render_loop(&mut self) -> RenderLoopFuture<'_, Self> {
        RenderLoopFuture::new(self)
    }

    /// Renders the element in a loop using a mock terminal, allowing you to simulate terminal
    /// events for testing purposes.
    ///
    /// A stream of canvases is returned, allowing you to inspect the output of each render pass.
    ///
    /// # Example
    ///
    /// ```
    /// # use iocraft::prelude::*;
    /// # use futures::stream::StreamExt;
    /// # #[component]
    /// # fn MyTextInput() -> impl Into<AnyElement<'static>> {
    /// #     element!(View)
    /// # }
    /// async fn test_text_input() {
    ///     let actual = element!(MyTextInput)
    ///         .mock_terminal_render_loop(MockTerminalConfig::with_events(futures::stream::iter(
    ///             vec![
    ///                 TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Char('f'))),
    ///                 TerminalEvent::Key(KeyEvent::new(KeyEventKind::Release, KeyCode::Char('f'))),
    ///                 TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Char('o'))),
    ///                 TerminalEvent::Key(KeyEvent::new(KeyEventKind::Repeat, KeyCode::Char('o'))),
    ///                 TerminalEvent::Key(KeyEvent::new(KeyEventKind::Release, KeyCode::Char('o'))),
    ///             ],
    ///         )))
    ///         .map(|c| c.to_string())
    ///         .collect::<Vec<_>>()
    ///         .await;
    ///     let expected = vec!["\n", "foo\n"];
    ///     assert_eq!(actual, expected);
    /// }
    /// ```
    fn mock_terminal_render_loop(
        &mut self,
        config: MockTerminalConfig,
    ) -> impl Stream<Item = Canvas> {
        mock_terminal_render_loop(self, config)
    }

    /// Renders the element in a mock terminal and reports per-frame profile data.
    ///
    /// This is useful for deterministic benchmarks and regression tests: it uses
    /// the same profile event shape as [`RenderLoopFuture::on_frame_profile`]
    /// without requiring a real TTY.
    fn mock_terminal_render_loop_with_profile<'a, F>(
        &'a mut self,
        config: MockTerminalConfig,
        callback: F,
    ) -> impl Stream<Item = Canvas> + 'a
    where
        F: FnMut(RenderFrameProfile) + Send + 'a,
    {
        mock_terminal_render_loop_with_profile(self, config, Some(Box::new(callback)))
    }

    /// Renders the element as fullscreen in a loop, allowing it to be dynamic and interactive.
    ///
    /// This method should only be used when stdio is a TTY terminal. If for example, stdout is a
    /// file, this will probably not produce the desired result. You can determine whether stdout
    /// is a terminal with [`IsTerminal`](std::io::IsTerminal).
    ///
    /// This is equivalent to `self.render_loop().fullscreen()`.
    fn fullscreen(&mut self) -> RenderLoopFuture<'_, Self> {
        self.render_loop().fullscreen()
    }
}

/// Specifies which handle to render the TUI to.
#[cfg_attr(not(feature = "unstable-output-streams"), doc(hidden))]
#[cfg_attr(docsrs, doc(cfg(feature = "unstable-output-streams")))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Output {
    /// Render to the stdout handle (default).
    #[default]
    Stdout,
    /// Render to the stderr handle.
    Stderr,
}

#[derive(Default)]
enum RenderLoopFutureState<'a, E: ElementExt> {
    #[default]
    Empty,
    Init {
        fullscreen: bool,
        mouse_capture: Option<bool>,
        ignore_ctrl_c: bool,
        suspend_on_ctrl_z: bool,
        output: Output,
        stdout_writer: Option<Box<dyn Write + Send + 'a>>,
        stderr_writer: Option<Box<dyn Write + Send + 'a>>,
        throttle: Option<std::time::Duration>,
        frame_profile: Option<FrameProfileCallback<'a>>,
        element: &'a mut E,
    },
    Running(Pin<Box<dyn Future<Output = io::Result<()>> + Send + 'a>>),
}

/// The shared default render-loop frame interval.
///
/// This mirrors CC Ink's exported `FRAME_INTERVAL_MS = 16` constant, used for
/// render throttling and animation pacing (~60fps).
pub const FRAME_INTERVAL: std::time::Duration = std::time::Duration::from_millis(16);

/// A future that renders an element in a loop, allowing it to be dynamic and interactive.
///
/// This is created by the [`ElementExt::render_loop`] method.
///
/// Before awaiting the future, you can use its methods to configure its behavior.
pub struct RenderLoopFuture<'a, E: ElementExt + 'a> {
    state: RenderLoopFutureState<'a, E>,
}

impl<'a, E: ElementExt + 'a> RenderLoopFuture<'a, E> {
    pub(crate) fn new(element: &'a mut E) -> Self {
        Self {
            state: RenderLoopFutureState::Init {
                fullscreen: false,
                mouse_capture: None,
                ignore_ctrl_c: false,
                suspend_on_ctrl_z: false,
                output: Output::default(),
                stdout_writer: None,
                stderr_writer: None,
                throttle: Some(FRAME_INTERVAL),
                frame_profile: None,
                element,
            },
        }
    }

    /// Caps the render rate at the given number of frames per second. High-frequency
    /// state updates (animations, streaming output) coalesce into at most one frame
    /// per interval. Defaults to 60fps.
    ///
    /// # Panics
    ///
    /// Panics if `fps` is zero.
    pub fn max_fps(mut self, fps: u32) -> Self {
        assert!(fps > 0, "max_fps must be greater than zero");
        match &mut self.state {
            RenderLoopFutureState::Init { throttle, .. } => {
                *throttle = Some(std::time::Duration::from_secs_f64(1.0 / fps as f64));
            }
            _ => panic!("max_fps() must be called before polling the future"),
        }
        self
    }

    /// Disables frame throttling entirely: every state change triggers an immediate
    /// render. Useful when you need minimal latency and your state update rate is
    /// already bounded.
    pub fn without_throttle(mut self) -> Self {
        match &mut self.state {
            RenderLoopFutureState::Init { throttle, .. } => {
                *throttle = None;
            }
            _ => panic!("without_throttle() must be called before polling the future"),
        }
        self
    }

    /// Registers a callback that receives per-frame render-loop profiling data.
    ///
    /// This is an opt-in benchmarking/debugging hook inspired by CC Ink's
    /// `onFrame` event. It reports update/layout/draw/write timings plus
    /// repaint metadata without changing rendering behavior or writing logs by
    /// itself.
    pub fn on_frame_profile<F>(mut self, callback: F) -> Self
    where
        F: FnMut(RenderFrameProfile) + Send + 'a,
    {
        match &mut self.state {
            RenderLoopFutureState::Init { frame_profile, .. } => {
                *frame_profile = Some(Box::new(callback));
            }
            _ => panic!("on_frame_profile() must be called before polling the future"),
        }
        self
    }

    /// Renders the element as fullscreen in a loop, allowing it to be dynamic and interactive.
    ///
    /// This method should only be used when stdio is a TTY terminal. If for example, stdout is a
    /// file, this will probably not produce the desired result. You can determine whether stdout
    /// is a terminal with [`IsTerminal`](std::io::IsTerminal).
    pub fn fullscreen(mut self) -> Self {
        match &mut self.state {
            RenderLoopFutureState::Init { fullscreen, .. } => {
                *fullscreen = true;
            }
            _ => panic!("fullscreen() must be called before polling the future"),
        }
        self
    }

    /// Enables mouse capture. By default, mouse capture is only enabled in fullscreen mode. Call
    /// this method to enable it in inline mode as well.
    pub fn enable_mouse_capture(mut self) -> Self {
        match &mut self.state {
            RenderLoopFutureState::Init { mouse_capture, .. } => {
                *mouse_capture = Some(true);
            }
            _ => panic!("enable_mouse_capture() must be called before polling the future"),
        }
        self
    }

    /// Disables mouse capture for fullscreen mode. By default, fullscreen mode enables mouse
    /// capture via crossterm's `EnableMouseCapture`. Call this method to opt out.
    pub fn disable_mouse_capture(mut self) -> Self {
        match &mut self.state {
            RenderLoopFutureState::Init { mouse_capture, .. } => {
                *mouse_capture = Some(false);
            }
            _ => panic!("disable_mouse_capture() must be called before polling the future"),
        }
        self
    }

    /// If the terminal is in raw mode, Ctrl-C presses will not trigger the usual interrupt
    /// signals. By default, if the terminal is in raw mode for any reason, iocraft will listen for
    /// Ctrl-C and stop the render loop in response. If you would like to prevent this behavior and
    /// implement your own handling for Ctrl-C, you can call this method.
    pub fn ignore_ctrl_c(mut self) -> Self {
        match &mut self.state {
            RenderLoopFutureState::Init { ignore_ctrl_c, .. } => {
                *ignore_ctrl_c = true;
            }
            _ => panic!("ignore_ctrl_c() must be called before polling the future"),
        }
        self
    }

    /// Enables CC Ink/Claude Code-style Ctrl+Z suspension on Unix.
    ///
    /// This is intentionally opt-in: generic iocraft apps receive Ctrl+Z as
    /// ordinary input by default. When enabled, Ctrl+Z hands the terminal back
    /// to the shell, suspends the process, and forces a full terminal repair and
    /// repaint after the process is foregrounded with `fg`.
    pub fn suspend_on_ctrl_z(mut self) -> Self {
        match &mut self.state {
            RenderLoopFutureState::Init {
                suspend_on_ctrl_z, ..
            } => {
                *suspend_on_ctrl_z = true;
            }
            _ => panic!("suspend_on_ctrl_z() must be called before polling the future"),
        }
        self
    }

    /// Set the stdout handle for hook output and TUI rendering (when output is Stdout).
    ///
    /// See [`output`](Self::output) for known crossterm caveats when mixing streams.
    ///
    /// Default: `std::io::stdout()`
    #[cfg(feature = "unstable-output-streams")]
    #[cfg_attr(docsrs, doc(cfg(feature = "unstable-output-streams")))]
    pub fn stdout<W: Write + Send + 'a>(mut self, writer: W) -> Self {
        match &mut self.state {
            RenderLoopFutureState::Init { stdout_writer, .. } => {
                *stdout_writer = Some(Box::new(writer));
            }
            _ => panic!("stdout() must be called before polling the future"),
        }
        self
    }

    /// Set the stderr handle for hook output and TUI rendering (when output is Stderr).
    ///
    /// See [`output`](Self::output) for known crossterm caveats when mixing streams.
    ///
    /// Default: `LineWriter::new(std::io::stderr())`
    #[cfg(feature = "unstable-output-streams")]
    #[cfg_attr(docsrs, doc(cfg(feature = "unstable-output-streams")))]
    pub fn stderr<W: Write + Send + 'a>(mut self, writer: W) -> Self {
        match &mut self.state {
            RenderLoopFutureState::Init { stderr_writer, .. } => {
                *stderr_writer = Some(Box::new(writer));
            }
            _ => panic!("stderr() must be called before polling the future"),
        }
        self
    }

    /// Choose which handle to render the TUI to.
    ///
    /// When set to [`Output::Stderr`], the TUI will be rendered to the stderr handle.
    /// This is useful for CLI tools that need to pipe stdout to other programs
    /// while still displaying a TUI to the user.
    ///
    /// ## Known crossterm caveats
    ///
    /// Some crossterm operations bypass the provided writer and write directly to
    /// stdout, which can cause issues when stdout is piped:
    ///
    /// - Cursor position queries always write to stdout
    ///   ([#652](https://github.com/crossterm-rs/crossterm/issues/652),
    ///   [#957](https://github.com/crossterm-rs/crossterm/pull/957)).
    /// - Keyboard enhancement queries fall back to stdout on unix
    ///   ([#1026](https://github.com/crossterm-rs/crossterm/pull/1026)).
    ///
    /// Default: [`Output::Stdout`]
    #[cfg(feature = "unstable-output-streams")]
    #[cfg_attr(docsrs, doc(cfg(feature = "unstable-output-streams")))]
    pub fn output(mut self, output: Output) -> Self {
        match &mut self.state {
            RenderLoopFutureState::Init { output: o, .. } => {
                *o = output;
            }
            _ => panic!("output() must be called before polling the future"),
        }
        self
    }
}

impl<'a, E: ElementExt + Send + 'a> Future for RenderLoopFuture<'a, E> {
    type Output = io::Result<()>;

    fn poll(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        loop {
            match &mut self.state {
                RenderLoopFutureState::Init { .. } => {
                    let (
                        fullscreen,
                        mouse_capture,
                        ignore_ctrl_c,
                        suspend_on_ctrl_z,
                        output,
                        stdout_writer,
                        stderr_writer,
                        throttle,
                        frame_profile,
                        element,
                    ) = match std::mem::replace(&mut self.state, RenderLoopFutureState::Empty) {
                        RenderLoopFutureState::Init {
                            fullscreen,
                            mouse_capture,
                            ignore_ctrl_c,
                            suspend_on_ctrl_z,
                            output,
                            stdout_writer,
                            stderr_writer,
                            throttle,
                            frame_profile,
                            element,
                        } => (
                            fullscreen,
                            mouse_capture,
                            ignore_ctrl_c,
                            suspend_on_ctrl_z,
                            output,
                            stdout_writer,
                            stderr_writer,
                            throttle,
                            frame_profile,
                            element,
                        ),
                        _ => unreachable!(),
                    };
                    let effective_mouse_capture = mouse_capture.unwrap_or(fullscreen);
                    let stdout_handle = stdout_writer.unwrap_or_else(|| Box::new(stdout()));
                    // Unlike stdout, stderr is unbuffered by default in the standard library
                    let stderr_handle =
                        stderr_writer.unwrap_or_else(|| Box::new(LineWriter::new(stderr())));

                    let mut terminal = match Terminal::new(
                        stdout_handle,
                        stderr_handle,
                        output,
                        fullscreen,
                        effective_mouse_capture,
                    ) {
                        Ok(t) => t,
                        Err(e) => return std::task::Poll::Ready(Err(e)),
                    };
                    if effective_mouse_capture && !fullscreen {
                        if let Err(e) = terminal.enable_mouse_capture() {
                            return std::task::Poll::Ready(Err(e));
                        }
                    }
                    if ignore_ctrl_c {
                        terminal.ignore_ctrl_c();
                    }
                    if suspend_on_ctrl_z {
                        terminal.suspend_on_ctrl_z();
                    }
                    let fut = Box::pin(terminal_render_loop(
                        element,
                        terminal,
                        throttle,
                        frame_profile,
                    ));
                    self.state = RenderLoopFutureState::Running(fut);
                }
                RenderLoopFutureState::Running(fut) => {
                    return fut.as_mut().poll(cx);
                }
                RenderLoopFutureState::Empty => {
                    panic!("polled after completion");
                }
            }
        }
    }
}

impl ElementExt for AnyElement<'_> {
    fn key(&self) -> &ElementKey {
        &self.key
    }

    fn props_mut(&mut self) -> AnyProps<'_> {
        self.props.borrow()
    }

    #[doc(hidden)]
    fn helper(&self) -> Box<dyn ComponentHelperExt> {
        self.helper.copy()
    }

    fn render(&mut self, max_width: Option<usize>) -> Canvas {
        render(self, max_width)
    }
}

impl ElementExt for &mut AnyElement<'_> {
    fn key(&self) -> &ElementKey {
        &self.key
    }

    fn props_mut(&mut self) -> AnyProps<'_> {
        self.props.borrow()
    }

    #[doc(hidden)]
    fn helper(&self) -> Box<dyn ComponentHelperExt> {
        self.helper.copy()
    }

    fn render(&mut self, max_width: Option<usize>) -> Canvas {
        render(&mut **self, max_width)
    }
}

impl<T> ElementExt for Element<'_, T>
where
    T: Component + 'static,
{
    fn key(&self) -> &ElementKey {
        &self.key
    }

    fn props_mut(&mut self) -> AnyProps<'_> {
        AnyProps::borrowed(&mut self.props)
    }

    #[doc(hidden)]
    fn helper(&self) -> Box<dyn ComponentHelperExt> {
        ComponentHelper::<T>::boxed()
    }

    fn render(&mut self, max_width: Option<usize>) -> Canvas {
        render(self, max_width)
    }
}

impl<T> ElementExt for &mut Element<'_, T>
where
    T: Component + 'static,
{
    fn key(&self) -> &ElementKey {
        &self.key
    }

    fn props_mut(&mut self) -> AnyProps<'_> {
        AnyProps::borrowed(&mut self.props)
    }

    #[doc(hidden)]
    fn helper(&self) -> Box<dyn ComponentHelperExt> {
        ComponentHelper::<T>::boxed()
    }

    fn render(&mut self, max_width: Option<usize>) -> Canvas {
        render(&mut **self, max_width)
    }
}

#[cfg(test)]
mod tests {
    use crate::prelude::*;
    use futures::Future;

    #[allow(clippy::unnecessary_mut_passed)]
    #[test]
    fn test_element() {
        let mut view_element = element!(View);
        view_element.key();
        view_element.print();
        view_element.eprint();
        #[allow(clippy::needless_borrow)]
        {
            (&mut view_element).key();
            (&mut view_element).print();
            (&mut view_element).eprint();
        }

        #[cfg(unix)]
        view_element.write_to_raw_fd(std::io::stdout()).unwrap();

        let mut any_element: AnyElement<'static> = view_element.into_any();
        any_element.key();
        any_element.print();
        any_element.eprint();
        #[allow(clippy::needless_borrow)]
        {
            (&mut any_element).key();
            (&mut any_element).print();
            (&mut any_element).eprint();
        }

        let mut view_element = element!(View);
        let mut any_element_ref: AnyElement = (&mut view_element).into();
        any_element_ref.print();
        any_element_ref.eprint();
    }

    #[test]
    fn test_render_to_screen_matches_cc_ink_side_render_helper() {
        let mut element = element! {
            View(flex_direction: FlexDirection::Column) {
                Text(content: "alpha")
                Text(content: "lazy 中c")
            }
        };
        let screen = element.render_to_screen(12);

        assert_eq!(screen.canvas.width(), 12);
        assert_eq!(screen.height, screen.canvas.height());
        assert_eq!(screen.canvas.to_string(), "alpha\nlazy 中c\n");
        assert_eq!(
            screen.scan_positions("中c"),
            vec![TextMatchPosition {
                row: 1,
                col: 5,
                len: 3,
            }]
        );

        let mut empty = element!(Text);
        let empty_screen = empty.render_to_screen(12);
        assert_eq!(empty_screen.height, 0);
        assert_eq!(empty_screen.canvas.width(), 12);
        assert_eq!(empty_screen.canvas.height(), 1);
        assert!(empty_screen.scan_positions("anything").is_empty());
    }

    #[test]
    fn test_render_loop_future() {
        fn assert_send<F: Future + Send>(_f: F) {}

        assert_eq!(FRAME_INTERVAL, std::time::Duration::from_millis(16));

        let mut element = element!(View);
        let render_loop_future = element.render_loop();
        assert_send(render_loop_future);
    }
}
