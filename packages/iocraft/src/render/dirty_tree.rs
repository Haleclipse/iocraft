use super::*;

/// Opt-in dirty tree for retained renderer experiments.
///
/// CC Ink stores `dirty` on mutable DOM nodes and `markDirty(...)` walks to the
/// root so clean subtree blits know where they are safe. iocraft's default
/// renderer does not use this helper, but custom retained renderers can use it
/// with stable Rust node identifiers to model the same invalidation flow without
/// JS object identity or hidden globals. Children are kept in attachment order
/// because CC Ink traverses `childNodes` order for sibling contamination and
/// retained subtree repair decisions. A separate `measure_dirty` set mirrors
/// CC Ink's yoga-measure dirtying for text/raw-ANSI leaf nodes.
#[derive(Clone, Debug)]
pub struct RendererDirtyTree<K> {
    parents: HashMap<K, K>,
    pub(super) children: HashMap<K, Vec<K>>,
    dirty: HashSet<K>,
    measure_dirty: HashSet<K>,
}

impl<K> Default for RendererDirtyTree<K> {
    fn default() -> Self {
        Self {
            parents: HashMap::new(),
            children: HashMap::new(),
            dirty: HashSet::new(),
            measure_dirty: HashSet::new(),
        }
    }
}

impl<K> RendererDirtyTree<K>
where
    K: Eq + Hash + Clone,
{
    /// Creates an empty dirty tree.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a root node with no parent.
    pub fn register_root(&mut self, node: K) {
        if let Some(old_parent) = self.parents.remove(&node) {
            if let Some(siblings) = self.children.get_mut(&old_parent) {
                siblings.retain(|sibling| sibling != &node);
            }
        }
        self.children.entry(node).or_default();
    }

    /// Attaches `node` under `parent`, updating any old parent edge.
    pub fn attach(&mut self, node: K, parent: K) {
        if let Some(old_parent) = self.parents.insert(node.clone(), parent.clone()) {
            if let Some(siblings) = self.children.get_mut(&old_parent) {
                siblings.retain(|sibling| sibling != &node);
            }
        }
        let siblings = self.children.entry(parent).or_default();
        if !siblings.contains(&node) {
            siblings.push(node.clone());
        }
        self.children.entry(node).or_default();
    }

    /// Returns the parent for `node`, if one is registered.
    pub fn parent(&self, node: &K) -> Option<&K> {
        self.parents.get(node)
    }

    /// Returns the currently registered children for `node` in attachment order.
    pub fn child_keys(&self, node: &K) -> Vec<K> {
        self.children.get(node).cloned().unwrap_or_default()
    }

    /// Returns `root` and all descendants currently registered under it.
    pub fn subtree_nodes(&self, root: &K) -> Vec<K> {
        let mut nodes = Vec::new();
        self.collect_subtree_nodes(root, &mut nodes);
        nodes
    }

    fn collect_subtree_nodes(&self, node: &K, nodes: &mut Vec<K>) {
        nodes.push(node.clone());
        if let Some(children) = self.children.get(node) {
            for child in children {
                self.collect_subtree_nodes(child, nodes);
            }
        }
    }

    /// Returns whether `node` is marked dirty.
    pub fn is_dirty(&self, node: &K) -> bool {
        self.dirty.contains(node)
    }

    /// Returns whether `node` needs text/raw measurement refresh.
    pub fn is_measure_dirty(&self, node: &K) -> bool {
        self.measure_dirty.contains(node)
    }

    /// Marks `node` and all ancestors dirty.
    ///
    /// Set `measure_dirty` for text-like leaf mutations that should refresh
    /// layout measurement. The measurement flag is applied only to `node`; the
    /// ancestor walk marks render dirtiness, matching CC Ink's `markDirty(...)`.
    pub fn mark_dirty(&mut self, node: &K, measure_dirty: bool) {
        if measure_dirty {
            self.measure_dirty.insert(node.clone());
        }
        let mut current = Some(node.clone());
        while let Some(key) = current {
            self.dirty.insert(key.clone());
            current = self.parents.get(&key).cloned();
        }
    }

    /// Clears render and measurement dirtiness for one node.
    pub fn clear_node(&mut self, node: &K) {
        self.dirty.remove(node);
        self.measure_dirty.remove(node);
    }

    /// Clears all frame dirtiness while preserving tree edges.
    pub fn clear_dirty(&mut self) {
        self.dirty.clear();
        self.measure_dirty.clear();
    }

    /// Returns the currently dirty nodes.
    pub fn dirty_nodes(&self) -> impl Iterator<Item = &K> {
        self.dirty.iter()
    }

    /// Returns the currently measurement-dirty nodes.
    pub fn measure_dirty_nodes(&self) -> impl Iterator<Item = &K> {
        self.measure_dirty.iter()
    }

    /// Removes a subtree and returns the removed node identifiers.
    ///
    /// The parent is marked dirty because its child list/layout changed. This
    /// helper only tracks invalidation; pair it with [`RendererNodeCache`] or
    /// [`RendererRetainedFrameState`] to queue clear rectangles for removed
    /// cached layouts.
    pub fn remove_subtree(&mut self, root: &K) -> Vec<K> {
        let parent = self.parents.get(root).cloned();
        if let Some(parent) = &parent {
            if let Some(siblings) = self.children.get_mut(parent) {
                siblings.retain(|sibling| sibling != root);
            }
            self.mark_dirty(parent, false);
        }

        let mut removed = Vec::new();
        self.remove_subtree_inner(root, &mut removed);
        removed
    }

    fn remove_subtree_inner(&mut self, node: &K, removed: &mut Vec<K>) {
        let children = self
            .children
            .remove(node)
            .unwrap_or_default()
            .into_iter()
            .collect::<Vec<_>>();
        for child in children {
            self.remove_subtree_inner(&child, removed);
        }
        self.parents.remove(node);
        self.dirty.remove(node);
        self.measure_dirty.remove(node);
        removed.push(node.clone());
    }

    /// Clears all tree edges and dirty state.
    pub fn clear(&mut self) {
        self.parents.clear();
        self.children.clear();
        self.dirty.clear();
        self.measure_dirty.clear();
    }
}
