use super::*;

/// Cached layout bounds for a rendered node.
///
/// This is the Rust counterpart to CC Ink's `node-cache.ts` `CachedLayout`:
/// custom retained renderers can store a node's previous absolute bounds and
/// decide whether a clean subtree may be restored via a screen-buffer blit.
/// `top` is the parent-local vertical layout position used by scroll viewport
/// culling optimizations; it may be omitted by renderers that do not need it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CachedLayoutBounds {
    /// Absolute X position in terminal cells.
    pub x: i32,
    /// Absolute Y position in terminal rows.
    pub y: i32,
    /// Width in terminal cells.
    pub width: i32,
    /// Height in terminal rows.
    pub height: i32,
    /// Optional parent-local top position.
    pub top: Option<i32>,
}

/// Cached rectangle used by [`RendererNodeCache`] pending clears.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CachedClearRegion {
    /// Absolute X position in terminal cells.
    pub x: i32,
    /// Absolute Y position in terminal rows.
    pub y: i32,
    /// Width in terminal cells.
    pub width: i32,
    /// Height in terminal rows.
    pub height: i32,
}

impl From<CachedLayoutBounds> for CachedClearRegion {
    fn from(value: CachedLayoutBounds) -> Self {
        Self {
            x: value.x,
            y: value.y,
            width: value.width,
            height: value.height,
        }
    }
}

impl CachedClearRegion {
    /// Clips this possibly-negative cached rectangle to a canvas and returns a
    /// [`DamageRegion`] suitable for [`Canvas::clear_region`] or
    /// [`Canvas::blit_region_from_excluding_clears`].
    ///
    /// This mirrors CC Ink `output.ts` clear-region clamping: absolute overlays
    /// can have negative coordinates, but retained buffers only track the
    /// visible intersection.
    pub fn clipped_to_canvas(self, width: usize, height: usize) -> Option<DamageRegion> {
        if self.width <= 0 || self.height <= 0 || width == 0 || height == 0 {
            return None;
        }

        let left = self.x.max(0) as usize;
        let top = self.y.max(0) as usize;
        let right = self.x.saturating_add(self.width).max(0) as usize;
        let bottom = self.y.saturating_add(self.height).max(0) as usize;
        let right = right.min(width);
        let bottom = bottom.min(height);
        if right <= left || bottom <= top {
            return None;
        }

        Some(DamageRegion {
            x: left,
            y: top,
            width: right - left,
            height: bottom - top,
        })
    }
}

/// Stable retained-renderer node identity with an explicit generation.
///
/// CC Ink can key caches by mutable DOM object identity (`WeakMap<DOMElement,
/// ...>`), so a removed component cannot accidentally reuse an old layout cache
/// unless it is the same DOM object. Rust retained renderers often start from a
/// caller-owned logical key (`row-42`, component id, etc.); adding a generation
/// makes key reuse/remounts explicit and prevents stale blits after removal.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct RendererStableNodeId<K> {
    /// Caller-owned logical key.
    pub key: K,
    /// Monotonic generation for this key.
    pub generation: u64,
}

/// Tracks generations for retained renderer node identifiers.
///
/// This helper is opt-in and mode-neutral. It does not inspect iocraft's
/// component tree or mutate caches by itself; callers can use the returned
/// [`RendererStableNodeId`] values as keys for [`RendererNodeCache`],
/// [`RendererDirtyTree`], or [`RendererRetainedTreeState`].
#[derive(Clone, Debug)]
pub struct RendererNodeGenerationState<K> {
    generations: HashMap<K, u64>,
}

impl<K> Default for RendererNodeGenerationState<K> {
    fn default() -> Self {
        Self {
            generations: HashMap::new(),
        }
    }
}

