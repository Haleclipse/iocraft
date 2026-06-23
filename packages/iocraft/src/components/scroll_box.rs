use crate::{
    component,
    components::{ScrollDrainMode, ScrollView},
    element,
    hooks::{Ref, SelectionContext},
    AnyElement, Color, Hooks, Props,
};

pub use crate::components::ScrollViewHandle as ScrollBoxHandle;
pub use crate::components::ScrollViewSubscription as ScrollBoxSubscription;

/// Props for [`ScrollBox`].
#[non_exhaustive]
#[derive(Default, Props)]
pub struct ScrollBoxProps<'a> {
    /// The children to render inside the scroll box.
    pub children: Vec<AnyElement<'a>>,
    /// Optional imperative scroll handle.
    pub handle: Option<Ref<ScrollBoxHandle>>,
    /// When true, keep the scroll position pinned to the bottom while content grows.
    pub sticky_scroll: bool,
    /// Number of lines to scroll per mouse wheel tick. Defaults to the underlying
    /// [`ScrollView`](crate::components::ScrollView) default.
    pub scroll_step: Option<u16>,
    /// Enables CC Ink-style mouse wheel acceleration for transcript/copy-mode
    /// scroll boxes. Defaults to `false`.
    pub wheel_acceleration: Option<bool>,
    /// Opt-in CC Ink-style wheel scroll drain strategy.
    ///
    /// Set this when you want bursty wheel input to be applied over animation
    /// frames instead of as one eager jump. Defaults to `None`.
    pub scroll_drain_mode: Option<ScrollDrainMode>,
    /// Whether keyboard events should scroll the box. Defaults to `true`.
    pub keyboard_scroll: Option<bool>,
    /// Enables CC Ink transcript/modal pager keys (`j`/`k`, Space/`b`,
    /// `g`/`G`, Ctrl+U/D/B/F/N/P). Defaults to `false` because printable
    /// keys are only safe when no text input is competing for them.
    pub modal_pager_keys: Option<bool>,
    /// Optional fullscreen selection context to keep selections anchored during
    /// keyboard scroll jumps and clear them on wheel scroll, matching CC Ink's
    /// ScrollKeybindingHandler behavior.
    pub selection: Option<SelectionContext>,
    /// Whether to show iocraft's visual scrollbar. Defaults to `false` to match
    /// CC Ink's `<ScrollBox>` overflow behavior.
    pub scrollbar: Option<bool>,
    /// Optional color for the scrollbar thumb.
    pub scrollbar_thumb_color: Option<Color>,
    /// Optional color for the scrollbar track.
    pub scrollbar_track_color: Option<Color>,
}

/// A CC Ink-style scroll container with an imperative handle.
///
/// This component is a thin compatibility layer over iocraft's [`ScrollView`].
/// It uses CC Ink naming (`sticky_scroll`, [`ScrollBoxHandle`]) and defaults to
/// no visible scrollbar, while retaining keyboard/mouse scrolling, bottom
/// stickiness, and viewport/content measurements.
#[component]
pub fn ScrollBox<'a>(_hooks: Hooks, props: &mut ScrollBoxProps<'a>) -> impl Into<AnyElement<'a>> {
    let scrollbar = props.scrollbar.unwrap_or(false);

    element! {
        ScrollView(
            auto_scroll: props.sticky_scroll,
            scroll_step: props.scroll_step,
            wheel_acceleration: props.wheel_acceleration,
            scroll_drain_mode: props.scroll_drain_mode,
            handle: props.handle,
            scrollbar: Some(scrollbar),
            scrollbar_thumb_color: props.scrollbar_thumb_color,
            scrollbar_track_color: props.scrollbar_track_color,
            keyboard_scroll: props.keyboard_scroll,
            modal_pager_keys: props.modal_pager_keys,
            selection: props.selection,
        ) {
            #(props.children.iter_mut())
        }
    }
}

/// Props for [`FastScrollBox`].
#[non_exhaustive]
#[derive(Default, Props)]
pub struct FastScrollBoxProps<'a> {
    /// The children to render inside the scroll box.
    pub children: Vec<AnyElement<'a>>,
    /// Optional imperative scroll handle.
    pub handle: Option<Ref<ScrollBoxHandle>>,
    /// When true, keep the scroll position pinned to the bottom while content grows.
    pub sticky_scroll: bool,
    /// Number of lines to scroll per mouse wheel tick.
    pub scroll_step: Option<u16>,
    /// Enables CC Ink-style mouse wheel acceleration. Defaults to `true` for
    /// this opt-in fast-scroll wrapper.
    pub wheel_acceleration: Option<bool>,
    /// Explicit drain strategy. Defaults to [`ScrollDrainMode::for_current_terminal`].
    pub scroll_drain_mode: Option<ScrollDrainMode>,
    /// Whether keyboard events should scroll the box. Defaults to `true`.
    pub keyboard_scroll: Option<bool>,
    /// Enables CC Ink transcript/modal pager keys. Defaults to `false`.
    pub modal_pager_keys: Option<bool>,
    /// Optional fullscreen selection context to track/clear selections during scrolling.
    pub selection: Option<SelectionContext>,
    /// Whether to show iocraft's visual scrollbar. Defaults to `false`.
    pub scrollbar: Option<bool>,
    /// Optional color for the scrollbar thumb.
    pub scrollbar_thumb_color: Option<Color>,
    /// Optional color for the scrollbar track.
    pub scrollbar_track_color: Option<Color>,
}

