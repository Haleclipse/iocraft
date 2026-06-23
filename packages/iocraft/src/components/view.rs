use super::raw_ansi::parse_ansi_line;
use crate::{
    hooks::UseTerminalEvents, AnyElement, CanvasSubviewMut, CanvasTextStyle, Color, Component,
    ComponentDrawer, ComponentUpdater, Context, Edges, FocusChange, FocusContext, FocusId,
    FocusOptions, FullscreenMouseEvent, HandlerMut, Hooks, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, MouseButton, MouseEventKind, Props, TerminalEvent, Weight,
};
use iocraft_macros::with_layout_style_props;
use std::{
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::Instant,
};
use taffy::{LengthPercentage, Rect};

/// A border style which can be applied to a [`View`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BorderStyle {
    /// No border.
    #[default]
    None,
    /// A single-line border with 90-degree corners.
    Single,
    /// A double-line border with 90-degree corners.
    Double,
    /// A single-line border with rounded corners.
    Round,
    /// A single-line border with bold lines and 90-degree corners.
    Bold,
    /// A double-line border on the left and right with a single-line border on the top and bottom.
    DoubleLeftRight,
    /// A double-line border on the top and bottom with a single-line border on the left and right.
    DoubleTopBottom,
    /// A dashed border matching CC Ink's custom dashed style.
    Dashed,
    /// A simple border consisting of basic ASCII characters.
    Classic,
    /// A custom border, rendered with characters of your choice.
    Custom(BorderCharacters),
}

/// The characters used to render a custom border for a [`View`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BorderCharacters {
    /// The character used for the top-left corner.
    pub top_left: char,
    /// The character used for the top-right corner.
    pub top_right: char,
    /// The character used for the bottom-left corner.
    pub bottom_left: char,
    /// The character used for the bottom-right corner.
    pub bottom_right: char,
    /// The character used for the left edge.
    pub left: char,
    /// The character used for the right edge.
    pub right: char,
    /// The character used for the top edge.
    pub top: char,
    /// The character used for the bottom edge.
    pub bottom: char,
}

/// Which horizontal border should contain [`BorderText`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BorderTextPosition {
    /// Embed text in the top border.
    #[default]
    Top,
    /// Embed text in the bottom border.
    Bottom,
}

/// Alignment for text embedded in a border.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BorderTextAlign {
    /// Align after the leading corner, plus [`BorderText::offset`].
    #[default]
    Start,
    /// Center within the border line.
    Center,
    /// Align before the trailing corner, minus [`BorderText::offset`].
    End,
}

/// Text embedded in the top or bottom border of a [`View`].
///
/// This mirrors CC Ink's `borderText` option. The border fill before/after the
/// text keeps the view's `border_color`; the text itself is rendered with the
/// default text style so callers can distinguish labels from the border chrome.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BorderText {
    /// Text to embed in the border.
    pub content: String,
    /// Whether the top or bottom border contains the text.
    pub position: BorderTextPosition,
    /// Horizontal alignment within the border.
    pub align: BorderTextAlign,
    /// Extra cells from the edge for start/end alignment.
    pub offset: usize,
}

/// A stable identifier for a mounted [`View`] instance.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ViewId(u64);

impl ViewId {
    /// Returns the underlying integer value. Useful for tests/logging only.
    pub fn as_u64(self) -> u64 {
        self.0
    }
}

static NEXT_VIEW_ID: AtomicU64 = AtomicU64::new(1);

fn next_view_id() -> ViewId {
    ViewId(NEXT_VIEW_ID.fetch_add(1, Ordering::Relaxed))
}

/// Mouse click event passed to [`ViewProps::on_click`].
///
/// The event is emitted on left-button release after a matching press inside
/// the view, as long as the pointer was not dragged. Coordinates are 0-indexed;
/// `column`/`row` are terminal screen coordinates and `local_column`/`local_row`
/// are relative to the view that is currently handling the bubbled click.
#[derive(Clone, Debug)]
pub struct ViewClickEvent {
    /// 0-indexed terminal screen column of the click release.
    pub column: u16,
    /// 0-indexed terminal screen row of the click release.
    pub row: u16,
    /// Click column relative to the current view.
    pub local_column: u16,
    /// Click row relative to the current view.
    pub local_row: u16,
    /// True if the clicked terminal cell was blank in the retained fullscreen
    /// screen buffer. Mirrors CC Ink's `ClickEvent.cellIsBlank`.
    pub cell_is_blank: bool,
    /// View id that originally received the click.
    pub target: Option<ViewId>,
    /// View id for the handler currently running.
    pub current_target: Option<ViewId>,
    /// Dispatch phase for this handler invocation. Click currently uses
    /// `AtTarget` and `Bubbling`, matching CC Ink's click dispatch.
    pub phase: ViewEventPhase,
    stopped: Arc<AtomicBool>,
}

impl ViewClickEvent {
    fn new(column: u16, row: u16, local_column: u16, local_row: u16, cell_is_blank: bool) -> Self {
        Self {
            column,
            row,
            local_column,
            local_row,
            cell_is_blank,
            target: None,
            current_target: None,
            phase: ViewEventPhase::AtTarget,
            stopped: Arc::new(AtomicBool::new(false)),
        }
    }

    fn for_dispatch(
        mut self,
        phase: ViewEventPhase,
        target: Option<ViewId>,
        current_target: Option<ViewId>,
        local_column: u16,
        local_row: u16,
    ) -> Self {
        self.phase = phase;
        self.target = target;
        self.current_target = current_target;
        self.local_column = local_column;
        self.local_row = local_row;
        self
    }

    /// Prevents ancestor [`View`] click handlers from observing this click.
    ///
    /// This mirrors CC Ink's `ClickEvent.stopImmediatePropagation()` for the
    /// subset of terminal click events routed through `View`.
    pub fn stop_immediate_propagation(&self) {
        self.stopped.store(true, Ordering::SeqCst);
    }

    /// Returns `true` if [`Self::stop_immediate_propagation`] was called.
    pub fn did_stop_immediate_propagation(&self) -> bool {
        self.stopped.load(Ordering::SeqCst)
    }
}

/// Dispatch phase for `View` terminal events.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewEventPhase {
    /// Capture handler on an ancestor of the focused/target view.
    Capturing,
    /// Handler on the focused/target view itself.
    AtTarget,
    /// Bubble handler on an ancestor of the focused/target view.
    Bubbling,
}

/// Keyboard event passed to [`ViewProps::on_key_down`].
///
/// This mirrors CC Ink's `KeyboardEvent` shape for focused `View`s while keeping
/// crossterm's raw key fields available. Printable keys use the literal
/// character in [`Self::key`]; special keys use names such as `"return"`,
/// `"escape"`, `"left"`, or `"f1"`.
#[derive(Clone, Debug)]
pub struct ViewKeyboardEvent {
    /// CC Ink/DOM-style event type (`"keydown"`).
    pub event_type: &'static str,
    /// Monotonic timestamp captured when this event was created.
    pub time_stamp: Instant,
    /// Whether this event bubbles through ancestor views.
    pub bubbles: bool,
    /// Whether [`prevent_default`](Self::prevent_default) can mark this event
    /// as handled.
    pub cancelable: bool,
    /// Original terminal key event.
    pub raw: KeyEvent,
    /// CC Ink-style key string.
    pub key: String,
    /// Original key code.
    pub code: KeyCode,
    /// Original key modifiers.
    pub modifiers: KeyModifiers,
    /// Press/repeat kind. Release events are not dispatched to `on_key_down`.
    pub kind: KeyEventKind,
    /// Whether Ctrl/Control was held.
    pub ctrl: bool,
    /// Whether Alt/Option was held.
    pub alt: bool,
    /// CC Ink-style alias for Alt/Option.
    pub meta: bool,
    /// Whether Shift was held.
    pub shift: bool,
    /// Whether Super/Cmd/Meta was held.
    pub super_key: bool,
    /// Whether this is an F-key.
    pub fn_key: bool,
    /// Focus id that originally received the key event.
    pub target: Option<FocusId>,
    /// Focus id for the view whose handler is currently running.
    pub current_target: Option<FocusId>,
    /// View id that originally received the key event.
    pub target_view: Option<ViewId>,
    /// View id for the handler currently running.
    pub current_target_view: Option<ViewId>,
    /// Dispatch phase for this handler invocation.
    pub phase: ViewEventPhase,
    stopped: Arc<AtomicBool>,
    immediate_stopped: Arc<AtomicBool>,
    default_prevented: Arc<AtomicBool>,
}

impl ViewKeyboardEvent {
    fn new(raw: &KeyEvent) -> Self {
        Self {
            event_type: "keydown",
            time_stamp: Instant::now(),
            bubbles: true,
            cancelable: true,
            raw: raw.clone(),
            key: key_name(raw.code),
            code: raw.code,
            modifiers: raw.modifiers,
            kind: raw.kind,
            ctrl: raw.modifiers.contains(KeyModifiers::CONTROL),
            alt: raw.modifiers.contains(KeyModifiers::ALT),
            meta: raw.modifiers.contains(KeyModifiers::ALT),
            shift: raw.modifiers.contains(KeyModifiers::SHIFT),
            super_key: raw.modifiers.contains(KeyModifiers::SUPER),
            fn_key: matches!(raw.code, KeyCode::F(_)),
            target: None,
            current_target: None,
            target_view: None,
            current_target_view: None,
            phase: ViewEventPhase::AtTarget,
            stopped: Arc::new(AtomicBool::new(false)),
            immediate_stopped: Arc::new(AtomicBool::new(false)),
            default_prevented: Arc::new(AtomicBool::new(false)),
        }
    }

    fn for_dispatch(
        mut self,
        phase: ViewEventPhase,
        target: Option<FocusId>,
        current_target: Option<FocusId>,
        target_view: Option<ViewId>,
        current_target_view: Option<ViewId>,
    ) -> Self {
        self.phase = phase;
        self.target = target;
        self.current_target = current_target;
        self.target_view = target_view;
        self.current_target_view = current_target_view;
        self
    }

    /// Stops ancestor propagation-aware listeners from observing this key.
    pub fn stop_propagation(&self) {
        self.stopped.store(true, Ordering::SeqCst);
    }

    /// Stops later handlers immediately, including other handlers on the same target.
    pub fn stop_immediate_propagation(&self) {
        self.immediate_stopped.store(true, Ordering::SeqCst);
        self.stop_propagation();
    }

    /// Returns whether immediate propagation has been stopped.
    pub fn did_stop_immediate_propagation(&self) -> bool {
        self.immediate_stopped.load(Ordering::SeqCst)
    }

    /// Returns whether propagation has been stopped.
    pub fn is_propagation_stopped(&self) -> bool {
        self.stopped.load(Ordering::SeqCst)
    }

    /// Marks the default action as prevented.
    pub fn prevent_default(&self) {
        if self.cancelable {
            self.default_prevented.store(true, Ordering::SeqCst);
        }
    }

    /// Returns whether [`Self::prevent_default`] was called.
    pub fn default_prevented(&self) -> bool {
        self.default_prevented.load(Ordering::SeqCst)
    }
}

/// Paste event passed to [`ViewProps::on_paste`].
#[derive(Clone, Debug)]
pub struct ViewPasteEvent {
    /// CC Ink/DOM-style event type (`"paste"`).
    pub event_type: &'static str,
    /// Monotonic timestamp captured when this event was created.
    pub time_stamp: Instant,
    /// Whether this event bubbles through ancestor views.
    pub bubbles: bool,
    /// Whether [`prevent_default`](Self::prevent_default) can mark this event
    /// as handled.
    pub cancelable: bool,
    /// Pasted text.
    pub text: String,
    /// Focus id that originally received the paste event.
    pub target: Option<FocusId>,
    /// Focus id for the view whose handler is currently running.
    pub current_target: Option<FocusId>,
    /// View id that originally received the paste event.
    pub target_view: Option<ViewId>,
    /// View id for the handler currently running.
    pub current_target_view: Option<ViewId>,
    /// Dispatch phase for this handler invocation.
    pub phase: ViewEventPhase,
    stopped: Arc<AtomicBool>,
    immediate_stopped: Arc<AtomicBool>,
    default_prevented: Arc<AtomicBool>,
}

impl ViewPasteEvent {
    fn new(text: String) -> Self {
        Self {
            event_type: "paste",
            time_stamp: Instant::now(),
            bubbles: true,
            cancelable: true,
            text,
            target: None,
            current_target: None,
            target_view: None,
            current_target_view: None,
            phase: ViewEventPhase::AtTarget,
            stopped: Arc::new(AtomicBool::new(false)),
            immediate_stopped: Arc::new(AtomicBool::new(false)),
            default_prevented: Arc::new(AtomicBool::new(false)),
        }
    }

    fn for_dispatch(
        mut self,
        phase: ViewEventPhase,
        target: Option<FocusId>,
        current_target: Option<FocusId>,
        target_view: Option<ViewId>,
        current_target_view: Option<ViewId>,
    ) -> Self {
        self.phase = phase;
        self.target = target;
        self.current_target = current_target;
        self.target_view = target_view;
        self.current_target_view = current_target_view;
        self
    }

    /// Stops ancestor propagation-aware listeners from observing this paste.
    pub fn stop_propagation(&self) {
        self.stopped.store(true, Ordering::SeqCst);
    }

    /// Stops later handlers immediately, including other handlers on the same target.
    pub fn stop_immediate_propagation(&self) {
        self.immediate_stopped.store(true, Ordering::SeqCst);
        self.stop_propagation();
    }

    /// Returns whether immediate propagation has been stopped.
    pub fn did_stop_immediate_propagation(&self) -> bool {
        self.immediate_stopped.load(Ordering::SeqCst)
    }

    /// Returns whether propagation has been stopped.
    pub fn is_propagation_stopped(&self) -> bool {
        self.stopped.load(Ordering::SeqCst)
    }

    /// Marks the default action as prevented.
    pub fn prevent_default(&self) {
        if self.cancelable {
            self.default_prevented.store(true, Ordering::SeqCst);
        }
    }

    /// Returns whether [`Self::prevent_default`] was called.
    pub fn default_prevented(&self) -> bool {
        self.default_prevented.load(Ordering::SeqCst)
    }
}

/// Resize event passed to [`ViewProps::on_resize`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ViewResizeEvent {
    /// Terminal columns after resize.
    pub columns: u16,
    /// Terminal rows after resize.
    pub rows: u16,
}

fn key_name(code: KeyCode) -> String {
    match code {
        KeyCode::Backspace => "backspace".to_string(),
        KeyCode::Enter => "return".to_string(),
        KeyCode::Left => "left".to_string(),
        KeyCode::Right => "right".to_string(),
        KeyCode::Up => "up".to_string(),
        KeyCode::Down => "down".to_string(),
        KeyCode::Home => "home".to_string(),
        KeyCode::End => "end".to_string(),
        KeyCode::PageUp => "pageup".to_string(),
        KeyCode::PageDown => "pagedown".to_string(),
        KeyCode::Tab | KeyCode::BackTab => "tab".to_string(),
        KeyCode::Delete => "delete".to_string(),
        KeyCode::Insert => "insert".to_string(),
        KeyCode::F(n) => format!("f{n}"),
        KeyCode::Char(ch) => ch.to_string(),
        KeyCode::Esc => "escape".to_string(),
        KeyCode::Null => String::new(),
        other => format!("{other:?}").to_lowercase(),
    }
}

/// Focus event kind passed to [`ViewProps::on_focus`] / [`ViewProps::on_blur`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewFocusEventKind {
    /// Focus moved to this view.
    Focus,
    /// Focus moved away from this view.
    Blur,
}

/// Focus/blur event passed to `View` focus handlers.
///
/// `related_target` mirrors CC Ink's `FocusEvent.relatedTarget`: for focus
/// events it is the previously focused view id, and for blur events it is the
/// newly focused view id.
#[derive(Clone, Debug)]
pub struct ViewFocusEvent {
    /// CC Ink/DOM-style event type (`"focus"` or `"blur"`).
    pub event_type: &'static str,
    /// Monotonic timestamp captured when this event was created.
    pub time_stamp: Instant,
    /// Whether this event bubbles through ancestor views.
    pub bubbles: bool,
    /// Focus/blur events mirror CC Ink's non-cancelable `FocusEvent`.
    pub cancelable: bool,
    /// Event kind.
    pub kind: ViewFocusEventKind,
    /// Focus id that originally received the focus/blur event.
    pub target: Option<FocusId>,
    /// Focus id for the view whose handler is currently running.
    pub current_target: Option<FocusId>,
    /// View id that originally received the focus/blur event.
    pub target_view: Option<ViewId>,
    /// View id for the handler currently running.
    pub current_target_view: Option<ViewId>,
    /// Previous/new focus target, depending on [`Self::kind`].
    pub related_target: Option<FocusId>,
    /// Dispatch phase for this handler invocation.
    pub phase: ViewEventPhase,
    stopped: Arc<AtomicBool>,
    immediate_stopped: Arc<AtomicBool>,
    default_prevented: Arc<AtomicBool>,
}

