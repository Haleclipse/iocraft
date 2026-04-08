use crate::{
    hooks::UseContext, FocusContext, FocusHandle, FocusId, FocusManager, FocusOptions, Hook, Hooks,
};

mod private {
    pub trait Sealed {}
    impl Sealed for crate::Hooks<'_, '_> {}
}

/// `UseFocus` is a hook that registers the calling component as a focusable element within the
/// nearest enclosing [`FocusScope`](crate::components::FocusScope).
///
/// It mirrors ink's `useFocus` hook in spirit, but follows iocraft's RAII style: the hook
/// automatically registers itself on first render and de-registers on drop, so you never need
/// an explicit cleanup step.
///
/// # Example
///
/// ```
/// # use iocraft::prelude::*;
/// #[component]
/// fn Field(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
///     let focus = hooks.use_focus(FocusOptions::new().auto_focus());
///     element! {
///         View(border_style: if focus.is_focused() { BorderStyle::Round } else { BorderStyle::Single }) {
///             Text(content: if focus.is_focused() { "[focused]" } else { "        " })
///         }
///     }
/// }
/// ```
pub trait UseFocus: private::Sealed {
    /// Registers the component as a focusable element with the given options and returns a
    /// [`FocusHandle`] that can be used to query whether it currently holds focus.
    ///
    /// # Panics
    ///
    /// Panics if there is no [`FocusScope`](crate::components::FocusScope) ancestor in the
    /// component tree. This mirrors ink's behaviour when `useFocus` is called outside of an
    /// `<App>` and gives a clear failure mode rather than a silent dead handle.
    fn use_focus(&mut self, opts: FocusOptions) -> FocusHandle;
}

/// `UseFocusManager` exposes the imperative side of focus control: enable/disable traversal,
/// jump to next/previous focusable, or directly focus a specific id.
pub trait UseFocusManager: private::Sealed {
    /// Returns a [`FocusManager`] for the nearest enclosing [`FocusScope`](crate::components::FocusScope).
    ///
    /// # Panics
    ///
    /// Panics if there is no `FocusScope` ancestor in the component tree.
    fn use_focus_manager(&mut self) -> FocusManager;
}

impl UseFocus for Hooks<'_, '_> {
    fn use_focus(&mut self, opts: FocusOptions) -> FocusHandle {
        // Read the FocusContext from the surrounding scope. This Ref is dropped at the end
        // of the expression, releasing the borrow before we touch `self.use_hook`.
        let ctx: FocusContext = *self
            .try_use_context::<FocusContext>()
            .expect("use_focus called outside of a FocusScope");

        let h = self.use_hook(move || UseFocusImpl {
            ctx,
            id: ctx.register(opts),
            last_is_active: opts.is_active,
        });

        // Reconcile is_active changes across renders so callers can flip a focusable on/off
        // declaratively without unmounting it.
        if h.last_is_active != opts.is_active {
            h.ctx.set_entry_active(h.id, opts.is_active);
            h.last_is_active = opts.is_active;
        }

        // Record this focusable's position in the *current* render order. The enclosing
        // FocusScope's boundary hook reads this list at the end of the render pass and
        // rewrites the entry order accordingly, so Tab traversal tracks the live UI
        // layout instead of the historical mount order. (Review issue #3.)
        h.ctx.note_render_position(h.id);

        FocusHandle::new(h.id, h.ctx)
    }
}

impl UseFocusManager for Hooks<'_, '_> {
    fn use_focus_manager(&mut self) -> FocusManager {
        let ctx: FocusContext = *self
            .try_use_context::<FocusContext>()
            .expect("use_focus_manager called outside of a FocusScope");
        FocusManager::new(ctx)
    }
}

struct UseFocusImpl {
    ctx: FocusContext,
    id: FocusId,
    last_is_active: bool,
}

