/// Child metadata for [`plan_retained_child_blits`].
///
/// This is the mode-neutral shape of the per-child state used by CC Ink's
/// `renderChildren(...)` contamination guard in `render-node-to-output.ts`.
/// It is intentionally independent of iocraft's component tree and canvas.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetainedChildBlitInput<K> {
    /// Caller-owned child identifier returned in the corresponding decision.
    pub key: K,
    /// Whether the child was dirty before rendering this frame.
    pub dirty: bool,
    /// Whether the child's own overflow clips on both axes.
    pub clips_both_axes: bool,
    /// Whether the child is absolutely positioned.
    pub absolute: bool,
    /// Whether the child fills its layout rectangle opaquely.
    pub opaque: bool,
    /// Whether the child has an explicit background fill.
    pub has_background: bool,
}

/// Per-child retained-blit decision returned by [`plan_retained_child_blits`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetainedChildBlitDecision<K> {
    /// Caller-owned child identifier from [`RetainedChildBlitInput::key`].
    pub key: K,
    /// Whether the caller may pass the previous retained screen/canvas to this
    /// child subtree for descendant blits.
    pub allow_previous_screen: bool,
    /// Whether this child must skip its own direct node-level blit even if its
    /// cached layout matches.
    pub skip_self_blit: bool,
}

/// Plans sibling retained-blit contamination guards.
///
/// This mirrors CC Ink's `renderChildren(...)` sibling-overflow guard in a
/// Rust-native, optimization-only helper. A dirty unclipped child can paint
/// outside its layout bounds, so later siblings must not blit from the previous
/// screen. A dirty clipped child is safe for ordinary later siblings, but a
/// later non-opaque absolute sibling overlapping the clipped region must skip
/// its own direct blit to avoid restoring stale cells. The dirty child itself
/// still receives the previous screen unless a child was removed before the
/// frame; its own direct blit should fail because it is dirty, but clean
/// descendants may still benefit.
///
/// The helper computes metadata only: it does not mutate a cache, inspect
/// layout nodes, draw, write terminal output, or change screen mode.
pub fn plan_retained_child_blits<K>(
    has_removed_child: bool,
    children: impl IntoIterator<Item = RetainedChildBlitInput<K>>,
) -> Vec<RetainedChildBlitDecision<K>> {
    let mut seen_dirty_unclipped = false;
    let mut seen_dirty_clipped = false;
    let mut decisions = Vec::new();

    for child in children {
        let allow_previous_screen = !has_removed_child && !seen_dirty_unclipped;
        let skip_self_blit =
            seen_dirty_clipped && child.absolute && !child.opaque && !child.has_background;

        let dirty = child.dirty;
        let clips_both_axes = child.clips_both_axes;
        let absolute = child.absolute;
        decisions.push(RetainedChildBlitDecision {
            key: child.key,
            allow_previous_screen,
            skip_self_blit,
        });

        if dirty && !seen_dirty_unclipped {
            if !clips_both_axes || absolute {
                seen_dirty_unclipped = true;
            } else {
                seen_dirty_clipped = true;
            }
        }
    }

    decisions
}
