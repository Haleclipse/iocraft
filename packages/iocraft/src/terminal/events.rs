use super::*;

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

pub(super) struct TerminalEventsInner {
    pub(super) pending: VecDeque<(TerminalEvent, Arc<SharedEventState>)>,
    pub(super) waker: Option<Waker>,
}

/// A stream of terminal events.
pub struct TerminalEvents {
    pub(super) inner: Arc<Mutex<TerminalEventsInner>>,
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
pub(super) struct EventCellSnapshot {
    width: usize,
    height: usize,
    blank: Vec<bool>,
}

impl EventCellSnapshot {
    pub(super) fn from_canvas(canvas: &Canvas) -> Self {
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

    pub(super) fn cell_is_blank(&self, column: u16, row: u16) -> bool {
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