impl ViewFocusEvent {
    fn from_change(
        kind: ViewFocusEventKind,
        target: FocusId,
        target_view: ViewId,
        change: Option<FocusChange>,
    ) -> Self {
        let related_target = match kind {
            ViewFocusEventKind::Focus => change.and_then(|change| change.previous),
            ViewFocusEventKind::Blur => change.and_then(|change| change.current),
        };
        Self {
            event_type: match kind {
                ViewFocusEventKind::Focus => "focus",
                ViewFocusEventKind::Blur => "blur",
            },
            time_stamp: Instant::now(),
            bubbles: true,
            cancelable: false,
            kind,
            target: Some(target),
            current_target: None,
            target_view: Some(target_view),
            current_target_view: None,
            related_target,
            phase: ViewEventPhase::AtTarget,
            stopped: Arc::new(AtomicBool::new(false)),
            immediate_stopped: Arc::new(AtomicBool::new(false)),
            default_prevented: Arc::new(AtomicBool::new(false)),
        }
    }

    fn for_dispatch(
        mut self,
        phase: ViewEventPhase,
        current_target: Option<FocusId>,
        current_target_view: Option<ViewId>,
    ) -> Self {
        self.phase = phase;
        self.current_target = current_target;
        self.current_target_view = current_target_view;
        self
    }

    /// Stops later focus/blur handlers in the same dispatch chain.
    pub fn stop_propagation(&self) {
        self.stopped.store(true, Ordering::SeqCst);
    }

    /// Stops later handlers immediately, including other handlers on the same target.
    pub fn stop_immediate_propagation(&self) {
        self.immediate_stopped.store(true, Ordering::SeqCst);
        self.stop_propagation();
    }

    /// Returns whether immediate propagation has been stopped.
    pub fn did_stop_immediate_propagation(&self) -> bool {
        self.immediate_stopped.load(Ordering::SeqCst)
    }

    /// Returns whether propagation has been stopped.
    pub fn is_propagation_stopped(&self) -> bool {
        self.stopped.load(Ordering::SeqCst)
    }

    /// Attempts to prevent the default focus action.
    ///
    /// Focus/blur events are not cancelable in CC Ink/DOM semantics, so this
    /// method is intentionally a no-op while still exposing the common
    /// TerminalEvent API shape.
    pub fn prevent_default(&self) {
        if self.cancelable {
            self.default_prevented.store(true, Ordering::SeqCst);
        }
    }

    /// Returns whether [`Self::prevent_default`] was able to mark the event.
    pub fn default_prevented(&self) -> bool {
        self.default_prevented.load(Ordering::SeqCst)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PendingClick {
    column: u16,
    row: u16,
    dragged: bool,
}

#[derive(Clone)]
struct ViewCaptureHandler<T> {
    focus_id: Option<FocusId>,
    view_id: ViewId,
    handler: Arc<Mutex<HandlerMut<'static, T>>>,
}

#[derive(Clone)]
struct ViewHitRecord {
    id: ViewId,
    parent: Option<ViewId>,
    rect: Arc<Mutex<Option<Rect<i32>>>>,
    handler: Option<Arc<Mutex<HandlerMut<'static, ViewClickEvent>>>>,
    focus: Option<(FocusId, FocusContext)>,
}

#[derive(Clone)]
struct ViewHoverRecord {
    id: ViewId,
    parent: Option<ViewId>,
    rect: Arc<Mutex<Option<Rect<i32>>>>,
    on_enter: Option<Arc<Mutex<HandlerMut<'static, ()>>>>,
    on_leave: Option<Arc<Mutex<HandlerMut<'static, ()>>>>,
}

fn view_record_contains(rect: &Arc<Mutex<Option<Rect<i32>>>>, column: u16, row: u16) -> bool {
    rect.lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .as_ref()
        .is_some_and(|rect| {
            (column as i32) >= rect.left
                && (column as i32) < rect.right
                && (row as i32) >= rect.top
                && (row as i32) < rect.bottom
        })
}

fn dispatch_hover(records: &[ViewHoverRecord], hovered: &mut Vec<ViewId>, column: u16, row: u16) {
    let mut next = Vec::new();
    let mut current = records
        .iter()
        .rev()
        .find(|record| view_record_contains(&record.rect, column, row))
        .map(|record| record.id);

    while let Some(id) = current {
        let Some(record) = records.iter().find(|record| record.id == id) else {
            break;
        };
        if record.on_enter.is_some() || record.on_leave.is_some() {
            next.push(id);
        }
        current = record.parent;
    }

    let previous = hovered.clone();
    for old_id in previous.iter().copied() {
        if !next.contains(&old_id) {
            if let Some(record) = records.iter().find(|record| record.id == old_id) {
                if let Some(handler_ref) = &record.on_leave {
                    let mut handler = handler_ref
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    handler(());
                }
            }
        }
    }
    for new_id in next.iter().copied() {
        if !previous.contains(&new_id) {
            if let Some(record) = records.iter().find(|record| record.id == new_id) {
                if let Some(handler_ref) = &record.on_enter {
                    let mut handler = handler_ref
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    handler(());
                }
            }
        }
    }
    *hovered = next;
}

type SharedKeyboardHandler = ViewCaptureHandler<ViewKeyboardEvent>;
type SharedPasteHandler = ViewCaptureHandler<ViewPasteEvent>;
type SharedFocusHandler = ViewCaptureHandler<ViewFocusEvent>;

#[derive(Clone, Default)]
pub(crate) struct ViewFocusParentContext {
    pub(crate) id: Option<FocusId>,
    view_id: Option<ViewId>,
    click_registry: Arc<Mutex<Vec<ViewHitRecord>>>,
    hover_registry: Arc<Mutex<Vec<ViewHoverRecord>>>,
    hovered_views: Arc<Mutex<Vec<ViewId>>>,
    pub(crate) shared_root_event_context: bool,
    focus_capture_handlers: Vec<SharedFocusHandler>,
    focus_handlers: Vec<SharedFocusHandler>,
    blur_capture_handlers: Vec<SharedFocusHandler>,
    blur_handlers: Vec<SharedFocusHandler>,
    key_down_capture_handlers: Vec<SharedKeyboardHandler>,
    key_down_handlers: Vec<SharedKeyboardHandler>,
    paste_capture_handlers: Vec<SharedPasteHandler>,
    paste_handlers: Vec<SharedPasteHandler>,
}

impl ViewFocusParentContext {
    pub(crate) fn shared_root() -> Self {
        let mut context = Self::default();
        context.shared_root_event_context = true;
        context
    }

    pub(crate) fn begin_root_event_frame(&self) {
        self.click_registry
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
        self.hover_registry
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
    }

    pub(crate) fn for_nested_focus_scope(&self) -> Self {
        let mut context = self.clone();
        // A FocusScope owns a new FocusContext, so parent FocusIds from the
        // enclosing scope are not valid for descendants. Keep the DOM-style
        // View parent/handler registries intact so click/hover/key/focus event
        // ancestry still crosses this transparent boundary.
        context.id = None;
        context
    }
}

impl BorderStyle {
    /// Returns `true` if the border style is `None`.
    pub fn is_none(&self) -> bool {
        matches!(self, Self::None)
    }

    /// Returns the characters used to render the border.
    pub fn border_characters(&self) -> Option<BorderCharacters> {
        Some(match self {
            Self::None => return None,
            Self::Single => BorderCharacters {
                top_left: '┌',
                top_right: '┐',
                bottom_left: '└',
                bottom_right: '┘',
                left: '│',
                right: '│',
                top: '─',
                bottom: '─',
            },
            Self::Double => BorderCharacters {
                top_left: '╔',
                top_right: '╗',
                bottom_left: '╚',
                bottom_right: '╝',
                left: '║',
                right: '║',
                top: '═',
                bottom: '═',
            },
            Self::Round => BorderCharacters {
                top_left: '╭',
                top_right: '╮',
                bottom_left: '╰',
                bottom_right: '╯',
                left: '│',
                right: '│',
                top: '─',
                bottom: '─',
            },
            Self::Bold => BorderCharacters {
                top_left: '┏',
                top_right: '┓',
                bottom_left: '┗',
                bottom_right: '┛',
                left: '┃',
                right: '┃',
                top: '━',
                bottom: '━',
            },
            Self::DoubleLeftRight => BorderCharacters {
                top_left: '╓',
                top_right: '╖',
                bottom_left: '╙',
                bottom_right: '╜',
                left: '║',
                right: '║',
                top: '─',
                bottom: '─',
            },
            Self::DoubleTopBottom => BorderCharacters {
                top_left: '╒',
                top_right: '╕',
                bottom_left: '╘',
                bottom_right: '╛',
                left: '│',
                right: '│',
                top: '═',
                bottom: '═',
            },
            Self::Dashed => BorderCharacters {
                top_left: ' ',
                top_right: ' ',
                bottom_left: ' ',
                bottom_right: ' ',
                left: '╎',
                right: '╎',
                top: '╌',
                bottom: '╌',
            },
            Self::Classic => BorderCharacters {
                top_left: '+',
                top_right: '+',
                bottom_left: '+',
                bottom_right: '+',
                left: '|',
                right: '|',
                top: '-',
                bottom: '-',
            },
            Self::Custom(chars) => *chars,
        })
    }
}

/// The props which can be passed to the [`View`] component.
#[non_exhaustive]
#[with_layout_style_props]
#[derive(Default, Props)]
pub struct ViewProps<'a> {
    /// The elements to render inside of the view.
    pub children: Vec<AnyElement<'a>>,

    /// The style of the border. By default, the view will have no border.
    pub border_style: BorderStyle,

    /// The color of the border.
    pub border_color: Option<Color>,

    /// Color of the top border; falls back to [`Self::border_color`].
    pub border_top_color: Option<Color>,

    /// Color of the bottom border; falls back to [`Self::border_color`].
    pub border_bottom_color: Option<Color>,

    /// Color of the left border; falls back to [`Self::border_color`].
    pub border_left_color: Option<Color>,

    /// Color of the right border; falls back to [`Self::border_color`].
    pub border_right_color: Option<Color>,

    /// Dim all borders, matching CC Ink's `borderDimColor` shorthand.
    pub border_dim_color: bool,

    /// Dim the top border; if unset, falls back to [`Self::border_dim_color`].
    pub border_top_dim_color: Option<bool>,

    /// Dim the bottom border; if unset, falls back to [`Self::border_dim_color`].
    pub border_bottom_dim_color: Option<bool>,

    /// Dim the left border; if unset, falls back to [`Self::border_dim_color`].
    pub border_left_dim_color: Option<bool>,

    /// Dim the right border; if unset, falls back to [`Self::border_dim_color`].
    pub border_right_dim_color: Option<bool>,

    /// Text to embed into the top or bottom border.
    pub border_text: Option<BorderText>,

    /// The edges to render the border on. By default, the border will be rendered on all edges.
    pub border_edges: Option<Edges>,

    /// Whether the top border is visible. This is a CC Ink-style alias for
    /// editing [`Self::border_edges`].
    pub border_top: Option<bool>,

    /// Whether the bottom border is visible. This is a CC Ink-style alias for
    /// editing [`Self::border_edges`].
    pub border_bottom: Option<bool>,

    /// Whether the left border is visible. This is a CC Ink-style alias for
    /// editing [`Self::border_edges`].
    pub border_left: Option<bool>,

    /// Whether the right border is visible. This is a CC Ink-style alias for
    /// editing [`Self::border_edges`].
    pub border_right: Option<bool>,

    /// The color of the background.
    pub background_color: Option<Color>,

    /// Fill the view's region with blank cells before rendering children, without
    /// changing the terminal background color.
    ///
    /// This mirrors CC Ink's `opaque` Box style for absolute overlays whose
    /// padding/gaps should hide whatever was rendered behind them while still
    /// using the terminal's default background.
    pub opaque: bool,

    /// Marks this view's cells as excluded from fullscreen text selection.
    ///
    /// This mirrors CC Ink's `noSelect` box metadata. It has no effect on the
    /// terminal output itself and is intended for gutters, line numbers, diff
    /// sigils, list bullets, and other chrome that should not be copied when a
    /// user selects rendered text in an alternate-screen UI.
    pub no_select: bool,

    /// Extends [`Self::no_select`] from terminal column 0 through this view's
    /// right edge on each occupied row.
    ///
    /// Use when the non-selectable gutter is rendered inside an indented
    /// container, matching CC Ink's `noSelect: 'from-left-edge'` behavior.
    pub no_select_from_left_edge: bool,

    /// Called when the user clicks inside this view in fullscreen/alternate-screen mode.
    ///
    /// This is a Rust counterpart to CC Ink's Box `onClick` handler: it fires
    /// on left-button release after a matching press without drag, and bubbles
    /// to ancestor views unless the handler calls
    /// [`ViewClickEvent::stop_immediate_propagation`].
    pub on_click: HandlerMut<'static, ViewClickEvent>,

    /// Called when the mouse first enters this view's rendered rectangle.
    pub on_mouse_enter: HandlerMut<'static, ()>,

    /// Called when the mouse leaves this view's rendered rectangle.
    pub on_mouse_leave: HandlerMut<'static, ()>,

    /// Registers this view with the nearest [`FocusScope`](super::FocusScope)
    /// and includes it in Tab/Shift+Tab traversal.
    ///
    /// This is the legacy iocraft shorthand for CC Ink's `tabIndex={0}`.
    pub focusable: bool,

    /// CC Ink-style tab index. `Some(0)` (or any non-negative value) makes the
    /// view focusable and tabbable; `Some(-1)` makes it programmatically/click
    /// focusable while skipping it during Tab traversal.
    pub tab_index: Option<i32>,

    /// Requests focus on first mount. Like CC Ink's `autoFocus`, this can focus
    /// a view even when it is not in the Tab traversal ring.
    pub auto_focus: bool,

    /// Called during capture phase when focus moves to this view or a descendant view.
    pub on_focus_capture: HandlerMut<'static, ViewFocusEvent>,

    /// Called when focus moves to this view or a descendant view.
    pub on_focus: HandlerMut<'static, ViewFocusEvent>,

    /// Called during capture phase when focus moves away from this view or a descendant view.
    pub on_blur_capture: HandlerMut<'static, ViewFocusEvent>,

    /// Called when focus moves away from this view or a descendant view.
    pub on_blur: HandlerMut<'static, ViewFocusEvent>,

    /// Called during capture phase for key press/repeat events targeting this
    /// view or a focused descendant view.
    pub on_key_down_capture: HandlerMut<'static, ViewKeyboardEvent>,

    /// Called for key press/repeat events while this view or a descendant view is focused.
    pub on_key_down: HandlerMut<'static, ViewKeyboardEvent>,

    /// Called during capture phase for bracketed paste events targeting this
    /// view or a focused descendant view.
    pub on_paste_capture: HandlerMut<'static, ViewPasteEvent>,

    /// Called for bracketed paste events while this view or a descendant view is focused.
    pub on_paste: HandlerMut<'static, ViewPasteEvent>,

    /// Called when the terminal is resized.
    pub on_resize: HandlerMut<'static, ViewResizeEvent>,
}

/// `View` is your most fundamental building block for laying out and styling components.
///
/// # Example
///
/// ```
/// # use iocraft::prelude::*;
/// # fn my_element() -> impl Into<AnyElement<'static>> {
/// element! {
///     View(padding: 2, border_style: BorderStyle::Round) {
///         Text(content: "Hello!")
///     }
/// }
/// # }
/// ```
pub struct View {
    view_id: ViewId,
    border_style: BorderStyle,
    border_top_style: CanvasTextStyle,
    border_bottom_style: CanvasTextStyle,
    border_left_style: CanvasTextStyle,
    border_right_style: CanvasTextStyle,
    border_text: Option<BorderText>,
    border_edges: Edges,
    background_color: Option<Color>,
    opaque: bool,
    no_select: bool,
    no_select_from_left_edge: bool,
    event_rect: Arc<Mutex<Option<Rect<i32>>>>,
    hovered_descendants: Arc<Mutex<Vec<ViewId>>>,
    pending_click: Arc<Mutex<Option<PendingClick>>>,
    focus_ctx: Option<FocusContext>,
    focus_id: Option<FocusId>,
    focus_active: bool,
    focus_tabbable: bool,
    was_focused: bool,
}

impl Default for View {
    fn default() -> Self {
        Self {
            view_id: next_view_id(),
            border_style: BorderStyle::default(),
            border_top_style: CanvasTextStyle::default(),
            border_bottom_style: CanvasTextStyle::default(),
            border_left_style: CanvasTextStyle::default(),
            border_right_style: CanvasTextStyle::default(),
            border_text: None,
            border_edges: Edges::default(),
            background_color: None,
            opaque: false,
            no_select: false,
            no_select_from_left_edge: false,
            event_rect: Arc::new(Mutex::new(None)),
            hovered_descendants: Arc::new(Mutex::new(Vec::new())),
            pending_click: Arc::new(Mutex::new(None)),
            focus_ctx: None,
            focus_id: None,
            focus_active: false,
            focus_tabbable: false,
            was_focused: false,
        }
    }
}

impl Drop for View {
    fn drop(&mut self) {
        if let (Some(ctx), Some(id)) = (self.focus_ctx, self.focus_id) {
            ctx.unregister(id);
        }
    }
}

impl View {
    fn border_edge_style(color: Option<Color>, dim: bool) -> CanvasTextStyle {
        CanvasTextStyle {
            color,
            weight: if dim { Weight::Light } else { Weight::Normal },
            ..Default::default()
        }
    }

