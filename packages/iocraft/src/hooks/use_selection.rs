use super::{
    Notification, NotificationContext, NotificationPriority, State, StdoutHandle, UseContext,
    UseState, UseTerminalDefaultEvents, UseTerminalEvents,
};
use crate::{
    Canvas, Color, ComponentDrawer, FullscreenMouseEvent, FullscreenSelectionEventOutcome,
    FullscreenSelectionKeyOutcome, Hook, Hooks, KeyCode, KeyEvent, KeyEventKind, KeyModifiers,
    MouseButton, MouseEventKind, SelectionCaptureSide, SelectionController,
    SelectionDragScrollDirection, SelectionFocusMove, SelectionHoverOutcome,
    SelectionScrollOutcome, StyleOverlay, TerminalEvent,
};
use std::{
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

mod private {
    pub trait Sealed {}
    impl Sealed for crate::Hooks<'_, '_> {}
}

/// App-level fullscreen text selection state.
///
/// This mirrors the CC Ink fork's instance-owned selection object: the
/// controller tracks anchor/focus/copy-on-select lifecycle while the theme
/// selection background is stored beside it so post-render overlays can use a
/// solid background instead of inverse-video.
#[derive(Clone)]
pub struct SelectionRuntimeState {
    controller: SelectionController,
    selection_bg_color: Color,
    subscribers: SharedSelectionSubscribers,
}

impl std::fmt::Debug for SelectionRuntimeState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SelectionRuntimeState")
            .field("controller", &self.controller)
            .field("selection_bg_color", &self.selection_bg_color)
            .finish_non_exhaustive()
    }
}

impl Default for SelectionRuntimeState {
    fn default() -> Self {
        Self {
            controller: SelectionController::new(),
            selection_bg_color: Color::Blue,
            subscribers: SharedSelectionSubscribers::default(),
        }
    }
}

#[derive(Clone, Default)]
struct SharedSelectionSubscribers(Arc<Mutex<SelectionSubscribers>>);

type SelectionListener = Arc<Mutex<Box<dyn FnMut() + Send + 'static>>>;

#[derive(Default)]
struct SelectionSubscribers {
    next_id: u64,
    listeners: Vec<(u64, SelectionListener)>,
}

impl SharedSelectionSubscribers {
    fn subscribe(&self, listener: impl FnMut() + Send + 'static) -> SelectionSubscription {
        let mut guard = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let id = guard.next_id;
        guard.next_id = guard.next_id.wrapping_add(1);
        guard
            .listeners
            .push((id, Arc::new(Mutex::new(Box::new(listener)))));
        SelectionSubscription {
            subscribers: Some(self.clone()),
            id,
        }
    }

    fn unsubscribe(&self, id: u64) {
        let mut guard = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard
            .listeners
            .retain(|(listener_id, _)| *listener_id != id);
    }

    fn notify(&self) {
        let listeners = {
            let guard = self
                .0
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            guard
                .listeners
                .iter()
                .map(|(_, listener)| listener.clone())
                .collect::<Vec<_>>()
        };
        for listener in listeners {
            let mut listener = listener
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            listener();
        }
    }
}

/// RAII subscription returned by [`SelectionContext::subscribe`].
///
/// Dropping the value removes the listener. Keep it in a hook such as
/// [`use_const`](crate::hooks::UseConst::use_const) or application state for as
/// long as you want to observe selection mutations.
pub struct SelectionSubscription {
    subscribers: Option<SharedSelectionSubscribers>,
    id: u64,
}

impl Default for SelectionSubscription {
    fn default() -> Self {
        Self {
            subscribers: None,
            id: 0,
        }
    }
}

impl Drop for SelectionSubscription {
    fn drop(&mut self) {
        if let Some(subscribers) = self.subscribers.take() {
            subscribers.unsubscribe(self.id);
        }
    }
}

/// Copyable handle to app-level fullscreen selection state.
///
/// The handle is cheap to pass through [`ContextProvider`](crate::components::ContextProvider)
/// and event closures. Mutating methods wake the owning component by writing to
/// the underlying [`State`], giving descendants an iocraft equivalent of CC
/// Ink's `useSelection()` / `useHasSelection()` pattern.
#[derive(Clone, Copy)]
pub struct SelectionContext {
    state: Option<State<SelectionRuntimeState>>,
}

impl Default for SelectionContext {
    fn default() -> Self {
        Self::disabled()
    }
}

impl SelectionContext {
    /// Creates a no-op selection handle. Used outside fullscreen/selection
    /// providers so hooks can return no-op behavior like CC Ink does outside
    /// alt-screen.
    pub fn disabled() -> Self {
        Self { state: None }
    }

    pub(crate) fn new(state: State<SelectionRuntimeState>) -> Self {
        Self { state: Some(state) }
    }

    /// Returns whether this handle is backed by live selection state.
    pub fn is_enabled(&self) -> bool {
        self.state.is_some()
    }

    fn with_ref<R>(&self, f: impl FnOnce(&SelectionRuntimeState) -> R) -> Option<R> {
        let state = self.state?;
        let guard = state.try_read()?;
        Some(f(&guard))
    }

