//! Focus management primitives.
//!
//! This module provides the data types that power [`FocusScope`](crate::components::FocusScope),
//! [`UseFocus`](crate::hooks::UseFocus), and [`UseFocusManager`](crate::hooks::UseFocusManager).
//!
//! In most applications you don't construct any of these types directly — you wrap an interactive
//! subtree in a `FocusScope` and call `hooks.use_focus(...)` from descendant components.

use crate::hooks::{Ref, State, UseRef, UseState};
use crate::{ComponentUpdater, Hook};
use std::collections::HashMap;

/// A unique identifier for a focusable element within a single [`FocusScope`].
///
/// `FocusId`s are allocated by the enclosing scope and are only meaningful within that scope.
/// They are intentionally `Copy` so they can be passed through closures and props with zero
/// ceremony.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FocusId(u64);

impl FocusId {
    /// Returns the underlying integer value of this id.
    ///
    /// This is mainly useful for logging and tests; do not rely on the numeric value being
    /// stable across renders or scopes.
    pub fn as_u64(self) -> u64 {
        self.0
    }
}

/// Options for registering a focusable element via [`UseFocus::use_focus`](crate::hooks::UseFocus::use_focus).
#[derive(Debug, Clone, Copy)]
pub struct FocusOptions {
    /// If `true`, this element requests focus on first mount when no other element currently
    /// holds focus. Only the first auto-focusing element wins; subsequent ones are ignored.
    pub auto_focus: bool,

    /// If `false`, this element keeps its slot in the focus order but is skipped during
    /// `Tab`/`Shift+Tab` traversal and cannot be focused. Setting it back to `true` re-enables
    /// the slot in place — the element does not lose its position in the order.
    ///
    /// Defaults to `true` (the slot is active).
    pub is_active: bool,
}

impl Default for FocusOptions {
    fn default() -> Self {
        Self::new()
    }
}

impl FocusOptions {
    /// Convenience constructor mirroring ink's defaults: not auto-focused, active.
    pub fn new() -> Self {
        Self {
            auto_focus: false,
            is_active: true,
        }
    }

    /// Builder: opt this element into receiving focus on mount.
    pub fn auto_focus(mut self) -> Self {
        self.auto_focus = true;
        self
    }

    /// Builder: mark this element as inactive (kept in the order, but skipped).
    pub fn inactive(mut self) -> Self {
        self.is_active = false;
        self
    }
}

#[derive(Debug, Clone, Copy)]
struct FocusEntry {
    id: FocusId,
    is_active: bool,
}

/// The mutable state owned by a single [`FocusScope`].
///
/// This is intentionally not part of the public API: callers interact with it through the
/// [`FocusContext`] handle, [`FocusHandle`], and [`FocusManager`].
#[derive(Debug)]
pub struct FocusState {
    entries: Vec<FocusEntry>,
    active: Option<FocusId>,
    enabled: bool,
    next_id: u64,
}

impl Default for FocusState {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            active: None,
            enabled: true,
            next_id: 0,
        }
    }
}

impl FocusState {
    fn alloc_id(&mut self) -> FocusId {
        let id = FocusId(self.next_id);
        self.next_id += 1;
        id
    }

    fn register(&mut self, auto_focus: bool, is_active: bool) -> FocusId {
        let id = self.alloc_id();
        self.entries.push(FocusEntry { id, is_active });
        if auto_focus && is_active && self.active.is_none() {
            self.active = Some(id);
        }
        id
    }

    fn unregister(&mut self, id: FocusId) {
        let was_active = self.active == Some(id);
        let removed_idx = self.entries.iter().position(|e| e.id == id);
        self.entries.retain(|e| e.id != id);
        // Also drop the id from the in-flight render order if it's there.
        if let Some(removed_idx) = removed_idx {
            if was_active {
                // Continue forward from the slot the removed entry used to occupy.
                // After `retain`, that slot is now occupied by what was the *next* entry,
                // so `find_next_from_index(removed_idx)` walks the post-removal list in
                // the same direction `set_entry_active` would have used.
                self.active = self.find_next_from_index(removed_idx);
            }
        }
    }