impl<K> RendererNodeGenerationState<K>
where
    K: Eq + Hash + Clone,
{
    /// Creates an empty generation tracker.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the current stable id for `key`, inserting generation `0` if needed.
    pub fn current_id(&mut self, key: K) -> RendererStableNodeId<K> {
        let generation = *self.generations.entry(key.clone()).or_insert(0);
        RendererStableNodeId { key, generation }
    }

    /// Returns the current stable id for `key` without inserting it.
    pub fn id(&self, key: &K) -> Option<RendererStableNodeId<K>> {
        self.generations
            .get(key)
            .copied()
            .map(|generation| RendererStableNodeId {
                key: key.clone(),
                generation,
            })
    }

    /// Bumps `key` to a new generation and returns the new stable id.
    pub fn remount(&mut self, key: K) -> RendererStableNodeId<K> {
        let generation = self
            .generations
            .entry(key.clone())
            .and_modify(|generation| *generation = generation.saturating_add(1))
            .or_insert(0);
        RendererStableNodeId {
            key,
            generation: *generation,
        }
    }

    /// Marks `key` removed by bumping its next generation.
    ///
    /// The returned id is the generation that was live before removal, suitable
    /// for dropping retained cache entries or queuing clears. Calling
    /// [`Self::current_id`] for the same logical key afterwards returns a fresh
    /// generation that will not collide with old cache keys.
    pub fn remove(&mut self, key: &K) -> Option<RendererStableNodeId<K>> {
        let generation = self.generations.get_mut(key)?;
        let removed = RendererStableNodeId {
            key: key.clone(),
            generation: *generation,
        };
        *generation = generation.saturating_add(1);
        Some(removed)
    }

    /// Bumps every tracked logical key that is not in `keys`.
    ///
    /// Returns the stable ids that were live before removal. This is useful for
    /// list diffing: remove those ids from retained caches, then ask
    /// [`Self::current_id`] for surviving/new keys. Generation tombstones remain
    /// tracked so a later reinserted logical key cannot collide with old caches.
    pub fn bump_unretained_keys<I>(&mut self, keys: I) -> Vec<RendererStableNodeId<K>>
    where
        I: IntoIterator<Item = K>,
    {
        let keep = keys.into_iter().collect::<HashSet<_>>();
        let removed_keys = self
            .generations
            .keys()
            .filter(|key| !keep.contains(*key))
            .cloned()
            .collect::<Vec<_>>();
        removed_keys
            .into_iter()
            .filter_map(|key| self.remove(&key))
            .collect()
    }

    /// Number of logical keys tracked.
    pub fn len(&self) -> usize {
        self.generations.len()
    }

    /// Whether no logical keys are tracked.
    pub fn is_empty(&self) -> bool {
        self.generations.is_empty()
    }

    /// Clears all generations.
    pub fn clear(&mut self) {
        self.generations.clear();
    }
}

/// Mode-neutral node layout/pending-clear cache for custom retained renderers.
///
/// This mirrors CC Ink's `node-cache.ts` at the framework-utility level: layout
/// bounds are keyed by the caller's node identifier, removed children can queue
/// clear regions for their parent, and clearing an absolute-positioned node sets
/// a one-shot contamination flag so the next frame can disable unsafe blits from
/// the previous screen. It does not draw, write terminal output, or change screen
/// mode; callers decide how to apply blits/clears to their own retained buffer.
#[derive(Clone, Debug)]
pub struct RendererNodeCache<K> {
    layouts: HashMap<K, CachedLayoutBounds>,
    pending_clears: HashMap<K, Vec<CachedClearRegion>>,
    absolute_node_removed: bool,
}

impl<K> Default for RendererNodeCache<K> {
    fn default() -> Self {
        Self {
            layouts: HashMap::new(),
            pending_clears: HashMap::new(),
            absolute_node_removed: false,
        }
    }
}