    fn with_mut<R>(&self, f: impl FnOnce(&mut SelectionRuntimeState) -> R) -> Option<R> {
        let mut state = self.state?;
        let (result, subscribers) = {
            let mut guard = state.try_write()?;
            let result = f(&mut guard);
            (result, guard.subscribers.clone())
        };
        subscribers.notify();
        Some(result)
    }

    /// Subscribes to selection runtime mutations.
    ///
    /// This mirrors CC Ink's external-store model for selection consumers that
    /// need a push signal instead of polling during render. The listener is
    /// called after controller/theme mutations such as `set_controller`,
    /// `clear_selection`, selection mouse/key routing, and copy-on-select guard
    /// consumption. Dropping the returned [`SelectionSubscription`] removes the
    /// listener.
    pub fn subscribe(&self, listener: impl FnMut() + Send + 'static) -> SelectionSubscription {
        self.with_ref(|s| s.subscribers.clone())
            .map(|subscribers| subscribers.subscribe(listener))
            .unwrap_or_default()
    }

    /// Returns a snapshot of the underlying controller.
    pub fn controller_snapshot(&self) -> SelectionController {
        self.with_ref(|s| s.controller.clone()).unwrap_or_default()
    }

    /// Replaces the underlying controller. This is primarily useful for tests
    /// and custom owners that already maintain a controller snapshot.
    pub fn set_controller(&self, controller: SelectionController) {
        self.with_mut(|s| s.controller = controller);
    }

    /// Returns whether text is currently selected.
    pub fn has_selection(&self) -> bool {
        self.with_ref(|s| s.controller.has_selection())
            .unwrap_or(false)
    }

    /// Returns selected text without clearing the highlight.
    pub fn copy_selection_no_clear_text(&self, canvas: &Canvas) -> String {
        self.with_ref(|s| s.controller.selected_text(canvas))
            .unwrap_or_default()
    }

    /// Returns selected text and clears the highlight.
    pub fn copy_selection_text(&self, canvas: &Canvas) -> String {
        self.with_mut(|s| s.controller.take_selected_text(canvas))
            .unwrap_or_default()
    }

    /// Returns whether [`SelectionContext::copy_on_select_text`] would mutate
    /// selection runtime bookkeeping.
    pub fn copy_on_select_would_mutate(&self) -> bool {
        self.with_ref(|s| s.controller.copy_on_select_would_mutate())
            .unwrap_or(false)
    }

    /// Returns text for CC Ink-style copy-on-select, at most once per settled
    /// selection, without clearing the highlight.
    pub fn copy_on_select_text(&self, canvas: &Canvas) -> Option<String> {
        if !self.copy_on_select_would_mutate() {
            return None;
        }
        self.with_mut(|s| s.controller.copy_on_select_text(canvas))
            .flatten()
    }

    /// Clears the current selection.
    pub fn clear_selection(&self) {
        if self.has_selection() {
            self.with_mut(|s| s.controller.clear());
        }
    }

    /// The solid background color used for selection overlays.
    pub fn selection_bg_color(&self) -> Color {
        self.with_ref(|s| s.selection_bg_color)
            .unwrap_or(Color::Blue)
    }

    /// Sets the solid background color used by [`SelectionContext::apply_overlay`].
    pub fn set_selection_bg_color(&self, color: Color) {
        if self.selection_bg_color() != color {
            self.with_mut(|s| s.selection_bg_color = color);
        }
    }

    /// Applies this selection's post-render overlay to `canvas`.
    pub fn apply_overlay(&self, canvas: &mut Canvas) -> bool {
        self.with_ref(|s| {
            s.controller.selection().apply_overlay(
                canvas,
                StyleOverlay::selection_background(s.selection_bg_color),
            )
        })
        .unwrap_or(false)
    }

    /// Routes a decoded fullscreen mouse event through the selection controller.
    pub fn handle_fullscreen_mouse_event(
        &self,
        canvas: &Canvas,
        event: &FullscreenMouseEvent,
        now_ms: u64,
        click_consumed: bool,
    ) -> FullscreenSelectionEventOutcome {
        match event.kind {
            MouseEventKind::ScrollUp
            | MouseEventKind::ScrollDown
            | MouseEventKind::ScrollLeft
            | MouseEventKind::ScrollRight
                if !self.has_selection() =>
            {
                return FullscreenSelectionEventOutcome::Wheel {
                    cleared_selection: false,
                };
            }
            MouseEventKind::Moved
                if !self
                    .with_ref(|s| {
                        s.controller.no_button_motion_would_change(
                            event.column as usize,
                            event.row as usize,
                        )
                    })
                    .unwrap_or(false) =>
            {
                return FullscreenSelectionEventOutcome::Hover(SelectionHoverOutcome {
                    finished_drag: false,
                    hover: None,
                });
            }
            MouseEventKind::Drag(button) if button != MouseButton::Left => {
                return FullscreenSelectionEventOutcome::Ignored;
            }
            MouseEventKind::Up(button)
                if button != MouseButton::Left
                    && !self
                        .with_ref(|s| s.controller.selection().is_dragging())
                        .unwrap_or(false) =>
            {
                return FullscreenSelectionEventOutcome::NonLeftRelease {
                    finished_drag: false,
                };
            }
            _ => {}
        }
        self.with_mut(|s| {
            s.controller
                .handle_fullscreen_mouse_event(canvas, event, now_ms, click_consumed)
        })
        .unwrap_or(FullscreenSelectionEventOutcome::Ignored)
    }