    fn set_entry_active(&mut self, id: FocusId, is_active: bool) {
        let mut changed = false;
        if let Some(entry) = self.entries.iter_mut().find(|e| e.id == id) {
            if entry.is_active != is_active {
                entry.is_active = is_active;
                changed = true;
            }
        }
        if changed && !is_active && self.active == Some(id) {
            // Move forward to the next still-active focusable, wrapping around.
            self.active = self.find_next(Some(id));
        }
    }

    fn focus(&mut self, id: FocusId) {
        if self.entries.iter().any(|e| e.id == id && e.is_active) {
            self.active = Some(id);
        }
    }

    /// Walk the entry list forward starting at `start_idx`, wrapping around, returning the
    /// first active entry's id. The shared primitive used by both [`find_next`] and
    /// [`unregister`].
    fn find_next_from_index(&self, start_idx: usize) -> Option<FocusId> {
        let len = self.entries.len();
        if len == 0 {
            return None;
        }
        let start_idx = start_idx % len;
        for i in 0..len {
            let idx = (start_idx + i) % len;
            if self.entries[idx].is_active {
                return Some(self.entries[idx].id);
            }
        }
        None
    }

    fn find_next(&self, from: Option<FocusId>) -> Option<FocusId> {
        let len = self.entries.len();
        if len == 0 {
            return None;
        }
        let start_idx = match from {
            Some(id) => self
                .entries
                .iter()
                .position(|e| e.id == id)
                .map(|i| (i + 1) % len)
                .unwrap_or(0),
            None => 0,
        };
        self.find_next_from_index(start_idx)
    }

    fn find_prev(&self, from: Option<FocusId>) -> Option<FocusId> {
        let len = self.entries.len();
        if len == 0 {
            return None;
        }
        // If `from` is None we start "past the end" so the first iteration lands on the last entry.
        let start_idx = match from {
            Some(id) => self.entries.iter().position(|e| e.id == id).unwrap_or(len),
            None => len,
        };
        for i in 1..=len {
            let idx = (start_idx + len - i) % len;
            if self.entries[idx].is_active {
                return Some(self.entries[idx].id);
            }
        }
        None
    }

    fn focus_next(&mut self) {
        if !self.enabled {
            return;
        }
        if let Some(id) = self.find_next(self.active) {
            self.active = Some(id);
        }
    }

    fn focus_prev(&mut self) {
        if !self.enabled {
            return;
        }
        if let Some(id) = self.find_prev(self.active) {
            self.active = Some(id);
        }
    }

    /// Total number of registered focusables (active or not). Useful for tests.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns true if there are no registered focusables.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Rewrite `self.entries` so its order matches `seq`. Entries whose id does not
    /// appear in `seq` are preserved at the end in their original relative order.
    ///
    /// Returns `true` if the entry list was actually mutated, `false` otherwise. The
    /// caller (typically [`FocusContext::finalize_render`]) relies on this signal to
    /// skip a write to the rendering-bound `State<FocusState>` whenever the computed
    /// order is identical to what's already stored — otherwise every frame would
    /// trigger `did_change`, producing a perpetual render loop.
    ///
    /// This is the core of the "Tab order tracks UI order" promise (review issue #3).
    /// The logic is extracted here so it can be unit-tested directly, without spinning
    /// up a render loop.
    ///
    /// Two sources of "no actual change" are handled:
    /// 1. **Happy path**: `seq` matches the current entry order element-for-element —
    ///    cheap early-exit via a direct comparison, no allocations.
    /// 2. **Defensive path**: `seq` misses some entries (e.g. an entry existed in
    ///    `entries` but no corresponding `use_focus` ran this frame — should not
    ///    happen under normal reconciliation, but we tolerate it). We compute the
    ///    full proposed order and compare it to the current one before committing
    ///    the write, so even a degenerate "seq is shorter than entries" state
    ///    cannot force repeated re-renders once it has stabilised.
    fn reorder_to_match(&mut self, seq: &[FocusId]) -> bool {
        // Happy path: already in the desired order.
        if self.entries.len() == seq.len()
            && self.entries.iter().zip(seq).all(|(e, id)| e.id == *id)
        {
            return false;
        }

        // Compute the proposed new order in a temporary buffer. We don't commit to
        // `self.entries` yet because we need one more equality check against the
        // *current* order to guarantee no-op stability for the defensive path above.
        let by_id: HashMap<FocusId, FocusEntry> =
            self.entries.iter().map(|e| (e.id, *e)).collect();
        let mut proposed: Vec<FocusEntry> = Vec::with_capacity(self.entries.len());
        let mut placed: std::collections::HashSet<FocusId> =
            std::collections::HashSet::with_capacity(seq.len());
        for id in seq {
            if placed.contains(id) {
                continue; // defensive: duplicates in seq are ignored
            }
            if let Some(e) = by_id.get(id) {
                proposed.push(*e);
                placed.insert(*id);
            }
        }
        // Append any leftovers (entries not mentioned in seq) in their original
        // relative order. Under normal reconciliation this loop is a no-op.
        for entry in &self.entries {
            if !placed.contains(&entry.id) {
                proposed.push(*entry);
            }
        }

        // Now compare the proposed order with the current order. If they happen to
        // match (for example because the leftover entries naturally fall back into
        // the same slots), bail out and leave `self.entries` untouched so that the
        // surrounding `finalize_render` does NOT take a write lock. This is what
        // keeps the render loop stable in the defensive branch.
        if proposed.len() == self.entries.len()
            && proposed
                .iter()
                .zip(self.entries.iter())
                .all(|(a, b)| a.id == b.id)
        {
            return false;
        }

        self.entries = proposed;
        true
    }
}

