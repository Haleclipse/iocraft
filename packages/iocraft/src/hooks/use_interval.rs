use crate::{Hook, Hooks};
use core::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};
use futures_timer::Delay;
use std::{
    sync::OnceLock,
    time::{Duration, Instant},
};

use super::UseState;

mod private {
    pub trait Sealed {}
    impl Sealed for crate::Hooks<'_, '_> {}
}

static SHARED_CLOCK_START: OnceLock<Instant> = OnceLock::new();

pub(crate) fn shared_clock_now_ms() -> u128 {
    SHARED_CLOCK_START
        .get_or_init(Instant::now)
        .elapsed()
        .as_millis()
}

/// Interval and animation-timer hooks.
///
/// These mirror the CC Ink fork's `useInterval(...)` / `useAnimationTimer(...)`
/// shape. Timers are driven by iocraft's hook polling, while animation time is
/// read from a process-wide shared clock so late-mounted timers stay in sync
/// instead of restarting at zero. Passing `None` pauses the interval without
/// clearing the last observed timer value.
pub trait UseInterval: private::Sealed {
    /// Calls `callback` every `interval` while the component is mounted.
    ///
    /// Passing `None` pauses the interval. The callback is refreshed on every
    /// render, so it can capture the latest props/state while keeping the hook
    /// slot stable.
    fn use_interval<F>(&mut self, callback: F, interval: Option<Duration>)
    where
        F: FnMut() + Send + Unpin + 'static;

    /// Returns milliseconds elapsed on the shared animation clock, updated no
    /// more often than `interval`.
    fn use_animation_timer(&mut self, interval: Duration) -> u128 {
        self.use_animation_timer_opt(Some(interval))
    }

    /// Pausable variant of [`UseInterval::use_animation_timer`].
    fn use_animation_timer_opt(&mut self, interval: Option<Duration>) -> u128;
}

impl UseInterval for Hooks<'_, '_> {
    fn use_interval<F>(&mut self, callback: F, interval: Option<Duration>)
    where
        F: FnMut() + Send + Unpin + 'static,
    {
        let hook = self.use_hook(UseIntervalImpl::<F>::default);
        if hook.interval != interval {
            hook.delay = None;
        }
        hook.interval = interval;
        hook.callback = Some(callback);
    }

    fn use_animation_timer_opt(&mut self, interval: Option<Duration>) -> u128 {
        let now = self.use_state(|| {
            if interval.is_some() {
                shared_clock_now_ms()
            } else {
                0
            }
        });
        let mut now_for_callback = now;
        self.use_interval(
            move || {
                now_for_callback.set(shared_clock_now_ms());
            },
            interval,
        );
        now.get()
    }
}

struct UseIntervalImpl<F> {
    callback: Option<F>,
    interval: Option<Duration>,
    delay: Option<Pin<Box<Delay>>>,
}

impl<F> Default for UseIntervalImpl<F> {
    fn default() -> Self {
        Self {
            callback: None,
            interval: None,
            delay: None,
        }
    }
}

impl<F> Hook for UseIntervalImpl<F>
where
    F: FnMut() + Send + Unpin,
{
    fn poll_change(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let Some(interval) = self.interval else {
            self.delay = None;
            return Poll::Pending;
        };

        if self.delay.is_none() {
            self.delay = Some(Box::pin(Delay::new(interval)));
        }

        let ready = self
            .delay
            .as_mut()
            .is_some_and(|delay| delay.as_mut().poll(cx).is_ready());
        if !ready {
            return Poll::Pending;
        }

        self.delay = Some(Box::pin(Delay::new(interval)));
        if let Some(callback) = self.callback.as_mut() {
            callback();
        }
        Poll::Ready(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prelude::*;
    use futures::StreamExt;

    #[component]
    fn IntervalCounter(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let count = hooks.use_state(|| 0u8);
        let count_for_callback = count;
        hooks.use_interval(
            move || {
                let mut count = count_for_callback;
                count += 1;
            },
            Some(Duration::from_millis(1)),
        );

        if count.get() >= 2 {
            system.exit();
        }

        element!(Text(content: format!("count={}", count.get())))
    }

    #[test]
    fn test_use_interval_ticks_until_paused_by_exit() {
        let canvases: Vec<_> = smol::block_on(
            element!(IntervalCounter)
                .mock_terminal_render_loop(MockTerminalConfig::default())
                .collect(),
        );
        assert_eq!(canvases.last().unwrap().to_string(), "count=2\n");
    }

    #[component]
    fn PausedTimer(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let timer = hooks.use_animation_timer_opt(None);
        system.exit();
        element!(Text(content: format!("timer={timer}")))
    }

    #[test]
    fn test_use_animation_timer_none_is_paused() {
        let canvases: Vec<_> = smol::block_on(
            element!(PausedTimer)
                .mock_terminal_render_loop(MockTerminalConfig::default())
                .collect(),
        );
        assert_eq!(canvases.last().unwrap().to_string(), "timer=0\n");
    }

    #[component]
    fn LateTimerChild(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let timer = hooks.use_animation_timer(Duration::from_millis(1));
        system.exit();
        element!(Text(content: format!("late={timer}")))
    }

    #[component]
    fn SharedTimerApp(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let timer = hooks.use_animation_timer(Duration::from_millis(1));
        element! {
            View(flex_direction: FlexDirection::Column) {
                Text(content: format!("parent={timer}"))
                #(if timer > 0 {
                    element!(LateTimerChild).into_any()
                } else {
                    element!(Text(content: "waiting")).into_any()
                })
            }
        }
    }

    #[test]
    fn test_use_animation_timer_uses_shared_clock_for_late_mounts() {
        let canvases: Vec<_> = smol::block_on(
            element!(SharedTimerApp)
                .mock_terminal_render_loop(MockTerminalConfig::default())
                .collect(),
        );
        let rendered = canvases.last().unwrap().to_string();
        let late = rendered
            .lines()
            .find_map(|line| line.strip_prefix("late="))
            .and_then(|value| value.parse::<u128>().ok())
            .expect("late-mounted timer should render its shared clock time");
        assert!(
            late > 0,
            "late-mounted animation timers should inherit the shared clock instead of restarting at 0: {rendered:?}"
        );
    }
}
