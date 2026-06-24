use super::*;

/// Explicit renderer optimization mode.
///
/// The default is [`Self::Baseline`], which preserves iocraft's existing full
/// traversal/render behavior. Retained rendering primitives are only enabled
/// when callers opt in with [`Self::Retained`]; this keeps benchmark and bug
/// bisect paths available while exposing a stable configuration boundary for
/// dirty-tree and clean-blit experiments.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RendererOptimizationMode {
    /// Preserve the existing renderer path.
    #[default]
    Baseline,
    /// Enable explicit retained-renderer planning helpers.
    Retained(RendererRetainedOptimizationConfig),
}

impl RendererOptimizationMode {
    /// Returns a retained mode with safe clean-blit guards enabled.
    pub fn retained_with_safe_clean_blit() -> Self {
        Self::Retained(RendererRetainedOptimizationConfig::safe_clean_blit())
    }

    /// Returns whether this mode keeps the baseline renderer path.
    pub fn is_baseline(self) -> bool {
        matches!(self, Self::Baseline)
    }

    /// Returns the retained configuration, if retained mode is enabled.
    pub fn retained_config(self) -> Option<RendererRetainedOptimizationConfig> {
        match self {
            Self::Baseline => None,
            Self::Retained(config) => Some(config),
        }
    }

    /// Returns whether dirty-tree invalidation is enabled.
    pub fn dirty_tree_enabled(self) -> bool {
        self.retained_config()
            .is_some_and(|config| config.dirty_tree)
    }

    /// Returns whether a clean subtree blit is allowed under the supplied guard.
    pub fn allows_clean_blit(self, guard: RendererCleanBlitGuard) -> bool {
        self.retained_config()
            .is_some_and(|config| config.allows_clean_blit(guard))
    }
}

/// Retained renderer optimization switches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RendererRetainedOptimizationConfig {
    /// Whether callers should use [`RendererDirtyTree`] for ancestor invalidation.
    pub dirty_tree: bool,
    /// Clean subtree blit policy.
    pub clean_blit: RendererCleanBlitMode,
}

impl Default for RendererRetainedOptimizationConfig {
    fn default() -> Self {
        Self {
            dirty_tree: true,
            clean_blit: RendererCleanBlitMode::Safe,
        }
    }
}

impl RendererRetainedOptimizationConfig {
    /// Enables retained dirty-tree planning without clean blits.
    pub fn dirty_tree_only() -> Self {
        Self {
            dirty_tree: true,
            clean_blit: RendererCleanBlitMode::Disabled,
        }
    }

    /// Enables retained dirty-tree planning and safe clean-blit guards.
    pub fn safe_clean_blit() -> Self {
        Self::default()
    }

    /// Returns whether this retained config allows a clean subtree blit.
    pub fn allows_clean_blit(self, guard: RendererCleanBlitGuard) -> bool {
        self.clean_blit == RendererCleanBlitMode::Safe && guard.is_safe_for_clean_blit()
    }
}

/// Clean subtree blit policy for retained renderer experiments.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RendererCleanBlitMode {
    /// Do not perform clean subtree blits.
    #[default]
    Disabled,
    /// Allow clean blits only when all explicit safety guards pass.
    Safe,
}

/// Explicit guard inputs for a clean subtree blit decision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RendererCleanBlitGuard {
    /// A trustworthy previous retained screen/canvas is available.
    pub previous_screen_available: bool,
    /// The node/subtree is dirty this frame.
    pub node_dirty: bool,
    /// The caller requested descent instead of self blit.
    pub skip_self_blit: bool,
    /// Render-time scroll draining is pending for the node.
    pub pending_scroll_delta: bool,
    /// The node is hidden/display:none.
    pub hidden: bool,
    /// Layout shifted since the previous retained frame.
    pub layout_shifted: bool,
    /// An absolute-positioned clear happened in this frame.
    pub absolute_clear_this_frame: bool,
    /// An absolute-positioned node removal contaminated previous-frame pixels.
    pub absolute_removed_at_frame_start: bool,
    /// Current or previous retained canvas carries damage metadata.
    pub current_or_previous_damage: bool,
    /// The stable node generation still matches the cached retained entry.
    pub stable_generation: bool,
}

impl RendererCleanBlitGuard {
    /// Returns a fully safe clean-blit guard.
    pub fn safe() -> Self {
        Self {
            previous_screen_available: true,
            node_dirty: false,
            skip_self_blit: false,
            pending_scroll_delta: false,
            hidden: false,
            layout_shifted: false,
            absolute_clear_this_frame: false,
            absolute_removed_at_frame_start: false,
            current_or_previous_damage: false,
            stable_generation: true,
        }
    }