/// A `Copy` handle to a [`FocusScope`]'s focus state.
///
/// `FocusContext` is what gets propagated to descendants via the context stack. It is cheap to
/// copy and safe to capture inside event closures: internally it points at two backing slots
/// that live in the scope's own hooks:
///
/// - A [`State<FocusState>`] that stores the canonical focus data. Writes to this slot
///   cause the scope to re-render (that is how Tab visibly moves the focus marker).
/// - A [`Ref<Vec<FocusId>>`] that captures the order in which descendants called
///   [`UseFocus::use_focus`](crate::hooks::UseFocus::use_focus) during the current render
///   pass. Writes to this slot do **not** re-render, which is intentional: the scope needs
///   to track UI order on every frame without triggering an infinite render loop.
///
/// You should not normally interact with this type directly — use [`FocusHandle`] (returned
/// from `use_focus`) or [`FocusManager`] (returned from `use_focus_manager`) instead.
#[derive(Clone, Copy)]
pub struct FocusContext {
    state: State<FocusState>,
    seq: Ref<Vec<FocusId>>,
}

impl FocusContext {
    pub(crate) fn new(state: State<FocusState>, seq: Ref<Vec<FocusId>>) -> Self {
        Self { state, seq }
    }

    /// Run the given closure with exclusive access to the underlying [`FocusState`], returning
    /// `None` if the owning [`FocusScope`] has already been dropped.
    ///
    /// All mutating helpers funnel through this method so the local-binding / drop-order
    /// dance only has to be written once.
    fn with_mut<R>(&self, f: impl FnOnce(&mut FocusState) -> R) -> Option<R> {
        let mut state = self.state;
        // Bind the guard to a named local declared *after* `state`, so reverse drop order
        // releases the borrow before the underlying state copy goes away. This sidesteps
        // edition-2021's "if-let scrutinee temporary lives until end of block" rule.
        let mut guard = state.try_write()?;
        Some(f(&mut guard))
    }

    fn with_ref<R>(&self, f: impl FnOnce(&FocusState) -> R) -> Option<R> {
        let state = self.state;
        let guard = state.try_read()?;
        Some(f(&guard))
    }

    pub(crate) fn register(&self, opts: FocusOptions) -> FocusId {
        self.with_mut(|s| s.register(opts.auto_focus, opts.is_active))
            .unwrap_or(FocusId(u64::MAX))
    }

    pub(crate) fn unregister(&self, id: FocusId) {
        self.with_mut(|s| s.unregister(id));
    }

    pub(crate) fn set_entry_active(&self, id: FocusId, is_active: bool) {
        self.with_mut(|s| s.set_entry_active(id, is_active));
    }

    /// Returns `true` if the given id currently holds focus in this scope.
    pub fn is_focused(&self, id: FocusId) -> bool {
        self.with_ref(|s| s.active == Some(id)).unwrap_or(false)
    }

