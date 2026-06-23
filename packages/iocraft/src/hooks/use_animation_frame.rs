use crate::Hooks;
use std::time::Duration;

use super::{
    use_interval::shared_clock_now_ms, TerminalViewportEntry, UseInterval, UseState,
    UseTerminalFocus, UseTerminalViewport,
};

mod private {
    pub trait Sealed {}
    impl Sealed for crate::Hooks<'_, '_> {}
}

/// Return value of [`UseAnimationFrame::use_animation_frame`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AnimationFrameState {
    /// Current terminal viewport entry for the component using the hook.
    pub viewport: TerminalViewportEntry,
    /// Milliseconds elapsed on the shared animation clock.
    pub time_ms: u128,
}

/// Hook for synchronized, pausable animations.
///
/// This follows the CC Ink fork's `useAnimationFrame(...)` intent: animations
/// stop ticking when their component is outside the live terminal viewport, and
/// the clock slows while terminal focus is lost instead of fully stopping.
pub trait UseAnimationFrame: private::Sealed {
    /// Returns the current animation state, ticking at `interval` while active.
    fn use_animation_frame(&mut self, interval: Option<Duration>) -> AnimationFrameState;
}

impl UseAnimationFrame for Hooks<'_, '_> {
    fn use_animation_frame(&mut self, interval: Option<Duration>) -> AnimationFrameState {
        let viewport = self.use_terminal_viewport();
        let focused = self.use_terminal_focus();
        let active_interval = if viewport.is_visible {
            interval.map(|interval| {
                if focused {
                    interval
                } else {
                    interval.saturating_mul(2)
                }
            })
        } else {
            None
        };
        let time = self.use_state(shared_clock_now_ms);
        let mut time_for_callback = time;
        self.use_interval(
            move || {
                time_for_callback.set(shared_clock_now_ms());
            },
            active_interval,
        );
        AnimationFrameState {
            viewport,
            time_ms: time.get(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prelude::*;
    use futures::StreamExt;

    #[component]
    fn OffscreenAnimation(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let mut tick = hooks.use_state(|| 0u8);
        if tick.get() < 2 {
            tick += 1;
        } else {
            system.exit();
        }

        let frame = hooks.use_animation_frame(Some(Duration::from_millis(1)));
        element! {
            View(flex_direction: FlexDirection::Column) {
                Text(content: "row 0")
                Text(content: "row 1")
                Text(content: "row 2")
                Text(content: "row 3")
                Text(content: format!(
                    "visible={} time={}",
                    frame.viewport.is_visible,
                    frame.time_ms,
                ))
            }
        }
    }

    #[test]
    fn test_use_animation_frame_reports_viewport_state() {
        let canvases: Vec<_> = smol::block_on(
            element!(OffscreenAnimation)
                .mock_terminal_render_loop(MockTerminalConfig::default().with_size(20, 3))
                .collect(),
        );
        assert!(
            canvases
                .last()
                .unwrap()
                .to_string()
                .contains("visible=true"),
            "component itself is the root and remains visible"
        );
    }

    #[component]
    fn LateClockChild(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let frame = hooks.use_animation_frame(None);
        system.exit();
        element!(Text(content: format!("late={}", frame.time_ms)))
    }

    #[component]
    fn SharedClockApp(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let frame = hooks.use_animation_frame(Some(Duration::from_millis(1)));
        element! {
            View(flex_direction: FlexDirection::Column) {
                Text(content: format!("parent={}", frame.time_ms))
                #(if frame.time_ms > 0 {
                    element!(LateClockChild).into_any()
                } else {
                    element!(Text(content: "waiting")).into_any()
                })
            }
        }
    }

    #[test]
    fn test_use_animation_frame_uses_shared_clock_for_late_mounts() {
        let canvases: Vec<_> = smol::block_on(
            element!(SharedClockApp)
                .mock_terminal_render_loop(MockTerminalConfig::default())
                .collect(),
        );
        let rendered = canvases.last().unwrap().to_string();
        let late = rendered
            .lines()
            .find_map(|line| line.strip_prefix("late="))
            .and_then(|value| value.parse::<u128>().ok())
            .expect("late-mounted child should render its clock time");
        assert!(
            late > 0,
            "late-mounted animations should inherit the shared clock instead of restarting at 0: {rendered:?}"
        );
    }
}