    /// Returns whether all explicit guard conditions allow a clean blit.
    pub fn is_safe_for_clean_blit(self) -> bool {
        self.previous_screen_available
            && !self.node_dirty
            && !self.skip_self_blit
            && !self.pending_scroll_delta
            && !self.hidden
            && !self.layout_shifted
            && !self.absolute_clear_this_frame
            && !self.absolute_removed_at_frame_start
            && !self.current_or_previous_damage
            && self.stable_generation
    }
}

impl Default for RendererCleanBlitGuard {
    fn default() -> Self {
        Self::safe()
    }
}

/// Input for [`RendererRetainedTreeState::plan_node`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetainedTreeNodeInput<K> {
    /// Caller-owned stable node identifier.
    pub key: K,
    /// Current absolute layout bounds for the node.
    pub current_layout: CachedLayoutBounds,
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

/// Opt-in retained renderer state that combines dirty invalidation and node cache.
///
/// This is the closest Rust-first building block to CC Ink's automatic
/// DOM-element `dirty` + `nodeCache` pipeline. It keeps the default iocraft
/// renderer unchanged, but custom retained renderers can use one explicit state
/// owner for tree edges, ancestor dirty propagation, cached layout bounds,
/// removed-child pending clears, and per-frame retained blit planning.
#[derive(Clone, Debug)]
pub struct RendererRetainedTreeState<K> {
    dirty_tree: RendererDirtyTree<K>,
    frame_state: RendererRetainedFrameState<K>,
}

impl<K> Default for RendererRetainedTreeState<K> {
    fn default() -> Self {
        Self {
            dirty_tree: RendererDirtyTree::default(),
            frame_state: RendererRetainedFrameState::default(),
        }
    }
}

impl<K> RendererRetainedTreeState<K>
where
    K: Eq + Hash + Clone,
{
    /// Creates an empty retained tree state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the dirty tree.
    pub fn dirty_tree(&self) -> &RendererDirtyTree<K> {
        &self.dirty_tree
    }

    /// Returns the dirty tree mutably.
    pub fn dirty_tree_mut(&mut self) -> &mut RendererDirtyTree<K> {
        &mut self.dirty_tree
    }

    /// Returns the retained frame/cache state.
    pub fn frame_state(&self) -> &RendererRetainedFrameState<K> {
        &self.frame_state
    }

    /// Returns the retained frame/cache state mutably.
    pub fn frame_state_mut(&mut self) -> &mut RendererRetainedFrameState<K> {
        &mut self.frame_state
    }

    /// Registers a root node with no parent.
    pub fn register_root(&mut self, node: K) {
        self.dirty_tree.register_root(node);
    }

    /// Attaches `node` under `parent` and dirties the parent, because the child
    /// list/layout may have changed.
    pub fn attach(&mut self, node: K, parent: K) {
        self.dirty_tree.attach(node, parent.clone());
        self.dirty_tree.mark_dirty(&parent, false);
    }

    /// Marks `node` and all ancestors dirty.
    pub fn mark_dirty(&mut self, node: &K, measure_dirty: bool) {
        self.dirty_tree.mark_dirty(node, measure_dirty);
    }

    /// Returns whether `node` is dirty.
    pub fn is_dirty(&self, node: &K) -> bool {
        self.dirty_tree.is_dirty(node)
    }

    /// Starts a retained planning frame.
    pub fn begin_frame(&mut self) -> bool {
        self.frame_state.begin_frame()
    }

    /// Plans one node using dirtiness from [`RendererDirtyTree`].
    pub fn plan_node(&mut self, input: RetainedTreeNodeInput<K>) -> RetainedFrameNodePlan<K> {
        let dirty = self.dirty_tree.is_dirty(&input.key);
        self.frame_state.plan_node(RetainedFrameNodeInput {
            key: input.key,
            current_layout: input.current_layout,
            dirty,
            skip_self_blit: input.skip_self_blit,
            pending_scroll_delta: input.pending_scroll_delta,
            previous_screen_available: input.previous_screen_available,
            hidden: input.hidden,
            absolute: input.absolute,
        })
    }

    /// Commits a node plan and clears that node's frame dirtiness.
    pub fn commit_node_plan(&mut self, plan: &RetainedFrameNodePlan<K>) {
        self.frame_state.commit_node_plan(plan);
        self.dirty_tree.clear_node(&plan.key);
    }

    /// Commits a node plan, dropping descendant cached layouts when requested.
    pub fn commit_node_plan_with_children(&mut self, plan: &RetainedFrameNodePlan<K>) {
        let children = self.dirty_tree.children.clone();
        let dirty_nodes_to_clear = plan
            .plan
            .drop_subtree_cache
            .then(|| self.dirty_tree.subtree_nodes(&plan.key));
        self.frame_state
            .commit_node_plan_with_children(plan, |key| {
                children
                    .get(key)
                    .map(|children| children.iter().cloned().collect::<Vec<_>>())
                    .unwrap_or_default()
            });
        if let Some(nodes) = dirty_nodes_to_clear {
            for node in nodes {
                self.dirty_tree.clear_node(&node);
            }
        } else {
            self.dirty_tree.clear_node(&plan.key);
        }
    }

    /// Removes a subtree from the dirty tree and retained cache.
    ///
    /// If the removed root had cached layout and a parent, a pending clear is
    /// queued on the parent. `is_absolute` should reflect the removed root's
    /// positioning; absolute removals poison unsafe prev-screen blits at the
    /// start of the next frame, matching CC Ink's `absoluteNodeRemoved` guard.
    pub fn remove_subtree(&mut self, root: &K, is_absolute: bool) -> Vec<K> {
        let parent = self.dirty_tree.parent(root).cloned();
        let cached_layout = self.frame_state.cache().layout(root);
        let children = self.dirty_tree.children.clone();
        self.frame_state.cache_mut().remove_subtree(root, |key| {
            children
                .get(key)
                .map(|children| children.iter().cloned().collect::<Vec<_>>())
                .unwrap_or_default()
        });
        let removed = self.dirty_tree.remove_subtree(root);
        if let (Some(parent), Some(layout)) = (parent, cached_layout) {
            self.frame_state
                .queue_child_clear(parent, layout.into(), is_absolute);
        }
        removed
    }

    /// Clears dirtiness for all nodes while preserving tree/cache state.
    pub fn clear_dirty(&mut self) {
        self.dirty_tree.clear_dirty();
    }

    /// Clears all retained tree/cache/frame state.
    pub fn clear(&mut self) {
        self.dirty_tree.clear();
        self.frame_state.clear();
    }
}