// `Hook` is implemented with all defaults — UseFocusImpl doesn't need to participate in
// poll_change/draw cycles. Re-rendering on focus change is driven by the FocusScope's own
// `State<FocusState>` waker, which propagates down through the normal child reconciliation.
impl Hook for UseFocusImpl {}

impl Drop for UseFocusImpl {
    fn drop(&mut self) {
        // RAII: when the component is unmounted, its hook vector is dropped, which runs us.
        // If the FocusScope has already been dropped, `unregister` becomes a silent no-op
        // because the underlying State storage is gone.
        self.ctx.unregister(self.id);
    }
}

#[cfg(test)]
mod tests {
    use crate::prelude::*;
    use futures::stream::{self, StreamExt};
    use macro_rules_attribute::apply;
    use smol_macros::test;

    #[component]
    fn Field(mut hooks: Hooks, props: &FieldProps) -> impl Into<AnyElement<'static>> {
        let focus = hooks.use_focus(if props.auto {
            FocusOptions::new().auto_focus()
        } else {
            FocusOptions::new()
        });
        let label = format!(
            "{}{}",
            props.label,
            if focus.is_focused() { "*" } else { " " }
        );
        element!(Text(content: label))
    }

    #[derive(Default, Props)]
    struct FieldProps {
        label: String,
        auto: bool,
    }

    #[component]
    fn Form(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let mut presses = hooks.use_state(|| 0u32);
        hooks.use_terminal_events(move |e| {
            if let TerminalEvent::Key(KeyEvent {
                kind: KeyEventKind::Press,
                ..
            }) = e
            {
                presses += 1;
            }
        });
        if presses.get() >= 2 {
            system.exit();
        }
        element! {
            View(flex_direction: FlexDirection::Column) {
                FocusScope {
                    Field(label: "a".to_string(), auto: true)
                    Field(label: "b".to_string())
                    Field(label: "c".to_string())
                }
            }
        }
    }

    /// End-to-end smoke test: Tab inside a [`FocusScope`] advances focus through the registered
    /// children. We can only assert on the *initial* and *post-batch* canvases because the
    /// mock render loop dedupes unchanged frames and the event stream is fully buffered, so
    /// all queued events drain in a single `poll_change` pass before the next render.
    #[apply(test!)]
    async fn test_tab_navigation_through_focus_scope() {
        let canvases: Vec<_> = element!(Form)
            .mock_terminal_render_loop(MockTerminalConfig::with_events(stream::iter(vec![
                TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Tab)),
                TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Tab)),
            ])))
            .collect()
            .await;
        let actual = canvases
            .iter()
            .map(|c| c.to_string())
            .collect::<Vec<_>>();
        // Initial canvas: auto_focus put `a` in focus.
        let first = &actual[0];
        assert!(first.contains("a*"), "expected initial a* in {first:?}");
        assert!(!first.contains("b*"));
        assert!(!first.contains("c*"));
        // After two Tabs, focus has moved a → b → c.
        let last = actual.last().unwrap();
        assert!(last.contains("c*"), "expected c* in {last:?}");
        assert!(!last.contains("a*"), "did not expect a* in {last:?}");
        assert!(!last.contains("b*"), "did not expect b* in {last:?}");
    }

    // NOTE: the core "render order → entry order" reconciliation is unit-tested
    // directly on `FocusState::reorder_to_match` in `focus.rs`, because any
    // render-loop-based assertion on this gets muddied by two factors:
    //
    //   1. `mock_terminal_render_loop` batches all queued events into a single
    //      `poll_change` pass, so you can't interleave "insert new child" and
    //      "press Tab" with a render in between.
    //   2. The visible canvas reflects the element-tree order, which is correct
    //      even under the *old* append-on-mount behaviour — meaning any forward
    //      or backward traversal test ends up being symmetric and non-discriminating.
    //
    // The unit tests in `focus::tests::reorder_to_match_*` pin the behaviour
    // precisely. This module only exercises the happy path: a static tree with
    // several `use_focus` calls still Tab-navigates correctly after the reorder
    // pipeline is in play.
}
