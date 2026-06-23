use super::*;

/// Cached absolute descendant input for [`plan_escaping_absolute_descendant_blits`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AbsoluteDescendantRect<K> {
    /// Caller-owned descendant identifier returned with any required blit.
    pub key: K,
    /// Previous-frame cached absolute rectangle for the absolute descendant.
    pub rect: CachedClearRegion,
}

/// Blit required for an absolute descendant that escapes its parent layout box.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EscapingAbsoluteDescendantBlit<K> {
    /// Caller-owned descendant identifier from [`AbsoluteDescendantRect::key`].
    pub key: K,
    /// Previous-frame absolute rectangle that should be restored from the retained screen.
    pub rect: CachedClearRegion,
}

/// Plans blits for absolute descendants that paint outside a blitted parent.
///
/// CC Ink's `blitEscapingAbsoluteDescendants(...)` repairs a retained parent
/// blit by re-blitting cached absolute descendants whose rectangles extend
/// outside the parent's layout bounds. Otherwise a dirty sibling that repainted
/// those outside cells can make an overlay disappear when the clean parent is
/// restored from the previous screen.
///
/// This helper is mode-neutral and optimization-only: callers provide the
/// already-discovered cached absolute descendant rectangles, receive only the
/// escaping rects that need a retained-buffer blit, and remain responsible for
/// cache traversal, clipping, drawing, and terminal output.
pub fn plan_escaping_absolute_descendant_blits<K>(
    parent: CachedClearRegion,
    descendants: impl IntoIterator<Item = AbsoluteDescendantRect<K>>,
) -> Vec<EscapingAbsoluteDescendantBlit<K>> {
    let parent_left = i64::from(parent.x);
    let parent_top = i64::from(parent.y);
    let parent_right = parent_left + i64::from(parent.width);
    let parent_bottom = parent_top + i64::from(parent.height);
    let mut blits = Vec::new();

    for descendant in descendants {
        let rect = descendant.rect;
        if rect.width <= 0 || rect.height <= 0 {
            continue;
        }

        let left = i64::from(rect.x);
        let top = i64::from(rect.y);
        let right = left + i64::from(rect.width);
        let bottom = top + i64::from(rect.height);
        if left < parent_left || top < parent_top || right > parent_right || bottom > parent_bottom
        {
            blits.push(EscapingAbsoluteDescendantBlit {
                key: descendant.key,
                rect,
            });
        }
    }

    blits
}

/// Canvas repair applied for an escaping absolute descendant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EscapingAbsoluteDescendantCanvasBlit<K> {
    /// Caller-owned descendant identifier from [`EscapingAbsoluteDescendantBlit::key`].
    pub key: K,
    /// Canvas-clipped rectangle restored from the previous retained canvas.
    pub region: DamageRegion,
}

/// Applies planned escaping absolute-descendant repairs to a canvas.
///
/// This is the canvas-side companion to
/// [`plan_escaping_absolute_descendant_blits`]. CC Ink re-blits cached absolute
/// descendants after a clean parent subtree was restored, because those
/// descendants may have painted outside the parent's layout rectangle and can be
/// overwritten by dirty siblings. This helper keeps that behavior opt-in and
/// renderer-owned: it only copies clipped rectangles from `previous` to `next`
/// and reports what was copied.
pub fn apply_escaping_absolute_descendant_blits_to_canvas<K>(
    next: &mut Canvas,
    previous: &Canvas,
    blits: impl IntoIterator<Item = EscapingAbsoluteDescendantBlit<K>>,
) -> Vec<EscapingAbsoluteDescendantCanvasBlit<K>> {
    let mut applied = Vec::new();
    for blit in blits {
        let Some(region) = blit.rect.clipped_to_canvas(next.width(), next.height()) else {
            continue;
        };
        next.blit_region_from(previous, region.x, region.y, region.width, region.height);
        applied.push(EscapingAbsoluteDescendantCanvasBlit {
            key: blit.key,
            region,
        });
    }
    applied
}