/// Input for [`RendererRetainedTreeReconciler::plan_node`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetainedLogicalTreeNodeInput<K> {
    /// Caller-owned logical node key. The reconciler maps it to a generation-stamped id.
    pub key: K,
    /// Current absolute layout bounds for the node.
    pub current_layout: CachedLayoutBounds,
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

/// Opt-in reconciler for logical retained trees.
///
/// This combines [`RendererNodeGenerationState`] with
/// [`RendererRetainedTreeState`]. It is a design building block for an automatic
/// Rust renderer path: logical keys are converted into generation-stamped ids,
/// tree attachment/removal updates dirty propagation in attachment order, and
/// removed subtrees bump generations so a future remount cannot reuse stale
/// cached layouts. It still performs no drawing, terminal I/O, or default
/// renderer integration.
#[derive(Clone, Debug)]
pub struct RendererRetainedTreeReconciler<K> {
    generations: RendererNodeGenerationState<K>,
    retained: RendererRetainedTreeState<RendererStableNodeId<K>>,
    parents: HashMap<K, K>,
    children: HashMap<K, Vec<K>>,
}

impl<K> Default for RendererRetainedTreeReconciler<K> {
    fn default() -> Self {
        Self {
            generations: RendererNodeGenerationState::default(),
            retained: RendererRetainedTreeState::default(),
            parents: HashMap::new(),
            children: HashMap::new(),
        }
    }
}

