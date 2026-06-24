use crate::{AnyElement, Component, ComponentUpdater, Hooks, Props};

/// Comparator used by [`Memo`] to decide whether a memo key is unchanged.
pub type MemoComparator = fn(previous_key: &str, next_key: &str) -> bool;

/// String equality comparator for [`MemoProps::compare`].
pub fn memo_key_eq(previous_key: &str, next_key: &str) -> bool {
    previous_key == next_key
}

/// The props which can be passed to the [`Memo`] component.
#[non_exhaustive]
#[derive(Props)]
pub struct MemoProps<'a> {
    /// Caller-owned render key for the memoized subtree.
    pub memo_key: String,
    /// Explicit comparator for [`Self::memo_key`]. When omitted, the wrapper
    /// behaves like a transparent fragment and does not memoize.
    pub compare: Option<MemoComparator>,
    /// The memoized subtree.
    pub children: Vec<AnyElement<'a>>,
}

impl Default for MemoProps<'_> {
    fn default() -> Self {
        Self {
            memo_key: String::new(),
            compare: None,
            children: Vec::new(),
        }
    }
}

/// Opt-in memo wrapper.
///
/// `Memo` reduces application-level memo boilerplate without changing ordinary
/// component semantics. It only skips child updates when the caller supplies an
/// explicit comparator and that comparator reports the memo key unchanged. Child
/// state/focus/input changes are still honored: if a retained child signaled an
/// internal change while the wrapper was being polled, the children update even
/// when the memo key is equal.
#[derive(Default)]
pub struct Memo {
    previous_key: Option<String>,
}

/// Alias for callers that prefer the CC Ink-aligned component name.
pub type MemoComponent = Memo;

impl Component for Memo {
    type Props<'a> = MemoProps<'a>;

    fn new(_props: &Self::Props<'_>) -> Self {
        Self::default()
    }

    fn update(
        &mut self,
        props: &mut Self::Props<'_>,
        _hooks: Hooks,
        updater: &mut ComponentUpdater,
    ) {
        let child_changed = updater.children_have_pending_change();
        let memo_equal = self
            .previous_key
            .as_deref()
            .zip(props.compare)
            .is_some_and(|(previous, compare)| compare(previous, &props.memo_key));

        if !memo_equal || child_changed {
            updater.update_children(props.children.iter_mut(), None);
        }

        self.previous_key = Some(props.memo_key.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prelude::*;
    use futures::StreamExt;
    use std::{
        sync::atomic::{AtomicUsize, Ordering},
        time::Duration,
    };

    static STABLE_CHILD_RENDERS: AtomicUsize = AtomicUsize::new(0);
    static CHANGING_CHILD_RENDERS: AtomicUsize = AtomicUsize::new(0);
    static NO_COMPARATOR_CHILD_RENDERS: AtomicUsize = AtomicUsize::new(0);

    #[derive(Default, Props)]
    struct CountingChildProps {
        label: String,
    }

    fn counting_child_text(
        props: &CountingChildProps,
        counter: &AtomicUsize,
    ) -> AnyElement<'static> {
        let renders = counter.fetch_add(1, Ordering::SeqCst) + 1;
        element!(Text(content: format!("{} renders={}", props.label, renders))).into()
    }

    #[component]
    fn StableCountingChild(props: &CountingChildProps) -> impl Into<AnyElement<'static>> {
        counting_child_text(props, &STABLE_CHILD_RENDERS)
    }