    fn repeat_border_char(ch: char, count: usize) -> String {
        std::iter::repeat(ch).take(count).collect()
    }

    fn truncate_to_width(text: &str, max_width: usize) -> String {
        if max_width == 0 {
            return String::new();
        }
        let mut ret = String::new();
        let mut width = 0;
        for grapheme in unicode_segmentation::UnicodeSegmentation::graphemes(text, true) {
            let next = width + crate::canvas::string_display_width(grapheme);
            if next > max_width {
                break;
            }
            ret.push_str(grapheme);
            width = next;
        }
        ret
    }

    fn ansi_visible_text(text: &str) -> String {
        parse_ansi_line(text)
            .into_iter()
            .map(|run| run.text)
            .collect::<Vec<_>>()
            .join("")
    }

    fn ansi_visible_width(text: &str) -> usize {
        parse_ansi_line(text)
            .into_iter()
            .map(|run| crate::canvas::string_display_width(&run.text))
            .sum()
    }

    fn embedded_border_line_segments(
        border_line: &str,
        text: &str,
        align: BorderTextAlign,
        offset: usize,
        border_char: char,
    ) -> (String, String, String) {
        let border_width = crate::canvas::string_display_width(border_line);
        if border_width == 0 {
            return (String::new(), String::new(), String::new());
        }

        let text_width = Self::ansi_visible_width(text);
        if text_width >= border_width.saturating_sub(2) {
            return (
                String::new(),
                Self::truncate_to_width(&Self::ansi_visible_text(text), border_width),
                String::new(),
            );
        }

        let mut position = match align {
            BorderTextAlign::Center => (border_width - text_width) / 2,
            BorderTextAlign::Start => offset + 1,
            BorderTextAlign::End => border_width - text_width - offset - 1,
        };
        position = position.max(1).min(border_width - text_width - 1);

        let first = border_line.chars().next().unwrap_or(border_char);
        let last = border_line.chars().next_back().unwrap_or(border_char);
        let before = format!(
            "{}{}",
            first,
            Self::repeat_border_char(border_char, position.saturating_sub(1))
        );
        let after = format!(
            "{}{}",
            Self::repeat_border_char(border_char, border_width - position - text_width - 1),
            last
        );
        (before, text.to_string(), after)
    }

    fn edge_visibility(mut edges: Edges, visibility: Option<bool>, edge: Edges) -> Edges {
        match visibility {
            Some(true) => edges.insert(edge),
            Some(false) => edges.remove(edge),
            None => {}
        }
        edges
    }

    fn write_border_line(
        canvas: &mut CanvasSubviewMut<'_>,
        y: isize,
        border_line: &str,
        border_char: char,
        border_text: Option<&BorderText>,
        position: BorderTextPosition,
        border_style: CanvasTextStyle,
    ) {
        if let Some(text) = border_text.filter(|text| text.position == position) {
            let (before, content, after) = Self::embedded_border_line_segments(
                border_line,
                &text.content,
                text.align,
                text.offset,
                border_char,
            );
            let mut x = 0isize;
            if !before.is_empty() {
                canvas.set_text(x, y, &before, border_style);
                x += crate::canvas::string_display_width(&before) as isize;
            }
            if !content.is_empty() {
                for run in parse_ansi_line(&content) {
                    let width = crate::canvas::string_display_width(&run.text);
                    if width == 0 {
                        continue;
                    }
                    if let Some(bg) = run.background_color {
                        canvas.set_background_color(x, y, width, 1, bg);
                    }
                    canvas.set_text_with_link(x, y, &run.text, run.style, run.hyperlink.as_deref());
                    x += width as isize;
                }
            }
            if !after.is_empty() {
                canvas.set_text(x, y, &after, border_style);
            }
        } else {
            canvas.set_text(0, y, border_line, border_style);
        }
    }
}

impl Component for View {
    type Props<'a> = ViewProps<'a>;

    fn new(_props: &Self::Props<'_>) -> Self {
        Default::default()
    }

    fn update(
        &mut self,
        props: &mut Self::Props<'_>,
        mut hooks: Hooks,
        updater: &mut ComponentUpdater,
    ) {
        self.border_style = props.border_style;
        self.border_top_style = Self::border_edge_style(
            props.border_top_color.or(props.border_color),
            props.border_top_dim_color.unwrap_or(props.border_dim_color),
        );
        self.border_bottom_style = Self::border_edge_style(
            props.border_bottom_color.or(props.border_color),
            props
                .border_bottom_dim_color
                .unwrap_or(props.border_dim_color),
        );
        self.border_left_style = Self::border_edge_style(
            props.border_left_color.or(props.border_color),
            props
                .border_left_dim_color
                .unwrap_or(props.border_dim_color),
        );
        self.border_right_style = Self::border_edge_style(
            props.border_right_color.or(props.border_color),
            props
                .border_right_dim_color
                .unwrap_or(props.border_dim_color),
        );
        self.border_text = props.border_text.clone();
        let mut border_edges = props.border_edges.unwrap_or(Edges::all());
        border_edges = Self::edge_visibility(border_edges, props.border_top, Edges::Top);
        border_edges = Self::edge_visibility(border_edges, props.border_bottom, Edges::Bottom);
        border_edges = Self::edge_visibility(border_edges, props.border_left, Edges::Left);
        border_edges = Self::edge_visibility(border_edges, props.border_right, Edges::Right);
        self.border_edges = border_edges;
        self.background_color = props.background_color;
        self.opaque = props.opaque;
        self.no_select = props.no_select || props.no_select_from_left_edge;
        self.no_select_from_left_edge = props.no_select_from_left_edge;

        let mut on_resize = props.on_resize.take();
        let wants_resize = !on_resize.is_default();
        hooks.use_terminal_events(move |event| {
            if !wants_resize {
                return;
            }
            if let TerminalEvent::Resize(columns, rows) = event {
                on_resize(ViewResizeEvent { columns, rows });
            }
        });

        let focus_active = props.focusable || props.tab_index.is_some() || props.auto_focus;
        let focus_tabbable = props
            .tab_index
            .map(|tab_index| tab_index >= 0)
            .unwrap_or(props.focusable)
            && focus_active;
        let parent_view_context = updater
            .get_context::<ViewFocusParentContext>()
            .map(|ctx| ctx.clone())
            .unwrap_or_default();
        let parent_focus_id = parent_view_context.id;
        let focus_ctx = updater.get_context::<FocusContext>().map(|ctx| *ctx);
        if let Some(ctx) = focus_ctx {
            let id = match self.focus_id {
                Some(id) => {
                    if self.focus_active != focus_active {
                        ctx.set_entry_active(id, focus_active);
                    }
                    if self.focus_tabbable != focus_tabbable {
                        ctx.set_entry_tabbable(id, focus_tabbable);
                    }
                    id
                }
                None => {
                    let id = ctx.register(FocusOptions {
                        auto_focus: props.auto_focus,
                        is_active: focus_active,
                    });
                    if focus_tabbable != focus_active {
                        ctx.set_entry_tabbable(id, focus_tabbable);
                    }
                    self.focus_id = Some(id);
                    id
                }
            };
            self.focus_active = focus_active;
            self.focus_tabbable = focus_tabbable;
            ctx.set_entry_parent(id, parent_focus_id);
            ctx.note_render_position(id);
            self.focus_ctx = Some(ctx);
        } else if let (Some(ctx), Some(id)) = (self.focus_ctx.take(), self.focus_id.take()) {
            ctx.unregister(id);
            self.focus_active = false;
            self.focus_tabbable = false;
            self.was_focused = false;
        }
        let focused = self
            .focus_id
            .zip(self.focus_ctx)
            .is_some_and(|(id, ctx)| ctx.is_focused(id));

        let on_mouse_enter = props.on_mouse_enter.take();
        let hover_enter_handler =
            (!on_mouse_enter.is_default()).then(|| Arc::new(Mutex::new(on_mouse_enter)));
        let on_mouse_leave = props.on_mouse_leave.take();
        let hover_leave_handler =
            (!on_mouse_leave.is_default()).then(|| Arc::new(Mutex::new(on_mouse_leave)));
        let hover_registry = parent_view_context.hover_registry.clone();
        hover_registry
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(ViewHoverRecord {
                id: self.view_id,
                parent: parent_view_context.view_id,
                rect: self.event_rect.clone(),
                on_enter: hover_enter_handler,
                on_leave: hover_leave_handler,
            });
        let is_hover_dispatch_root = parent_view_context.view_id.is_none();
        let hovered_views =
            if is_hover_dispatch_root && !parent_view_context.shared_root_event_context {
                self.hovered_descendants.clone()
            } else {
                parent_view_context.hovered_views.clone()
            };
        let hovered_views_for_children = hovered_views.clone();
        let hover_registry_for_event = hover_registry.clone();
        let hovered_views_for_event = hovered_views.clone();
        hooks.use_terminal_events(move |event| {
            if !is_hover_dispatch_root {
                return;
            }
            let TerminalEvent::FullscreenMouse(FullscreenMouseEvent {
                column, row, kind, ..
            }) = event
            else {
                return;
            };
            if !matches!(
                kind,
                MouseEventKind::Moved | MouseEventKind::Down(_) | MouseEventKind::Drag(_)
            ) {
                return;
            }
            let records = hover_registry_for_event
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone();
            if !records
                .iter()
                .any(|record| record.on_enter.is_some() || record.on_leave.is_some())
            {
                return;
            }
            let mut hovered = hovered_views_for_event
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            dispatch_hover(&records, &mut hovered, column, row);
        });

        let on_focus_capture = props.on_focus_capture.take();
        let focus_capture_handler = (!on_focus_capture.is_default()).then(|| ViewCaptureHandler {
            focus_id: self.focus_id,
            view_id: self.view_id,
            handler: Arc::new(Mutex::new(on_focus_capture)),
        });
        let focus_capture_for_children = focus_capture_handler.clone();
        let focus_capture_for_event = focus_capture_handler.clone();
        let on_focus = props.on_focus.take();
        let focus_handler = (!on_focus.is_default()).then(|| ViewCaptureHandler {
            focus_id: self.focus_id,
            view_id: self.view_id,
            handler: Arc::new(Mutex::new(on_focus)),
        });
        let focus_for_children = focus_handler.clone();
        let focus_for_event = focus_handler.clone();
        let inherited_focus_captures = parent_view_context.focus_capture_handlers.clone();
        let inherited_focus_handlers = parent_view_context.focus_handlers.clone();

        let on_blur_capture = props.on_blur_capture.take();
        let blur_capture_handler = (!on_blur_capture.is_default()).then(|| ViewCaptureHandler {
            focus_id: self.focus_id,
            view_id: self.view_id,
            handler: Arc::new(Mutex::new(on_blur_capture)),
        });
        let blur_capture_for_children = blur_capture_handler.clone();
        let blur_capture_for_event = blur_capture_handler.clone();
        let on_blur = props.on_blur.take();
        let blur_handler = (!on_blur.is_default()).then(|| ViewCaptureHandler {
            focus_id: self.focus_id,
            view_id: self.view_id,
            handler: Arc::new(Mutex::new(on_blur)),
        });
        let blur_for_children = blur_handler.clone();
        let blur_for_event = blur_handler.clone();
        let inherited_blur_captures = parent_view_context.blur_capture_handlers.clone();
        let inherited_blur_handlers = parent_view_context.blur_handlers.clone();

        let wants_focus_chain = focus_capture_for_event.is_some()
            || focus_for_event.is_some()
            || !inherited_focus_captures.is_empty()
            || !inherited_focus_handlers.is_empty();
        let wants_blur_chain = blur_capture_for_event.is_some()
            || blur_for_event.is_some()
            || !inherited_blur_captures.is_empty()
            || !inherited_blur_handlers.is_empty();
        if self.was_focused != focused && (wants_focus_chain || wants_blur_chain) {
            if let Some((id, ctx)) = self.focus_id.zip(self.focus_ctx) {
                let change = ctx.last_focus_change();
                let (kind, captures, target_capture, target_handler, bubbles) = if focused {
                    (
                        ViewFocusEventKind::Focus,
                        &inherited_focus_captures,
                        &focus_capture_for_event,
                        &focus_for_event,
                        &inherited_focus_handlers,
                    )
                } else {
                    (
                        ViewFocusEventKind::Blur,
                        &inherited_blur_captures,
                        &blur_capture_for_event,
                        &blur_for_event,
                        &inherited_blur_handlers,
                    )
                };
                let focus_event = ViewFocusEvent::from_change(kind, id, self.view_id, change);
                let stopped = focus_event.stopped.clone();
                let immediate_stopped = focus_event.immediate_stopped.clone();
                for capture in captures {
                    if stopped.load(Ordering::SeqCst) || immediate_stopped.load(Ordering::SeqCst) {
                        break;
                    }
                    let mut handler = capture
                        .handler
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    handler(focus_event.clone().for_dispatch(
                        ViewEventPhase::Capturing,
                        capture.focus_id,
                        Some(capture.view_id),
                    ));
                }
                let target_reached =
                    !stopped.load(Ordering::SeqCst) && !immediate_stopped.load(Ordering::SeqCst);
                if target_reached {
                    if let Some(capture) = target_capture {
                        let mut handler = capture
                            .handler
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                        handler(focus_event.clone().for_dispatch(
                            ViewEventPhase::AtTarget,
                            capture.focus_id,
                            Some(capture.view_id),
                        ));
                    }
                }
                if target_reached && !immediate_stopped.load(Ordering::SeqCst) {
                    if let Some(handler_ref) = target_handler {
                        let mut handler = handler_ref
                            .handler
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                        handler(focus_event.clone().for_dispatch(
                            ViewEventPhase::AtTarget,
                            handler_ref.focus_id,
                            Some(handler_ref.view_id),
                        ));
                    }
                }
                if !stopped.load(Ordering::SeqCst) && !immediate_stopped.load(Ordering::SeqCst) {
                    for bubble in bubbles.iter().rev() {
                        if stopped.load(Ordering::SeqCst)
                            || immediate_stopped.load(Ordering::SeqCst)
                        {
                            break;
                        }
                        let mut handler = bubble
                            .handler
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                        handler(focus_event.clone().for_dispatch(
                            ViewEventPhase::Bubbling,
                            bubble.focus_id,
                            Some(bubble.view_id),
                        ));
                    }
                }
            }
        }
        if self.was_focused != focused {
            self.was_focused = focused;
        }

        let on_key_down_capture = props.on_key_down_capture.take();
        let key_capture_handler = (!on_key_down_capture.is_default()).then(|| ViewCaptureHandler {
            focus_id: self.focus_id,
            view_id: self.view_id,
            handler: Arc::new(Mutex::new(on_key_down_capture)),
        });
        let key_capture_for_children = key_capture_handler.clone();
        let key_capture_for_event = key_capture_handler.clone();
        let inherited_key_captures = parent_view_context.key_down_capture_handlers.clone();
        let on_key_down = props.on_key_down.take();
        let key_handler = (!on_key_down.is_default()).then(|| ViewCaptureHandler {
            focus_id: self.focus_id,
            view_id: self.view_id,
            handler: Arc::new(Mutex::new(on_key_down)),
        });
        let key_handler_for_children = key_handler.clone();
        let key_handler_for_event = key_handler.clone();
        let inherited_key_handlers = parent_view_context.key_down_handlers.clone();
        let key_target_view = Some(self.view_id);
        let key_focus = self.focus_id.zip(self.focus_ctx);
        hooks.use_propagated_terminal_events(move |event| {
            let Some((id, ctx)) = key_focus else {
                return;
            };
            if !ctx.is_focused(id) {
                return;
            }
            if let TerminalEvent::Key(key) = event.event() {
                if key.kind != KeyEventKind::Release {
                    let key_event = ViewKeyboardEvent::new(key);
                    if event.is_default_prevented() {
                        key_event.prevent_default();
                    }
                    let stopped = key_event.stopped.clone();
                    let immediate_stopped = key_event.immediate_stopped.clone();
                    let default_prevented = key_event.default_prevented.clone();
                    let target = ctx.active();
                    for capture in &inherited_key_captures {
                        if stopped.load(Ordering::SeqCst)
                            || immediate_stopped.load(Ordering::SeqCst)
                        {
                            break;
                        }
                        let mut handler = capture
                            .handler
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                        handler(key_event.clone().for_dispatch(
                            ViewEventPhase::Capturing,
                            target,
                            capture.focus_id,
                            key_target_view,
                            Some(capture.view_id),
                        ));
                    }
                    let target_reached = !stopped.load(Ordering::SeqCst)
                        && !immediate_stopped.load(Ordering::SeqCst);
                    if target_reached {
                        if let Some(capture) = &key_capture_for_event {
                            let mut handler = capture
                                .handler
                                .lock()
                                .unwrap_or_else(|poisoned| poisoned.into_inner());
                            handler(key_event.clone().for_dispatch(
                                ViewEventPhase::AtTarget,
                                target,
                                capture.focus_id,
                                key_target_view,
                                Some(capture.view_id),
                            ));
                        }
                    }
                    if target_reached && !immediate_stopped.load(Ordering::SeqCst) {
                        if let Some(handler_ref) = &key_handler_for_event {
                            let mut handler = handler_ref
                                .handler
                                .lock()
                                .unwrap_or_else(|poisoned| poisoned.into_inner());
                            handler(key_event.clone().for_dispatch(
                                ViewEventPhase::AtTarget,
                                target,
                                handler_ref.focus_id,
                                key_target_view,
                                Some(handler_ref.view_id),
                            ));
                        }
                    }
                    if !stopped.load(Ordering::SeqCst) && !immediate_stopped.load(Ordering::SeqCst)
                    {
                        for bubble in inherited_key_handlers.iter().rev() {
                            if stopped.load(Ordering::SeqCst)
                                || immediate_stopped.load(Ordering::SeqCst)
                            {
                                break;
                            }
                            let mut handler = bubble
                                .handler
                                .lock()
                                .unwrap_or_else(|poisoned| poisoned.into_inner());
                            handler(key_event.clone().for_dispatch(
                                ViewEventPhase::Bubbling,
                                target,
                                bubble.focus_id,
                                key_target_view,
                                Some(bubble.view_id),
                            ));
                        }
                    }
                    if default_prevented.load(Ordering::SeqCst) {
                        event.prevent_default();
                    }
                    if stopped.load(Ordering::SeqCst) {
                        event.stop_component_propagation();
                    }
                }
            }
        });

        let on_paste_capture = props.on_paste_capture.take();
        let paste_capture_handler = (!on_paste_capture.is_default()).then(|| ViewCaptureHandler {
            focus_id: self.focus_id,
            view_id: self.view_id,
            handler: Arc::new(Mutex::new(on_paste_capture)),
        });
        let paste_capture_for_children = paste_capture_handler.clone();
        let paste_capture_for_event = paste_capture_handler.clone();
        let inherited_paste_captures = parent_view_context.paste_capture_handlers.clone();
        let on_paste = props.on_paste.take();
        let paste_handler = (!on_paste.is_default()).then(|| ViewCaptureHandler {
            focus_id: self.focus_id,
            view_id: self.view_id,
            handler: Arc::new(Mutex::new(on_paste)),
        });
        let paste_handler_for_children = paste_handler.clone();
        let paste_handler_for_event = paste_handler.clone();
        let inherited_paste_handlers = parent_view_context.paste_handlers.clone();
        let paste_target_view = Some(self.view_id);
        let paste_focus = self.focus_id.zip(self.focus_ctx);
        hooks.use_propagated_terminal_events(move |event| {
            let Some((id, ctx)) = paste_focus else {
                return;
            };
            if !ctx.is_focused(id) {
                return;
            }
            if let TerminalEvent::Paste(text) = event.event() {
                let paste_event = ViewPasteEvent::new(text.clone());
                if event.is_default_prevented() {
                    paste_event.prevent_default();
                }
                let stopped = paste_event.stopped.clone();
                let immediate_stopped = paste_event.immediate_stopped.clone();
                let default_prevented = paste_event.default_prevented.clone();
                let target = ctx.active();
                for capture in &inherited_paste_captures {
                    if stopped.load(Ordering::SeqCst) || immediate_stopped.load(Ordering::SeqCst) {
                        break;
                    }
                    let mut handler = capture
                        .handler
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    handler(paste_event.clone().for_dispatch(
                        ViewEventPhase::Capturing,
                        target,
                        capture.focus_id,
                        paste_target_view,
                        Some(capture.view_id),
                    ));
                }
                let target_reached =
                    !stopped.load(Ordering::SeqCst) && !immediate_stopped.load(Ordering::SeqCst);
                if target_reached {
                    if let Some(capture) = &paste_capture_for_event {
                        let mut handler = capture
                            .handler
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                        handler(paste_event.clone().for_dispatch(
                            ViewEventPhase::AtTarget,
                            target,
                            capture.focus_id,
                            paste_target_view,
                            Some(capture.view_id),
                        ));
                    }
                }
                if target_reached && !immediate_stopped.load(Ordering::SeqCst) {
                    if let Some(handler_ref) = &paste_handler_for_event {
                        let mut handler = handler_ref
                            .handler
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                        handler(paste_event.clone().for_dispatch(
                            ViewEventPhase::AtTarget,
                            target,
                            handler_ref.focus_id,
                            paste_target_view,
                            Some(handler_ref.view_id),
                        ));
                    }
                }
                if !stopped.load(Ordering::SeqCst) && !immediate_stopped.load(Ordering::SeqCst) {
                    for bubble in inherited_paste_handlers.iter().rev() {
                        if stopped.load(Ordering::SeqCst)
                            || immediate_stopped.load(Ordering::SeqCst)
                        {
                            break;
                        }
                        let mut handler = bubble
                            .handler
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                        handler(paste_event.clone().for_dispatch(
                            ViewEventPhase::Bubbling,
                            target,
                            bubble.focus_id,
                            paste_target_view,
                            Some(bubble.view_id),
                        ));
                    }
                }
                if default_prevented.load(Ordering::SeqCst) {
                    event.prevent_default();
                }
                if stopped.load(Ordering::SeqCst) {
                    event.stop_component_propagation();
                }
            }
        });

        let rect_for_click = self.event_rect.clone();
        let pending_click = self.pending_click.clone();
        let on_click = props.on_click.take();
        let click_handler = (!on_click.is_default()).then(|| Arc::new(Mutex::new(on_click)));
        let click_registry = parent_view_context.click_registry.clone();
        let click_record = ViewHitRecord {
            id: self.view_id,
            parent: parent_view_context.view_id,
            rect: rect_for_click.clone(),
            handler: click_handler.clone(),
            focus: if props.focusable || props.tab_index.is_some() {
                self.focus_id.zip(self.focus_ctx)
            } else {
                None
            },
        };
        click_registry
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(click_record);
        let view_id = self.view_id;
        hooks.use_propagated_terminal_events(move |event| {
            let TerminalEvent::FullscreenMouse(FullscreenMouseEvent {
                column,
                row,
                kind,
                cell_is_blank,
                ..
            }) = event.event()
            else {
                return;
            };

            let rect = *rect_for_click
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let Some(rect) = rect else {
                return;
            };
            let inside = (*column as i32) >= rect.left
                && (*column as i32) < rect.right
                && (*row as i32) >= rect.top
                && (*row as i32) < rect.bottom;
            let records = click_registry
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone();
            let target_record = records.iter().rev().find(|record| {
                record
                    .rect
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .as_ref()
                    .is_some_and(|rect| {
                        (*column as i32) >= rect.left
                            && (*column as i32) < rect.right
                            && (*row as i32) >= rect.top
                            && (*row as i32) < rect.bottom
                    })
            });
            if target_record.is_none_or(|record| record.id != view_id) {
                return;
            }

            let mut pending = pending_click
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            match *kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    if inside {
                        *pending = Some(PendingClick {
                            column: *column,
                            row: *row,
                            dragged: false,
                        });
                    } else {
                        *pending = None;
                    }
                }
                MouseEventKind::Drag(MouseButton::Left) => {
                    if let Some(click) = pending.as_mut() {
                        if click.column != *column || click.row != *row {
                            click.dragged = true;
                        }
                    }
                }
                MouseEventKind::Up(MouseButton::Left) => {
                    let click = pending.take();
                    if let Some(click) = click {
                        if inside && !click.dragged {
                            // Match CC Ink's dispatchClick: click-to-focus is part of
                            // the completed release-click, not the initial mouse down.
                            // A drag that started on a focusable view must not steal focus.
                            let mut focus_cursor = target_record.map(|record| record.id);
                            while let Some(current) = focus_cursor {
                                let Some(record) =
                                    records.iter().find(|record| record.id == current)
                                else {
                                    break;
                                };
                                if let Some((id, ctx)) = record.focus {
                                    ctx.focus(id);
                                    break;
                                }
                                focus_cursor = record.parent;
                            }

                            let click_event = ViewClickEvent::new(
                                *column,
                                *row,
                                (*column as i32 - rect.left) as u16,
                                (*row as i32 - rect.top) as u16,
                                *cell_is_blank,
                            );
                            let stopped = click_event.stopped.clone();
                            let target = Some(view_id);
                            let mut handled = false;
                            let mut current = Some(view_id);
                            while let Some(current_id) = current {
                                let Some(record) =
                                    records.iter().find(|record| record.id == current_id)
                                else {
                                    break;
                                };
                                if let Some(handler_ref) = &record.handler {
                                    let handler_rect = *record
                                        .rect
                                        .lock()
                                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                                    if let Some(handler_rect) = handler_rect {
                                        handled = true;
                                        let phase = if record.id == view_id {
                                            ViewEventPhase::AtTarget
                                        } else {
                                            ViewEventPhase::Bubbling
                                        };
                                        let mut handler = handler_ref
                                            .lock()
                                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                                        handler(click_event.clone().for_dispatch(
                                            phase,
                                            target,
                                            Some(record.id),
                                            (*column as i32 - handler_rect.left) as u16,
                                            (*row as i32 - handler_rect.top) as u16,
                                        ));
                                    }
                                }
                                if stopped.load(Ordering::SeqCst) {
                                    break;
                                }
                                current = record.parent;
                            }
                            if handled || stopped.load(Ordering::SeqCst) {
                                event.stop_component_propagation();
                            }
                        }
                    }
                }
                MouseEventKind::Down(_) => {
                    *pending = None;
                }
                _ => {}
            }
        });