impl<K> RendererNodeCache<K>
where
    K: Eq + Hash,
{
    /// Creates an empty renderer node cache.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns cached layout bounds for `node`, if present.
    pub fn layout(&self, node: &K) -> Option<CachedLayoutBounds> {
        self.layouts.get(node).copied()
    }

    /// Stores layout bounds for `node`.
    pub fn set_layout(&mut self, node: K, layout: CachedLayoutBounds) {
        self.layouts.insert(node, layout);
    }

    /// Removes and returns cached layout bounds for `node`.
    pub fn remove_layout(&mut self, node: &K) -> Option<CachedLayoutBounds> {
        self.layouts.remove(node)
    }

    /// Drops cached layout and pending-clear metadata for a subtree.
    ///
    /// This mirrors CC Ink `render-node-to-output.ts` `dropSubtreeCache(...)`,
    /// used when hidden/culled descendants should not later blit or clear stale
    /// coordinates after re-entering the retained render tree. The caller owns
    /// the tree structure and supplies `children`, keeping this helper
    /// mode-neutral and independent of iocraft's internal component tree.
    pub fn remove_subtree<I, F>(&mut self, root: &K, mut children: F)
    where
        K: Clone,
        F: FnMut(&K) -> I,
        I: IntoIterator<Item = K>,
    {
        fn walk<K, I, F>(cache: &mut RendererNodeCache<K>, node: &K, children: &mut F)
        where
            K: Eq + Hash + Clone,
            F: FnMut(&K) -> I,
            I: IntoIterator<Item = K>,
        {
            cache.layouts.remove(node);
            cache.pending_clears.remove(node);
            let kids = children(node).into_iter().collect::<Vec<_>>();
            for child in kids {
                walk(cache, &child, children);
            }
        }

        walk(self, root, &mut children);
    }

    /// Returns whether a clean node with `layout` can be blitted from the
    /// previous retained buffer according to the cached bounds.
    pub fn can_blit(&self, node: &K, layout: CachedLayoutBounds) -> bool {
        self.layout(node) == Some(layout)
    }

    /// Queues a clear region on `parent` for a removed or hidden child.
    ///
    /// When `is_absolute` is true, [`Self::consume_absolute_removed_flag`] will
    /// return true once. This matches CC Ink's absolute-overlay safeguard: a
    /// removed absolute node may have painted over unrelated siblings, so the
    /// next frame should avoid prev-screen blits that could restore stale pixels.
    pub fn add_pending_clear(&mut self, parent: K, region: CachedClearRegion, is_absolute: bool) {
        self.pending_clears.entry(parent).or_default().push(region);
        if is_absolute {
            self.absolute_node_removed = true;
        }
    }

    /// Takes all pending clear regions queued for `parent`.
    pub fn take_pending_clears(&mut self, parent: &K) -> Vec<CachedClearRegion> {
        self.pending_clears.remove(parent).unwrap_or_default()
    }

    /// Consumes the one-shot "absolute node removed" contamination flag.
    pub fn consume_absolute_removed_flag(&mut self) -> bool {
        let had = self.absolute_node_removed;
        self.absolute_node_removed = false;
        had
    }

    /// Clears all cached layouts, pending clears, and contamination flags.
    pub fn clear(&mut self) {
        self.layouts.clear();
        self.pending_clears.clear();
        self.absolute_node_removed = false;
    }
}

/// Input for [`RendererRetainedFrameState::plan_node`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetainedFrameNodeInput<K> {
    /// Caller-owned stable node identifier.
    pub key: K,
    /// Current absolute layout bounds for the node.
    pub current_layout: CachedLayoutBounds,
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
}

/// Node plan produced by [`RendererRetainedFrameState::plan_node`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetainedFrameNodePlan<K> {
    /// Caller-owned stable node identifier.
    pub key: K,
    /// Current absolute layout bounds used for the decision.
    pub current_layout: CachedLayoutBounds,
    /// Retained-render decision and clear/blit metadata.
    pub plan: RetainedNodeRenderPlan,
}

/// Stateful opt-in owner for retained renderer node-cache planning.
///
/// This is a Rust-native integration layer around [`RendererNodeCache`] and
/// [`plan_retained_node_render`]. It mirrors the CC Ink renderer's automatic
/// per-frame bookkeeping without becoming iocraft's default render path:
/// callers still own stable node IDs, dirty invalidation, traversal order,
/// actual canvas blits/clears, and terminal writes. The state consumes pending
/// child clears, tracks layout-shift damage backstops, exposes absolute-clear
/// contamination, and commits cached bounds when a node decision has been
/// applied.
#[derive(Clone, Debug)]
pub struct RendererRetainedFrameState<K> {
    cache: RendererNodeCache<K>,
    layout_shifted: bool,
    absolute_clear_this_frame: bool,
    absolute_removed_at_frame_start: bool,
}

impl<K> Default for RendererRetainedFrameState<K> {
    fn default() -> Self {
        Self {
            cache: RendererNodeCache::default(),
            layout_shifted: false,
            absolute_clear_this_frame: false,
            absolute_removed_at_frame_start: false,
        }
    }
}