    /// Routes a decoded fullscreen key event through the selection controller.
    pub fn handle_fullscreen_key_event(
        &self,
        event: &KeyEvent,
        width: usize,
        height: usize,
    ) -> FullscreenSelectionKeyOutcome {
        if event.kind != KeyEventKind::Press || !self.has_selection() {
            return FullscreenSelectionKeyOutcome::Ignored;
        }
        self.with_mut(|s| {
            s.controller
                .handle_fullscreen_key_event(event, width, height)
        })
        .unwrap_or(FullscreenSelectionKeyOutcome::Ignored)
    }

    /// Shifts only the anchor row, used by drag autoscroll.
    pub fn shift_anchor(&self, delta: isize, min_row: usize, max_row: usize) {
        let has_anchor = self
            .with_ref(|s| s.controller.selection().anchor().is_some())
            .unwrap_or(false);
        if !has_anchor {
            return;
        }
        self.with_mut(|s| {
            s.controller
                .selection_mut()
                .shift_anchor(delta, min_row, max_row)
        });
    }

    /// Shifts anchor and focus together after keyboard/programmatic scrolling.
    pub fn shift_selection(&self, delta: isize, min_row: usize, max_row: usize, width: usize) {
        if !self.has_selection() {
            return;
        }
        self.with_mut(|s| {
            s.controller
                .selection_mut()
                .shift_rows(delta, min_row, max_row, width)
        });
    }

    /// Moves selection focus for Shift+arrow/Home/End keyboard selection.
    pub fn move_focus(&self, movement: SelectionFocusMove, width: usize, height: usize) -> bool {
        if !self.has_selection() {
            return false;
        }
        self.with_mut(|s| {
            s.controller
                .selection_mut()
                .move_focus_by(movement, width, height)
        })
        .unwrap_or(false)
    }

    /// Captures text from rows about to scroll out of the viewport.
    pub fn capture_scrolled_rows(
        &self,
        canvas: &Canvas,
        first_row: usize,
        last_row: usize,
        side: SelectionCaptureSide,
    ) {
        if !self.has_selection() || first_row > last_row {
            return;
        }
        self.with_mut(|s| {
            s.controller
                .selection_mut()
                .capture_scrolled_rows(canvas, first_row, last_row, side)
        });
    }

    /// Translates selection for sticky follow-scroll.
    pub fn translate_for_follow_scroll(
        &self,
        canvas: &Canvas,
        delta: usize,
        viewport_top: usize,
        viewport_bottom: usize,
    ) -> SelectionScrollOutcome {
        if delta == 0 || viewport_top > viewport_bottom || !self.has_selection() {
            return SelectionScrollOutcome::default();
        }
        self.with_mut(|s| {
            s.controller
                .translate_for_follow_scroll(canvas, delta, viewport_top, viewport_bottom)
        })
        .unwrap_or_default()
    }

    /// Translates selection for a synchronous scroll jump.
    ///
    /// This mirrors CC Ink's `translateSelectionForJump(...)`: callers should
    /// pass the pre-scroll screen buffer plus the actual signed scroll delta.
    /// Positive deltas mean content moved up; negative deltas mean content
    /// moved down. Rows that leave the viewport are captured before endpoints
    /// shift, so copying still includes text that just scrolled offscreen.
    pub fn translate_for_scroll_jump(
        &self,
        canvas: &Canvas,
        delta: isize,
        viewport_top: usize,
        viewport_bottom: usize,
    ) -> SelectionScrollOutcome {
        if delta == 0 || viewport_top > viewport_bottom || !self.has_selection() {
            return SelectionScrollOutcome::default();
        }
        self.with_mut(|s| {
            s.controller
                .translate_for_scroll_jump(canvas, delta, viewport_top, viewport_bottom)
        })
        .unwrap_or_default()
    }

    /// Computes drag-autoscroll direction relative to a scroll viewport.
    ///
    /// This is the context-level counterpart to CC Ink's
    /// `dragScrollDirection(...)`. It returns `None` when the current drag is
    /// not owned by the viewport (for example the anchor started in a static
    /// footer/header) or when reversing direction would corrupt captured rows.
    pub fn drag_scroll_direction(
        &self,
        viewport_top: usize,
        viewport_bottom: usize,
    ) -> Option<SelectionDragScrollDirection> {
        if viewport_top > viewport_bottom {
            return None;
        }
        self.with_mut(|s| {
            s.controller
                .drag_scroll_direction(viewport_top, viewport_bottom)
        })
        .flatten()
    }

    /// Translates anchor for drag autoscroll.
    pub fn translate_for_drag_autoscroll(
        &self,
        canvas: &Canvas,
        direction: SelectionDragScrollDirection,
        lines: usize,
        viewport_top: usize,
        viewport_bottom: usize,
    ) -> SelectionScrollOutcome {
        let has_anchor = self
            .with_ref(|s| s.controller.selection().anchor().is_some())
            .unwrap_or(false);
        if lines == 0 || viewport_top > viewport_bottom || !has_anchor {
            return SelectionScrollOutcome::default();
        }
        self.with_mut(|s| {
            s.controller.translate_for_drag_autoscroll(
                canvas,
                direction,
                lines,
                viewport_top,
                viewport_bottom,
            )
        })
        .unwrap_or_default()
    }
}