        let mut style: taffy::style::Style = props.layout_style().into();
        style.border = if self.border_style.is_none() {
            Rect::zero()
        } else {
            Rect {
                top: LengthPercentage::length(if self.border_edges.contains(Edges::Top) {
                    1.0
                } else {
                    0.0
                }),
                bottom: LengthPercentage::length(if self.border_edges.contains(Edges::Bottom) {
                    1.0
                } else {
                    0.0
                }),
                left: LengthPercentage::length(if self.border_edges.contains(Edges::Left) {
                    1.0
                } else {
                    0.0
                }),
                right: LengthPercentage::length(if self.border_edges.contains(Edges::Right) {
                    1.0
                } else {
                    0.0
                }),
            }
        };
        updater.set_layout_style(style);
        let child_focus_context = {
            let mut context = parent_view_context.clone();
            context.id = self.focus_id.or(parent_view_context.id);
            context.view_id = Some(self.view_id);
            context.hovered_views = hovered_views_for_children;
            if let Some(handler) = focus_capture_for_children {
                context.focus_capture_handlers.push(handler);
            }
            if let Some(handler) = focus_for_children {
                context.focus_handlers.push(handler);
            }
            if let Some(handler) = blur_capture_for_children {
                context.blur_capture_handlers.push(handler);
            }
            if let Some(handler) = blur_for_children {
                context.blur_handlers.push(handler);
            }
            if let Some(handler) = key_capture_for_children {
                context.key_down_capture_handlers.push(handler);
            }
            if let Some(handler) = key_handler_for_children {
                context.key_down_handlers.push(handler);
            }
            if let Some(handler) = paste_capture_for_children {
                context.paste_capture_handlers.push(handler);
            }
            if let Some(handler) = paste_handler_for_children {
                context.paste_handlers.push(handler);
            }
            Some(Context::owned(context))
        };
        updater.update_children(props.children.iter_mut(), child_focus_context);
    }

    fn draw(&mut self, drawer: &mut ComponentDrawer<'_>) {
        let layout = drawer.layout();
        let position = drawer.canvas_position();
        let size = drawer.size();
        *self
            .event_rect
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(Rect {
            left: position.x as i32,
            right: position.x as i32 + size.width as i32,
            top: position.y as i32,
            bottom: position.y as i32 + size.height as i32,
        });

        if drawer.zero_height_sibling_shares_y() {
            // CC Ink's siblingSharesY guard: don't let a zero-height Box paint
            // children that can leave stale tail glyphs under a sibling on the
            // same row. Keep View hooks/event metadata alive; only skip output.
            drawer.skip_children();
            return;
        }

        if self.no_select {
            if self.no_select_from_left_edge {
                let x = drawer.canvas_position().x as isize;
                let right = x + layout.size.width as isize;
                if right > 0 && layout.size.height > 0.0 {
                    drawer.mark_no_select_region_signed(
                        -x,
                        0,
                        right as usize,
                        layout.size.height as usize,
                    );
                }
            } else {
                drawer.mark_no_select_region(
                    0,
                    0,
                    layout.size.width as usize,
                    layout.size.height as usize,
                );
            }
        }

        let mut canvas = drawer.canvas();
        let border = self.border_style.border_characters();
        let left_border_size = if border.is_some() && self.border_edges.contains(Edges::Left) {
            1usize
        } else {
            0
        };
        let right_border_size = if border.is_some() && self.border_edges.contains(Edges::Right) {
            1usize
        } else {
            0
        };
        let top_border_size = if border.is_some() && self.border_edges.contains(Edges::Top) {
            1usize
        } else {
            0
        };
        let bottom_border_size = if border.is_some() && self.border_edges.contains(Edges::Bottom) {
            1usize
        } else {
            0
        };

        if self.background_color.is_some() || self.opaque {
            let inner_width = (layout.size.width as usize)
                .saturating_sub(left_border_size)
                .saturating_sub(right_border_size);
            let inner_height = (layout.size.height as usize)
                .saturating_sub(top_border_size)
                .saturating_sub(bottom_border_size);
            if inner_width > 0 && inner_height > 0 {
                canvas.clear_text(
                    left_border_size as isize,
                    top_border_size as isize,
                    inner_width,
                    inner_height,
                );
                if let Some(color) = self.background_color {
                    canvas.set_background_color(
                        left_border_size as isize,
                        top_border_size as isize,
                        inner_width,
                        inner_height,
                        color,
                    );
                }
            }
        }

        if let Some(border) = border {
            if self.border_edges.contains(Edges::Top) {
                let content_width = (layout.size.width as usize)
                    .saturating_sub(left_border_size)
                    .saturating_sub(right_border_size);
                let top = format!(
                    "{}{}{}",
                    if self.border_edges.contains(Edges::Left) {
                        border.top_left.to_string()
                    } else {
                        String::new()
                    },
                    Self::repeat_border_char(border.top, content_width),
                    if self.border_edges.contains(Edges::Right) {
                        border.top_right.to_string()
                    } else {
                        String::new()
                    }
                );
                Self::write_border_line(
                    &mut canvas,
                    0,
                    &top,
                    border.top,
                    self.border_text.as_ref(),
                    BorderTextPosition::Top,
                    self.border_top_style,
                );
            }

            for y in top_border_size as isize
                ..(layout.size.height as isize - bottom_border_size as isize)
            {
                if self.border_edges.contains(Edges::Left) {
                    canvas.set_text(0, y, &border.left.to_string(), self.border_left_style);
                }
                if self.border_edges.contains(Edges::Right) {
                    canvas.set_text(
                        layout.size.width as isize - 1,
                        y,
                        &border.right.to_string(),
                        self.border_right_style,
                    );
                }
            }

            if self.border_edges.contains(Edges::Bottom) {
                let content_width = (layout.size.width as usize)
                    .saturating_sub(left_border_size)
                    .saturating_sub(right_border_size);
                let bottom = format!(
                    "{}{}{}",
                    if self.border_edges.contains(Edges::Left) {
                        border.bottom_left.to_string()
                    } else {
                        String::new()
                    },
                    Self::repeat_border_char(border.bottom, content_width),
                    if self.border_edges.contains(Edges::Right) {
                        border.bottom_right.to_string()
                    } else {
                        String::new()
                    }
                );
                Self::write_border_line(
                    &mut canvas,
                    layout.size.height as isize - 1,
                    &bottom,
                    border.bottom,
                    self.border_text.as_ref(),
                    BorderTextPosition::Bottom,
                    self.border_bottom_style,
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::prelude::*;
    use futures::StreamExt;
    use indoc::indoc;
    use std::time::Duration;

    fn delayed_terminal_events(
        events: Vec<TerminalEvent>,
    ) -> impl futures::Stream<Item = TerminalEvent> {
        futures::stream::unfold(events.into_iter(), |mut events| async move {
            smol::Timer::after(Duration::from_millis(1)).await;
            events.next().map(|event| (event, events))
        })
    }

    #[derive(Default, Props)]
    pub struct MyTextProps {
        pub content: String,
    }

    #[component]
    pub fn MyText<'a>(props: &MyTextProps) -> impl Into<AnyElement<'a>> {
        element! {
            Text(content: &props.content)
        }
    }

    #[test]
    fn test_view_border_edge_styles_and_dashed_style() {
        let canvas = element! {
            View(
                width: 4,
                height: 3,
                border_style: BorderStyle::Single,
                border_top_color: Some(Color::Red),
                border_right_color: Some(Color::Green),
                border_bottom_color: Some(Color::Blue),
                border_left_color: Some(Color::Yellow),
                border_dim_color: true,
                border_right_dim_color: Some(false),
            )
        }
        .render(None);

        assert_eq!(canvas.to_string(), "┌──┐\n│  │\n└──┘\n");
        assert_eq!(
            canvas.resolved_text_style(0, 0).unwrap().color,
            Some(Color::Red)
        );
        assert_eq!(
            canvas.resolved_text_style(0, 0).unwrap().weight,
            Weight::Light
        );
        assert_eq!(
            canvas.resolved_text_style(3, 1).unwrap().color,
            Some(Color::Green)
        );
        assert_eq!(
            canvas.resolved_text_style(3, 1).unwrap().weight,
            Weight::Normal
        );
        assert_eq!(
            canvas.resolved_text_style(0, 2).unwrap().color,
            Some(Color::Blue)
        );
        assert_eq!(
            canvas.resolved_text_style(0, 2).unwrap().weight,
            Weight::Light
        );
        assert_eq!(
            canvas.resolved_text_style(0, 1).unwrap().color,
            Some(Color::Yellow)
        );
        assert_eq!(
            canvas.resolved_text_style(0, 1).unwrap().weight,
            Weight::Light
        );

        assert_eq!(
            element!(View(width: 4, height: 3, border_style: BorderStyle::Dashed)).to_string(),
            " ╌╌\n╎  ╎\n ╌╌\n"
        );

        assert_eq!(
            element!(View(
                width: 4,
                height: 3,
                border_style: BorderStyle::Single,
                border_top: false,
            ))
            .to_string(),
            "│  │\n│  │\n└──┘\n"
        );
        assert_eq!(
            element!(View(
                width: 4,
                height: 3,
                border_style: BorderStyle::Single,
                border_left: false,
                border_right: false,
            ))
            .to_string(),
            "────\n\n────\n"
        );
    }

    #[test]
    fn test_view_background_color_fills_interior_not_border() {
        let canvas = element! {
            View(width: 5, height: 3, border_style: BorderStyle::Single, background_color: Color::Blue)
        }
        .render(None);

        assert_eq!(canvas.to_string(), "┌───┐\n│   │\n└───┘\n");
        assert_eq!(
            canvas.cell(1, 1).unwrap().background_color,
            Some(Color::Blue)
        );
        assert_eq!(canvas.cell(0, 0).unwrap().background_color, None);
        assert_eq!(canvas.cell(0, 1).unwrap().background_color, None);
        assert_eq!(canvas.cell(4, 1).unwrap().background_color, None);
        assert_eq!(canvas.cell(0, 2).unwrap().background_color, None);
    }

    #[test]
    fn test_view_border_text_parses_ansi_content() {
        let canvas = element! {
            View(width: 12, border_style: BorderStyle::Single, border_text: Some(BorderText {
                content: "\x1b[31;1mHi\x1b[0m".to_string(),
                position: BorderTextPosition::Top,
                align: BorderTextAlign::Center,
                offset: 0,
            })) {
                Text(content: "body")
            }
        }
        .render(None);

        assert_eq!(
            canvas.to_string(),
            "┌────Hi────┐\n│body      │\n└──────────┘\n"
        );
        let label = canvas.resolved_text_style(5, 0).unwrap();
        assert_eq!(label.color, Some(Color::DarkRed));
        assert_eq!(label.weight, Weight::Bold);
        assert_eq!(canvas.resolved_text_style(4, 0).unwrap().color, None);
    }

    #[test]
    fn test_view_terminal_event_metadata_matches_ink_defaults() {
        let key = ViewKeyboardEvent::new(&KeyEvent::new(KeyEventKind::Press, KeyCode::Char('a')));
        assert_eq!(key.event_type, "keydown");
        assert!(key.bubbles);
        assert!(key.cancelable);
        key.prevent_default();
        assert!(key.default_prevented());

        let paste = ViewPasteEvent::new("hello".to_string());
        assert_eq!(paste.event_type, "paste");
        assert!(paste.bubbles);
        assert!(paste.cancelable);
        paste.prevent_default();
        assert!(paste.default_prevented());
    }

    #[test]
    fn test_view() {
        assert_eq!(element!(View).to_string(), "");

        assert_eq!(
            element! {
                View {
                    Text(content: "foo")
                    Text(content: "bar")
                }
            }
            .to_string(),
            "foobar\n"
        );

        assert_eq!(
            element! {
                View(padding: 1) {
                    Text(content: "foo")
                }
            }
            .to_string(),
            "\n foo\n\n"
        );

        assert_eq!(
            element! {
                View(margin: 2) {
                    Text(content: "foo")
                }
            }
            .to_string(),
            "\n\n  foo\n\n\n"
        );

        assert_eq!(
            element! {
                View(width: 20) {
                    View(width: 60pct) {
                        Text(content: "foo")
                    }
                    View(width: 40pct) {
                        Text(content: "bar")
                    }
                }
            }
            .to_string(),
            "foo         bar\n"
        );

        assert_eq!(
            element! {
                View(width: 20, border_style: BorderStyle::Single) {
                    View(width: 60pct) {
                        Text(content: "foo")
                    }
                    View(width: 40pct) {
                        Text(content: "bar")
                    }
                }
            }
            .to_string(),
            indoc! {"
                ┌──────────────────┐
                │foo        bar    │
                └──────────────────┘
            "},
        );

        assert_eq!(
            element! {
                View(flex_direction: FlexDirection::Column) {
                    View {
                        View(border_style: BorderStyle::Single, margin_right: 2) {
                            Text(content: "Single")
                        }
                        View(border_style: BorderStyle::Double, margin_right: 2) {
                            Text(content: "Double")
                        }
                        View(border_style: BorderStyle::Round, margin_right: 2) {
                            Text(content: "Round")
                        }
                        View(border_style: BorderStyle::Bold) {
                            Text(content: "Bold")
                        }
                    }

                    View(margin_top: 1) {
                        View(border_style: BorderStyle::DoubleLeftRight, margin_right: 2) {
                            Text(content: "DoubleLeftRight")
                        }
                        View(border_style: BorderStyle::DoubleTopBottom, margin_right: 2) {
                            Text(content: "DoubleTopBottom")
                        }
                        View(border_style: BorderStyle::Classic) {
                            Text(content: "Classic")
                        }
                    }
                }
            }
            .to_string(),
            indoc! {"
                ┌──────┐  ╔══════╗  ╭─────╮  ┏━━━━┓
                │Single│  ║Double║  │Round│  ┃Bold┃
                └──────┘  ╚══════╝  ╰─────╯  ┗━━━━┛

                ╓───────────────╖  ╒═══════════════╕  +-------+
                ║DoubleLeftRight║  │DoubleTopBottom│  |Classic|
                ╙───────────────╜  ╘═══════════════╛  +-------+
            "},
        );

        assert_eq!(
            element! {
                View(width: 8, border_style: BorderStyle::Single, justify_content: JustifyContent::CENTER) {
                    Text(content: "✅")
                }
            }
            .to_string(),
            indoc! {"
                ┌──────┐
                │  ✅  │
                └──────┘
            "},
        );

        // ☀️ (U+2600 + U+FE0F) is a grapheme cluster rendered as 2 columns on modern
        // terminals. The extra_space hack for handles_vs16_incorrectly() terminals
        // compensates for the rare case where the cursor only advances 1 column.
        let extra_space = if handles_vs16_incorrectly() { " " } else { "" };

        // With grapheme-based width, ☀️ (U+2600 + VS16) is correctly measured as
        // 2 columns. Centering in a 6-column content area: 2 pad + 2 emoji + 2 pad.
        assert_eq!(
            element! {
                View(width: 8, border_style: BorderStyle::Single, justify_content: JustifyContent::CENTER) {
                    Text(content: "☀️")
                }
            }
            .to_string(),
            format!(indoc! {"
                ┌──────┐
                │  ☀️{}  │
                └──────┘
            "}, extra_space),
        );

        // Two sun emojis: 2*2=4 columns of content in 6 columns: 1 pad each side.
        assert_eq!(
            element! {
                View(width: 8, border_style: BorderStyle::Single, justify_content: JustifyContent::CENTER) {
                    Text(content: "☀️☀️")
                }
            }
            .to_string(),
            format!(indoc! {"
                ┌──────┐
                │ ☀️{}☀️{} │
                └──────┘
            "}, extra_space, extra_space),
        );

        assert_eq!(
            element! {
                View(width: 12, border_style: BorderStyle::Single, justify_content: JustifyContent::CENTER) {
                    Text(content: "フーバー")
                }
            }
            .to_string(),
            indoc! {"
                ┌──────────┐
                │ フーバー │
                └──────────┘
            "},
        );

        assert_eq!(
            element! {
                View(
                    border_style: BorderStyle::Round,
                    flex_direction: FlexDirection::Column,
                ) {
                    View(
                        margin_top: -1,
                    ) {
                        Text(content: "Title")
                    }
                    Text(content: "Hello, world!")
                }
            }
            .to_string(),
            indoc! {"
                ╭Title────────╮
                │Hello, world!│
                ╰─────────────╯
            "},
        );

        assert_eq!(
            element! {
                View {
                    Text(content: "This is the background text.")
                    View(
                        position: Position::Absolute,
                        top: 0,
                        left: 3,
                    ) {
                        Text(content: "Foo!")
                    }
                }
            }
            .to_string(),
            "ThiFoo! the background text.\n",
        );

        assert_eq!(
            element! {
                View {
                    Text(content: "This is the background text.")
                    View(
                        position: Position::Absolute,
                        top: 0,
                        left: 3,
                        width: 6,
                        height: 1,
                        background_color: Color::Red,
                    )
                }
            }
            .to_string(),
            "Thi      he background text.\n",
        );

        assert_eq!(
            element! {
                View {
                    Text(content: "This is the background text.")
                    View(
                        position: Position::Absolute,
                        top: 0,
                        left: 3,
                        width: 6,
                        height: 1,
                        opaque: true,
                    )
                }
            }
            .to_string(),
            "Thi      he background text.\n",
        );

        assert_eq!(
            element! {
                View(width: 16, border_style: BorderStyle::Round, border_text: Some(BorderText {
                    content: "Title".to_string(),
                    position: BorderTextPosition::Top,
                    align: BorderTextAlign::Center,
                    offset: 0,
                })) {
                    Text(content: "body")
                }
            }
            .to_string(),
            indoc! {"
                ╭────Title─────╮
                │body          │
                ╰──────────────╯
            "},
        );

        assert_eq!(
            element! {
                View(width: 12, border_style: BorderStyle::Classic, border_text: Some(BorderText {
                    content: "ok".to_string(),
                    position: BorderTextPosition::Bottom,
                    align: BorderTextAlign::End,
                    offset: 1,
                })) {
                    Text(content: "body")
                }
            }
            .to_string(),
            indoc! {"
                +----------+
                |body      |
                +-------ok-+
            "},
        );

        assert_eq!(
            element! {
                View(width: 20, border_style: BorderStyle::Single, column_gap: 2) {
                    View(width: 3) {
                        Text(content: "foo")
                    }
                    View(width: 3) {
                        Text(content: "bar")
                    }
                }
            }
            .to_string(),
            indoc! {"
                ┌──────────────────┐
                │foo  bar          │
                └──────────────────┘
            "},
        );

        // regression test for https://github.com/ccbrown/iocraft/issues/52
        assert_eq!(
            element! {
                View(width: 20, border_style: BorderStyle::Single, row_gap: 1, flex_direction: FlexDirection::Column) {
                    Text(content: "foo")
                    MyText(content: "bar")
                    MyText(content: "baz")
                }
            }
            .to_string(),
            indoc! {"
                ┌──────────────────┐
                │foo               │
                │                  │
                │bar               │
                │                  │
                │baz               │
                └──────────────────┘
            "},
        );

        assert_eq!(
            element! {
                View(width: 20, height: 7, margin_top: 1, border_style: BorderStyle::Single) {
                    View(width: 5, height: 3, position: Position::Absolute, top: -2) {
                        Text(content: "foo")
                    }
                }
            }
            .to_string(),
            indoc! {"
                 foo
                ┌──────────────────┐
                │                  │
                │                  │
                │                  │
                │                  │
                │                  │
                └──────────────────┘
            "},
        );

        // CC Ink clamps absolute overlays with negative screen-space Y to the
        // canvas top so the overlay's first row remains visible.
        assert_eq!(
            element! {
                View(width: 20, height: 7, margin_top: 1, border_style: BorderStyle::Single) {
                    View(width: 5, height: 3, position: Position::Absolute, top: -3) {
                        Text(content: "foo\nbar")
                    }
                }
            }
            .to_string(),
            indoc! {"
                 foo
                ┌bar───────────────┐
                │                  │
                │                  │
                │                  │
                │                  │
                │                  │
                └──────────────────┘
            "},
        );

        // Text wrapping is clamped to the remaining root canvas width like CC Ink's
        // render-node-to-output.ts, then clipped by the overflow:hidden parent.
        assert_eq!(
            element! {
                View(width: 20, height: 7, border_style: BorderStyle::Single, overflow: Overflow::Hidden) {
                    View(position: Position::Absolute, top: -1, left: 17) {
                        Text(content: "foo\nbar")
                    }
                }
            }
            .to_string(),
            indoc! {"
                ┌──────────────────┐
                │                 o│
                │                 b│
                │                 r│
                │                  │
                │                  │
                └──────────────────┘
            "},
        );
    }

    #[test]
    fn test_view_zero_height_sibling_overlap_skips_hidden_tail_like_cc_ink() {
        assert_eq!(
            element! {
                View(width: 5, height: 1, flex_direction: FlexDirection::Column) {
                    View(height: 0) {
                        Text(content: "false")
                    }
                    Text(content: "true")
                }
            }
            .to_string(),
            "true\n",
        );
    }

    #[component]
    fn ViewClickApp(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let mut child_clicks = hooks.use_state(|| 0usize);
        let mut parent_clicks = hooks.use_state(|| 0usize);

        if child_clicks.get() > 0 {
            system.exit();
        }

        element! {
            View(width: 6, on_click: move |_| parent_clicks += 1) {
                View(width: 3, on_click: move |_| child_clicks += 1) {
                    Text(content: format!("{}/{}", child_clicks.get(), parent_clicks.get()))
                }
            }
        }
    }

    #[test]
    fn test_view_on_click_bubbles_to_ancestors() {
        let canvases: Vec<_> = smol::block_on(
            element!(ViewClickApp)
                .mock_terminal_render_loop(MockTerminalConfig::with_events(futures::stream::iter(
                    vec![
                        TerminalEvent::FullscreenMouse(FullscreenMouseEvent::new(
                            MouseEventKind::Down(MouseButton::Left),
                            1,
                            0,
                        )),
                        TerminalEvent::FullscreenMouse(FullscreenMouseEvent::new(
                            MouseEventKind::Up(MouseButton::Left),
                            1,
                            0,
                        )),
                    ],
                )))
                .collect(),
        );
        assert_eq!(canvases.last().unwrap().to_string(), "1/1\n");
    }

    #[component]
    fn ViewClickStopApp(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let mut child_clicks = hooks.use_state(|| 0usize);
        let mut parent_clicks = hooks.use_state(|| 0usize);

        if child_clicks.get() > 0 {
            system.exit();
        }

        element! {
            View(width: 6, on_click: move |_| parent_clicks += 1) {
                View(
                    width: 3,
                    on_click: move |event: ViewClickEvent| {
                        event.stop_immediate_propagation();
                        child_clicks += 1;
                    },
                ) {
                    Text(content: format!("{}/{}", child_clicks.get(), parent_clicks.get()))
                }
            }
        }
    }

    #[test]
    fn test_view_on_click_can_stop_bubbling_to_ancestors() {
        let canvases: Vec<_> = smol::block_on(
            element!(ViewClickStopApp)
                .mock_terminal_render_loop(MockTerminalConfig::with_events(futures::stream::iter(
                    vec![
                        TerminalEvent::FullscreenMouse(FullscreenMouseEvent::new(
                            MouseEventKind::Down(MouseButton::Left),
                            1,
                            0,
                        )),
                        TerminalEvent::FullscreenMouse(FullscreenMouseEvent::new(
                            MouseEventKind::Up(MouseButton::Left),
                            1,
                            0,
                        )),
                    ],
                )))
                .collect(),
        );
        assert_eq!(canvases.last().unwrap().to_string(), "1/0\n");
    }

    #[component]
    fn ViewClickDragApp(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let mut clicks = hooks.use_state(|| 0usize);
        let mut releases = hooks.use_state(|| 0usize);
        hooks.use_terminal_events(move |event| {
            if matches!(
                event,
                TerminalEvent::FullscreenMouse(FullscreenMouseEvent {
                    kind: MouseEventKind::Up(MouseButton::Left),
                    ..
                })
            ) {
                releases += 1;
            }
        });

        if releases.get() > 0 {
            system.exit();
        }

        element! {
            View(width: 12, on_click: move |_| clicks += 1) {
                Text(content: format!("clicks={}", clicks.get()))
            }
        }
    }

    #[test]
    fn test_view_on_click_ignores_drag_between_press_and_release() {
        let canvases: Vec<_> = smol::block_on(
            element!(ViewClickDragApp)
                .mock_terminal_render_loop(MockTerminalConfig::with_events(futures::stream::iter(
                    vec![
                        TerminalEvent::FullscreenMouse(FullscreenMouseEvent::new(
                            MouseEventKind::Down(MouseButton::Left),
                            1,
                            0,
                        )),
                        TerminalEvent::FullscreenMouse(FullscreenMouseEvent::new(
                            MouseEventKind::Drag(MouseButton::Left),
                            2,
                            0,
                        )),
                        TerminalEvent::FullscreenMouse(FullscreenMouseEvent::new(
                            MouseEventKind::Up(MouseButton::Left),
                            2,
                            0,
                        )),
                    ],
                )))
                .collect(),
        );
        assert_eq!(canvases.last().unwrap().to_string(), "clicks=0\n");
    }

    #[component]
    fn ViewClickFocusTargetApp(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let mut key_target = hooks.use_state(|| None::<&'static str>);
        let mut key_events = hooks.use_state(|| 0usize);
        hooks.use_terminal_events(move |event| {
            if matches!(event, TerminalEvent::Key(_)) {
                key_events += 1;
            }
        });

        if key_events.get() > 0 {
            system.exit();
        }

        element! {
            FocusScope {
                View(flex_direction: FlexDirection::Column) {
                    View(
                        width: 8,
                        focusable: true,
                        auto_focus: true,
                        on_key_down: move |event: ViewKeyboardEvent| {
                            if event.key == "x" {
                                key_target.set(Some("first"));
                            }
                        },
                    ) {
                        Text(content: "one")
                    }
                    View(
                        width: 8,
                        focusable: true,
                        on_key_down: move |event: ViewKeyboardEvent| {
                            if event.key == "x" {
                                key_target.set(Some("second"));
                            }
                        },
                    ) {
                        Text(content: key_target.get().unwrap_or("none"))
                    }
                }
            }
        }
    }

    #[test]
    fn test_view_click_to_focus_happens_on_release_click() {
        let canvases: Vec<_> = smol::block_on(
            element!(ViewClickFocusTargetApp)
                .mock_terminal_render_loop(MockTerminalConfig::with_events(
                    delayed_terminal_events(vec![
                        TerminalEvent::FullscreenMouse(FullscreenMouseEvent::new(
                            MouseEventKind::Down(MouseButton::Left),
                            0,
                            1,
                        )),
                        TerminalEvent::FullscreenMouse(FullscreenMouseEvent::new(
                            MouseEventKind::Up(MouseButton::Left),
                            0,
                            1,
                        )),
                        TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Char('x'))),
                    ]),
                ))
                .collect(),
        );
        assert_eq!(canvases.last().unwrap().to_string(), "one\nsecond\n");
    }

    #[test]
    fn test_view_drag_does_not_click_to_focus() {
        let canvases: Vec<_> = smol::block_on(
            element!(ViewClickFocusTargetApp)
                .mock_terminal_render_loop(MockTerminalConfig::with_events(
                    delayed_terminal_events(vec![
                        TerminalEvent::FullscreenMouse(FullscreenMouseEvent::new(
                            MouseEventKind::Down(MouseButton::Left),
                            0,
                            1,
                        )),
                        TerminalEvent::FullscreenMouse(FullscreenMouseEvent::new(
                            MouseEventKind::Drag(MouseButton::Left),
                            1,
                            1,
                        )),
                        TerminalEvent::FullscreenMouse(FullscreenMouseEvent::new(
                            MouseEventKind::Up(MouseButton::Left),
                            1,
                            1,
                        )),
                        TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Char('x'))),
                    ]),
                ))
                .collect(),
        );
        assert_eq!(canvases.last().unwrap().to_string(), "one\nfirst\n");
    }

    #[component]
    fn ViewClickCoordinateApp(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let mut coords = hooks.use_state(|| None::<(u16, u16, u16, u16)>);

        if coords.get().is_some() {
            system.exit();
        }

        element! {
            View(padding_left: 2) {
                View(
                    width: 24,
                    on_click: move |event: ViewClickEvent| {
                        coords.set(Some((
                            event.column,
                            event.row,
                            event.local_column,
                            event.local_row,
                        )));
                    },
                ) {
                    Text(content: format!("{:?}", coords.get()))
                }
            }
        }
    }

    #[test]
    fn test_view_on_click_reports_screen_and_local_coordinates() {
        let canvases: Vec<_> = smol::block_on(
            element!(ViewClickCoordinateApp)
                .mock_terminal_render_loop(MockTerminalConfig::with_events(futures::stream::iter(
                    vec![
                        TerminalEvent::FullscreenMouse(FullscreenMouseEvent::new(
                            MouseEventKind::Down(MouseButton::Left),
                            3,
                            0,
                        )),
                        TerminalEvent::FullscreenMouse(FullscreenMouseEvent::new(
                            MouseEventKind::Up(MouseButton::Left),
                            3,
                            0,
                        )),
                    ],
                )))
                .collect(),
        );
        assert_eq!(
            canvases.last().unwrap().to_string(),
            "  Some((3, 0, 1, 0))\n"
        );
    }

    #[component]
    fn ViewClickBlankCellApp(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let mut blank = hooks.use_state(|| None::<bool>);

        if blank.get().is_some() {
            system.exit();
        }

        element! {
            View(
                width: 12,
                on_click: move |event: ViewClickEvent| {
                    blank.set(Some(event.cell_is_blank));
                },
            ) {
                Text(content: blank.get().map(|value| format!("blank={value}")).unwrap_or_else(|| "abc".to_string()))
            }
        }
    }

    #[test]
    fn test_view_on_click_reports_blank_cell_from_retained_canvas() {
        let canvases: Vec<_> = smol::block_on(
            element!(ViewClickBlankCellApp)
                .mock_terminal_render_loop(MockTerminalConfig::with_events(futures::stream::iter(
                    vec![
                        TerminalEvent::FullscreenMouse(FullscreenMouseEvent::new(
                            MouseEventKind::Down(MouseButton::Left),
                            4,
                            0,
                        )),
                        TerminalEvent::FullscreenMouse(FullscreenMouseEvent::new(
                            MouseEventKind::Up(MouseButton::Left),
                            4,
                            0,
                        )),
                    ],
                )))
                .collect(),
        );
        assert_eq!(canvases.last().unwrap().to_string(), "blank=true\n");
    }

    #[component]
    fn ViewClickTargetApp(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let mut seen = hooks.use_state(|| None::<(bool, ViewEventPhase, u16, u16)>);

        if seen.get().is_some() {
            system.exit();
        }

        element! {
            View(
                width: 40,
                padding_left: 2,
                on_click: move |event: ViewClickEvent| {
                    seen.set(Some((
                        event.target.is_some()
                            && event.current_target.is_some()
                            && event.target != event.current_target,
                        event.phase,
                        event.local_column,
                        event.local_row,
                    )));
                },
            ) {
                View(width: 36) {
                    Text(content: format!("{:?}", seen.get()))
                }
            }
        }
    }

    #[test]
    fn test_view_on_click_bubbles_from_child_target_to_parent_current_target() {
        let canvases: Vec<_> = smol::block_on(
            element!(ViewClickTargetApp)
                .mock_terminal_render_loop(MockTerminalConfig::with_events(futures::stream::iter(
                    vec![
                        TerminalEvent::FullscreenMouse(FullscreenMouseEvent::new(
                            MouseEventKind::Down(MouseButton::Left),
                            2,
                            0,
                        )),
                        TerminalEvent::FullscreenMouse(FullscreenMouseEvent::new(
                            MouseEventKind::Up(MouseButton::Left),
                            2,
                            0,
                        )),
                    ],
                )))
                .collect(),
        );
        assert_eq!(
            canvases.last().unwrap().to_string(),
            "  Some((true, Bubbling, 2, 0))\n"
        );
    }

    #[component]
    fn ViewClickAcrossFocusScopeApp(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let mut clicks = hooks.use_state(|| 0usize);
        let mut releases = hooks.use_state(|| 0usize);
        hooks.use_terminal_events(move |event| {
            if matches!(
                event,
                TerminalEvent::FullscreenMouse(FullscreenMouseEvent {
                    kind: MouseEventKind::Up(MouseButton::Left),
                    ..
                })
            ) {
                releases += 1;
            }
        });

        if releases.get() > 0 {
            system.exit();
        }

        element! {
            View(
                width: 10,
                height: 1,
                on_click: move |_| clicks += 1,
            ) {
                FocusScope {
                    View(width: 10, height: 1) {
                        Text(content: format!("clicks={}", clicks.get()))
                    }
                }
            }
        }
    }

    #[test]
    fn test_view_click_bubbles_across_focus_scope_boundary() {
        let canvases: Vec<_> = smol::block_on(
            element!(ViewClickAcrossFocusScopeApp)
                .mock_terminal_render_loop(MockTerminalConfig::with_events(futures::stream::iter(
                    vec![
                        TerminalEvent::FullscreenMouse(FullscreenMouseEvent::new(
                            MouseEventKind::Down(MouseButton::Left),
                            1,
                            0,
                        )),
                        TerminalEvent::FullscreenMouse(FullscreenMouseEvent::new(
                            MouseEventKind::Up(MouseButton::Left),
                            1,
                            0,
                        )),
                    ],
                )))
                .collect(),
        );
        assert_eq!(canvases.last().unwrap().to_string(), "clicks=1\n");
    }

    #[component]
    fn ViewClickTopmostApp(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let mut clicked = hooks.use_state(|| "none".to_string());

        if clicked.read().as_str() != "none" {
            system.exit();
        }

        element! {
            View(width: 8, height: 1) {
                View(
                    position: Position::Absolute,
                    top: 0,
                    left: 0,
                    width: 6,
                    on_click: move |_| clicked.set("first".to_string()),
                ) {
                    Text(content: "first")
                }
                View(
                    position: Position::Absolute,
                    top: 0,
                    left: 0,
                    width: 6,
                    on_click: move |_| clicked.set("second".to_string()),
                ) {
                    Text(content: clicked.read().clone())
                }
            }
        }
    }

    #[test]
    fn test_view_on_click_prefers_topmost_later_sibling() {
        let canvases: Vec<_> = smol::block_on(
            element!(ViewClickTopmostApp)
                .mock_terminal_render_loop(MockTerminalConfig::with_events(futures::stream::iter(
                    vec![
                        TerminalEvent::FullscreenMouse(FullscreenMouseEvent::new(
                            MouseEventKind::Down(MouseButton::Left),
                            1,
                            0,
                        )),
                        TerminalEvent::FullscreenMouse(FullscreenMouseEvent::new(
                            MouseEventKind::Up(MouseButton::Left),
                            1,
                            0,
                        )),
                    ],
                )))
                .collect(),
        );
        assert_eq!(canvases.last().unwrap().to_string(), "second\n");
    }

    #[component]
    fn ViewHoverApp(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let mut enters = hooks.use_state(|| 0usize);
        let mut leaves = hooks.use_state(|| 0usize);

        if enters.get() == 1 && leaves.get() == 1 {
            system.exit();
        }

        element! {
            View(
                width: 3,
                on_mouse_enter: move |_| enters += 1,
                on_mouse_leave: move |_| leaves += 1,
            ) {
                Text(content: format!("{}/{}", enters.get(), leaves.get()))
            }
        }
    }

    #[test]
    fn test_view_mouse_enter_leave_handlers() {
        let canvases: Vec<_> = smol::block_on(
            element!(ViewHoverApp)
                .mock_terminal_render_loop(MockTerminalConfig::with_events(futures::stream::iter(
                    vec![
                        TerminalEvent::FullscreenMouse(FullscreenMouseEvent::new(
                            MouseEventKind::Moved,
                            1,
                            0,
                        )),
                        TerminalEvent::FullscreenMouse(FullscreenMouseEvent::new(
                            MouseEventKind::Moved,
                            5,
                            0,
                        )),
                    ],
                )))
                .collect(),
        );
        assert_eq!(canvases.last().unwrap().to_string(), "1/1\n");
    }

    #[component]
    fn ViewHoverPersistsAcrossRenderApp(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let mut enters = hooks.use_state(|| 0usize);
        let mut moves = hooks.use_state(|| 0usize);
        hooks.use_terminal_events(move |event| {
            if matches!(
                event,
                TerminalEvent::FullscreenMouse(FullscreenMouseEvent {
                    kind: MouseEventKind::Moved,
                    ..
                })
            ) {
                moves += 1;
            }
        });

        if moves.get() >= 2 {
            system.exit();
        }

        element! {
            View(
                width: 8,
                height: 1,
                on_mouse_enter: move |_| enters += 1,
            ) {
                Text(content: format!("enters={}", enters.get()))
            }
        }
    }

    #[test]
    fn test_view_hover_state_persists_across_rerendered_root_context() {
        let canvases: Vec<_> = smol::block_on(
            element!(ViewHoverPersistsAcrossRenderApp)
                .mock_terminal_render_loop(MockTerminalConfig::with_events(futures::stream::iter(
                    vec![
                        TerminalEvent::FullscreenMouse(FullscreenMouseEvent::new(
                            MouseEventKind::Moved,
                            1,
                            0,
                        )),
                        TerminalEvent::FullscreenMouse(FullscreenMouseEvent::new(
                            MouseEventKind::Moved,
                            1,
                            0,
                        )),
                    ],
                )))
                .collect(),
        );
        assert_eq!(canvases.last().unwrap().to_string(), "enters=1\n");
    }

    #[component]
    fn ViewHoverTopmostApp(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let mut first_enters = hooks.use_state(|| 0usize);
        let mut first_leaves = hooks.use_state(|| 0usize);
        let mut second_enters = hooks.use_state(|| 0usize);
        let mut second_leaves = hooks.use_state(|| 0usize);

        if second_enters.get() == 1 && second_leaves.get() == 1 {
            system.exit();
        }

        element! {
            View(width: 10, height: 1) {
                View(
                    position: Position::Absolute,
                    top: 0,
                    left: 0,
                    width: 8,
                    on_mouse_enter: move |_| first_enters += 1,
                    on_mouse_leave: move |_| first_leaves += 1,
                ) {
                    Text(content: "first")
                }
                View(
                    position: Position::Absolute,
                    top: 0,
                    left: 0,
                    width: 8,
                    on_mouse_enter: move |_| second_enters += 1,
                    on_mouse_leave: move |_| second_leaves += 1,
                ) {
                    Text(content: format!(
                        "{}/{}/{}/{}",
                        first_enters.get(),
                        first_leaves.get(),
                        second_enters.get(),
                        second_leaves.get()
                    ))
                }
            }
        }
    }

    #[test]
    fn test_view_mouse_enter_prefers_topmost_later_sibling() {
        let canvases: Vec<_> = smol::block_on(
            element!(ViewHoverTopmostApp)
                .mock_terminal_render_loop(MockTerminalConfig::with_events(futures::stream::iter(
                    vec![
                        TerminalEvent::FullscreenMouse(FullscreenMouseEvent::new(
                            MouseEventKind::Moved,
                            1,
                            0,
                        )),
                        TerminalEvent::FullscreenMouse(FullscreenMouseEvent::new(
                            MouseEventKind::Moved,
                            9,
                            0,
                        )),
                    ],
                )))
                .collect(),
        );
        assert_eq!(canvases.last().unwrap().to_string(), "0/0/1/1\n");
    }

    #[component]
    fn ViewResizeApp(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let mut seen = hooks.use_state(|| None::<(u16, u16)>);

        if seen.get().is_some() {
            system.exit();
        }

        element! {
            View(
                on_resize: move |event: ViewResizeEvent| {
                    seen.set(Some((event.columns, event.rows)));
                },
            ) {
                Text(content: format!("resize={:?}", seen.get()))
            }
        }
    }

    #[test]
    fn test_view_on_resize_receives_terminal_size() {
        let canvases: Vec<_> = smol::block_on(
            element!(ViewResizeApp)
                .mock_terminal_render_loop(MockTerminalConfig::with_events(futures::stream::iter(
                    vec![TerminalEvent::Resize(120, 40)],
                )))
                .collect(),
        );
        assert_eq!(
            canvases.last().unwrap().to_string(),
            "resize=Some((120, 40))\n"
        );
    }

    #[component]
    fn ViewFocusedKeyApp(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let mut key_count = hooks.use_state(|| 0usize);
        let mut events = hooks.use_state(|| 0usize);
        hooks.use_terminal_events(move |event| {
            if matches!(event, TerminalEvent::Key(_)) {
                events += 1;
            }
        });

        if events.get() > 0 {
            system.exit();
        }

        element! {
            FocusScope {
                View(
                    focusable: true,
                    auto_focus: true,
                    on_key_down: move |key: ViewKeyboardEvent| {
                        if key.code == KeyCode::Char('a') {
                            key_count += 1;
                        }
                    },
                ) {
                    Text(content: format!("keys={}", key_count.get()))
                }
            }
        }
    }

    #[test]
    fn test_view_focusable_on_key_down() {
        let canvases: Vec<_> = smol::block_on(
            element!(ViewFocusedKeyApp)
                .mock_terminal_render_loop(MockTerminalConfig::with_events(futures::stream::iter(
                    vec![TerminalEvent::Key(KeyEvent::new(
                        KeyEventKind::Press,
                        KeyCode::Char('a'),
                    ))],
                )))
                .collect(),
        );
        assert_eq!(canvases.last().unwrap().to_string(), "keys=1\n");
    }

    #[component]
    fn ViewKeyboardEventApp(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let mut child_keys = hooks.use_state(|| 0usize);
        let mut parent_keys = hooks.use_state(|| 0usize);
        let mut key_events = hooks.use_state(|| 0usize);
        hooks.use_terminal_events(move |event| {
            if matches!(event, TerminalEvent::Key(_)) {
                key_events += 1;
            }
        });
        hooks.use_propagated_terminal_events(move |event| {
            if matches!(
                event.event(),
                TerminalEvent::Key(KeyEvent {
                    code: KeyCode::Char('a'),
                    ..
                })
            ) {
                parent_keys += 1;
            }
        });

        if key_events.get() > 0 {
            system.exit();
        }

        element! {
            FocusScope {
                View(
                    focusable: true,
                    auto_focus: true,
                    on_key_down: move |event: ViewKeyboardEvent| {
                        if event.key == "a" && !event.default_prevented() {
                            event.prevent_default();
                            event.stop_propagation();
                            child_keys += 1;
                        }
                    },
                ) {
                    Text(content: format!("{}/{}", child_keys.get(), parent_keys.get()))
                }
            }
        }
    }

    #[test]
    fn test_view_keyboard_event_can_stop_propagation() {
        let canvases: Vec<_> = smol::block_on(
            element!(ViewKeyboardEventApp)
                .mock_terminal_render_loop(MockTerminalConfig::with_events(futures::stream::iter(
                    vec![TerminalEvent::Key(KeyEvent::new(
                        KeyEventKind::Press,
                        KeyCode::Char('a'),
                    ))],
                )))
                .collect(),
        );
        assert_eq!(canvases.last().unwrap().to_string(), "1/0\n");
    }

    #[component]
    fn ViewKeyboardBubbleApp(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let mut parent_keys = hooks.use_state(|| 0usize);
        let mut sibling_keys = hooks.use_state(|| 0usize);
        let mut key_events = hooks.use_state(|| 0usize);
        hooks.use_terminal_events(move |event| {
            if matches!(event, TerminalEvent::Key(_)) {
                key_events += 1;
            }
        });

        if key_events.get() > 0 {
            system.exit();
        }

        element! {
            FocusScope {
                View(
                    on_key_down: move |event: ViewKeyboardEvent| {
                        if event.key == "a" {
                            parent_keys += 1;
                        }
                    },
                ) {
                    View(focusable: true, auto_focus: true) {
                        Text(content: format!("{}:{}", parent_keys.get(), sibling_keys.get()))
                    }
                }
                View(
                    on_key_down: move |event: ViewKeyboardEvent| {
                        if event.key == "a" {
                            sibling_keys += 1;
                        }
                    },
                ) {
                    Text(content: "sibling")
                }
            }
        }
    }

    #[test]
    fn test_view_keyboard_event_bubbles_to_ancestor_not_sibling() {
        let canvases: Vec<_> = smol::block_on(
            element!(ViewKeyboardBubbleApp)
                .mock_terminal_render_loop(MockTerminalConfig::with_events(futures::stream::iter(
                    vec![TerminalEvent::Key(KeyEvent::new(
                        KeyEventKind::Press,
                        KeyCode::Char('a'),
                    ))],
                )))
                .collect(),
        );
        assert_eq!(canvases.last().unwrap().to_string(), "1:0sibling\n");
    }

    #[component]
    fn ViewKeyboardAcrossFocusScopeApp(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let mut parent_keys = hooks.use_state(|| 0usize);
        let mut key_events = hooks.use_state(|| 0usize);
        hooks.use_terminal_events(move |event| {
            if matches!(event, TerminalEvent::Key(_)) {
                key_events += 1;
            }
        });

        if key_events.get() > 0 {
            system.exit();
        }

        element! {
            View(
                on_key_down: move |event: ViewKeyboardEvent| {
                    if event.key == "a"
                        && event.phase == ViewEventPhase::Bubbling
                        && event.current_target.is_none()
                        && event.target_view.is_some()
                        && event.current_target_view.is_some()
                        && event.target_view != event.current_target_view
                    {
                        parent_keys += 1;
                    }
                },
            ) {
                FocusScope {
                    View(focusable: true, auto_focus: true) {
                        Text(content: format!("parent_keys={}", parent_keys.get()))
                    }
                }
            }
        }
    }

    #[test]
    fn test_view_keyboard_bubbles_across_focus_scope_boundary_with_view_targets() {
        let canvases: Vec<_> = smol::block_on(
            element!(ViewKeyboardAcrossFocusScopeApp)
                .mock_terminal_render_loop(MockTerminalConfig::with_events(futures::stream::iter(
                    vec![TerminalEvent::Key(KeyEvent::new(
                        KeyEventKind::Press,
                        KeyCode::Char('a'),
                    ))],
                )))
                .collect(),
        );
        assert_eq!(canvases.last().unwrap().to_string(), "parent_keys=1\n");
    }

    #[component]
    fn ViewKeyboardCaptureOrderApp(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let log = hooks.use_state(String::new);
        let mut key_events = hooks.use_state(|| 0usize);
        hooks.use_terminal_events(move |event| {
            if matches!(event, TerminalEvent::Key(_)) {
                key_events += 1;
            }
        });

        if key_events.get() > 0 {
            system.exit();
        }

        let parent_capture_log = log;
        let parent_bubble_log = log;
        let target_capture_log = log;
        let target_bubble_log = log;
        let content = log.read().clone();
        element! {
            FocusScope {
                View(
                    on_key_down_capture: move |event: ViewKeyboardEvent| {
                        if event.key == "a"
                            && event.phase == ViewEventPhase::Capturing
                            && event.target.is_some()
                            && event.current_target.is_some()
                            && event.target != event.current_target
                        {
                            let next = format!("{}pc>", &*parent_capture_log.read());
                            let mut log = parent_capture_log;
                            log.set(next);
                        }
                    },
                    on_key_down: move |event: ViewKeyboardEvent| {
                        if event.key == "a"
                            && event.phase == ViewEventPhase::Bubbling
                            && event.target.is_some()
                            && event.current_target.is_some()
                            && event.target != event.current_target
                        {
                            let next = format!("{}pb>", &*parent_bubble_log.read());
                            let mut log = parent_bubble_log;
                            log.set(next);
                        }
                    },
                ) {
                    View(
                        focusable: true,
                        auto_focus: true,
                        on_key_down_capture: move |event: ViewKeyboardEvent| {
                            if event.key == "a"
                                && event.phase == ViewEventPhase::AtTarget
                                && event.target.is_some()
                                && event.target == event.current_target
                            {
                                let next = format!("{}tc>", &*target_capture_log.read());
                                let mut log = target_capture_log;
                                log.set(next);
                            }
                        },
                        on_key_down: move |event: ViewKeyboardEvent| {
                            if event.key == "a"
                                && event.phase == ViewEventPhase::AtTarget
                                && event.target.is_some()
                                && event.target == event.current_target
                            {
                                let next = format!("{}tb>", &*target_bubble_log.read());
                                let mut log = target_bubble_log;
                                log.set(next);
                            }
                        },
                    ) {
                        Text(content)
                    }
                }
            }
        }
    }

    #[test]
    fn test_view_keyboard_capture_runs_before_target_and_bubble() {
        let canvases: Vec<_> = smol::block_on(
            element!(ViewKeyboardCaptureOrderApp)
                .mock_terminal_render_loop(MockTerminalConfig::with_events(futures::stream::iter(
                    vec![TerminalEvent::Key(KeyEvent::new(
                        KeyEventKind::Press,
                        KeyCode::Char('a'),
                    ))],
                )))
                .collect(),
        );
        assert_eq!(canvases.last().unwrap().to_string(), "pc>tc>tb>pb>\n");
    }

    #[component]
    fn ViewKeyboardStopPropagationAtTargetApp(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let mut target_capture = hooks.use_state(|| 0usize);
        let mut target_bubble = hooks.use_state(|| 0usize);
        let mut parent_bubble = hooks.use_state(|| 0usize);
        let mut key_events = hooks.use_state(|| 0usize);
        hooks.use_terminal_events(move |event| {
            if matches!(event, TerminalEvent::Key(_)) {
                key_events += 1;
            }
        });

        if key_events.get() > 0 {
            system.exit();
        }

        element! {
            FocusScope {
                View(on_key_down: move |_| parent_bubble += 1) {
                    View(
                        focusable: true,
                        auto_focus: true,
                        on_key_down_capture: move |event: ViewKeyboardEvent| {
                            if event.key == "a" {
                                event.stop_propagation();
                                target_capture += 1;
                            }
                        },
                        on_key_down: move |event: ViewKeyboardEvent| {
                            if event.key == "a" {
                                target_bubble += 1;
                            }
                        },
                    ) {
                        Text(content: format!(
                            "{}/{}/{}",
                            target_capture.get(),
                            target_bubble.get(),
                            parent_bubble.get()
                        ))
                    }
                }
            }
        }
    }

    #[test]
    fn test_view_keyboard_stop_propagation_allows_same_target_bubble() {
        let canvases: Vec<_> = smol::block_on(
            element!(ViewKeyboardStopPropagationAtTargetApp)
                .mock_terminal_render_loop(MockTerminalConfig::with_events(futures::stream::iter(
                    vec![TerminalEvent::Key(KeyEvent::new(
                        KeyEventKind::Press,
                        KeyCode::Char('a'),
                    ))],
                )))
                .collect(),
        );
        assert_eq!(canvases.last().unwrap().to_string(), "1/1/0\n");
    }

    #[component]
    fn ViewKeyboardImmediateStopAtTargetApp(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let mut target_capture = hooks.use_state(|| 0usize);
        let mut target_bubble = hooks.use_state(|| 0usize);
        let mut parent_bubble = hooks.use_state(|| 0usize);
        let mut key_events = hooks.use_state(|| 0usize);
        hooks.use_terminal_events(move |event| {
            if matches!(event, TerminalEvent::Key(_)) {
                key_events += 1;
            }
        });

        if key_events.get() > 0 {
            system.exit();
        }

        element! {
            FocusScope {
                View(on_key_down: move |_| parent_bubble += 1) {
                    View(
                        focusable: true,
                        auto_focus: true,
                        on_key_down_capture: move |event: ViewKeyboardEvent| {
                            if event.key == "a" {
                                event.stop_immediate_propagation();
                                target_capture += 1;
                            }
                        },
                        on_key_down: move |event: ViewKeyboardEvent| {
                            if event.key == "a" {
                                target_bubble += 1;
                            }
                        },
                    ) {
                        Text(content: format!(
                            "{}/{}/{}",
                            target_capture.get(),
                            target_bubble.get(),
                            parent_bubble.get()
                        ))
                    }
                }
            }
        }
    }

    #[test]
    fn test_view_keyboard_stop_immediate_blocks_same_target_bubble() {
        let canvases: Vec<_> = smol::block_on(
            element!(ViewKeyboardImmediateStopAtTargetApp)
                .mock_terminal_render_loop(MockTerminalConfig::with_events(futures::stream::iter(
                    vec![TerminalEvent::Key(KeyEvent::new(
                        KeyEventKind::Press,
                        KeyCode::Char('a'),
                    ))],
                )))
                .collect(),
        );
        assert_eq!(canvases.last().unwrap().to_string(), "1/0/0\n");
    }

    #[component]
    fn ViewKeyboardPreventDefaultApp(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let mut prevented = hooks.use_state(|| false);
        let mut second_focuses = hooks.use_state(|| 0usize);
        let mut key_events = hooks.use_state(|| 0usize);
        hooks.use_terminal_events(move |event| {
            if matches!(event, TerminalEvent::Key(_)) {
                key_events += 1;
            }
        });

        if key_events.get() > 0 {
            system.exit();
        }

        element! {
            FocusScope {
                View(
                    focusable: true,
                    auto_focus: true,
                    on_key_down: move |event: ViewKeyboardEvent| {
                        if event.key == "tab" {
                            event.prevent_default();
                            prevented.set(true);
                        }
                    },
                ) {
                    Text(content: format!("{}:{}", prevented.get(), second_focuses.get()))
                }
                View(
                    focusable: true,
                    on_focus: move |_| second_focuses += 1,
                ) {
                    Text(content: "second")
                }
            }
        }
    }

    #[test]
    fn test_view_keyboard_event_prevent_default_blocks_focus_scope_tab() {
        let canvases: Vec<_> = smol::block_on(
            element!(ViewKeyboardPreventDefaultApp)
                .mock_terminal_render_loop(MockTerminalConfig::with_events(futures::stream::iter(
                    vec![TerminalEvent::Key(KeyEvent::new(
                        KeyEventKind::Press,
                        KeyCode::Tab,
                    ))],
                )))
                .collect(),
        );
        assert_eq!(canvases.last().unwrap().to_string(), "true:0second\n");
    }

    #[component]
    fn ViewKeyboardStopDoesNotPreventDefaultApp(
        mut hooks: Hooks,
    ) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let mut stopped = hooks.use_state(|| false);
        let mut second_focuses = hooks.use_state(|| 0usize);
        let mut key_events = hooks.use_state(|| 0usize);
        hooks.use_terminal_events(move |event| {
            if matches!(event, TerminalEvent::Key(_)) {
                key_events += 1;
            }
        });

        if key_events.get() > 0 && second_focuses.get() > 0 {
            system.exit();
        }

        element! {
            FocusScope {
                View(
                    focusable: true,
                    auto_focus: true,
                    on_key_down_capture: move |event: ViewKeyboardEvent| {
                        if event.key == "tab" {
                            event.stop_propagation();
                            stopped.set(true);
                        }
                    },
                ) {
                    Text(content: format!("{}:{}", stopped.get(), second_focuses.get()))
                }
                View(
                    focusable: true,
                    on_focus: move |_| second_focuses += 1,
                ) {
                    Text(content: "second")
                }
            }
        }
    }

    #[test]
    fn test_view_keyboard_stop_propagation_does_not_block_focus_scope_tab_default() {
        let canvases: Vec<_> = smol::block_on(
            element!(ViewKeyboardStopDoesNotPreventDefaultApp)
                .mock_terminal_render_loop(MockTerminalConfig::with_events(futures::stream::iter(
                    vec![TerminalEvent::Key(KeyEvent::new(
                        KeyEventKind::Press,
                        KeyCode::Tab,
                    ))],
                )))
                .collect(),
        );
        assert_eq!(canvases.last().unwrap().to_string(), "true:1second\n");
    }

    #[component]
    fn ViewKeyboardDefaultPreventedBubblesApp(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let mut child_prevented = hooks.use_state(|| false);
        let mut parent_saw_prevented = hooks.use_state(|| false);
        let mut key_events = hooks.use_state(|| 0usize);
        hooks.use_terminal_events(move |event| {
            if matches!(event, TerminalEvent::Key(_)) {
                key_events += 1;
            }
        });

        if key_events.get() > 0 {
            system.exit();
        }

        element! {
            FocusScope {
                View(
                    on_key_down: move |event: ViewKeyboardEvent| {
                        if event.key == "a" {
                            parent_saw_prevented.set(event.default_prevented());
                        }
                    },
                ) {
                    View(
                        focusable: true,
                        auto_focus: true,
                        on_key_down: move |event: ViewKeyboardEvent| {
                            if event.key == "a" {
                                event.prevent_default();
                                child_prevented.set(event.default_prevented());
                            }
                        },
                    ) {
                        Text(content: format!(
                            "{}:{}",
                            child_prevented.get(),
                            parent_saw_prevented.get()
                        ))
                    }
                }
            }
        }
    }

    #[test]
    fn test_view_keyboard_default_prevented_is_visible_to_ancestor_bubble() {
        let canvases: Vec<_> = smol::block_on(
            element!(ViewKeyboardDefaultPreventedBubblesApp)
                .mock_terminal_render_loop(MockTerminalConfig::with_events(futures::stream::iter(
                    vec![TerminalEvent::Key(KeyEvent::new(
                        KeyEventKind::Press,
                        KeyCode::Char('a'),
                    ))],
                )))
                .collect(),
        );
        assert_eq!(canvases.last().unwrap().to_string(), "true:true\n");
    }

    #[component]
    fn ViewPasteEventApp(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let mut child_pastes = hooks.use_state(|| 0usize);
        let mut parent_pastes = hooks.use_state(|| 0usize);
        let mut paste_events = hooks.use_state(|| 0usize);
        hooks.use_terminal_events(move |event| {
            if matches!(event, TerminalEvent::Paste(_)) {
                paste_events += 1;
            }
        });
        hooks.use_propagated_terminal_events(move |event| {
            if matches!(event.event(), TerminalEvent::Paste(_)) {
                parent_pastes += 1;
            }
        });

        if paste_events.get() > 0 {
            system.exit();
        }

        element! {
            FocusScope {
                View(
                    focusable: true,
                    auto_focus: true,
                    on_paste: move |event: ViewPasteEvent| {
                        if event.text == "xy" {
                            event.stop_propagation();
                            child_pastes += 1;
                        }
                    },
                ) {
                    Text(content: format!("{}/{}", child_pastes.get(), parent_pastes.get()))
                }
            }
        }
    }

    #[test]
    fn test_view_paste_event_can_stop_propagation() {
        let canvases: Vec<_> = smol::block_on(
            element!(ViewPasteEventApp)
                .mock_terminal_render_loop(MockTerminalConfig::with_events(futures::stream::iter(
                    vec![TerminalEvent::Paste("xy".to_string())],
                )))
                .collect(),
        );
        assert_eq!(canvases.last().unwrap().to_string(), "1/0\n");
    }

    #[component]
    fn ViewPasteStopPropagationAtTargetApp(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let mut target_capture = hooks.use_state(|| 0usize);
        let mut target_bubble = hooks.use_state(|| 0usize);
        let mut parent_bubble = hooks.use_state(|| 0usize);
        let mut paste_events = hooks.use_state(|| 0usize);
        hooks.use_terminal_events(move |event| {
            if matches!(event, TerminalEvent::Paste(_)) {
                paste_events += 1;
            }
        });

        if paste_events.get() > 0 {
            system.exit();
        }

        element! {
            FocusScope {
                View(on_paste: move |_| parent_bubble += 1) {
                    View(
                        focusable: true,
                        auto_focus: true,
                        on_paste_capture: move |event: ViewPasteEvent| {
                            if event.text == "xy" {
                                event.stop_propagation();
                                target_capture += 1;
                            }
                        },
                        on_paste: move |event: ViewPasteEvent| {
                            if event.text == "xy" {
                                target_bubble += 1;
                            }
                        },
                    ) {
                        Text(content: format!(
                            "{}/{}/{}",
                            target_capture.get(),
                            target_bubble.get(),
                            parent_bubble.get()
                        ))
                    }
                }
            }
        }
    }

    #[test]
    fn test_view_paste_stop_propagation_allows_same_target_bubble() {
        let canvases: Vec<_> = smol::block_on(
            element!(ViewPasteStopPropagationAtTargetApp)
                .mock_terminal_render_loop(MockTerminalConfig::with_events(futures::stream::iter(
                    vec![TerminalEvent::Paste("xy".to_string())],
                )))
                .collect(),
        );
        assert_eq!(canvases.last().unwrap().to_string(), "1/1/0\n");
    }

    #[component]
    fn ViewPasteDefaultPreventedBubblesApp(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let mut child_prevented = hooks.use_state(|| false);
        let mut parent_saw_prevented = hooks.use_state(|| false);
        let mut paste_events = hooks.use_state(|| 0usize);
        hooks.use_terminal_events(move |event| {
            if matches!(event, TerminalEvent::Paste(_)) {
                paste_events += 1;
            }
        });

        if paste_events.get() > 0 {
            system.exit();
        }

        element! {
            FocusScope {
                View(
                    on_paste: move |event: ViewPasteEvent| {
                        if event.text == "xy" {
                            parent_saw_prevented.set(event.default_prevented());
                        }
                    },
                ) {
                    View(
                        focusable: true,
                        auto_focus: true,
                        on_paste: move |event: ViewPasteEvent| {
                            if event.text == "xy" {
                                event.prevent_default();
                                child_prevented.set(event.default_prevented());
                            }
                        },
                    ) {
                        Text(content: format!(
                            "{}:{}",
                            child_prevented.get(),
                            parent_saw_prevented.get()
                        ))
                    }
                }
            }
        }
    }

    #[test]
    fn test_view_paste_default_prevented_is_visible_to_ancestor_bubble() {
        let canvases: Vec<_> = smol::block_on(
            element!(ViewPasteDefaultPreventedBubblesApp)
                .mock_terminal_render_loop(MockTerminalConfig::with_events(futures::stream::iter(
                    vec![TerminalEvent::Paste("xy".to_string())],
                )))
                .collect(),
        );
        assert_eq!(canvases.last().unwrap().to_string(), "true:true\n");
    }

    #[component]
    fn ViewFocusBlurApp(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let mut first_focus = hooks.use_state(|| 0usize);
        let mut first_blur = hooks.use_state(|| 0usize);
        let mut second_focus = hooks.use_state(|| 0usize);

        if first_focus.get() == 1 && first_blur.get() == 1 && second_focus.get() == 1 {
            system.exit();
        }

        element! {
            FocusScope {
                View(
                    focusable: true,
                    auto_focus: true,
                    on_focus: move |event: ViewFocusEvent| {
                        if event.event_type == "focus" && event.bubbles && !event.cancelable {
                            event.prevent_default();
                            if !event.default_prevented() {
                                first_focus += 1;
                            }
                        }
                    },
                    on_blur: move |event: ViewFocusEvent| {
                        if event.event_type == "blur" && event.bubbles && !event.cancelable {
                            event.prevent_default();
                            if !event.default_prevented() {
                                first_blur += 1;
                            }
                        }
                    },
                ) {
                    Text(content: "one")
                }
                View(
                    focusable: true,
                    on_focus: move |event: ViewFocusEvent| {
                        if event.event_type == "focus" && event.bubbles && !event.cancelable {
                            second_focus += 1;
                        }
                    },
                ) {
                    Text(content: format!("{}:{}:{}", first_focus.get(), first_blur.get(), second_focus.get()))
                }
            }
        }
    }

    #[test]
    fn test_view_focus_and_blur_handlers() {
        let canvases: Vec<_> = smol::block_on(
            element!(ViewFocusBlurApp)
                .mock_terminal_render_loop(MockTerminalConfig::with_events(futures::stream::iter(
                    vec![TerminalEvent::Key(KeyEvent::new(
                        KeyEventKind::Press,
                        KeyCode::Tab,
                    ))],
                )))
                .collect(),
        );
        assert_eq!(canvases.last().unwrap().to_string(), "one1:1:1\n");
    }

    #[component]
    fn ViewFocusStopPropagationAtTargetApp(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let mut target_capture = hooks.use_state(|| 0usize);
        let mut target_bubble = hooks.use_state(|| 0usize);
        let mut parent_bubble = hooks.use_state(|| 0usize);

        if target_bubble.get() > 0 {
            system.exit();
        }

        element! {
            FocusScope {
                View(on_focus: move |_| parent_bubble += 1) {
                    View(
                        focusable: true,
                        auto_focus: true,
                        on_focus_capture: move |event: ViewFocusEvent| {
                            if event.kind == ViewFocusEventKind::Focus {
                                event.stop_propagation();
                                target_capture += 1;
                            }
                        },
                        on_focus: move |event: ViewFocusEvent| {
                            if event.kind == ViewFocusEventKind::Focus {
                                target_bubble += 1;
                            }
                        },
                    ) {
                        Text(content: format!(
                            "{}/{}/{}",
                            target_capture.get(),
                            target_bubble.get(),
                            parent_bubble.get()
                        ))
                    }
                }
            }
        }
    }

    #[test]
    fn test_view_focus_stop_propagation_allows_same_target_bubble() {
        let canvases: Vec<_> = smol::block_on(
            element!(ViewFocusStopPropagationAtTargetApp)
                .mock_terminal_render_loop(MockTerminalConfig::default())
                .collect(),
        );
        assert_eq!(canvases.last().unwrap().to_string(), "1/1/0\n");
    }

    #[component]
    fn ViewFocusRelatedTargetApp(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let mut first_focus_related_none = hooks.use_state(|| false);
        let mut first_blur_related_some = hooks.use_state(|| false);
        let mut second_focus_related_some = hooks.use_state(|| false);

        if first_focus_related_none.get()
            && first_blur_related_some.get()
            && second_focus_related_some.get()
        {
            system.exit();
        }

        element! {
            FocusScope {
                View(
                    focusable: true,
                    auto_focus: true,
                    on_focus: move |event: ViewFocusEvent| {
                        first_focus_related_none.set(event.related_target.is_none());
                    },
                    on_blur: move |event: ViewFocusEvent| {
                        first_blur_related_some.set(event.related_target.is_some());
                    },
                ) {
                    Text(content: "one")
                }
                View(
                    focusable: true,
                    on_focus: move |event: ViewFocusEvent| {
                        second_focus_related_some.set(event.related_target.is_some());
                    },
                ) {
                    Text(content: format!(
                        "{}:{}:{}",
                        first_focus_related_none.get(),
                        first_blur_related_some.get(),
                        second_focus_related_some.get()
                    ))
                }
            }
        }
    }

    #[test]
    fn test_view_focus_event_related_target() {
        let canvases: Vec<_> = smol::block_on(
            element!(ViewFocusRelatedTargetApp)
                .mock_terminal_render_loop(MockTerminalConfig::with_events(futures::stream::iter(
                    vec![TerminalEvent::Key(KeyEvent::new(
                        KeyEventKind::Press,
                        KeyCode::Tab,
                    ))],
                )))
                .collect(),
        );
        assert_eq!(canvases.last().unwrap().to_string(), "onetrue:true:true\n");
    }

    #[component]
    fn ViewFocusCaptureOrderApp(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let log = hooks.use_state(String::new);
        if log.read().contains("pb>") {
            system.exit();
        }

        let parent_capture_log = log;
        let parent_bubble_log = log;
        let target_capture_log = log;
        let target_bubble_log = log;
        let content = log.read().clone();
        element! {
            FocusScope {
                View(
                    on_focus_capture: move |event: ViewFocusEvent| {
                        if event.phase == ViewEventPhase::Capturing
                            && event.target.is_some()
                            && event.current_target.is_some()
                            && event.target != event.current_target
                        {
                            let next = format!("{}pc>", &*parent_capture_log.read());
                            let mut log = parent_capture_log;
                            log.set(next);
                        }
                    },
                    on_focus: move |event: ViewFocusEvent| {
                        if event.phase == ViewEventPhase::Bubbling
                            && event.target.is_some()
                            && event.current_target.is_some()
                            && event.target != event.current_target
                        {
                            let next = format!("{}pb>", &*parent_bubble_log.read());
                            let mut log = parent_bubble_log;
                            log.set(next);
                        }
                    },
                ) {
                    View(
                        focusable: true,
                        auto_focus: true,
                        on_focus_capture: move |event: ViewFocusEvent| {
                            if event.phase == ViewEventPhase::AtTarget
                                && event.target.is_some()
                                && event.target == event.current_target
                            {
                                let next = format!("{}tc>", &*target_capture_log.read());
                                let mut log = target_capture_log;
                                log.set(next);
                            }
                        },
                        on_focus: move |event: ViewFocusEvent| {
                            if event.phase == ViewEventPhase::AtTarget
                                && event.target.is_some()
                                && event.target == event.current_target
                            {
                                let next = format!("{}tb>", &*target_bubble_log.read());
                                let mut log = target_bubble_log;
                                log.set(next);
                            }
                        },
                    ) {
                        Text(content)
                    }
                }
            }
        }
    }

    #[test]
    fn test_view_focus_capture_runs_before_target_and_bubble() {
        let canvases: Vec<_> = smol::block_on(
            element!(ViewFocusCaptureOrderApp)
                .mock_terminal_render_loop(MockTerminalConfig::default())
                .collect(),
        );
        assert_eq!(canvases.last().unwrap().to_string(), "pc>tc>tb>pb>\n");
    }

    #[component]
    fn ViewTabIndexSkipApp(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let mut key_events = hooks.use_state(|| 0usize);
        let mut programmatic_focuses = hooks.use_state(|| 0usize);
        let mut tabbable_focuses = hooks.use_state(|| 0usize);
        hooks.use_terminal_events(move |event| {
            if matches!(event, TerminalEvent::Key(_)) {
                key_events += 1;
            }
        });

        if key_events.get() > 0 && tabbable_focuses.get() == 1 {
            system.exit();
        }

        element! {
            FocusScope {
                View(
                    tab_index: Some(-1),
                    on_focus: move |_| programmatic_focuses += 1,
                ) {
                    Text(content: "p")
                }
                View(
                    focusable: true,
                    on_focus: move |_| tabbable_focuses += 1,
                ) {
                    Text(content: format!("{}:{}", programmatic_focuses.get(), tabbable_focuses.get()))
                }
            }
        }
    }

    #[test]
    fn test_view_tab_index_minus_one_skips_tab_traversal() {
        let canvases: Vec<_> = smol::block_on(
            element!(ViewTabIndexSkipApp)
                .mock_terminal_render_loop(MockTerminalConfig::with_events(futures::stream::iter(
                    vec![TerminalEvent::Key(KeyEvent::new(
                        KeyEventKind::Press,
                        KeyCode::Tab,
                    ))],
                )))
                .collect(),
        );
        assert_eq!(canvases.last().unwrap().to_string(), "p0:1\n");
    }

    #[component]
    fn ViewTabIndexAutoFocusApp(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let mut key_count = hooks.use_state(|| 0usize);
        let mut key_events = hooks.use_state(|| 0usize);
        hooks.use_terminal_events(move |event| {
            if matches!(event, TerminalEvent::Key(_)) {
                key_events += 1;
            }
        });

        if key_events.get() > 0 {
            system.exit();
        }

        element! {
            FocusScope {
                View(
                    tab_index: Some(-1),
                    auto_focus: true,
                    on_key_down: move |key: ViewKeyboardEvent| {
                        if key.code == KeyCode::Char('a') {
                            key_count += 1;
                        }
                    },
                ) {
                    Text(content: format!("keys={}", key_count.get()))
                }
            }
        }
    }

    #[test]
    fn test_view_tab_index_minus_one_can_autofocus_and_receive_keys() {
        let canvases: Vec<_> = smol::block_on(
            element!(ViewTabIndexAutoFocusApp)
                .mock_terminal_render_loop(MockTerminalConfig::with_events(futures::stream::iter(
                    vec![TerminalEvent::Key(KeyEvent::new(
                        KeyEventKind::Press,
                        KeyCode::Char('a'),
                    ))],
                )))
                .collect(),
        );
        assert_eq!(canvases.last().unwrap().to_string(), "keys=1\n");
    }

    #[component]
    fn NoSelectViewApp(hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        system.exit();
        element! {
            View(width: 4) {
                View(width: 2, no_select: true) {
                    Text(content: "ab")
                }
                Text(content: "cd")
            }
        }
    }

    #[component]
    fn NoSelectFromLeftEdgeViewApp(hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        system.exit();
        element! {
            View(width: 6) {
                View(margin_left: 2, width: 2, no_select_from_left_edge: true) {
                    Text(content: "xx")
                }
                Text(content: "yy")
            }
        }
    }

    #[test]
    fn test_view_no_select_marks_metadata_without_output_changes() {
        let canvases: Vec<_> = smol::block_on(
            element!(NoSelectViewApp)
                .mock_terminal_render_loop(MockTerminalConfig::default())
                .collect(),
        );

        assert_eq!(canvases.len(), 1);
        assert_eq!(canvases[0].to_string(), "abcd\n");
        assert!(canvases[0].is_no_select(0, 0));
        assert!(canvases[0].is_no_select(1, 0));
        assert!(!canvases[0].is_no_select(2, 0));
        assert!(!canvases[0].is_no_select(3, 0));
    }

    #[test]
    fn test_view_no_select_from_left_edge_marks_metadata_without_output_changes() {
        let canvases: Vec<_> = smol::block_on(
            element!(NoSelectFromLeftEdgeViewApp)
                .mock_terminal_render_loop(MockTerminalConfig::default())
                .collect(),
        );

        assert_eq!(canvases.len(), 1);
        assert_eq!(canvases[0].to_string(), "  xxyy\n");
        for x in 0..4 {
            assert!(canvases[0].is_no_select(x, 0), "col {x} should be noSelect");
        }
        assert!(!canvases[0].is_no_select(4, 0));
        assert!(!canvases[0].is_no_select(5, 0));
    }
}