    /// The id that currently holds focus in this scope, if any.
    pub fn active(&self) -> Option<FocusId> {
        self.with_ref(|s| s.active).unwrap_or(None)
    }

    /// Returns `true` if focus traversal (Tab / Shift+Tab) is currently enabled for this scope.
    pub fn is_enabled(&self) -> bool {
        self.with_ref(|s| s.enabled).unwrap_or(false)
    }

    /// Move focus to the given id, if it is currently active.
    pub fn focus(&self, id: FocusId) {
        self.with_mut(|s| s.focus(id));
    }

    /// Move focus forward to the next active focusable, wrapping around.
    pub fn focus_next(&self) {
        self.with_mut(|s| s.focus_next());
    }

    /// Move focus backward to the previous active focusable, wrapping around.
    pub fn focus_prev(&self) {
        self.with_mut(|s| s.focus_prev());
    }

    /// Globally enable focus traversal.
    pub fn enable(&self) {
        self.with_mut(|s| s.enabled = true);
    }

    /// Globally disable focus traversal. Existing focus is retained but Tab navigation is a no-op.
    pub fn disable(&self) {
        self.with_mut(|s| s.enabled = false);
    }

    /// Clear the active focus entirely.
    pub fn clear(&self) {
        self.with_mut(|s| s.active = None);
    }

    // ---- Render-cycle wiring for UI-order tracking (review issue #3) ----
    //
    // Every frame, the enclosing FocusScope rebuilds a `seq` list of the ids that its
    // descendant `use_focus` hooks called out *in render order*. At the end of the render
    // pass, `finalize_render` uses that list to rewrite `state.entries` so the Tab order
    // matches the current UI order — not the historical mount order.
    //
    // Writes to `seq` go through [`Ref`], which deliberately does NOT cause a re-render.
    // Writes to `state.entries` do cause a re-render, so `finalize_render` performs a
    // read-compare-write to skip the write entirely when the order has not changed,
    // avoiding a steady-state render loop.

    /// Run the given closure with mutable access to the render-order sequence. Mirrors
    /// [`with_mut`] but targets the non-rendering `seq` slot.
    fn with_seq_mut<R>(&self, f: impl FnOnce(&mut Vec<FocusId>) -> R) -> Option<R> {
        let mut seq = self.seq;
        let mut guard = seq.try_write()?;
        Some(f(&mut guard))
    }

    /// Return a snapshot copy of the render-order sequence, or `None` if the scope is
    /// gone. Returns an owned `Vec` so the borrow is released immediately, which lets
    /// callers subsequently take a write lock on either slot without deadlocking.
    fn seq_snapshot(&self) -> Option<Vec<FocusId>> {
        let seq = self.seq;
        let guard = seq.try_read()?;
        Some(guard.clone())
    }

    /// Called by the enclosing `FocusScope` at the top of its component body, before
    /// children get a chance to register themselves.
    pub(crate) fn begin_render(&self) {
        self.with_seq_mut(|s| s.clear());
    }

    /// Called by each descendant `use_focus` on every render, immediately after the
    /// hook slot has been acquired.
    pub(crate) fn note_render_position(&self, id: FocusId) {
        self.with_seq_mut(|s| s.push(id));
    }

    /// Called by [`FocusScopeBoundaryHook::post_component_update`] after the children
    /// subtree has finished updating. Rewrites the entry order to match the captured
    /// render order — but only if the order actually changed, so that a stable tree
    /// doesn't re-trigger a render.
    ///
    /// Implementation note: we read the current order first and compare it to `seq`
    /// *without* taking a write lock on the state. Only if the fast-path comparison
    /// says "different" do we upgrade to a write. `State::write` would otherwise
    /// flag `did_change`, and because `finalize_render` itself runs from inside a
    /// render pass, a perpetual "write → re-render → write" loop would result.
    pub(crate) fn finalize_render(&self) {
        let Some(seq_snapshot) = self.seq_snapshot() else {
            return;
        };
        // Fast path: cheap read-only comparison. If the order already matches, skip
        // the write entirely so the render loop can settle.
        let already_in_order = self.with_ref(|s| {
            s.entries.len() == seq_snapshot.len()
                && s.entries
                    .iter()
                    .zip(seq_snapshot.iter())
                    .all(|(e, id)| e.id == *id)
        });
        if already_in_order == Some(true) {
            return;
        }
        // Real reorder required. One follow-up render is enough to settle, because
        // the next `finalize_render` will observe a stable order and bail out above.
        self.with_mut(|s| {
            s.reorder_to_match(&seq_snapshot);
        });
    }
}