/// Creates a selection context owned by the current component.
///
/// Provide the returned handle to descendants with
/// [`ContextProvider`](crate::components::ContextProvider) and access it via
/// [`UseSelection::use_selection`].
pub fn create_selection_context(hooks: &mut Hooks<'_, '_>) -> SelectionContext {
    SelectionContext::new(hooks.use_state(SelectionRuntimeState::default))
}

#[derive(Default)]
struct UseSelectionOverlayImpl {
    selection: SelectionContext,
}

impl Hook for UseSelectionOverlayImpl {
    fn post_component_draw(&mut self, drawer: &mut ComponentDrawer) {
        self.selection.apply_overlay(drawer.root_canvas_mut());
    }
}

/// Outcome produced by [`UseSelection::use_fullscreen_selection_events`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FullscreenSelectionDispatchOutcome {
    /// A fullscreen mouse event was routed through the selection controller.
    Mouse(FullscreenSelectionEventOutcome),
    /// A key event was routed through the selection controller.
    Key(FullscreenSelectionKeyOutcome),
}

#[derive(Default)]
struct UseFullscreenSelectionEventsImpl {
    canvas: Arc<Mutex<Option<Canvas>>>,
}

impl Hook for UseFullscreenSelectionEventsImpl {
    fn post_component_draw(&mut self, drawer: &mut ComponentDrawer) {
        *self
            .canvas
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) =
            Some(drawer.root_canvas_mut().clone());
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

#[derive(Default)]
struct UseCopyOnSelectClipboardImpl {
    selection: SelectionContext,
    stdout: Option<StdoutHandle>,
    active: bool,
}

impl Hook for UseCopyOnSelectClipboardImpl {
    fn post_component_draw(&mut self, drawer: &mut ComponentDrawer) {
        if !self.active {
            return;
        }
        if let Some(stdout) = &self.stdout {
            stdout.copy_on_select_context(&self.selection, drawer.root_canvas_mut());
        }
    }
}

struct UseCopyOnSelectTextImpl<F> {
    selection: SelectionContext,
    active: bool,
    on_copied: Option<F>,
}

impl<F> Default for UseCopyOnSelectTextImpl<F> {
    fn default() -> Self {
        Self {
            selection: SelectionContext::disabled(),
            active: false,
            on_copied: None,
        }
    }
}

impl<F> Hook for UseCopyOnSelectTextImpl<F>
where
    F: FnMut(String) + Send + Unpin,
{
    fn post_component_draw(&mut self, drawer: &mut ComponentDrawer) {
        if !self.active {
            return;
        }
        let Some(text) = self.selection.copy_on_select_text(drawer.root_canvas_mut()) else {
            return;
        };
        if let Some(on_copied) = self.on_copied.as_mut() {
            on_copied(text);
        }
    }
}

/// Clipboard transport label used for copied-selection notifications.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SelectionClipboardPath {
    /// Native clipboard transport, matching Claude Code's `native` toast wording.
    Native,
    /// tmux buffer transport, matching Claude Code's tmux paste hint wording.
    TmuxBuffer,
    /// OSC 52 clipboard transport. This is iocraft's default clipboard path.
    #[default]
    Osc52,
}

fn copied_selection_notification(text: &str, path: SelectionClipboardPath) -> Notification {
    let n = text.chars().count();
    let (message, timeout) = match path {
        SelectionClipboardPath::Native => (format!("copied {n} chars to clipboard"), 2000),
        SelectionClipboardPath::TmuxBuffer => (
            format!("copied {n} chars to tmux buffer · paste with prefix + ]"),
            4000,
        ),
        SelectionClipboardPath::Osc52 => (
            format!("sent {n} chars via OSC 52 · check terminal clipboard settings if paste fails"),
            4000,
        ),
    };
    Notification::new("selection-copied", message, NotificationPriority::Immediate)
        .with_color(Color::Cyan)
        .with_timeout(Duration::from_millis(timeout))
}

#[derive(Default)]
struct UseSelectionCopyNotificationsImpl {
    selection: SelectionContext,
    notifications: NotificationContext,
    stdout: Option<StdoutHandle>,
    active: bool,
    path: SelectionClipboardPath,
    canvas: Arc<Mutex<Option<Canvas>>>,
}

impl Hook for UseSelectionCopyNotificationsImpl {
    fn post_component_draw(&mut self, drawer: &mut ComponentDrawer) {
        let canvas = drawer.root_canvas_mut().clone();
        if self.active {
            if let Some(stdout) = &self.stdout {
                if let Some(text) = stdout.copy_on_select_context(&self.selection, &canvas) {
                    self.notifications
                        .add_notification(copied_selection_notification(&text, self.path));
                }
            }
        }
        *self
            .canvas
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(canvas);
    }
}

fn is_legacy_copy_key(event: &KeyEvent) -> bool {
    matches!(event.code, KeyCode::Char('c') | KeyCode::Char('C'))
        && event.modifiers.contains(KeyModifiers::CONTROL)
        && !event
            .modifiers
            .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT | KeyModifiers::META)
}