impl<K> RendererRetainedTreeReconciler<K>
where
    K: Eq + Hash + Clone,
{
    /// Creates an empty retained tree reconciler.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the generation tracker.
    pub fn generations(&self) -> &RendererNodeGenerationState<K> {
        &self.generations
    }

    /// Returns the retained tree state keyed by stable ids.
    pub fn retained_state(&self) -> &RendererRetainedTreeState<RendererStableNodeId<K>> {
        &self.retained
    }

    /// Returns the retained tree state mutably.
    pub fn retained_state_mut(
        &mut self,
    ) -> &mut RendererRetainedTreeState<RendererStableNodeId<K>> {
        &mut self.retained
    }

    /// Returns the current stable id for a logical key, if tracked.
    pub fn id(&self, key: &K) -> Option<RendererStableNodeId<K>> {
        self.generations.id(key)
    }

    /// Returns or creates the current stable id for a logical key.
    pub fn current_id(&mut self, key: K) -> RendererStableNodeId<K> {
        self.generations.current_id(key)
    }

    /// Registers a logical root node and returns its stable id.
    pub fn register_root(&mut self, key: K) -> RendererStableNodeId<K> {
        if let Some(old_parent) = self.parents.remove(&key) {
            if let Some(siblings) = self.children.get_mut(&old_parent) {
                siblings.retain(|sibling| sibling != &key);
            }
        }
        let id = self.generations.current_id(key.clone());
        self.children.entry(key).or_default();
        self.retained.register_root(id.clone());
        id
    }

    /// Attaches a logical node under a logical parent and returns their stable ids.
    pub fn attach(
        &mut self,
        key: K,
        parent: K,
    ) -> (RendererStableNodeId<K>, RendererStableNodeId<K>) {
        if let Some(old_parent) = self.parents.insert(key.clone(), parent.clone()) {
            if let Some(siblings) = self.children.get_mut(&old_parent) {
                siblings.retain(|sibling| sibling != &key);
            }
        }
        let siblings = self.children.entry(parent.clone()).or_default();
        if !siblings.contains(&key) {
            siblings.push(key.clone());
        }
        self.children.entry(key.clone()).or_default();

        let parent_id = self.generations.current_id(parent);
        let id = self.generations.current_id(key);
        self.retained.attach(id.clone(), parent_id.clone());
        (id, parent_id)
    }

    /// Marks a logical node and its stable-id ancestors dirty.
    pub fn mark_dirty(&mut self, key: &K, measure_dirty: bool) -> bool {
        let Some(id) = self.generations.id(key) else {
            return false;
        };
        self.retained.mark_dirty(&id, measure_dirty);
        true
    }

    /// Returns whether a logical node is dirty.
    pub fn is_dirty(&self, key: &K) -> bool {
        self.generations
            .id(key)
            .is_some_and(|id| self.retained.is_dirty(&id))
    }

    /// Starts a retained planning frame.
    pub fn begin_frame(&mut self) -> bool {
        self.retained.begin_frame()
    }

    /// Plans one logical node by mapping it to its generation-stamped id.
    pub fn plan_node(
        &mut self,
        input: RetainedLogicalTreeNodeInput<K>,
    ) -> RetainedFrameNodePlan<RendererStableNodeId<K>> {
        let key = self.generations.current_id(input.key);
        self.retained.plan_node(RetainedTreeNodeInput {
            key,
            current_layout: input.current_layout,
            skip_self_blit: input.skip_self_blit,
            pending_scroll_delta: input.pending_scroll_delta,
            previous_screen_available: input.previous_screen_available,
            hidden: input.hidden,
            absolute: input.absolute,
        })
    }

    /// Commits a retained node plan.
    pub fn commit_node_plan(&mut self, plan: &RetainedFrameNodePlan<RendererStableNodeId<K>>) {
        self.retained.commit_node_plan(plan);
    }

    /// Removes a logical subtree, drops stable-id cache state, and bumps generations.
    pub fn remove_subtree(&mut self, root: &K, is_absolute: bool) -> Vec<RendererStableNodeId<K>> {
        let Some(root_id) = self.generations.id(root) else {
            return Vec::new();
        };
        let logical_nodes = self.logical_subtree(root);
        let removed = self.retained.remove_subtree(&root_id, is_absolute);

        if let Some(parent) = self.parents.get(root).cloned() {
            if let Some(siblings) = self.children.get_mut(&parent) {
                siblings.retain(|sibling| sibling != root);
            }
        }
        for key in logical_nodes {
            self.parents.remove(&key);
            self.children.remove(&key);
            self.generations.remove(&key);
        }
        removed
    }

    fn logical_subtree(&self, root: &K) -> Vec<K> {
        let mut nodes = Vec::new();
        self.collect_logical_subtree(root, &mut nodes);
        nodes
    }

    fn collect_logical_subtree(&self, key: &K, nodes: &mut Vec<K>) {
        nodes.push(key.clone());
        if let Some(children) = self.children.get(key) {
            for child in children {
                self.collect_logical_subtree(child, nodes);
            }
        }
    }

    /// Clears all dirty flags while preserving generations, tree edges, and cache.
    pub fn clear_dirty(&mut self) {
        self.retained.clear_dirty();
    }

    /// Clears generations, logical tree edges, and retained state.
    pub fn clear(&mut self) {
        self.generations.clear();
        self.retained.clear();
        self.parents.clear();
        self.children.clear();
    }
}