    #[component]
    fn ChangingCountingChild(props: &CountingChildProps) -> impl Into<AnyElement<'static>> {
        counting_child_text(props, &CHANGING_CHILD_RENDERS)
    }

    #[component]
    fn NoComparatorCountingChild(props: &CountingChildProps) -> impl Into<AnyElement<'static>> {
        counting_child_text(props, &NO_COMPARATOR_CHILD_RENDERS)
    }

    #[component]
    fn StableMemoApp(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let mut tick = hooks.use_state(|| 0u8);
        if tick.get() < 3 {
            tick += 1;
        } else {
            system.exit();
        }

        element! {
            Memo(memo_key: "stable".to_string(), compare: memo_key_eq as MemoComparator) {
                StableCountingChild(label: format!("tick {}", tick.get()))
            }
        }
    }

    #[test]
    fn test_memo_equal_comparator_skips_child_update() {
        STABLE_CHILD_RENDERS.store(0, Ordering::SeqCst);
        let canvases: Vec<_> = smol::block_on(
            element!(StableMemoApp)
                .mock_terminal_render_loop(MockTerminalConfig::default())
                .collect(),
        );
        let rendered = canvases.last().unwrap().to_string();
        assert!(
            rendered.starts_with("tick 1 renders=1"),
            "stable memo key should keep the initial child render: {rendered:?}"
        );
    }

    #[component]
    fn ChangingMemoApp(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let mut tick = hooks.use_state(|| 0u8);
        if tick.get() < 3 {
            tick += 1;
        } else {
            system.exit();
        }

        element! {
            Memo(memo_key: format!("tick-{}", tick.get()), compare: memo_key_eq as MemoComparator) {
                ChangingCountingChild(label: format!("tick {}", tick.get()))
            }
        }
    }

    #[test]
    fn test_memo_changed_comparator_rerenders_child() {
        CHANGING_CHILD_RENDERS.store(0, Ordering::SeqCst);
        let canvases: Vec<_> = smol::block_on(
            element!(ChangingMemoApp)
                .mock_terminal_render_loop(MockTerminalConfig::default())
                .collect(),
        );
        let rendered = canvases.last().unwrap().to_string();
        assert!(
            rendered.starts_with("tick 3 renders=3"),
            "changing memo key should update the child every frame: {rendered:?}"
        );
    }

    #[component]
    fn NoComparatorMemoApp(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let mut tick = hooks.use_state(|| 0u8);
        if tick.get() < 3 {
            tick += 1;
        } else {
            system.exit();
        }

        element! {
            Memo(memo_key: "stable".to_string()) {
                NoComparatorCountingChild(label: format!("tick {}", tick.get()))
            }
        }
    }

    #[test]
    fn test_memo_requires_explicit_comparator() {
        NO_COMPARATOR_CHILD_RENDERS.store(0, Ordering::SeqCst);
        let canvases: Vec<_> = smol::block_on(
            element!(NoComparatorMemoApp)
                .mock_terminal_render_loop(MockTerminalConfig::default())
                .collect(),
        );
        let rendered = canvases.last().unwrap().to_string();
        assert!(
            rendered.starts_with("tick 3 renders=4"),
            "missing comparator should preserve normal child updates: {rendered:?}"
        );
    }

    #[component]
    fn PulsingChild(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let pulse = hooks.use_state(|| 0u8);
        let mut pulse_for_interval = pulse;
        hooks.use_interval(
            move || {
                if pulse_for_interval.get() < 3 {
                    pulse_for_interval += 1;
                }
            },
            Some(Duration::from_millis(0)),
        );
        element!(Text(content: format!("pulse={}", pulse.get())))
    }

    #[component]
    fn StatefulChildMemoApp(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let mut tick = hooks.use_state(|| 0u8);
        if tick.get() < 8 {
            tick += 1;
        } else {
            system.exit();
        }

        element! {
            Memo(memo_key: "stable".to_string(), compare: memo_key_eq as MemoComparator) {
                PulsingChild
            }
        }
    }

    #[test]
    fn test_memo_does_not_skip_stateful_child_changes() {
        let canvases: Vec<_> = smol::block_on(
            element!(StatefulChildMemoApp)
                .mock_terminal_render_loop(MockTerminalConfig::default())
                .collect(),
        );
        let rendered = canvases.last().unwrap().to_string();
        assert!(
            rendered.starts_with("pulse=3"),
            "child hook polling should still update through a stable memo key: {rendered:?}"
        );
    }
}
