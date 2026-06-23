use super::*;

/// Input for [`plan_retained_node_render`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetainedNodeRenderInput {
    /// Current absolute layout bounds for the node.
    pub current_layout: CachedLayoutBounds,
    /// Previous cached layout bounds for the node, if present.
    pub cached_layout: Option<CachedLayoutBounds>,
    /// Whether the node's content/subtree is dirty this frame.
    pub dirty: bool,
    /// Whether the caller must force descent instead of direct self blit.
    pub skip_self_blit: bool,
    /// Whether render-time scroll draining is pending for this node.
    pub pending_scroll_delta: bool,
    /// Whether a trustworthy previous retained screen/canvas is available.
    pub previous_screen_available: bool,
    /// Whether the node is currently hidden/display:none.
    pub hidden: bool,
    /// Whether the node is absolutely positioned.
    pub absolute: bool,
    /// Pending clear regions for removed children under this node.
    pub pending_clears: Vec<CachedClearRegion>,
}

/// High-level retained node render decision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RetainedNodeRenderAction {
    /// Restore this node's cached rectangle from the previous retained buffer.
    Blit,
    /// Descend and render the node/subtree normally.
    Render,
    /// Skip rendering because the node is hidden.
    Hidden,
}

/// Plan returned by [`plan_retained_node_render`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetainedNodeRenderPlan {
    /// High-level action for this node.
    pub action: RetainedNodeRenderAction,
    /// Region to blit from the previous retained buffer when [`Self::action`] is
    /// [`RetainedNodeRenderAction::Blit`].
    pub blit_region: Option<CachedClearRegion>,
    /// Cached old region to clear before re-rendering or after becoming hidden.
    pub clear_old_region: Option<CachedClearRegion>,
    /// Whether [`Self::clear_old_region`] belongs to an absolute node and should
    /// poison unsafe sibling blits.
    pub clear_old_from_absolute: bool,
    /// Pending clear regions for removed children.
    pub pending_clear_regions: Vec<CachedClearRegion>,
    /// Whether pending child clears were present.
    pub has_removed_child: bool,
    /// Whether this node's layout changed relative to the cache.
    pub position_changed: bool,
    /// Whether the frame should use a broad layout-shift damage backstop.
    pub layout_shifted: bool,
    /// Whether a hidden node's subtree cache should be dropped.
    pub drop_subtree_cache: bool,
    /// Whether an absolute node's current/cached rect should be recorded for
    /// later scroll/escaping-overlay repairs.
    pub record_absolute_rect: bool,
}

/// Plans CC Ink-style retained node blit/clear behavior for a custom renderer.
///
/// This helper captures the first-stage decision in CC Ink
/// `renderNodeToOutput(...)`: clean unchanged nodes can direct-blit from the
/// previous screen, dirty or moved nodes clear their old cached rectangle before
/// rendering, removed children contribute pending clears and a layout-shift
/// backstop, and hidden dirty nodes clear/drop their subtree cache. It is an
/// explicit Rust helper, not a hidden global renderer policy: callers own node
/// keys, cache mutation, subtree traversal, canvas writes, and terminal output.
pub fn plan_retained_node_render(input: RetainedNodeRenderInput) -> RetainedNodeRenderPlan {
    let pending_clear_regions = input.pending_clears;
    let has_removed_child = !pending_clear_regions.is_empty();

    if input.hidden {
        let clear_old_region = input
            .dirty
            .then_some(input.cached_layout)
            .flatten()
            .map(Into::into);
        let drop_subtree_cache = clear_old_region.is_some();
        return RetainedNodeRenderPlan {
            action: RetainedNodeRenderAction::Hidden,
            blit_region: None,
            clear_old_region,
            clear_old_from_absolute: input.absolute && clear_old_region.is_some(),
            pending_clear_regions: Vec::new(),
            has_removed_child: false,
            position_changed: false,
            layout_shifted: drop_subtree_cache,
            drop_subtree_cache,
            record_absolute_rect: false,
        };
    }

    let can_blit = !input.dirty
        && !input.skip_self_blit
        && !input.pending_scroll_delta
        && input.previous_screen_available
        && input.cached_layout == Some(input.current_layout);
    if can_blit {
        return RetainedNodeRenderPlan {
            action: RetainedNodeRenderAction::Blit,
            blit_region: Some(input.current_layout.into()),
            clear_old_region: None,
            clear_old_from_absolute: false,
            pending_clear_regions,
            has_removed_child,
            position_changed: false,
            layout_shifted: has_removed_child,
            drop_subtree_cache: false,
            record_absolute_rect: input.absolute,
        };
    }

    let position_changed = input
        .cached_layout
        .is_some_and(|cached| cached != input.current_layout);
    let clear_old_region = input
        .cached_layout
        .filter(|_| input.dirty || position_changed)
        .map(Into::into);
    let layout_shifted = position_changed || has_removed_child;

    RetainedNodeRenderPlan {
        action: RetainedNodeRenderAction::Render,
        blit_region: None,
        clear_old_region,
        clear_old_from_absolute: input.absolute && clear_old_region.is_some(),
        pending_clear_regions,
        has_removed_child,
        position_changed,
        layout_shifted,
        drop_subtree_cache: false,
        record_absolute_rect: false,
    }
}

/// Canvas mutations applied from a retained node render plan.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RetainedNodeCanvasApplication {
    /// Region restored from the previous retained canvas for clean blit plans.
    pub blitted_region: Option<DamageRegion>,
    /// Old cached region cleared before a dirty/moved/hidden node is rendered.
    pub cleared_old_region: Option<DamageRegion>,
    /// Pending child clear regions that were visible and cleared.
    pub pending_clear_regions: Vec<DamageRegion>,
}

/// Applies the canvas-side skeleton of a retained node render plan.
///
/// This is an opt-in bridge between [`plan_retained_node_render`] and a custom
/// retained renderer. It performs only the mechanical canvas operations that CC
/// Ink's renderer does around a node decision: clean nodes blit their cached
/// rectangle from the previous retained canvas, dirty/moved/hidden nodes clear
/// their old cached rectangle, and removed children clear their pending regions.
/// The caller is still responsible for traversing children, drawing dirty
/// content after clears, committing cache state, and writing terminal patches.
pub fn apply_retained_node_render_plan_to_canvas(
    next: &mut Canvas,
    previous: &Canvas,
    plan: &RetainedNodeRenderPlan,
) -> RetainedNodeCanvasApplication {
    let mut application = RetainedNodeCanvasApplication::default();

    if let Some(region) = plan
        .clear_old_region
        .and_then(|region| region.clipped_to_canvas(next.width(), next.height()))
    {
        next.clear_region(region.x, region.y, region.width, region.height);
        application.cleared_old_region = Some(region);
    }

    if plan.action == RetainedNodeRenderAction::Blit {
        if let Some(region) = plan
            .blit_region
            .and_then(|region| region.clipped_to_canvas(next.width(), next.height()))
        {
            next.blit_region_from(previous, region.x, region.y, region.width, region.height);
            application.blitted_region = Some(region);
        }
    }

    for region in &plan.pending_clear_regions {
        if let Some(region) = region.clipped_to_canvas(next.width(), next.height()) {
            next.clear_region(region.x, region.y, region.width, region.height);
            application.pending_clear_regions.push(region);
        }
    }

    application
}