/// Opt-in CC Ink-style fast scroll container.
///
/// This is a convenience wrapper over [`ScrollBox`] for transcript-like panes:
/// wheel acceleration is enabled by default, scroll deltas drain over animation
/// frames, and the drain strategy follows the current terminal host
/// (`xterm.js` adaptive vs native proportional). It does not change iocraft's
/// default [`ScrollBox`] behavior and still relies on the fullscreen-safe
/// scroll-hint/DECSTBM path only when the terminal backend can use it.
#[component]
pub fn FastScrollBox<'a>(
    _hooks: Hooks,
    props: &mut FastScrollBoxProps<'a>,
) -> impl Into<AnyElement<'a>> {
    let scroll_drain_mode = props
        .scroll_drain_mode
        .unwrap_or_else(ScrollDrainMode::for_current_terminal);
    let wheel_acceleration = props.wheel_acceleration.unwrap_or(true);

    element! {
        ScrollBox(
            handle: props.handle,
            sticky_scroll: props.sticky_scroll,
            scroll_step: props.scroll_step,
            wheel_acceleration: Some(wheel_acceleration),
            scroll_drain_mode: Some(scroll_drain_mode),
            keyboard_scroll: props.keyboard_scroll,
            modal_pager_keys: props.modal_pager_keys,
            selection: props.selection,
            scrollbar: props.scrollbar,
            scrollbar_thumb_color: props.scrollbar_thumb_color,
            scrollbar_track_color: props.scrollbar_track_color,
        ) {
            #(props.children.iter_mut())
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::prelude::*;
    use futures::{stream, StreamExt};

    #[component]
    fn ScrollBoxProbe(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let handle = hooks.use_ref_default::<ScrollBoxHandle>();
        let mut did_scroll = hooks.use_state(|| false);

        hooks.use_terminal_events({
            let mut handle = handle;
            move |event| {
                if matches!(
                    event,
                    TerminalEvent::Key(KeyEvent {
                        code: KeyCode::Char('j'),
                        kind: KeyEventKind::Press,
                        ..
                    })
                ) {
                    handle.write().scroll_by(2);
                    did_scroll.set(true);
                }
            }
        });

        if did_scroll.get() && handle.read().get_scroll_top() == 2 {
            system.exit();
        }

        let lines = (0..8)
            .map(|i| format!("Line {i}"))
            .collect::<Vec<_>>()
            .join("\n");

        element! {
            View(width: 12, height: 3) {
                ScrollBox(handle) {
                    Text(content: lines)
                }
            }
        }
    }

    #[test]
    fn test_scroll_box_handle_and_default_no_scrollbar() {
        let canvases: Vec<_> = smol::block_on(
            element!(ScrollBoxProbe)
                .mock_terminal_render_loop(MockTerminalConfig::with_events(stream::iter(vec![
                    TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Char('j'))),
                ])))
                .collect(),
        );
        let output = canvases.last().unwrap().to_string();
        assert!(output.contains("Line 2"), "{output:?}");
        assert!(
            !output.contains('\u{2502}'),
            "ScrollBox should hide scrollbar by default"
        );
        assert!(
            !output.contains('\u{2503}'),
            "ScrollBox should hide scrollbar by default"
        );
    }

    #[component]
    fn ScrollBoxDrainProbe(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let handle = hooks.use_ref_default::<ScrollBoxHandle>();
        let mut saw_pending = hooks.use_state(|| false);
        let mut ticks = hooks.use_state(|| 0usize);

        hooks.use_interval(
            move || {
                ticks.set(ticks.get().saturating_add(1));
            },
            Some(FRAME_INTERVAL),
        );

        let (top, pending) = {
            let handle = handle.read();
            (handle.get_scroll_top(), handle.get_pending_delta())
        };
        if pending != 0 && !saw_pending.get() {
            saw_pending.set(true);
        }
        if saw_pending.get() && pending == 0 && top > 0 {
            system.exit();
        }

        let lines = (0..80)
            .map(|i| format!("Drain {i:02}"))
            .collect::<Vec<_>>()
            .join("\n");

        element! {
            View(width: 20, height: 10, flex_direction: FlexDirection::Column) {
                Text(content: format!("top={top} pending={pending}"))
                View(height: 9) {
                    ScrollBox(
                        handle,
                        scroll_step: Some(40),
                        scroll_drain_mode: Some(ScrollDrainMode::XtermJs),
                    ) {
                        Text(content: lines)
                    }
                }
            }
        }
    }

    #[test]
    fn test_scroll_box_opt_in_drain_exposes_pending_delta_for_wheel_and_handle() {
        let canvases: Vec<_> = smol::block_on(
            element!(ScrollBoxDrainProbe)
                .mock_terminal_render_loop(MockTerminalConfig::with_events(stream::iter(vec![
                    TerminalEvent::FullscreenMouse(FullscreenMouseEvent::new(
                        MouseEventKind::ScrollDown,
                        0,
                        1,
                    )),
                ])))
                .take(12)
                .collect(),
        );
        let outputs = canvases
            .iter()
            .map(|canvas| canvas.to_string())
            .collect::<Vec<_>>();
        assert!(
            outputs
                .iter()
                .any(|output| output.contains("pending=") && !output.contains("pending=0")),
            "wheel drain should expose non-zero pending delta: {outputs:#?}"
        );
        assert!(
            outputs.iter().any(|output| !output.contains("top=0 ")),
            "wheel drain should eventually move the viewport: {outputs:#?}"
        );

        let canvases: Vec<_> = smol::block_on(
            element!(ScrollBoxImperativeDrainProbe)
                .mock_terminal_render_loop(MockTerminalConfig::with_events(stream::iter(vec![])))
                .take(12)
                .collect(),
        );
        let outputs = canvases
            .iter()
            .map(|canvas| canvas.to_string())
            .collect::<Vec<_>>();
        assert!(
            outputs
                .iter()
                .any(|output| output.contains("pending=") && !output.contains("pending=0")),
            "imperative scroll_by should expose non-zero pending delta: {outputs:#?}"
        );
        assert!(
            outputs.iter().any(|output| !output.contains("top=0 ")),
            "imperative scroll_by drain should eventually move the viewport: {outputs:#?}"
        );
    }

    #[component]
    fn FastScrollBoxProbe(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut handle = hooks.use_ref_default::<ScrollBoxHandle>();
        let mut did_queue = hooks.use_state(|| false);
        let mut ticks = hooks.use_state(|| 0usize);

        hooks.use_interval(
            move || {
                ticks.set(ticks.get().saturating_add(1));
            },
            Some(FRAME_INTERVAL),
        );

        if ticks.get() >= 1 && !did_queue.get() {
            handle.write().scroll_by(40);
            did_queue.set(true);
        }

        let (top, pending) = {
            let handle = handle.read();
            (handle.get_scroll_top(), handle.get_pending_delta())
        };
        let lines = (0..80)
            .map(|i| format!("Fast {i:02}"))
            .collect::<Vec<_>>()
            .join("\n");

        element! {
            View(width: 20, height: 10, flex_direction: FlexDirection::Column) {
                Text(content: format!("top={top} pending={pending}"))
                View(height: 9) {
                    FastScrollBox(handle, wheel_acceleration: Some(false)) {
                        Text(content: lines)
                    }
                }
            }
        }
    }

    #[test]
    fn test_fast_scroll_box_defaults_to_opt_in_drain() {
        let canvases: Vec<_> = smol::block_on(
            element!(FastScrollBoxProbe)
                .mock_terminal_render_loop(MockTerminalConfig::with_events(stream::iter(vec![])))
                .take(12)
                .collect(),
        );
        let outputs = canvases
            .iter()
            .map(|canvas| canvas.to_string())
            .collect::<Vec<_>>();
        assert!(
            outputs
                .iter()
                .any(|output| output.contains("pending=") && !output.contains("pending=0")),
            "FastScrollBox should enable pending drain by default: {outputs:#?}"
        );
        assert!(
            outputs.iter().any(|output| !output.contains("top=0 ")),
            "FastScrollBox drain should eventually move the viewport: {outputs:#?}"
        );
    }

    #[component]
    fn ScrollBoxImperativeDrainProbe(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let mut handle = hooks.use_ref_default::<ScrollBoxHandle>();
        let mut did_queue = hooks.use_state(|| false);
        let mut saw_pending = hooks.use_state(|| false);
        let mut ticks = hooks.use_state(|| 0usize);

        hooks.use_interval(
            move || {
                ticks.set(ticks.get().saturating_add(1));
            },
            Some(FRAME_INTERVAL),
        );

        let (top, pending, viewport_height) = {
            let handle = handle.read();
            (
                handle.get_scroll_top(),
                handle.get_pending_delta(),
                handle.get_viewport_height(),
            )
        };
        if !did_queue.get() && viewport_height > 0 {
            handle.write().scroll_by(40);
            did_queue.set(true);
        }
        if pending != 0 && !saw_pending.get() {
            saw_pending.set(true);
        }
        if saw_pending.get() && pending == 0 && top > 0 {
            system.exit();
        }

        let lines = (0..80)
            .map(|i| format!("Imperative {i:02}"))
            .collect::<Vec<_>>()
            .join("\n");

        element! {
            View(width: 24, height: 10, flex_direction: FlexDirection::Column) {
                Text(content: format!("top={top} pending={pending}"))
                View(height: 9) {
                    ScrollBox(
                        handle,
                        scroll_drain_mode: Some(ScrollDrainMode::XtermJs),
                    ) {
                        Text(content: lines)
                    }
                }
            }
        }
    }
}