/// Hook installed by every [`FocusScope`] that fires once children have finished updating
/// and rewrites the scope's entry order to match the render-time UI order.
///
/// The work happens in `post_component_update` because that is guaranteed to run *after*
/// `update_children` (which processes the scope's children and runs their `use_focus`
/// calls), and *before* any subsequent render that would read the updated order.
pub(crate) struct FocusScopeBoundaryHook {
    ctx: FocusContext,
}

impl FocusScopeBoundaryHook {
    pub(crate) fn new(ctx: FocusContext) -> Self {
        Self { ctx }
    }
}

impl Hook for FocusScopeBoundaryHook {
    fn post_component_update(&mut self, _updater: &mut ComponentUpdater) {
        self.ctx.finalize_render();
    }
}

/// A `Copy` handle returned by [`UseFocus::use_focus`](crate::hooks::UseFocus::use_focus).
///
/// `FocusHandle` is the user-facing API for an individual focusable element. It is cheap to copy,
/// safe to capture in closures, and exposes the focus state for the element it represents.
#[derive(Clone, Copy)]
pub struct FocusHandle {
    id: FocusId,
    ctx: FocusContext,
}

impl FocusHandle {
    pub(crate) fn new(id: FocusId, ctx: FocusContext) -> Self {
        Self { id, ctx }
    }

    /// Returns `true` if this element currently holds focus in its enclosing scope.
    pub fn is_focused(&self) -> bool {
        self.ctx.is_focused(self.id)
    }

    /// Programmatically move focus to this element.
    pub fn focus(&self) {
        self.ctx.focus(self.id);
    }

    /// Returns the focus id that was assigned to this element.
    pub fn id(&self) -> FocusId {
        self.id
    }
}

/// A `Copy` handle returned by [`UseFocusManager::use_focus_manager`](crate::hooks::UseFocusManager::use_focus_manager).
///
/// `FocusManager` exposes the imperative side of focus control: enable/disable traversal, jump
/// to next/previous, or directly focus a specific id.
#[derive(Clone, Copy)]
pub struct FocusManager {
    ctx: FocusContext,
}

impl FocusManager {
    pub(crate) fn new(ctx: FocusContext) -> Self {
        Self { ctx }
    }

    /// Re-enable sequential focus traversal in this scope.
    pub fn enable(&self) {
        self.ctx.enable();
    }

    /// Disable **sequential** focus traversal in this scope.
    ///
    /// After `disable()`:
    ///
    /// - Tab / Shift+Tab are a no-op (when `handle_keys` is still `true` the keys
    ///   reach the scope but [`focus_next`](Self::focus_next) / [`focus_prev`](Self::focus_prev)
    ///   themselves short-circuit).
    /// - [`focus_next`](Self::focus_next) and [`focus_prev`](Self::focus_prev) — whether
    ///   called programmatically or via keys — are no-ops.
    /// - [`focus(id)`](Self::focus) **still works**. Direct, targeted focus is
    ///   intentionally exempt so "disable" means "freeze the traversal ring" rather
    ///   than "freeze all focus mutation".
    /// - The currently active focus (if any) is retained.
    ///
    /// If you need the stronger "freeze everything" semantics, combine `disable()`
    /// with avoiding calls to `focus(id)` — or drop the scope entirely.
    pub fn disable(&self) {
        self.ctx.disable();
    }

    /// Move focus forward to the next active focusable, wrapping around.
    pub fn focus_next(&self) {
        self.ctx.focus_next();
    }

    /// Move focus backward to the previous active focusable, wrapping around.
    pub fn focus_prev(&self) {
        self.ctx.focus_prev();
    }

    /// Move focus to the given id, if it is currently registered and active.
    pub fn focus(&self, id: FocusId) {
        self.ctx.focus(id);
    }

    /// Clear focus entirely.
    pub fn clear(&self) {
        self.ctx.clear();
    }

    /// The id that currently holds focus, if any.
    pub fn active(&self) -> Option<FocusId> {
        self.ctx.active()
    }

