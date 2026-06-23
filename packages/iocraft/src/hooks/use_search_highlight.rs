use super::{State, UseContext, UseState};
use crate::{Canvas, Color, ComponentDrawer, Hook, Hooks, StyleOverlay, TextMatchPosition};
use std::sync::{Arc, Mutex};

mod private {
    pub trait Sealed {}
    impl Sealed for crate::Hooks<'_, '_> {}
}

/// Current-match highlight coordinates relative to a rendered canvas/subtree.
///
/// This mirrors the CC Ink fork's position-based current highlight state:
/// pre-scanned match positions stay stable relative to a message/subtree, while
/// `row_offset` tracks where that subtree is currently rendered.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PositionedSearchHighlight {
    /// Pre-scanned match positions.
    pub positions: Vec<TextMatchPosition>,
    /// Row offset to add when applying positions to the current screen.
    pub row_offset: isize,
    /// Index of the current match inside [`Self::positions`].
    pub current_idx: usize,
}

/// App-level rendered-screen search highlight state.
///
/// CC Ink stores search query/positions on the Ink instance and applies them to
/// the finished screen buffer before diffing. This runtime state provides the
/// same split for iocraft: components set query/positions, then a post-render
/// hook applies overlays to the retained [`Canvas`].
#[derive(Clone)]
pub struct SearchHighlightRuntimeState {
    query: String,
    positioned: Option<PositionedSearchHighlight>,
    match_overlay: StyleOverlay,
    current_overlay: StyleOverlay,
    subscribers: SharedSearchHighlightSubscribers,
}

impl std::fmt::Debug for SearchHighlightRuntimeState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SearchHighlightRuntimeState")
            .field("query", &self.query)
            .field("positioned", &self.positioned)
            .field("match_overlay", &self.match_overlay)
            .field("current_overlay", &self.current_overlay)
            .finish_non_exhaustive()
    }
}

impl Default for SearchHighlightRuntimeState {
    fn default() -> Self {
        Self {
            query: String::new(),
            positioned: None,
            match_overlay: StyleOverlay::inverse(),
            current_overlay: StyleOverlay::current_match(Color::Yellow),
            subscribers: SharedSearchHighlightSubscribers::default(),
        }
    }
}

#[derive(Clone, Default)]
struct SharedSearchHighlightSubscribers(Arc<Mutex<SearchHighlightSubscribers>>);

type SearchHighlightListener = Arc<Mutex<Box<dyn FnMut() + Send + 'static>>>;

#[derive(Default)]
struct SearchHighlightSubscribers {
    next_id: u64,
    listeners: Vec<(u64, SearchHighlightListener)>,
}