/// Access to fullscreen text selection operations.
pub trait UseSelection<'a>: private::Sealed {
    /// Returns the nearest selection context, or a no-op handle when none is
    /// provided.
    fn use_selection(&self) -> SelectionContext;

    /// Returns `true` when the nearest selection context currently has text
    /// selected. This is the iocraft counterpart to CC Ink's `useHasSelection`.
    fn use_has_selection(&self) -> bool;

    /// Pipes a theme/application selection background color into the selection
    /// context.
    ///
    /// This is the iocraft counterpart to CC Ink's `useSelectionBgColor(...)`:
    /// the retained renderer stays theme-agnostic, while the component layer
    /// provides the solid background color used by post-render selection
    /// overlays.
    fn use_selection_bg_color(&mut self, selection: SelectionContext, color: Color);

    /// Applies the given selection context's post-render overlay after this
    /// component and all of its children have drawn.
    ///
    /// Call this from a fullscreen root/owner component to mirror CC Ink's
    /// `applySelectionOverlay(frame.screen, selection, ...)` path: components
    /// render normally, then the retained screen buffer receives the solid
    /// selection background and damage metadata immediately before diffing.
    fn use_selection_overlay(&mut self, selection: SelectionContext);

    /// Routes fullscreen mouse/key events through a selection context using the
    /// last rendered root canvas as the screen buffer.
    ///
    /// This is the iocraft counterpart to CC Ink's App-level selection event
    /// path. It observes events after `View` click dispatch, so a consumed
    /// click is passed to [`SelectionContext::handle_fullscreen_mouse_event`]
    /// as `click_consumed` and link fallback is suppressed just like
    /// `dispatchClick(...)` returning true in CC Ink.
    fn use_fullscreen_selection_events<F>(
        &mut self,
        selection: SelectionContext,
        active: bool,
        on_outcome: F,
    ) where
        F: FnMut(FullscreenSelectionDispatchOutcome) + Send + 'static;

    /// Automatically copies a settled fullscreen selection to the terminal
    /// clipboard via OSC 52, leaving the highlight intact.
    ///
    /// This is the iocraft counterpart to CC Ink's `useCopyOnSelect(...)`.
    /// `active=false` is a no-op and does not reset the copy guard.
    fn use_copy_on_select(
        &mut self,
        selection: SelectionContext,
        stdout: StdoutHandle,
        active: bool,
    );

    /// Automatically observes copy-on-select text without writing to the real
    /// clipboard. This is useful for demos/tests or for applications that want
    /// to route clipboard transport themselves.
    fn use_copy_on_select_text<F>(
        &mut self,
        selection: SelectionContext,
        active: bool,
        on_copied: F,
    ) where
        F: FnMut(String) + Send + Unpin + 'static;

    /// Wires CC-style copy feedback for fullscreen selections.
    ///
    /// This combines `useCopyOnSelect(...)` and `showCopiedToast(...)` from the
    /// CC app layer: settled drag/multi-click selections are copied without
    /// clearing the highlight, while legacy Ctrl+C copies and clears the active
    /// selection. Both paths enqueue a toast through [`NotificationContext`].
    fn use_selection_copy_notifications(
        &mut self,
        selection: SelectionContext,
        stdout: StdoutHandle,
        notifications: NotificationContext,
        active: bool,
        path: SelectionClipboardPath,
    );
}

