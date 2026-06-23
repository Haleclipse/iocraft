use super::*;

/// Layout snapshot used by [`RendererLayoutShiftTracker`].
///
/// This is a stable, renderer-agnostic counterpart to CC Ink's per-node cached
/// layout fields used by `resetLayoutShifted()` / `didLayoutShift()`. It avoids
/// binding public APIs to iocraft's internal `taffy::NodeId` or a DOM-like tree.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct RendererLayoutSnapshot {
    /// Absolute or renderer-defined X position in terminal cells.
    pub x: i32,
    /// Absolute or renderer-defined Y position in terminal rows.
    pub y: i32,
    /// Width in terminal cells.
    pub width: i32,
    /// Height in terminal rows.
    pub height: i32,
}

impl From<CachedLayoutBounds> for RendererLayoutSnapshot {
    fn from(value: CachedLayoutBounds) -> Self {
        Self {
            x: value.x,
            y: value.y,
            width: value.width,
            height: value.height,
        }
    }
}

/// Explicit layout-shift tracker for retained custom renderers.
///
/// CC Ink keeps a per-frame module-global `layoutShifted` flag: any node layout
/// change or child removal forces a broad damage backstop so stale retained
/// blits cannot survive. iocraft's built-in renderer has its own internal
/// tracker; this public helper exposes the same idea in a Rust-native form for
/// custom renderers and benchmark harnesses. It records caller-owned node keys
/// and returns `true` only after a previous snapshot exists and the current set
/// differs by key, position, or size.
#[derive(Clone, Debug)]
pub struct RendererLayoutShiftTracker<K> {
    previous: HashMap<K, RendererLayoutSnapshot>,
}

impl<K> Default for RendererLayoutShiftTracker<K> {
    fn default() -> Self {
        Self {
            previous: HashMap::new(),
        }
    }
}

impl<K> RendererLayoutShiftTracker<K>
where
    K: Eq + Hash,
{
    /// Creates an empty layout-shift tracker.
    pub fn new() -> Self {
        Self::default()
    }

    /// Replaces the previous snapshot set and returns whether layout shifted.
    ///
    /// The first call returns `false`, matching iocraft's built-in renderer and
    /// CC Ink's behavior: there is no retained previous layout to protect yet.
    pub fn update(
        &mut self,
        current: impl IntoIterator<Item = (K, RendererLayoutSnapshot)>,
    ) -> bool {
        let current = current.into_iter().collect::<HashMap<_, _>>();
        let shifted = !self.previous.is_empty()
            && (self.previous.len() != current.len()
                || current
                    .iter()
                    .any(|(key, snapshot)| self.previous.get(key) != Some(snapshot)));
        self.previous = current;
        shifted
    }

    /// Replaces the previous snapshot set from [`CachedLayoutBounds`] values.
    pub fn update_from_layouts(
        &mut self,
        current: impl IntoIterator<Item = (K, CachedLayoutBounds)>,
    ) -> bool {
        self.update(
            current
                .into_iter()
                .map(|(key, bounds)| (key, RendererLayoutSnapshot::from(bounds))),
        )
    }

    /// Returns a previously recorded snapshot for `key`, if present.
    pub fn snapshot(&self, key: &K) -> Option<RendererLayoutSnapshot> {
        self.previous.get(key).copied()
    }

    /// Number of snapshots currently retained.
    pub fn len(&self) -> usize {
        self.previous.len()
    }

    /// Whether no snapshots are currently retained.
    pub fn is_empty(&self) -> bool {
        self.previous.is_empty()
    }

    /// Clears all retained snapshots.
    pub fn clear(&mut self) {
        self.previous.clear();
    }
}