impl<K> RendererRetainedFrameState<K>
where
    K: Eq + Hash,
{
    /// Creates an empty retained-frame state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the underlying node cache.
    pub fn cache(&self) -> &RendererNodeCache<K> {
        &self.cache
    }

    /// Returns the underlying node cache mutably.
    pub fn cache_mut(&mut self) -> &mut RendererNodeCache<K> {
        &mut self.cache
    }

    /// Starts a new retained planning frame.
    ///
    /// This resets one-frame layout/absolute-clear flags and consumes the
    /// previous frame's queued absolute-removal contamination flag. The return
    /// value tells callers whether they should disable unsafe prev-screen blits
    /// because an absolute node was removed before this frame.
    pub fn begin_frame(&mut self) -> bool {
        self.layout_shifted = false;
        self.absolute_clear_this_frame = false;
        self.absolute_removed_at_frame_start = self.cache.consume_absolute_removed_flag();
        self.absolute_removed_at_frame_start
    }

    /// Returns whether any planned node marked the frame as layout-shifted.
    pub fn layout_shifted(&self) -> bool {
        self.layout_shifted
    }

    /// Returns whether an absolute clear happened while planning this frame.
    pub fn absolute_clear_this_frame(&self) -> bool {
        self.absolute_clear_this_frame
    }

    /// Returns the absolute-removal contamination consumed at frame start.
    pub fn absolute_removed_at_frame_start(&self) -> bool {
        self.absolute_removed_at_frame_start
    }

    /// Queues a pending clear for a removed or hidden child.
    pub fn queue_child_clear(&mut self, parent: K, region: CachedClearRegion, is_absolute: bool) {
        self.cache.add_pending_clear(parent, region, is_absolute);
    }

    /// Plans retained rendering for one node and consumes its pending child clears.
    pub fn plan_node(&mut self, input: RetainedFrameNodeInput<K>) -> RetainedFrameNodePlan<K>
    where
        K: Clone,
    {
        let cached_layout = self.cache.layout(&input.key);
        let pending_clears = self.cache.take_pending_clears(&input.key);
        let plan = plan_retained_node_render(RetainedNodeRenderInput {
            current_layout: input.current_layout,
            cached_layout,
            dirty: input.dirty,
            skip_self_blit: input.skip_self_blit,
            pending_scroll_delta: input.pending_scroll_delta,
            previous_screen_available: input.previous_screen_available,
            hidden: input.hidden,
            absolute: input.absolute,
            pending_clears,
        });
        self.layout_shifted |= plan.layout_shifted;
        self.absolute_clear_this_frame |= plan.clear_old_from_absolute;
        RetainedFrameNodePlan {
            key: input.key,
            current_layout: input.current_layout,
            plan,
        }
    }

    /// Commits a node plan after the caller has applied its blit/clear/render work.
    ///
    /// Hidden nodes remove their own cached layout; visible nodes store their
    /// latest bounds. If [`RetainedNodeRenderPlan::drop_subtree_cache`] is true,
    /// callers that track descendants should prefer
    /// [`Self::commit_node_plan_with_children`] so descendant layouts are dropped
    /// at the same time.
    pub fn commit_node_plan(&mut self, plan: &RetainedFrameNodePlan<K>)
    where
        K: Clone,
    {
        if plan.plan.action == RetainedNodeRenderAction::Hidden {
            self.cache.remove_layout(&plan.key);
        } else {
            self.cache.set_layout(plan.key.clone(), plan.current_layout);
        }
    }

    /// Commits a node plan and drops descendant cache when requested by the plan.
    pub fn commit_node_plan_with_children<I, F>(
        &mut self,
        plan: &RetainedFrameNodePlan<K>,
        mut children: F,
    ) where
        K: Clone,
        F: FnMut(&K) -> I,
        I: IntoIterator<Item = K>,
    {
        if plan.plan.drop_subtree_cache {
            self.cache.remove_subtree(&plan.key, &mut children);
        } else {
            self.commit_node_plan(plan);
        }
    }

    /// Clears all retained cache and frame flags.
    pub fn clear(&mut self) {
        self.cache.clear();
        self.layout_shifted = false;
        self.absolute_clear_this_frame = false;
        self.absolute_removed_at_frame_start = false;
    }
}
