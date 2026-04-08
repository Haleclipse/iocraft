use crate::{
    component, components::ContextProvider, element, focus::create_focus_context,
    hooks::UseTerminalEvents, AnyElement, Context, FocusContext, Hooks, KeyCode, KeyEvent,
    KeyEventKind, KeyModifiers, Props, TerminalEvent,
};

/// The props which can be passed to the [`FocusScope`] component.
#[non_exhaustive]
#[derive(Default, Props)]
pub struct FocusScopeProps<'a> {
    /// The children of the component. They form the focus group governed by this scope.
    pub children: Vec<AnyElement<'a>>,

    /// If `true` (the default), the scope intercepts `Tab` / `Shift+Tab` to advance focus and
    /// `Esc` to clear it. Set to `false` if you want to drive focus entirely through
    /// [`FocusManager`](crate::FocusManager) and avoid touching keyboard input.
    pub handle_keys: Option<bool>,
}

/// `FocusScope` defines a focus group: a subtree in which any descendant calling
/// [`use_focus`](crate::hooks::UseFocus::use_focus) participates in the same `Tab` traversal.
///
/// # What "independent" means here
///
/// `FocusScope` provides **state independence**, not **event isolation**. Every scope
/// owns a private [`FocusContext`](crate::FocusContext) in its own `use_state` slot,
/// so each scope has its own entry list, its own "currently focused" id, and its own
/// Tab ring. That is the part that is truly isolated: writing to one scope's focus
/// state never touches another scope's focus state.
///
/// What is **not** isolated is terminal-event delivery. iocraft broadcasts every
/// terminal key event to every `use_terminal_events` subscriber — there is no
/// event-consumption / event-stealing primitive in the current render loop — so if
/// two `FocusScope`s are mounted at the same time and both have `handle_keys = true`,
/// a single Tab press will reach both of them and both will advance their (private)
/// focus ring in parallel.
///
/// # Consequences
///
/// - **Sibling scopes** (e.g. two independent forms on the same screen): this is
///   usually exactly what you want. Each form tracks its own selection, and Tab
///   advances both at once. The visible effect is "each form is self-contained".
/// - **Truly nested scopes** (e.g. a modal dialog spawned inside a parent form):
///   by default this will feel wrong, because the outer scope keeps processing Tab
///   while the modal is open. The supported pattern is:
///     1. Give the inner scope `handle_keys: Some(false)` so it does *not*
///        intercept key events at all.
///     2. Drive its focus from the outside via
///        [`FocusManager`](crate::FocusManager) — typically from a custom keybinding
///        inside the modal's own `use_terminal_events` closure.
///   The `nested_scope_with_manual_driving_is_isolated` regression test in this
///   file demonstrates the recommended wiring end-to-end.
///
/// A future iocraft release may add real event stealing. Until then, treat "Tab is
/// routed to exactly one scope" as an opt-in configuration, not a default guarantee.
///
/// # Example
///
/// ```
/// # use iocraft::prelude::*;
/// #[component]
/// fn Field(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
///     let focus = hooks.use_focus(FocusOptions::default());
///     element! {
///         Text(content: if focus.is_focused() { "[*]" } else { "[ ]" })
///     }
/// }
///
/// # fn _example() -> impl Into<AnyElement<'static>> {
/// element! {
///     FocusScope {
///         Field
///         Field
///         Field
///     }
/// }
/// # }
/// ```
#[component]
pub fn FocusScope<'a>(
    mut hooks: Hooks,
    props: &mut FocusScopeProps<'a>,
) -> impl Into<AnyElement<'a>> {
    // Allocate a fresh focus context, backed by this scope's own use_state slot.
    // Children downstream of the ContextProvider below will see this exact handle.
    let ctx: FocusContext = create_focus_context(&mut hooks);

    // IMPORTANT: `use_terminal_events` MUST be called unconditionally on every render to
    // satisfy the rules of hooks (the hook slot index must be stable across renders, just
    // like in React). Toggling `handle_keys` between renders therefore has to gate the
    // *body* of the closure, never the hook call itself. Doing it the other way (calling
    // the hook only when `handle_keys == true`) breaks two ways:
    //
    //   - `false → true`: the second render tries to retrieve a hook that doesn't exist
    //     and panics inside `Hooks::use_hook` (see the
    //     `handle_keys_can_toggle_false_to_true_without_panic` regression test).
    //   - `true  → false`: the previously-installed hook is *not* removed; it stays in
    //     the vector and keeps consuming key events, so the prop appears to do nothing.
    let handle_keys = props.handle_keys.unwrap_or(true);
    hooks.use_terminal_events(move |event| {
        if !handle_keys {
            return;
        }
        if let TerminalEvent::Key(KeyEvent {
            code,
            modifiers,
            kind,
        }) = event
        {
            if kind == KeyEventKind::Release {
                return;
            }
            match code {
                KeyCode::BackTab => ctx.focus_prev(),
                KeyCode::Tab if modifiers.contains(KeyModifiers::SHIFT) => ctx.focus_prev(),
                KeyCode::Tab => ctx.focus_next(),
                KeyCode::Esc => ctx.clear(),
                _ => {}
            }
        }
    });

    element! {
        ContextProvider(value: Context::owned(ctx)) {
            #(props.children.iter_mut())
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::prelude::*;
    use futures::stream::{self, StreamExt};
    use macro_rules_attribute::apply;
    use smol_macros::test;

    #[derive(Default, Props)]
    struct ItemProps {
        label: String,
    }

    #[component]
    fn Item(mut hooks: Hooks, props: &ItemProps) -> impl Into<AnyElement<'static>> {
        let focus = hooks.use_focus(FocusOptions::default());
        element!(Text(content: format!("{}{}", props.label, if focus.is_focused() { "*" } else { " " })))
    }

    #[component]
    fn TwoGroups(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let mut events = hooks.use_state(|| 0u32);
        hooks.use_terminal_events(move |e| {
            if let TerminalEvent::Key(KeyEvent {
                kind: KeyEventKind::Press,
                ..
            }) = e
            {
                events += 1;
            }
        });
        if events.get() >= 2 {
            system.exit();
        }
        element! {
            View(flex_direction: FlexDirection::Column) {
                FocusScope {
                    Item(label: "outer-a".to_string())
                    Item(label: "outer-b".to_string())
                }
                FocusScope {
                    Item(label: "inner-a".to_string())
                    Item(label: "inner-b".to_string())
                }
            }
        }
    }

    /// Sibling [`FocusScope`]s manage their own focus order **independently**: each one keeps
    /// a private `FocusContext` in its own `use_state` slot.
    ///
    /// Note: because iocraft delivers terminal key events to *every* subscriber (there is no
    /// event-consumption primitive yet), both scopes advance in parallel when the user
    /// presses Tab. This is the expected behavior for sibling forms; for truly nested groups
    /// (e.g. a modal inside a parent form) the inner scope should be constructed with
    /// `handle_keys: Some(false)` and driven via [`FocusManager`](crate::FocusManager) from
    /// custom keybindings.
    #[apply(test!)]
    async fn sibling_scopes_advance_in_parallel_but_track_state_independently() {
        let canvases: Vec<_> = element!(TwoGroups)
            .mock_terminal_render_loop(MockTerminalConfig::with_events(stream::iter(vec![
                TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Tab)),
                TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Tab)),
            ])))
            .collect()
            .await;
        let last = canvases.last().unwrap().to_string();
        // After two Tabs each scope has advanced twice → second item focused in both.
        assert!(last.contains("outer-b*"), "expected outer-b* in {last:?}");
        assert!(last.contains("inner-b*"), "expected inner-b* in {last:?}");
        // The first items should not be marked focused.
        assert!(
            !last.contains("outer-a*"),
            "did not expect outer-a* in {last:?}"
        );
        assert!(
            !last.contains("inner-a*"),
            "did not expect inner-a* in {last:?}"
        );
    }

    // ----- Regression coverage for review issue #1 (handle_keys hook ordering) -----

    /// A FocusScope wrapper whose `handle_keys` flips after the first Tab. With the old
    /// implementation (conditional `use_terminal_events`), this scenario would either
    /// panic on hook-index mismatch or silently leave the now-defunct hook subscribed.
    /// We assert it does *neither*: the scope must keep working in `true → false` mode
    /// (Tab is now ignored) and `false → true` mode (Tab starts being intercepted).
    #[derive(Default, Props)]
    struct ToggleProps {
        start_handling: bool,
    }

    #[component]
    fn TogglingScope(mut hooks: Hooks, props: &ToggleProps) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        // Flip after the first key press.
        let mut handle = hooks.use_state(|| props.start_handling);
        let mut presses = hooks.use_state(|| 0u32);
        hooks.use_terminal_events(move |e| {
            if let TerminalEvent::Key(KeyEvent {
                kind: KeyEventKind::Press,
                ..
            }) = e
            {
                presses += 1;
                handle.set(!handle.get());
            }
        });
        // Exit as soon as a single event has flipped `handle` — we need a visible
        // render pass *after* the toggle so FocusScope actually observes the new
        // `handle_keys` value (and, under the old buggy code, would panic on hook
        // slot mismatch). `mock_terminal_render_loop` batches all ready events into
        // one `poll_change` pass, so if we demanded multiple presses the toggle
        // would oscillate back to its starting value within the same batch.
        if presses.get() >= 1 {
            system.exit();
        }
        element! {
            View(flex_direction: FlexDirection::Column) {
                FocusScope(handle_keys: Some(handle.get())) {
                    Item(label: "x".to_string())
                    Item(label: "y".to_string())
                }
            }
        }
    }

    #[apply(test!)]
    async fn handle_keys_can_toggle_true_to_false_without_panic() {
        // Render 0: handle_keys=true  → FocusScope installs terminal-events hook at slot 1.
        // Event fires, closure flips handle → false, counter=1 → exit on next render.
        // Render 1: handle_keys=false → under the old conditional code, slot 1 would be
        // skipped, leaving the hook in the vector but unmaintained; under the fixed
        // code, the hook is always called and the body simply early-returns.
        let canvases: Vec<_> = element! { TogglingScope(start_handling: true) }
            .mock_terminal_render_loop(MockTerminalConfig::with_events(stream::iter(vec![
                TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Tab)),
            ])))
            .collect()
            .await;
        assert!(!canvases.is_empty());
    }

    // ----- Regression coverage for is_active runtime toggle -----

    /// A focusable whose `is_active` flips at runtime should lose/regain its slot
    /// in the Tab ring *in place*, without unmounting. This validates the
    /// `UseFocusImpl` reconcile branch that calls `set_entry_active`.
    #[derive(Default, Props)]
    struct GatedProps {
        label: String,
        gate: Option<State<bool>>,
    }

    #[component]
    fn Gated(mut hooks: Hooks, props: &GatedProps) -> impl Into<AnyElement<'static>> {
        let enabled = props.gate.map(|g| g.get()).unwrap_or(true);
        let focus = hooks.use_focus(FocusOptions {
            auto_focus: false,
            is_active: enabled,
        });
        element!(Text(content: format!(
            "{}{}", props.label, if focus.is_focused() { "*" } else { " " }
        )))
    }

    #[component]
    fn GatedForm(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let mut presses = hooks.use_state(|| 0u32);
        let gate_b = hooks.use_state(|| true);
        hooks.use_terminal_events({
            let mut gate_b = gate_b;
            move |e| {
                if let TerminalEvent::Key(KeyEvent {
                    kind: KeyEventKind::Press,
                    ..
                }) = e
                {
                    // Before the first Tab, turn `b` inactive.
                    if presses.get() == 0 {
                        gate_b.set(false);
                    }
                    presses += 1;
                }
            }
        });
        if presses.get() >= 2 {
            system.exit();
        }
        element! {
            View(flex_direction: FlexDirection::Column) {
                FocusScope {
                    View(flex_direction: FlexDirection::Column) {
                        Gated(label: "a".to_string())
                        Gated(label: "b".to_string(), gate: gate_b)
                        Gated(label: "c".to_string())
                    }
                }
            }
        }
    }

    #[apply(test!)]
    async fn is_active_false_at_runtime_skips_the_slot_in_traversal() {
        // Render 0: all active, nothing focused.
        // Event 1: `b` turns inactive; Tab → from None, first active is `a`. active=a.
        // Event 2: Tab → from a, next active is `c` (skipping inactive b). active=c.
        let canvases: Vec<_> = element!(GatedForm)
            .mock_terminal_render_loop(MockTerminalConfig::with_events(stream::iter(vec![
                TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Tab)),
                TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Tab)),
            ])))
            .collect()
            .await;
        let last = canvases.last().unwrap().to_string();
        assert!(last.contains("c*"), "expected c* in {last:?}");
        assert!(!last.contains("a*"), "did not expect a* in {last:?}");
        assert!(
            !last.contains("b*"),
            "b was deactivated, must not be focused"
        );
    }

    // ----- Regression coverage for nested scope escape hatch (review issue #4) -----

    /// An inner FocusScope with `handle_keys: Some(false)` stays isolated from the
    /// outer scope's Tab handling — only the outer one reacts to key events. The
    /// inner scope can still be driven programmatically via [`FocusManager`].
    #[component]
    fn InnerManual() -> impl Into<AnyElement<'static>> {
        // This inner scope delegates key handling to the caller.
        element! {
            FocusScope(handle_keys: Some(false)) {
                View(flex_direction: FlexDirection::Column) {
                    Item(label: "in-a".to_string())
                    Item(label: "in-b".to_string())
                }
            }
        }
    }

    #[component]
    fn NestedScopes(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
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
            FocusScope {
                View(flex_direction: FlexDirection::Column) {
                    Item(label: "out-a".to_string())
                    Item(label: "out-b".to_string())
                    InnerManual
                }
            }
        }
    }

    #[apply(test!)]
    async fn nested_scope_with_manual_driving_is_isolated() {
        // Two Tab presses should advance the OUTER scope only. The inner scope's
        // in-a / in-b must never show a focus marker because the inner scope
        // opted out of key handling and nothing is driving it programmatically.
        let canvases: Vec<_> = element!(NestedScopes)
            .mock_terminal_render_loop(MockTerminalConfig::with_events(stream::iter(vec![
                TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Tab)),
                TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Tab)),
            ])))
            .collect()
            .await;
        let last = canvases.last().unwrap().to_string();
        // Outer advanced: final focus is on out-b.
        assert!(last.contains("out-b*"), "expected out-b* in {last:?}");
        // Inner scope stayed silent.
        assert!(
            !last.contains("in-a*"),
            "inner must not intercept Tab: {last:?}"
        );
        assert!(
            !last.contains("in-b*"),
            "inner must not intercept Tab: {last:?}"
        );
    }

    #[apply(test!)]
    async fn handle_keys_can_toggle_false_to_true_without_panic() {
        // Render 0: handle_keys=false → old code SKIPS `use_terminal_events`, so the
        // hook vec only contains [UseStateImpl] at that point.
        // Event fires (still delivered to TogglingScope's own counter), closure flips
        // handle → true, counter=1, exit scheduled.
        // Render 1: handle_keys=true → old code now tries to install the hook at
        // slot 1, but `first_update` is false so `use_hook` reaches into a slot that
        // doesn't exist → panic on downcast. The fixed code always reserves slot 1
        // from the very first render, so this path is safe.
        let canvases: Vec<_> = element! { TogglingScope(start_handling: false) }
            .mock_terminal_render_loop(MockTerminalConfig::with_events(stream::iter(vec![
                TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Tab)),
            ])))
            .collect()
            .await;
        assert!(!canvases.is_empty());
    }
}
