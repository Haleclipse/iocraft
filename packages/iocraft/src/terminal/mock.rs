use super::backend::TerminalImpl;
use super::*;

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
    /// Opt-in retained-canvas diff planning mode for render-loop tests.
    pub canvas_diff_planning: TerminalDiffPlanning,
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
            canvas_diff_planning: TerminalDiffPlanning::Baseline,
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

    /// Sets the opt-in retained-canvas diff planning mode for this mock render loop.
    pub fn with_canvas_diff_planning(mut self, planning: TerminalDiffPlanning) -> Self {
        self.canvas_diff_planning = planning;
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
            canvas_diff_planning: TerminalDiffPlanning::Baseline,
        }
    }
}

pub(super) struct MockTerminal {
    config: MockTerminalConfig,
    output: mpsc::UnboundedSender<Canvas>,
    dummy_dest: io::Sink,
    dummy_alt: io::Sink,
    size: Option<(u16, u16)>,
    pub(super) fullscreen: bool,
    raw_mode_enabled: bool,
    resumed: bool,
}

impl MockTerminal {
    pub(super) fn new(config: MockTerminalConfig) -> (Self, MockTerminalOutputStream) {
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