    /// Returns `true` if focus traversal is currently enabled.
    pub fn is_enabled(&self) -> bool {
        self.ctx.is_enabled()
    }
}

/// Helper used by [`FocusScope`](crate::components::FocusScope) to spin up a fresh focus
/// context, install the boundary hook that runs `finalize_render` at the end of each
/// render, and kick off this frame's render-order tracking.
///
/// The three `use_*` calls below MUST stay in this exact order so the hook-slot layout
/// stays stable across renders (rules of hooks).
///
/// Crate-private: this is strictly an implementation detail shared between `focus.rs`
/// and `components::focus_scope`. Users should compose a `FocusScope` in the element
/// tree instead of calling this directly.
pub(crate) fn create_focus_context(hooks: &mut crate::Hooks<'_, '_>) -> FocusContext {
    let state = hooks.use_state(FocusState::default);
    let seq = hooks.use_ref(Vec::<FocusId>::new);
    let ctx = FocusContext::new(state, seq);
    // Install the boundary hook once (subsequent renders return the existing one).
    // We update its `ctx` field every render so that state/seq Copy handles stay fresh
    // even in edge cases where the scope remounts or its backing storage changes
    // — in practice this is a no-op because State and Ref are stable for the life of
    // their owning hook, but being explicit keeps the invariant obvious.
    let boundary = hooks.use_hook(|| FocusScopeBoundaryHook::new(ctx));
    boundary.ctx = ctx;
    // Begin this frame's render-order capture. Children that follow in the element
    // tree will push their ids into `seq` via `note_render_position`.
    ctx.begin_render();
    ctx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registration_order_drives_traversal() {
        let mut s = FocusState::default();
        let a = s.register(false, true);
        let b = s.register(false, true);
        let c = s.register(false, true);
        assert_eq!(s.active, None);
        s.focus_next();
        assert_eq!(s.active, Some(a));
        s.focus_next();
        assert_eq!(s.active, Some(b));
        s.focus_next();
        assert_eq!(s.active, Some(c));
        // wrap around
        s.focus_next();
        assert_eq!(s.active, Some(a));
        // and backwards
        s.focus_prev();
        assert_eq!(s.active, Some(c));
    }

    #[test]
    fn auto_focus_first_wins() {
        let mut s = FocusState::default();
        let a = s.register(true, true);
        let _b = s.register(true, true);
        assert_eq!(s.active, Some(a));
    }

    #[test]
    fn inactive_entries_are_skipped_but_keep_position() {
        let mut s = FocusState::default();
        let a = s.register(false, true);
        let _b = s.register(false, false);
        let c = s.register(false, true);
        s.focus_next();
        assert_eq!(s.active, Some(a));
        s.focus_next();
        assert_eq!(s.active, Some(c));
    }

    #[test]
    fn unregister_promotes_next_active() {
        let mut s = FocusState::default();
        let a = s.register(false, true);
        let b = s.register(false, true);
        s.focus(a);
        s.unregister(a);
        assert_eq!(s.active, Some(b));
    }

    /// Regression for review issue #2: removing the *middle* active entry should advance
    /// to the entry that follows it in the list, not jump back to the head. This must
    /// match `set_entry_active(id, false)` behaviour for consistency.
    #[test]
    fn unregister_middle_promotes_following_entry_not_head() {
        let mut s = FocusState::default();
        let _a = s.register(false, true);
        let b = s.register(false, true);
        let c = s.register(false, true);
        s.focus(b);
        s.unregister(b);
        // After removing b, focus should land on c (the next entry), not a (the head).
        assert_eq!(s.active, Some(c));
    }

    /// Regression for review issue #2: removing the *last* active entry should wrap
    /// to the head, matching the wrap-around semantics of `find_next`.
    #[test]
    fn unregister_tail_wraps_to_head() {
        let mut s = FocusState::default();
        let a = s.register(false, true);
        let _b = s.register(false, true);
        let c = s.register(false, true);
        s.focus(c);
        s.unregister(c);
        assert_eq!(s.active, Some(a));
    }

    /// `unregister` and `set_entry_active(_, false)` must agree on where focus moves when
    /// the active entry is removed/disabled. This test pins the equivalence.
    #[test]
    fn unregister_and_deactivate_agree_on_focus_target() {
        let setup = || {
            let mut s = FocusState::default();
            let _a = s.register(false, true);
            let b = s.register(false, true);
            let c = s.register(false, true);
            let _d = s.register(false, true);
            s.focus(b);
            (s, b, c)
        };

        let (mut via_unregister, b, c) = setup();
        via_unregister.unregister(b);

        let (mut via_deactivate, b, _) = setup();
        via_deactivate.set_entry_active(b, false);

        assert_eq!(via_unregister.active, via_deactivate.active);
        // Sanity: both moved to the entry following `b`, which is `c`.
        assert_eq!(via_unregister.active, Some(c));
    }

    #[test]
    fn deactivating_active_entry_advances_focus() {
        let mut s = FocusState::default();
        let a = s.register(false, true);
        let b = s.register(false, true);
        s.focus(a);
        s.set_entry_active(a, false);
        assert_eq!(s.active, Some(b));
    }

    #[test]
    fn disabled_traversal_is_a_noop() {
        let mut s = FocusState::default();
        let a = s.register(false, true);
        let _b = s.register(false, true);
        s.focus(a);
        s.enabled = false;
        s.focus_next();
        assert_eq!(s.active, Some(a));
    }

    #[test]
    fn focus_specific_active_id() {
        let mut s = FocusState::default();
        let _a = s.register(false, true);
        let b = s.register(false, true);
        s.focus(b);
        assert_eq!(s.active, Some(b));
    }

    #[test]
    fn focus_inactive_id_is_ignored() {
        let mut s = FocusState::default();
        let _a = s.register(false, true);
        let b = s.register(false, false);
        s.focus(b);
        assert_eq!(s.active, None);
    }

    // ----- Regression coverage for review issue #3 (UI order tracking) -----

    /// Inserting a new focusable at the *front* of the UI must put it at the front
    /// of the focus entry list after `reorder_to_match`, regardless of the mount
    /// order that produced the original list.
    #[test]
    fn reorder_to_match_places_front_insertion_at_the_front() {
        let mut s = FocusState::default();
        // Mount order: [b, c] (b and c render first)
        let b = s.register(false, true);
        let c = s.register(false, true);
        // Then a new focusable `a` mounts — it appends because registration is O(append).
        let a = s.register(false, true);
        assert_eq!(
            s.entries.iter().map(|e| e.id).collect::<Vec<_>>(),
            vec![b, c, a],
            "sanity: mount order is append-only"
        );
        // The enclosing scope captured the *render* order as [a, b, c] (a is now at
        // the front of the element tree). Reorder should rewrite entries to match.
        let did_change = s.reorder_to_match(&[a, b, c]);
        assert!(did_change, "order differs, expected a reorder to occur");
        assert_eq!(
            s.entries.iter().map(|e| e.id).collect::<Vec<_>>(),
            vec![a, b, c]
        );
    }

    /// After reorder, `find_next(None)` should return the UI-first element, not the
    /// mount-first element. This is the pure-logic counterpart to "press Tab from
    /// no-focus and see the first visible item focused".
    #[test]
    fn reorder_to_match_changes_find_next_from_none() {
        let mut s = FocusState::default();
        let b = s.register(false, true);
        let c = s.register(false, true);
        let a = s.register(false, true);
        assert_eq!(
            s.find_next(None),
            Some(b),
            "before reorder, first in mount order is b"
        );
        s.reorder_to_match(&[a, b, c]);
        assert_eq!(
            s.find_next(None),
            Some(a),
            "after reorder, first in UI order is a"
        );
    }

    /// The no-op fast path must return `false` and leave state untouched when the
    /// current order already matches the render sequence. This is what prevents a
    /// steady-state render loop (`finalize_render` relies on this to skip the write).
    #[test]
    fn reorder_to_match_is_noop_when_order_already_matches() {
        let mut s = FocusState::default();
        let a = s.register(false, true);
        let b = s.register(false, true);
        let c = s.register(false, true);
        let did_change = s.reorder_to_match(&[a, b, c]);
        assert!(!did_change, "stable order should not trigger a reorder");
        assert_eq!(
            s.entries.iter().map(|e| e.id).collect::<Vec<_>>(),
            vec![a, b, c]
        );
    }

    /// An entry that isn't present in `seq` (e.g. something the enclosing scope
    /// didn't render this frame but also didn't unregister) must NOT disappear —
    /// it gets kept at the end in its original relative order. This is a defensive
    /// guarantee, not a normal code path.
    #[test]
    fn reorder_to_match_preserves_entries_missing_from_seq() {
        let mut s = FocusState::default();
        let a = s.register(false, true);
        let b = s.register(false, true);
        let c = s.register(false, true);
        s.reorder_to_match(&[c, a]); // `b` deliberately omitted
        let ids: Vec<_> = s.entries.iter().map(|e| e.id).collect();
        // `c` and `a` move to the front in the requested order; `b` is appended.
        assert_eq!(ids, vec![c, a, b]);
    }

    /// Robustness: if the defensive "append missing entries at the tail" path ends
    /// up producing the *same* order that was already stored, `reorder_to_match`
    /// must return `false` so `finalize_render` skips the write. Without this
    /// guarantee, a degenerate "seq is a prefix of entries" state would re-trigger
    /// a render every frame forever.
    #[test]
    fn reorder_to_match_is_noop_when_defensive_append_produces_identical_order() {
        let mut s = FocusState::default();
        let a = s.register(false, true);
        let b = s.register(false, true);
        let c = s.register(false, true);
        // seq is a strict prefix of `entries`, so `c` would be re-appended at the
        // tail — producing the exact same order we started with. The fast path
        // alone (element-wise equality of entries vs seq) doesn't catch this,
        // because entries.len() != seq.len(). The two-step comparison does.
        let did_change = s.reorder_to_match(&[a, b]);
        assert!(
            !did_change,
            "defensive append must be reported as no-op when order is unchanged"
        );
        let ids: Vec<_> = s.entries.iter().map(|e| e.id).collect();
        assert_eq!(ids, vec![a, b, c]);
    }

    /// Complement: if the defensive append DOES change the order, `reorder_to_match`
    /// must return `true` and commit the write. Sanity check that the new logic
    /// didn't over-suppress writes.
    #[test]
    fn reorder_to_match_commits_when_defensive_append_changes_order() {
        let mut s = FocusState::default();
        let a = s.register(false, true);
        let b = s.register(false, true);
        let c = s.register(false, true);
        // seq reorders c before a,b, and omits b (which gets appended at the tail).
        let did_change = s.reorder_to_match(&[c, a]);
        assert!(did_change);
        let ids: Vec<_> = s.entries.iter().map(|e| e.id).collect();
        assert_eq!(ids, vec![c, a, b]);
    }

    /// Preserve `is_active` metadata across a reorder. Without this, toggling a
    /// focusable off and then rearranging would silently re-enable it.
    #[test]
    fn reorder_to_match_preserves_is_active_flag() {
        let mut s = FocusState::default();
        let a = s.register(false, true);
        let b = s.register(false, true);
        let c = s.register(false, true);
        s.set_entry_active(b, false);
        assert!(!s.entries.iter().find(|e| e.id == b).unwrap().is_active);
        s.reorder_to_match(&[c, b, a]);
        let b_entry = s.entries.iter().find(|e| e.id == b).unwrap();
        assert!(!b_entry.is_active, "is_active should survive reorder");
    }

    // ----- Regression coverage for disable() semantics (review issue #5) -----

    /// `disable()` freezes sequential traversal only: `focus(id)` must still land.
    /// This pins the documented distinction between "freeze the Tab ring" and
    /// "freeze all focus mutation".
    #[test]
    fn disable_blocks_traversal_but_not_direct_focus() {
        let mut s = FocusState::default();
        let a = s.register(false, true);
        let b = s.register(false, true);
        let _c = s.register(false, true);
        s.focus(a);
        s.enabled = false;

        // Sequential navigation is a no-op while disabled.
        s.focus_next();
        assert_eq!(s.active, Some(a), "focus_next should be blocked");
        s.focus_prev();
        assert_eq!(s.active, Some(a), "focus_prev should be blocked");

        // Direct focus still lands on the targeted id.
        s.focus(b);
        assert_eq!(s.active, Some(b), "direct focus() must still work");

        // Re-enabling restores normal traversal.
        s.enabled = true;
        s.focus_next();
        assert_ne!(s.active, Some(b));
    }
}