impl SharedSearchHighlightSubscribers {
    fn subscribe(&self, listener: impl FnMut() + Send + 'static) -> SearchHighlightSubscription {
        let mut guard = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let id = guard.next_id;
        guard.next_id = guard.next_id.wrapping_add(1);
        guard
            .listeners
            .push((id, Arc::new(Mutex::new(Box::new(listener)))));
        SearchHighlightSubscription {
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

/// RAII subscription returned by [`SearchHighlightContext::subscribe`].
///
/// Dropping the value removes the listener.
#[derive(Default)]
pub struct SearchHighlightSubscription {
    subscribers: Option<SharedSearchHighlightSubscribers>,
    id: u64,
}

impl Drop for SearchHighlightSubscription {
    fn drop(&mut self) {
        if let Some(subscribers) = self.subscribers.take() {
            subscribers.unsubscribe(self.id);
        }
    }
}

/// Copyable handle to app-level search highlight state.
#[derive(Clone, Copy)]
pub struct SearchHighlightContext {
    state: Option<State<SearchHighlightRuntimeState>>,
}

impl Default for SearchHighlightContext {
    fn default() -> Self {
        Self::disabled()
    }
}

impl SearchHighlightContext {
    /// Creates a no-op search-highlight handle.
    pub fn disabled() -> Self {
        Self { state: None }
    }

    pub(crate) fn new(state: State<SearchHighlightRuntimeState>) -> Self {
        Self { state: Some(state) }
    }

    /// Returns whether this handle is backed by live state.
    pub fn is_enabled(&self) -> bool {
        self.state.is_some()
    }

    fn with_ref<R>(&self, f: impl FnOnce(&SearchHighlightRuntimeState) -> R) -> Option<R> {
        let state = self.state?;
        let guard = state.try_read()?;
        Some(f(&guard))
    }

    fn with_mut<R>(&self, f: impl FnOnce(&mut SearchHighlightRuntimeState) -> R) -> Option<R> {
        let mut state = self.state?;
        let (result, subscribers) = {
            let mut guard = state.try_write()?;
            let result = f(&mut guard);
            (result, guard.subscribers.clone())
        };
        subscribers.notify();
        Some(result)
    }

    /// Subscribes to search-highlight runtime mutations.
    ///
    /// This is an external-store style push signal for consumers that need to
    /// react to `set_query`, `set_positions`, or overlay theme changes outside
    /// ordinary render-time polling. Dropping the returned
    /// [`SearchHighlightSubscription`] removes the listener.
    pub fn subscribe(
        &self,
        listener: impl FnMut() + Send + 'static,
    ) -> SearchHighlightSubscription {
        self.with_ref(|s| s.subscribers.clone())
            .map(|subscribers| subscribers.subscribe(listener))
            .unwrap_or_default()
    }

    /// Returns the current screen-space search query.
    pub fn query(&self) -> String {
        self.with_ref(|s| s.query.clone()).unwrap_or_default()
    }

    /// Sets the screen-space search query. Empty strings disable all-match
    /// highlighting while leaving current-position state untouched, matching CC
    /// Ink's independent `setQuery` / `setPositions` controls.
    pub fn set_query<S: Into<String>>(&self, query: S) {
        let query = query.into();
        if self.with_ref(|s| s.query == query).unwrap_or(false) {
            return;
        }
        self.with_mut(|s| s.query = query);
    }

    /// Clears the screen-space query highlight.
    pub fn clear_query(&self) {
        self.set_query("");
    }

    /// Returns the current positioned-highlight state.
    pub fn positioned(&self) -> Option<PositionedSearchHighlight> {
        self.with_ref(|s| s.positioned.clone()).flatten()
    }

    /// Sets position-based current-match highlighting.
    pub fn set_positions(
        &self,
        positions: Vec<TextMatchPosition>,
        row_offset: isize,
        current_idx: usize,
    ) {
        let positioned = PositionedSearchHighlight {
            positions,
            row_offset,
            current_idx,
        };
        if self
            .with_ref(|s| s.positioned.as_ref() == Some(&positioned))
            .unwrap_or(false)
        {
            return;
        }
        self.with_mut(|s| s.positioned = Some(positioned));
    }

    /// Clears position-based current-match highlighting.
    pub fn clear_positions(&self) {
        if self.with_ref(|s| s.positioned.is_none()).unwrap_or(true) {
            return;
        }
        self.with_mut(|s| s.positioned = None);
    }

    /// Sets the overlay used for all query matches.
    pub fn set_match_overlay(&self, overlay: StyleOverlay) {
        if self
            .with_ref(|s| s.match_overlay == overlay)
            .unwrap_or(false)
        {
            return;
        }
        self.with_mut(|s| s.match_overlay = overlay);
    }

    /// Sets the overlay used for the current positioned match.
    pub fn set_current_overlay(&self, overlay: StyleOverlay) {
        if self
            .with_ref(|s| s.current_overlay == overlay)
            .unwrap_or(false)
        {
            return;
        }
        self.with_mut(|s| s.current_overlay = overlay);
    }

    /// Scans `canvas` for rendered positions using the current query.
    pub fn scan_canvas(&self, canvas: &Canvas) -> Vec<TextMatchPosition> {
        let query = self.query();
        canvas.scan_text_positions(&query)
    }

    /// Scans a rendered region using the current query and returns positions
    /// relative to that region's top-left corner.
    ///
    /// This mirrors CC Ink's `scanElement(...)` shape for Rust callers that
    /// have a known canvas rectangle rather than an Ink DOM element.
    pub fn scan_canvas_region(
        &self,
        canvas: &Canvas,
        x: usize,
        y: usize,
        width: usize,
        height: usize,
    ) -> Vec<TextMatchPosition> {
        let query = self.query();
        canvas.scan_text_positions_region(x, y, width, height, &query)
    }

    /// Applies the current query and positioned overlays to `canvas`.
    pub fn apply_overlay(&self, canvas: &mut Canvas) -> bool {
        self.with_ref(|s| {
            let mut applied = false;
            if !s.query.is_empty() {
                applied |= canvas.apply_search_highlight(&s.query, s.match_overlay);
            }
            if let Some(positioned) = &s.positioned {
                applied |= canvas.apply_positioned_highlight(
                    &positioned.positions,
                    positioned.row_offset,
                    positioned.current_idx,
                    s.current_overlay,
                );
            }
            applied
        })
        .unwrap_or(false)
    }
}

/// Creates a search-highlight context owned by the current component.
pub fn create_search_highlight_context(hooks: &mut Hooks<'_, '_>) -> SearchHighlightContext {
    SearchHighlightContext::new(hooks.use_state(SearchHighlightRuntimeState::default))
}

#[derive(Default)]
struct UseSearchHighlightOverlayImpl {
    highlight: SearchHighlightContext,
}

impl Hook for UseSearchHighlightOverlayImpl {
    fn post_component_draw(&mut self, drawer: &mut ComponentDrawer) {
        self.highlight.apply_overlay(drawer.root_canvas_mut());
    }
}

/// Access to app-level rendered-screen search highlighting.
pub trait UseSearchHighlight<'a>: private::Sealed {
    /// Returns the nearest search-highlight context, or a no-op handle when none
    /// is provided.
    fn use_search_highlight(&self) -> SearchHighlightContext;

    /// Applies search-highlight overlays after this component and all of its
    /// children have drawn.
    fn use_search_highlight_overlay(&mut self, highlight: SearchHighlightContext);
}

impl UseSearchHighlight<'_> for Hooks<'_, '_> {
    fn use_search_highlight(&self) -> SearchHighlightContext {
        self.try_use_context::<SearchHighlightContext>()
            .map(|ctx| *ctx)
            .unwrap_or_else(SearchHighlightContext::disabled)
    }

    fn use_search_highlight_overlay(&mut self, highlight: SearchHighlightContext) {
        self.use_hook(UseSearchHighlightOverlayImpl::default)
            .highlight = highlight;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{components::ContextProvider, prelude::*, Context};
    use crossterm::style::Colored;
    use futures::StreamExt;

    #[component]
    fn SearchHighlightConsumer(hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let highlight = hooks.use_search_highlight();
        element!(Text(content: format!(
            "enabled={} query={:?}",
            highlight.is_enabled(),
            highlight.query()
        )))
    }

    #[component]
    fn SearchHighlightProviderApp(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let highlight = create_search_highlight_context(&mut hooks);
        highlight.set_query("lazy");
        highlight.set_positions(
            vec![
                TextMatchPosition {
                    row: 0,
                    col: 0,
                    len: 4,
                },
                TextMatchPosition {
                    row: 0,
                    col: 5,
                    len: 4,
                },
            ],
            0,
            1,
        );
        hooks.use_search_highlight_overlay(highlight);
        system.exit();
        element! {
            ContextProvider(value: Context::owned(highlight)) {
                View(flex_direction: FlexDirection::Column) {
                    Text(content: "lazy lazy")
                    SearchHighlightConsumer
                }
            }
        }
    }

    #[test]
    fn test_search_highlight_context_query_current_overlay_and_hook() {
        let canvases: Vec<_> = smol::block_on(
            element!(SearchHighlightProviderApp)
                .mock_terminal_render_loop(MockTerminalConfig::default())
                .collect::<Vec<_>>(),
        );
        let canvas = canvases.last().unwrap();
        assert!(canvas.resolved_text_style(0, 0).unwrap().invert);
        let current = canvas.resolved_text_style(5, 0).unwrap();
        assert!(current.invert);
        assert_eq!(current.color, Some(Color::Yellow));
        assert!(current.underline);
        assert_eq!(current.weight, Weight::Bold);
        let mut ansi = Vec::new();
        canvas.write_ansi(&mut ansi).unwrap();
        let ansi = String::from_utf8_lossy(&ansi);
        assert!(ansi.contains(&format!("{}", Colored::ForegroundColor(Color::Yellow))));
        assert!(canvas.to_string().contains("enabled=true query=\"lazy\""));
    }

    #[component]
    fn SearchHighlightSubscribeApp(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let highlight = create_search_highlight_context(&mut hooks);
        let mut phase = hooks.use_state(|| 0u8);
        let count = hooks.use_state(|| 0usize);
        let subscription: Arc<Mutex<Option<SearchHighlightSubscription>>> =
            hooks.use_const_default();
        if subscription.lock().unwrap().is_none() {
            let mut count_for_listener = count;
            *subscription.lock().unwrap() = Some(highlight.subscribe(move || {
                count_for_listener += 1;
            }));
        }

        match phase.get() {
            0 => {
                highlight.set_query("lazy");
                phase.set(1);
            }
            1 => {
                subscription.lock().unwrap().take();
                highlight.set_query("other");
                phase.set(2);
            }
            _ => system.exit(),
        }

        element!(Text(content: format!("count={}", count.get())))
    }

    #[test]
    fn test_search_highlight_context_subscribe_and_drop() {
        let canvases: Vec<_> = smol::block_on(
            element!(SearchHighlightSubscribeApp)
                .mock_terminal_render_loop(MockTerminalConfig::default())
                .collect::<Vec<_>>(),
        );
        assert_eq!(canvases.last().unwrap().to_string(), "count=1\n");
    }

    #[test]
    fn test_disabled_search_highlight_context_is_noop() {
        let highlight = SearchHighlightContext::disabled();
        let mut canvas = Canvas::new(8, 1);
        canvas
            .subview_mut(0, 0, 0, 0, 8, 1)
            .set_text(0, 0, "lazy", CanvasTextStyle::default());
        assert!(!highlight.is_enabled());
        assert_eq!(highlight.query(), "");
        assert!(highlight.scan_canvas(&canvas).is_empty());
        assert!(highlight.scan_canvas_region(&canvas, 0, 0, 8, 1).is_empty());
        assert!(!highlight.apply_overlay(&mut canvas));
        assert_eq!(canvas.damage_region(), None);
    }
}