impl UseSelection<'_> for Hooks<'_, '_> {
    fn use_selection(&self) -> SelectionContext {
        self.try_use_context::<SelectionContext>()
            .map(|ctx| *ctx)
            .unwrap_or_else(SelectionContext::disabled)
    }

    fn use_has_selection(&self) -> bool {
        self.use_selection().has_selection()
    }

    fn use_selection_bg_color(&mut self, selection: SelectionContext, color: Color) {
        selection.set_selection_bg_color(color);
    }

    fn use_selection_overlay(&mut self, selection: SelectionContext) {
        self.use_hook(UseSelectionOverlayImpl::default).selection = selection;
    }

    fn use_fullscreen_selection_events<F>(
        &mut self,
        selection: SelectionContext,
        active: bool,
        mut on_outcome: F,
    ) where
        F: FnMut(FullscreenSelectionDispatchOutcome) + Send + 'static,
    {
        let canvas = self
            .use_hook(UseFullscreenSelectionEventsImpl::default)
            .canvas
            .clone();
        self.use_terminal_default_events(move |event| {
            if !active || !selection.is_enabled() {
                return;
            }
            match event.event() {
                TerminalEvent::FullscreenMouse(mouse) => {
                    let canvas = canvas
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .clone();
                    let Some(canvas) = canvas else {
                        return;
                    };
                    let outcome = selection.handle_fullscreen_mouse_event(
                        &canvas,
                        mouse,
                        now_ms(),
                        event.is_propagation_stopped(),
                    );
                    on_outcome(FullscreenSelectionDispatchOutcome::Mouse(outcome));
                }
                TerminalEvent::Key(key) => {
                    let size = canvas
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .as_ref()
                        .map(|canvas| (canvas.width(), canvas.height()))
                        .unwrap_or((0, 0));
                    let outcome = selection.handle_fullscreen_key_event(key, size.0, size.1);
                    on_outcome(FullscreenSelectionDispatchOutcome::Key(outcome));
                }
                _ => {}
            }
        });
    }

    fn use_copy_on_select(
        &mut self,
        selection: SelectionContext,
        stdout: StdoutHandle,
        active: bool,
    ) {
        let hook = self.use_hook(UseCopyOnSelectClipboardImpl::default);
        hook.selection = selection;
        hook.stdout = Some(stdout);
        hook.active = active;
    }

    fn use_copy_on_select_text<F>(
        &mut self,
        selection: SelectionContext,
        active: bool,
        on_copied: F,
    ) where
        F: FnMut(String) + Send + Unpin + 'static,
    {
        let hook = self.use_hook(UseCopyOnSelectTextImpl::<F>::default);
        hook.selection = selection;
        hook.active = active;
        hook.on_copied = Some(on_copied);
    }

    fn use_selection_copy_notifications(
        &mut self,
        selection: SelectionContext,
        stdout: StdoutHandle,
        notifications: NotificationContext,
        active: bool,
        path: SelectionClipboardPath,
    ) {
        let hook = self.use_hook(UseSelectionCopyNotificationsImpl::default);
        hook.selection = selection;
        hook.stdout = Some(stdout.clone());
        hook.notifications = notifications;
        hook.active = active;
        hook.path = path;
        let canvas = hook.canvas.clone();

        self.use_propagated_terminal_events(move |event| {
            if !active || !selection.has_selection() {
                return;
            }
            let TerminalEvent::Key(key) = event.event() else {
                return;
            };
            if key.kind != KeyEventKind::Press || !is_legacy_copy_key(key) {
                return;
            }
            let canvas = canvas
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone();
            let Some(canvas) = canvas else {
                return;
            };
            let text = stdout.copy_selection_context(&selection, &canvas);
            if !text.trim().is_empty() {
                notifications.add_notification(copied_selection_notification(&text, path));
            }
            event.stop_propagation();
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{components::ContextProvider, prelude::*, Context};
    use crossterm::style::Colored;
    use futures::StreamExt;

    fn canvas_with_text() -> Canvas {
        let mut canvas = Canvas::new(8, 1);
        canvas
            .subview_mut(0, 0, 0, 0, 8, 1)
            .set_text(0, 0, "abcdef", CanvasTextStyle::default());
        canvas
    }

    fn canvas_with_rows() -> Canvas {
        let mut canvas = Canvas::new(6, 3);
        for row in 0..3 {
            canvas.subview_mut(0, 0, 0, 0, 6, 3).set_text(
                0,
                row,
                &format!("row{row}"),
                CanvasTextStyle::default(),
            );
        }
        canvas
    }

    #[component]
    fn SelectionConsumer(hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let selection = hooks.use_selection();
        let mut canvas = canvas_with_text();
        let text = selection.copy_selection_no_clear_text(&canvas);
        selection.apply_overlay(&mut canvas);
        let highlighted = canvas
            .resolved_text_style(1, 0)
            .is_some_and(|style| !style.invert);
        let mut ansi = Vec::new();
        canvas.write_ansi(&mut ansi).unwrap();
        let ansi = String::from_utf8_lossy(&ansi);
        let theme_bg = ansi.contains(&format!(
            "{}",
            Colored::BackgroundColor(selection.selection_bg_color())
        ));
        element!(Text(content: format!(
            "enabled={} has={} text={text:?} highlighted={highlighted} theme_bg={theme_bg}",
            selection.is_enabled(),
            hooks.use_has_selection()
        )))
    }

    #[component]
    fn SelectionProviderApp(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let selection = create_selection_context(&mut hooks);
        selection.set_selection_bg_color(Color::DarkBlue);
        if !selection.has_selection() {
            let mut controller = SelectionController::new();
            controller.selection_mut().start(1, 0);
            controller.selection_mut().update(3, 0);
            controller.selection_mut().finish();
            selection.set_controller(controller);
        }
        system.exit();
        element! {
            ContextProvider(value: Context::owned(selection)) {
                SelectionConsumer
            }
        }
    }

    #[test]
    fn test_selection_context_hooks_and_overlay() {
        let canvases: Vec<_> = smol::block_on(
            element!(SelectionProviderApp)
                .mock_terminal_render_loop(MockTerminalConfig::default())
                .collect::<Vec<_>>(),
        );
        assert_eq!(
            canvases.last().unwrap().to_string(),
            "enabled=true has=true text=\"bcd\" highlighted=true theme_bg=true\n"
        );
    }

    #[component]
    fn SelectionScrollJumpContextApp(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let selection = create_selection_context(&mut hooks);
        let mut result = hooks.use_state(String::new);

        if result.read().is_empty() {
            let mut canvas = canvas_with_rows();
            let mut controller = SelectionController::new();
            controller.selection_mut().start(0, 0);
            controller.selection_mut().update(3, 2);
            selection.set_controller(controller);

            let outcome = selection.translate_for_scroll_jump(&canvas, 1, 0, 2);
            canvas.shift_rows(0, 2, 1);
            result.set(format!(
                "translated={} cleared={} text={:?}",
                outcome.translated,
                outcome.cleared,
                selection.copy_selection_no_clear_text(&canvas)
            ));
        } else {
            system.exit();
        }

        element!(Text(content: result.read().clone()))
    }

    #[test]
    fn test_selection_context_scroll_jump_preserves_copied_text() {
        let canvases: Vec<_> = smol::block_on(
            element!(SelectionScrollJumpContextApp)
                .mock_terminal_render_loop(MockTerminalConfig::default())
                .collect::<Vec<_>>(),
        );
        assert_eq!(
            canvases.last().unwrap().to_string(),
            "translated=true cleared=false text=\"row0\\nrow1\\nrow2\"\n"
        );
    }

    #[component]
    fn SelectionOverlayHookApp(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let selection = create_selection_context(&mut hooks);
        hooks.use_selection_bg_color(selection, Color::DarkBlue);
        if !selection.has_selection() {
            let mut controller = SelectionController::new();
            controller.selection_mut().start(1, 0);
            controller.selection_mut().update(3, 0);
            controller.selection_mut().finish();
            selection.set_controller(controller);
        }
        hooks.use_selection_overlay(selection);
        system.exit();
        element! {
            ContextProvider(value: Context::owned(selection)) {
                Text(content: "abcdef")
            }
        }
    }

    #[test]
    fn test_use_selection_overlay_applies_after_child_draw_and_marks_damage() {
        let canvases: Vec<_> = smol::block_on(
            element!(SelectionOverlayHookApp)
                .mock_terminal_render_loop(MockTerminalConfig::default())
                .collect::<Vec<_>>(),
        );
        let canvas = canvases.last().unwrap();
        let mut ansi = Vec::new();
        canvas.write_ansi(&mut ansi).unwrap();
        let ansi = String::from_utf8_lossy(&ansi);
        assert!(ansi.contains(&format!("{}", Colored::BackgroundColor(Color::DarkBlue))));
        assert_eq!(
            canvas.damage_region(),
            Some(crate::canvas::DamageRegion {
                x: 1,
                y: 0,
                width: 3,
                height: 1,
            })
        );
    }

    fn set_bcd_selection(selection: SelectionContext) {
        let mut controller = SelectionController::new();
        controller.selection_mut().start(1, 0);
        controller.selection_mut().update(3, 0);
        controller.selection_mut().finish();
        selection.set_controller(controller);
    }

    #[component]
    fn SelectionBgColorHookApp(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let selection = create_selection_context(&mut hooks);
        let mut phase = hooks.use_state(|| 0u8);
        if !selection.has_selection() {
            set_bcd_selection(selection);
        }

        let phase_value = *phase.read();
        let color = if phase_value == 0 {
            Color::DarkBlue
        } else {
            Color::DarkRed
        };
        hooks.use_selection_bg_color(selection, color);
        hooks.use_selection_overlay(selection);

        if phase_value == 0 {
            phase.set(1);
        } else {
            system.exit();
        }

        element!(Text(content: "abcdef"))
    }

    #[test]
    fn test_use_selection_bg_color_tracks_theme_color_changes() {
        let canvases: Vec<_> = smol::block_on(
            element!(SelectionBgColorHookApp)
                .mock_terminal_render_loop(MockTerminalConfig::default())
                .collect::<Vec<_>>(),
        );
        let canvas = canvases.last().unwrap();
        let mut ansi = Vec::new();
        canvas.write_ansi(&mut ansi).unwrap();
        let ansi = String::from_utf8_lossy(&ansi);
        assert!(ansi.contains(&format!("{}", Colored::BackgroundColor(Color::DarkRed))));
        assert!(!ansi.contains(&format!("{}", Colored::BackgroundColor(Color::DarkBlue))));
    }

    #[component]
    fn SelectionSubscribeApp(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let selection = create_selection_context(&mut hooks);
        let mut phase = hooks.use_state(|| 0u8);
        let count = hooks.use_state(|| 0usize);
        let subscription: Arc<Mutex<Option<SelectionSubscription>>> = hooks.use_const_default();
        if subscription.lock().unwrap().is_none() {
            let mut count_for_listener = count;
            *subscription.lock().unwrap() = Some(selection.subscribe(move || {
                count_for_listener += 1;
            }));
        }

        match phase.get() {
            0 => {
                selection.set_selection_bg_color(Color::DarkRed);
                phase.set(1);
            }
            1 => {
                subscription.lock().unwrap().take();
                selection.set_selection_bg_color(Color::DarkBlue);
                phase.set(2);
            }
            _ => system.exit(),
        }

        element!(Text(content: format!("count={}", count.get())))
    }

    #[test]
    fn test_selection_context_subscribe_and_drop() {
        let canvases: Vec<_> = smol::block_on(
            element!(SelectionSubscribeApp)
                .mock_terminal_render_loop(MockTerminalConfig::default())
                .collect::<Vec<_>>(),
        );
        assert_eq!(canvases.last().unwrap().to_string(), "count=1\n");
    }

    #[component]
    fn CopyOnSelectTextHookApp(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let selection = create_selection_context(&mut hooks);
        let last_copy = hooks.use_state(String::new);
        if !selection.has_selection() && last_copy.read().is_empty() {
            set_bcd_selection(selection);
        }

        let mut last_copy_for_callback = last_copy;
        hooks.use_copy_on_select_text(selection, true, move |text| {
            last_copy_for_callback.set(text);
        });

        if !last_copy.read().is_empty() && !selection.copy_on_select_would_mutate() {
            system.exit();
        }

        element! {
            View(flex_direction: FlexDirection::Column) {
                Text(content: "abcdef")
                Text(content: format!("last={}", &*last_copy.read()))
            }
        }
    }

    #[test]
    fn test_use_copy_on_select_text_observes_settled_selection_once() {
        let canvases: Vec<_> = smol::block_on(
            element!(CopyOnSelectTextHookApp)
                .mock_terminal_render_loop(MockTerminalConfig::default())
                .collect::<Vec<_>>(),
        );
        assert_eq!(canvases.last().unwrap().to_string(), "abcdef\nlast=bcd\n");
    }

    #[component]
    fn CopyOnSelectClipboardHookApp(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let (stdout, _) = hooks.use_output();
        let selection = create_selection_context(&mut hooks);
        if !selection.has_selection() {
            set_bcd_selection(selection);
        }
        hooks.use_copy_on_select(selection, stdout, true);
        if !selection.copy_on_select_would_mutate() {
            system.exit();
        }
        element!(Text(content: format!(
            "pending={}",
            selection.copy_on_select_would_mutate()
        )))
    }

    #[test]
    fn test_use_copy_on_select_clipboard_hook_consumes_guard_without_clearing() {
        let canvases: Vec<_> = smol::block_on(
            element!(CopyOnSelectClipboardHookApp)
                .mock_terminal_render_loop(MockTerminalConfig::default())
                .collect::<Vec<_>>(),
        );
        assert_eq!(canvases.last().unwrap().to_string(), "pending=false\n");
    }

    #[component]
    fn FullscreenSelectionEventsHookApp(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let selection = create_selection_context(&mut hooks);
        let mut clicks = hooks.use_state(|| 0usize);
        let last_link = hooks.use_state(|| "<pending>".to_string());
        let mut releases = hooks.use_state(|| 0usize);
        let mut last_link_for_hook = last_link;
        hooks.use_fullscreen_selection_events(selection, true, move |outcome| {
            if let FullscreenSelectionDispatchOutcome::Mouse(
                FullscreenSelectionEventOutcome::Release(release),
            ) = outcome
            {
                releases += 1;
                last_link_for_hook.set(release.hyperlink.unwrap_or_else(|| "<none>".to_string()));
            }
        });

        if releases.get() > 0 {
            system.exit();
        }

        element! {
            View(flex_direction: FlexDirection::Column) {
                View(
                    width: 4,
                    height: 1,
                    on_click: move |_| clicks += 1,
                ) {
                    Link(url: "https://example.com".to_string(), label: Some("docs".to_string()))
                }
                Text(content: format!("clicks={} link={}", clicks.get(), &*last_link.read()))
            }
        }
    }

    #[test]
    fn test_fullscreen_selection_events_hook_observes_consumed_view_click_for_link_suppression() {
        let canvases: Vec<_> = smol::block_on(
            element!(FullscreenSelectionEventsHookApp)
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
                .collect::<Vec<_>>(),
        );
        assert_eq!(
            canvases.last().unwrap().to_string(),
            "docs\nclicks=1 link=<none>\n"
        );
    }

    #[test]
    fn test_copied_selection_notification_matches_cc_toast_wording() {
        let osc = copied_selection_notification("abc", SelectionClipboardPath::Osc52);
        assert_eq!(osc.key, "selection-copied");
        assert_eq!(osc.priority, NotificationPriority::Immediate);
        assert!(osc.text.contains("sent 3 chars via OSC 52"));
        assert_eq!(osc.timeout, Some(Duration::from_millis(4000)));

        let native = copied_selection_notification("abcd", SelectionClipboardPath::Native);
        assert_eq!(native.text, "copied 4 chars to clipboard");
        assert_eq!(native.timeout, Some(Duration::from_millis(2000)));
    }

    #[test]
    fn test_legacy_copy_key_matches_ctrl_c_without_shift_or_meta() {
        let mut ctrl_c = KeyEvent::new(KeyEventKind::Press, KeyCode::Char('c'));
        ctrl_c.modifiers = KeyModifiers::CONTROL;
        assert!(is_legacy_copy_key(&ctrl_c));

        let mut shifted = KeyEvent::new(KeyEventKind::Press, KeyCode::Char('C'));
        shifted.modifiers = KeyModifiers::CONTROL | KeyModifiers::SHIFT;
        assert!(!is_legacy_copy_key(&shifted));
    }

    #[test]
    fn test_disabled_selection_context_is_noop() {
        let selection = SelectionContext::disabled();
        let canvas = canvas_with_text();
        assert!(!selection.is_enabled());
        assert!(!selection.has_selection());
        assert_eq!(selection.copy_selection_no_clear_text(&canvas), "");
        assert_eq!(selection.copy_on_select_text(&canvas), None);
        assert_eq!(
            selection.handle_fullscreen_key_event(
                &KeyEvent::new(KeyEventKind::Press, KeyCode::Esc),
                canvas.width(),
                canvas.height(),
            ),
            FullscreenSelectionKeyOutcome::Ignored
        );
        assert_eq!(
            selection.handle_fullscreen_mouse_event(
                &canvas,
                &FullscreenMouseEvent::new(MouseEventKind::ScrollDown, 0, 0),
                1_000,
                false,
            ),
            FullscreenSelectionEventOutcome::Wheel {
                cleared_selection: false,
            }
        );
        assert!(!selection.move_focus(SelectionFocusMove::Right, canvas.width(), canvas.height()));
    }
}
