use crate::{
    canvas::{Canvas, CanvasSubviewMut, DamageRegion, ScrollHint},
    component::{ComponentHelperExt, Components, InstantiatedComponent},
    context::{Context, ContextStack, ExitOnCtrlCContext, SystemContext},
    element::ElementExt,
    multimap::AppendOnlyMultimap,
    props::AnyProps,
    terminal::{
        plan_terminal_scroll_hint_patch, terminal_scroll_hint_to_ansi, MockTerminalConfig,
        MockTerminalOutputStream, PendingTerminalFlush, PendingTerminalQuery, Terminal,
        TerminalEvents, TerminalQuery, TerminalScrollHintBounds, TerminalScrollHintPatchOptions,
        TerminalScrollHintPatchPlan, TerminalScrollHintPatchSkipReason,
        TerminalScrollHintRejection,
    },
};
use core::{
    any::Any,
    cell::{Ref, RefMut},
    pin::Pin,
    task::{self, Poll},
};
use futures::{
    future::{select, FutureExt, LocalBoxFuture},
    stream::{Stream, StreamExt},
};
use std::{
    collections::{HashMap, HashSet},
    hash::Hash,
    io,
    time::Duration,
};
use taffy::{
    AvailableSpace, Display, Layout, NodeId, Overflow, Point, Position, Rect, Size, Style,
    TaffyTree,
};

pub(crate) struct UpdateContext<'a, 'w> {
    terminal: Option<&'a mut Terminal<'w>>,
    layout_engine: &'a mut LayoutEngine,
    did_clear_terminal_output: bool,
    force_full_repaint: bool,
    invalidate_prev_frame: bool,
}

/// Callback invoked after a terminal render-loop frame when frame profiling is enabled.
pub type FrameProfileCallback<'a> = Box<dyn FnMut(RenderFrameProfile) + Send + 'a>;

/// Per-frame render-loop profiling event.
///
/// This is the Rust-native counterpart to CC Ink's `FrameEvent`: it reports
/// phase timings, repaint cause, canvas size, and retained-canvas change counts
/// without forcing any particular logging or analytics policy into the framework.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenderFrameProfile {
    /// Total wall-clock duration of the frame.
    pub duration: Duration,
    /// Phase timings and counters collected for the frame.
    pub phases: RenderFramePhases,
    /// Repaint metadata for the frame, if terminal output was written.
    pub repaint: Option<DebugRepaintInfo>,
}

/// Phase timings and counters for one render-loop frame.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RenderFramePhases {
    /// Component update/reconciliation time.
    pub update: Duration,
    /// Layout calculation time.
    pub layout: Duration,
    /// Canvas drawing time.
    pub draw: Duration,
    /// Terminal diff/write/cursor-positioning time.
    pub terminal_write: Duration,
    /// Number of retained-canvas cells that differed from the previous frame.
    pub changed_cells: usize,
    /// Width of the rendered canvas in terminal cells.
    pub canvas_width: usize,
    /// Height of the rendered canvas in terminal rows.
    pub canvas_height: usize,
}

/// Accumulator for [`RenderFrameProfile`] events.
///
/// Benchmark harnesses can pass [`RenderLoopFuture::on_frame_profile`](crate::RenderLoopFuture::on_frame_profile)
/// a closure that records into this struct and then compare totals, maxima, or
/// averages across renderer implementations. The helper is deliberately small
/// and deterministic: it does not sample clocks itself, spawn threads, write
/// logs, or depend on terminal mode.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RenderFrameProfileStats {
    /// Number of frames recorded.
    pub frames: usize,
    /// Number of frames that wrote terminal output.
    pub repaint_frames: usize,
    /// Total wall-clock frame duration.
    pub total_duration: Duration,
    /// Maximum single-frame duration.
    pub max_duration: Duration,
    /// Total update/reconciliation time.
    pub total_update: Duration,
    /// Total layout time.
    pub total_layout: Duration,
    /// Total canvas draw time.
    pub total_draw: Duration,
    /// Total terminal diff/write/cursor-positioning time.
    pub total_terminal_write: Duration,
    /// Sum of retained-canvas changed cells across frames.
    pub total_changed_cells: usize,
    /// Maximum retained-canvas changed cells in any frame.
    pub max_changed_cells: usize,
}

impl RenderFrameProfileStats {
    /// Records one frame profile.
    pub fn record(&mut self, event: &RenderFrameProfile) {
        self.frames = self.frames.saturating_add(1);
        if event.repaint.is_some() {
            self.repaint_frames = self.repaint_frames.saturating_add(1);
        }
        saturating_duration_add(&mut self.total_duration, event.duration);
        self.max_duration = self.max_duration.max(event.duration);
        saturating_duration_add(&mut self.total_update, event.phases.update);
        saturating_duration_add(&mut self.total_layout, event.phases.layout);
        saturating_duration_add(&mut self.total_draw, event.phases.draw);
        saturating_duration_add(&mut self.total_terminal_write, event.phases.terminal_write);
        self.total_changed_cells = self
            .total_changed_cells
            .saturating_add(event.phases.changed_cells);
        self.max_changed_cells = self.max_changed_cells.max(event.phases.changed_cells);
    }

    /// Average frame duration, or zero when no frames were recorded.
    pub fn average_duration(&self) -> Duration {
        average_duration(self.total_duration, self.frames)
    }

    /// Average update/reconciliation time.
    pub fn average_update(&self) -> Duration {
        average_duration(self.total_update, self.frames)
    }

    /// Average layout time.
    pub fn average_layout(&self) -> Duration {
        average_duration(self.total_layout, self.frames)
    }

    /// Average canvas draw time.
    pub fn average_draw(&self) -> Duration {
        average_duration(self.total_draw, self.frames)
    }

    /// Average terminal write time.
    pub fn average_terminal_write(&self) -> Duration {
        average_duration(self.total_terminal_write, self.frames)
    }

    /// Average changed cell count per frame.
    pub fn average_changed_cells(&self) -> f64 {
        if self.frames == 0 {
            0.0
        } else {
            self.total_changed_cells as f64 / self.frames as f64
        }
    }

    /// Fraction of recorded frames that repainted terminal output.
    pub fn repaint_ratio(&self) -> f64 {
        if self.frames == 0 {
            0.0
        } else {
            self.repaint_frames as f64 / self.frames as f64
        }
    }
}

fn saturating_duration_add(total: &mut Duration, delta: Duration) {
    *total = total.checked_add(delta).unwrap_or(Duration::MAX);
}

fn average_duration(total: Duration, count: usize) -> Duration {
    if count == 0 {
        Duration::ZERO
    } else {
        Duration::from_secs_f64(total.as_secs_f64() / count as f64)
    }
}

/// Reason a frame wrote terminal output.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DebugRepaintReason {
    /// No previous frame existed, so the terminal needed an initial paint.
    FirstFrame,
    /// Application code cleared terminal output during update.
    TerminalCleared,
    /// Dynamic alternate-screen state changed and invalidated the previous buffer.
    AlternateScreenChanged,
    /// The terminal size changed.
    TerminalResized,
    /// The current canvas requested a full repaint.
    ForceFullRepaint,
    /// The current canvas carries explicit damage metadata.
    CurrentDamage,
    /// The previous retained canvas carried damage metadata that must be cleaned up.
    PreviousDamage,
    /// The logical retained canvas changed.
    CanvasChanged,
}

/// Debug repaint metadata for a frame that wrote terminal output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DebugRepaintInfo {
    /// Why the frame wrote terminal output.
    pub reason: DebugRepaintReason,
    /// Current canvas damage region, if any.
    pub damage: Option<DamageRegion>,
    /// Previous canvas damage region, if any.
    pub previous_damage: Option<DamageRegion>,
    /// Number of changed retained-canvas cells observed for this frame.
    pub changed_cells: usize,
    /// Width of the rendered canvas in terminal cells.
    pub canvas_width: usize,
    /// Height of the rendered canvas in terminal rows.
    pub canvas_height: usize,
}

#[derive(Clone, Copy, Debug, Default)]
struct RenderPhaseProfile {
    update: Duration,
    layout: Duration,
    draw: Duration,
}

mod absolute_descendants;
mod dirty_tree;
mod layout_shift;
mod node_cache;
mod render_children;
mod render_node_to_output;
mod render_scrolled_children;
mod renderer_state;
mod scroll_fast_path;

pub use absolute_descendants::*;
pub use dirty_tree::*;
pub use layout_shift::*;
pub use node_cache::*;
pub use render_children::*;
pub use render_node_to_output::*;
pub use render_scrolled_children::*;
pub use renderer_state::*;
pub use scroll_fast_path::*;

/// Provides information and operations that low level component implementations may need to
/// utilize during the update phase.
pub struct ComponentUpdater<'a, 'b: 'a, 'c: 'a, 'w> {
    node_id: NodeId,
    transparent_layout: bool,
    children: &'a mut Components,
    unattached_child_node_ids: &'a mut Vec<NodeId>,
    context: &'a mut UpdateContext<'b, 'w>,
    component_context_stack: &'a mut ContextStack<'c>,
}

impl<'a, 'b, 'c, 'w> ComponentUpdater<'a, 'b, 'c, 'w> {
    pub(crate) fn new(
        node_id: NodeId,
        children: &'a mut Components,
        unattached_child_node_ids: &'a mut Vec<NodeId>,
        context: &'a mut UpdateContext<'b, 'w>,
        component_context_stack: &'a mut ContextStack<'c>,
    ) -> Self {
        Self {
            node_id,
            transparent_layout: false,
            children,
            unattached_child_node_ids,
            context,
            component_context_stack,
        }
    }

    /// Puts the terminal into raw mode if it isn't already, and returns a stream of terminal
    /// events.
    pub fn terminal_events(&mut self) -> Option<TerminalEvents> {
        self.context.terminal.as_mut().and_then(|t| t.events().ok())
    }

    /// Sends a CC Ink-style terminal query on the render output side band.
    ///
    /// Pair this with [`Self::flush_terminal_queries`] to resolve unsupported
    /// queries without timeouts. Sending a query starts the backend event stream
    /// so any parsed [`crate::TerminalEvent::Response`] values can be routed by
    /// the render loop into the query queue even when no component separately
    /// subscribes to input.
    pub fn send_terminal_query(
        &mut self,
        query: TerminalQuery,
    ) -> io::Result<Option<PendingTerminalQuery>> {
        if let Some(terminal) = self.context.terminal.as_mut() {
            terminal.send_terminal_query(query).map(Some)
        } else {
            Ok(None)
        }
    }

    /// Sends the DA1 sentinel used to flush pending terminal queries.
    pub fn flush_terminal_queries(&mut self) -> io::Result<Option<PendingTerminalFlush>> {
        if let Some(terminal) = self.context.terminal.as_mut() {
            terminal.flush_terminal_queries().map(Some)
        } else {
            Ok(None)
        }
    }

    /// Returns whether the terminal input supports raw mode.
    pub fn is_terminal_raw_mode_supported(&self) -> bool {
        self.context
            .terminal
            .as_ref()
            .map(|t| t.is_raw_mode_supported())
            .unwrap_or(false)
    }

    /// Returns whether the terminal is in raw mode.
    pub fn is_terminal_raw_mode_enabled(&self) -> bool {
        self.context
            .terminal
            .as_ref()
            .map(|t| t.is_raw_mode_enabled())
            .unwrap_or(false)
    }

    /// Removes the currently rendered output from the terminal, e.g. to allow for the printing of
    /// output above the component.
    pub fn clear_terminal_output(&mut self) {
        if !self.context.did_clear_terminal_output {
            if let Some(terminal) = self.context.terminal.as_mut() {
                terminal.clear_canvas().unwrap();
            }
            self.context.did_clear_terminal_output = true;
        }
    }

    /// Clears the visible terminal screen while preserving scrollback, then treats
    /// the next canvas write as a fresh frame. This mirrors Ink's main-screen
    /// repaint path: no alternate screen, native terminal scrollback remains usable.
    pub fn clear_screen(&mut self) {
        if !self.context.did_clear_terminal_output {
            if let Some(terminal) = self.context.terminal.as_mut() {
                terminal.clear_screen().unwrap();
            }
            self.context.did_clear_terminal_output = true;
        }
    }

    /// Clears the full terminal screen and scrollback, then treats the next canvas
    /// write as a fresh frame.
    pub fn clear_terminal(&mut self) {
        if !self.context.did_clear_terminal_output {
            if let Some(terminal) = self.context.terminal.as_mut() {
                terminal.clear_terminal().unwrap();
            }
            self.context.did_clear_terminal_output = true;
        }
    }

    /// Requests a full repaint for the current frame. Unlike clearing helpers,
    /// this does not write to the terminal immediately; it marks the canvas so
    /// the backend refreshes every row during the normal frame commit. This is
    /// useful for one-shot recovery from contaminated retained output.
    pub fn force_full_repaint(&mut self) {
        self.context.force_full_repaint = true;
    }

    /// Marks the previously retained frame as untrustworthy without clearing the
    /// terminal. The next rendered canvas is tagged with full-screen damage,
    /// mirroring CC Ink's `invalidatePrevFrame()` / `prevFrameContaminated`
    /// path: all rows are considered during diffing, but scroll optimizations
    /// such as DECSTBM remain available.
    pub fn invalidate_previous_frame(&mut self) {
        self.context.invalidate_prev_frame = true;
    }

    /// Returns a mutable reference to the terminal, if we're in a terminal render loop.
    pub(crate) fn terminal_mut(&mut self) -> Option<&mut Terminal<'w>> {
        self.context.terminal.as_deref_mut()
    }

    #[doc(hidden)]
    pub fn component_context_stack(&self) -> &ContextStack<'c> {
        self.component_context_stack
    }

    /// Gets an immutable reference to context of the given type.
    pub fn get_context<T: Any>(&self) -> Option<Ref<'_, T>> {
        self.component_context_stack.get_context()
    }

    /// Gets a mutable reference to context of the given type.
    pub fn get_context_mut<T: Any>(&self) -> Option<RefMut<'_, T>> {
        self.component_context_stack.get_context_mut()
    }

    /// Sets the layout style of the current component.
    pub fn set_layout_style(&mut self, layout_style: taffy::style::Style) {
        self.context
            .layout_engine
            .set_style(self.node_id, layout_style)
            .expect("we should be able to set the style");
    }

    /// Sets the layout style only when it differs from the current style.
    ///
    /// This mirrors CC Ink's style equality guard: rebuilding an equivalent
    /// style value should not dirty layout or wake Taffy recomputation.
    /// Returns `true` when the style was changed.
    pub fn set_layout_style_if_changed(&mut self, layout_style: taffy::style::Style) -> bool {
        if self
            .context
            .layout_engine
            .style(self.node_id)
            .is_ok_and(|current| current == &layout_style)
        {
            return false;
        }

        self.set_layout_style(layout_style);
        true
    }

    /// Sets the measure function of the current component, which is invoked to calculate the area
    /// that the component's content should occupy.
    pub fn set_measure_func(&mut self, measure_func: MeasureFunc) {
        self.context
            .layout_engine
            .get_node_context_mut(self.node_id)
            .expect("we should be able to get the node")
            .measure_func = Some(measure_func);
        self.context
            .layout_engine
            .mark_dirty(self.node_id)
            .expect("we should be able to mark the node as dirty");
    }

    /// If set to `true`, the layout of the current component will be transparent, meaning that
    /// children will effectively be direct descendants of the parent of the current component for
    /// layout purposes.
    pub fn set_transparent_layout(&mut self, transparent_layout: bool) {
        if transparent_layout && !self.transparent_layout {
            self.context
                .layout_engine
                .set_style(
                    self.node_id,
                    Style {
                        display: Display::None,
                        ..Default::default()
                    },
                )
                .expect("we should be able to set the style");
        }
        self.transparent_layout = transparent_layout;
    }

    pub(crate) fn has_transparent_layout(&self) -> bool {
        self.transparent_layout
    }

    /// Updates the children of the current component.
    pub fn update_children<I, T>(&mut self, children: I, context: Option<Context>)
    where
        I: IntoIterator<Item = T>,
        T: ElementExt,
    {
        self.component_context_stack
            .with_context(context, |component_context_stack| {
                let mut used_components = AppendOnlyMultimap::default();

                let mut direct_child_node_ids = Vec::new();
                let child_node_ids = if self.transparent_layout {
                    &mut self.unattached_child_node_ids
                } else {
                    &mut direct_child_node_ids
                };

                for mut child in children {
                    let mut component: InstantiatedComponent =
                        match self.children.components.pop_front(child.key()) {
                            Some(component)
                                if component.component().type_id()
                                    == child.helper().component_type_id() =>
                            {
                                child_node_ids.push(component.node_id());
                                component
                            }
                            _ => {
                                let new_node_id = self
                                    .context
                                    .layout_engine
                                    .new_leaf_with_context(
                                        Style::default(),
                                        LayoutEngineNodeContext::default(),
                                    )
                                    .expect("we should be able to add the node");
                                child_node_ids.push(new_node_id);
                                let h = child.helper();
                                InstantiatedComponent::new(new_node_id, child.props_mut(), h)
                            }
                        };
                    component.update(
                        self.context,
                        child_node_ids,
                        component_context_stack,
                        child.props_mut(),
                    );

                    used_components.push_back(child.key().clone(), component);
                }

                self.context
                    .layout_engine
                    .set_children(self.node_id, &direct_child_node_ids)
                    .expect("we should be able to set the children");

                for component in self.children.components.iter() {
                    self.context
                        .layout_engine
                        .remove(component.node_id())
                        .expect("we should be able to remove the node");
                }
                self.children.components = used_components.into();
            });
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DeferredNoSelectRegion {
    x: usize,
    y: usize,
    width: usize,
    height: usize,
}

struct DrawContext<'a> {
    layout_engine: &'a LayoutEngine,
    canvas: &'a mut Canvas,
    deferred_no_select: &'a mut Vec<DeferredNoSelectRegion>,
}

/// Provides information and operations that low level component implementations may need to
/// utilize during the draw phase.
pub struct ComponentDrawer<'a> {
    node_id: NodeId,
    node_position: Point<i16>,
    node_size: Size<u16>,
    clip_rect: Rect<u16>,
    vertical_clip_active: bool,
    skip_children: bool,
    context: DrawContext<'a>,
}

impl ComponentDrawer<'_> {
    /// Gets the calculated layout of the current node.
    pub fn layout(&self) -> Layout {
        *self
            .context
            .layout_engine
            .layout(self.node_id)
            .expect("we should be able to get the layout")
    }

    /// Gets the style of the current node.
    pub fn style(&self) -> &Style {
        self.context
            .layout_engine
            .style(self.node_id)
            .expect("we should be able to get the style")
    }

    /// Gets the size of the component.
    pub fn size(&self) -> Size<u16> {
        self.node_size
    }

    /// Gets the drawable content size of the component, excluding padding and border.
    ///
    /// This mirrors CC Ink's `get-max-width.ts` helper, generalized to both
    /// axes: content width/height are the computed node size minus computed
    /// padding and border. Values saturate at zero.
    pub fn content_size(&self) -> Size<u16> {
        let layout = self.layout();
        Size {
            width: (layout.size.width
                - layout.padding.left
                - layout.padding.right
                - layout.border.left
                - layout.border.right)
                .max(0.0) as u16,
            height: (layout.size.height
                - layout.padding.top
                - layout.padding.bottom
                - layout.border.top
                - layout.border.bottom)
                .max(0.0) as u16,
        }
    }

    /// Gets the visible component size inside the current canvas clip.
    pub fn visible_size(&self) -> Size<u16> {
        let x = self.node_position.x as isize;
        let y = self.node_position.y as isize;
        let node_right = x + self.node_size.width as isize;
        let node_bottom = y + self.node_size.height as isize;
        let left = x.max(self.clip_rect.left as isize).max(0);
        let top = y.max(self.clip_rect.top as isize).max(0);
        let right = node_right.min(self.clip_rect.right as isize).max(0);
        let bottom = node_bottom.min(self.clip_rect.bottom as isize).max(0);
        Size {
            width: (right - left).max(0) as u16,
            height: (bottom - top).max(0) as u16,
        }
    }

    /// Gets the component size remaining before the root canvas edge.
    ///
    /// Text renderers use this to mirror CC Ink's `render-node-to-output.ts`
    /// clamp of `getMaxWidth(yogaNode)` to `output.width - x`. Yoga can compute
    /// an overflowing cross-axis width that is wider than the terminal canvas;
    /// wrapping against this clamped width keeps rendered line count consistent
    /// with the cells that can actually exist in the screen buffer.
    pub fn remaining_canvas_size(&self) -> Size<u16> {
        let x = self.node_position.x as isize;
        let y = self.node_position.y as isize;
        Size {
            width: (self.context.canvas.width() as isize - x)
                .max(0)
                .min(self.node_size.width as isize) as u16,
            height: (self.context.canvas.height() as isize - y)
                .max(0)
                .min(self.node_size.height as isize) as u16,
        }
    }

    /// Gets the position of the component relative to the top left of the canvas.
    pub fn canvas_position(&self) -> Point<i16> {
        self.node_position
    }

    /// Requests that this frame be fully repainted even if the produced canvas
    /// is otherwise identical to the previous frame. Use this as a one-shot
    /// recovery path when previously rendered terminal cells may be contaminated
    /// by external output or post-render overlays.
    pub fn force_full_repaint(&mut self) {
        self.context.canvas.force_full_repaint();
    }

    /// Marks this component's current canvas bounds as damaged. Damaged rows
    /// are repainted even if their cells are otherwise equal to the previous
    /// frame, mirroring CC Ink's screen damage escape hatch for retained-output
    /// contamination.
    pub fn mark_damage(&mut self) {
        self.mark_damage_region(
            0,
            0,
            self.node_size.width as usize,
            self.node_size.height as usize,
        );
    }

    /// Marks a component-local rectangle as damaged. Coordinates are relative
    /// to this component's top-left canvas position.
    pub fn mark_damage_region(&mut self, x: usize, y: usize, width: usize, height: usize) {
        if width == 0 || height == 0 {
            return;
        }

        let node_width = self.node_size.width as usize;
        let node_height = self.node_size.height as usize;
        if x >= node_width || y >= node_height {
            return;
        }

        let local_left = x;
        let local_top = y;
        let local_right = x.saturating_add(width).min(node_width);
        let local_bottom = y.saturating_add(height).min(node_height);

        let abs_left = self.node_position.x as isize + local_left as isize;
        let abs_top = self.node_position.y as isize + local_top as isize;
        let abs_right = self.node_position.x as isize + local_right as isize;
        let abs_bottom = self.node_position.y as isize + local_bottom as isize;

        let clipped_left = abs_left.max(self.clip_rect.left as isize).max(0);
        let clipped_top = abs_top.max(self.clip_rect.top as isize).max(0);
        let clipped_right = abs_right.min(self.clip_rect.right as isize).max(0);
        let clipped_bottom = abs_bottom.min(self.clip_rect.bottom as isize).max(0);

        if clipped_right <= clipped_left || clipped_bottom <= clipped_top {
            return;
        }

        self.context.canvas.mark_damage(DamageRegion {
            x: clipped_left as usize,
            y: clipped_top as usize,
            width: (clipped_right - clipped_left) as usize,
            height: (clipped_bottom - clipped_top) as usize,
        });
    }

    /// Marks this component's current canvas bounds as excluded from fullscreen
    /// text selection. This mirrors CC Ink's `noSelect` metadata: it has no
    /// terminal output effect and is consumed by selection/copy overlays.
    pub fn mark_no_select(&mut self) {
        self.mark_no_select_region(
            0,
            0,
            self.node_size.width as usize,
            self.node_size.height as usize,
        );
    }

    /// Marks a component-local rectangle as excluded from fullscreen text selection.
    pub fn mark_no_select_region(&mut self, x: usize, y: usize, width: usize, height: usize) {
        self.mark_no_select_region_signed(x as isize, y as isize, width, height);
    }

    pub(crate) fn mark_no_select_region_signed(
        &mut self,
        x: isize,
        y: isize,
        width: usize,
        height: usize,
    ) {
        if width == 0 || height == 0 {
            return;
        }

        let clipped = self.clipped_canvas_region(x, y, width, height);
        self.canvas().mark_no_select_region(x, y, width, height);
        if let Some((x, y, width, height)) = clipped {
            // CC Ink's Output applies noSelect ops after writes and blits so
            // a parent <NoSelect> still wins when a clean child subtree is
            // restored from a cached/previous screen buffer. Keep the immediate
            // mark above so retained snapshots also preserve noSelect metadata,
            // then replay it before post-render overlays and at frame end.
            self.context
                .deferred_no_select
                .push(DeferredNoSelectRegion {
                    x,
                    y,
                    width,
                    height,
                });
        }
    }

    fn clipped_canvas_region(
        &self,
        x: isize,
        y: isize,
        width: usize,
        height: usize,
    ) -> Option<(usize, usize, usize, usize)> {
        let mut left = self.node_position.x as isize + x;
        let mut top = self.node_position.y as isize + y;
        let mut right = left + width as isize;
        let mut bottom = top + height as isize;

        left = left.max(self.clip_rect.left as isize).max(0);
        top = top.max(self.clip_rect.top as isize).max(0);
        right = right.min(self.clip_rect.right as isize).max(0);
        bottom = bottom.min(self.clip_rect.bottom as isize).max(0);

        if right <= left || bottom <= top {
            return None;
        }

        Some((
            left as usize,
            top as usize,
            (right - left) as usize,
            (bottom - top) as usize,
        ))
    }

    /// Declares that this component's fullscreen rows shifted vertically since
    /// the previous frame. The terminal renderer may turn this into a DECSTBM
    /// hardware scroll and then repaint the rows that changed.
    pub fn set_scroll_hint(&mut self, delta: i32) {
        if delta == 0 || self.node_size.height == 0 {
            return;
        }

        let canvas_width = self.context.canvas.width() as isize;
        if canvas_width <= 0 {
            return;
        }

        let abs_left = self.node_position.x as isize;
        let abs_right = self.node_position.x as isize + self.node_size.width as isize;
        let clipped_left = abs_left.max(self.clip_rect.left as isize).max(0);
        let clipped_right = abs_right
            .min(self.clip_rect.right as isize)
            .min(canvas_width)
            .max(0);
        if clipped_left != 0 || clipped_right < canvas_width {
            return;
        }

        let abs_top = self.node_position.y as isize;
        let abs_bottom = self.node_position.y as isize + self.node_size.height as isize;
        let clipped_top = abs_top.max(self.clip_rect.top as isize).max(0);
        let clipped_bottom = abs_bottom.min(self.clip_rect.bottom as isize).max(0);
        if clipped_bottom <= clipped_top {
            return;
        }

        self.context.canvas.set_scroll_hint(ScrollHint {
            top: clipped_top as usize,
            bottom: (clipped_bottom - 1) as usize,
            delta,
        });
    }

    /// Skips drawing this component's children for the current frame.
    ///
    /// This is intended for retained/cached subtree components that have
    /// already restored their child output into the canvas during
    /// `pre_component_draw`, mirroring CC Ink's clean-subtree blit fast path.
    pub fn skip_children(&mut self) {
        self.skip_children = true;
    }

    pub(crate) fn take_skip_children(&mut self) -> bool {
        let skip = self.skip_children;
        self.skip_children = false;
        skip
    }

    pub(crate) fn replay_deferred_no_select(&mut self) {
        // Re-apply queued noSelect regions without draining them. A later sibling
        // blit can still overwrite the bitmap, so ancestors/root replay the same
        // regions again before their post-render overlay hooks run.
        for region in self.context.deferred_no_select.iter().copied() {
            self.context.canvas.mark_no_select_region(
                region.x,
                region.y,
                region.width,
                region.height,
            );
        }
    }

    pub(crate) fn clear_deferred_no_select(&mut self) {
        self.context.deferred_no_select.clear();
    }

    /// Gets the root canvas for crate-level post-render hooks.
    pub(crate) fn root_canvas_mut(&mut self) -> &mut Canvas {
        self.context.canvas
    }

    /// Gets the region of the canvas that the component should be drawn to.
    pub fn canvas(&mut self) -> CanvasSubviewMut<'_> {
        self.context.canvas.subview_mut(
            self.node_position.x as _,
            self.node_position.y as _,
            self.clip_rect.left as _,
            self.clip_rect.top as _,
            self.clip_rect.right.saturating_sub(self.clip_rect.left) as _,
            self.clip_rect.bottom.saturating_sub(self.clip_rect.top) as _,
        )
    }

    /// Prepares to begin drawing a node by moving to the node's position and invoking the given
    /// closure.
    pub(crate) fn for_child_node_layout<F>(&mut self, node_id: NodeId, f: F)
    where
        F: FnOnce(&mut Self),
    {
        let old_node_id = self.node_id;
        let old_node_position = self.node_position;
        let old_node_size = self.node_size;
        let old_skip_children = self.skip_children;
        self.skip_children = false;
        self.node_id = node_id;
        let layout = self.layout();
        let mut next_position = Point {
            x: self.node_position.x + layout.location.x as i16,
            y: self.node_position.y + layout.location.y as i16,
        };
        if next_position.y < 0
            && self.style().position == Position::Absolute
            && !self.vertical_clip_active
        {
            // CC Ink's render-node-to-output.ts clamps negative screen-space Y
            // for absolute overlays so menus/tooltips that extend above the
            // viewport keep their top rows visible instead of clipping them.
            // iocraft's ScrollView uses an absolute child translated upward
            // inside an overflow-hidden viewport, so retain negative Y while
            // vertical clipping is active to preserve scroll-window semantics.
            next_position.y = 0;
        }
        self.node_position = next_position;
        self.node_size = Size {
            width: layout.size.width as u16,
            height: layout.size.height as u16,
        };
        f(self);
        let requested_skip_children = self.skip_children;
        self.node_id = old_node_id;
        self.node_position = old_node_position;
        self.node_size = old_node_size;
        self.skip_children = old_skip_children || requested_skip_children;
    }

    /// Returns whether this component is a zero-height box sharing a row with a sibling.
    ///
    /// CC Ink's `render-node-to-output.ts` uses this as a ghost-text guard for
    /// boxes whose Yoga height is squeezed to zero: if a later sibling paints on
    /// the same row, the zero-height box's children must not leave tail glyphs
    /// behind that sibling.
    pub(crate) fn zero_height_sibling_shares_y(&self) -> bool {
        let layout = self.layout();
        if layout.size.height != 0.0 {
            return false;
        }
        let Some(parent) = self.context.layout_engine.parent(self.node_id) else {
            return false;
        };
        let Ok(siblings) = self.context.layout_engine.children(parent) else {
            return false;
        };
        let Some(index) = siblings.iter().position(|sibling| *sibling == self.node_id) else {
            return false;
        };
        let my_top = layout.location.y;
        siblings[index + 1..]
            .iter()
            .chain(siblings[..index].iter().rev())
            .any(|sibling| {
                self.context
                    .layout_engine
                    .layout(*sibling)
                    .is_ok_and(|sibling_layout| sibling_layout.location.y == my_top)
            })
    }

    /// Prepares to begin drawing a node's children by shrinking the clipping rectangle if necessary.
    pub(crate) fn with_clip_rect_for_children<F>(&mut self, f: F)
    where
        F: FnOnce(&mut Self),
    {
        let overflow = self.style().overflow;
        if overflow.x == Overflow::Visible && overflow.y == Overflow::Visible {
            // No need to do anything.
            f(self);
            return;
        }

        let old_clip_rect = self.clip_rect;
        let old_vertical_clip_active = self.vertical_clip_active;
        let layout = self.layout();
        if overflow.x != Overflow::Visible {
            self.clip_rect.left = self
                .clip_rect
                .left
                .max((self.node_position.x + layout.border.left as i16).max(0) as u16);
            self.clip_rect.right = self.clip_rect.right.min(
                (self.node_position.x + self.node_size.width as i16 - layout.border.right as i16)
                    .max(0) as u16,
            );
        }
        if overflow.y != Overflow::Visible {
            self.vertical_clip_active = true;
            self.clip_rect.top = self
                .clip_rect
                .top
                .max((self.node_position.y + layout.border.top as i16).max(0) as u16);
            self.clip_rect.bottom = self.clip_rect.bottom.min(
                (self.node_position.y + self.node_size.height as i16 - layout.border.bottom as i16)
                    .max(0) as u16,
            );
        }
        f(self);
        self.clip_rect = old_clip_rect;
        self.vertical_clip_active = old_vertical_clip_active;
    }
}

/// The measure function of the current component, which is invoked to calculate the area that the
/// component's content should occupy.
pub type MeasureFunc =
    Box<dyn Fn(Size<Option<f32>>, Size<AvailableSpace>, &Style) -> Size<f32> + Send>;

#[derive(Default)]
pub(crate) struct LayoutEngineNodeContext {
    measure_func: Option<MeasureFunc>,
}

/// Newtype around [`TaffyTree`] that restores `Send`.
///
/// SAFETY: `TaffyTree` lost its `Send` implementation in taffy 0.8, solely because the
/// `calc()` feature stores type-erased `*const ()` handles inside style values
/// (`CompactLength`'s tagged-pointer representation). iocraft never constructs `calc()`
/// values — `TaffyTree`'s high-level API offers no way to do so, and every style we set
/// comes from `LayoutStyle`, which only produces `length`/`percent`/`auto` variants.
/// Therefore no actual pointer is ever stored, let alone dereferenced across threads.
///
/// This approach was endorsed by the taffy maintainer in
/// <https://github.com/ccbrown/iocraft/issues/119>. If iocraft ever exposes `calc()`,
/// this impl must be replaced with the generic-`Calc` mechanism tracked in
/// <https://github.com/DioxusLabs/taffy/pull/855>.
pub(crate) struct LayoutEngine(TaffyTree<LayoutEngineNodeContext>);

unsafe impl Send for LayoutEngine {}

impl LayoutEngine {
    pub fn new() -> Self {
        Self(TaffyTree::new())
    }
}

impl std::ops::Deref for LayoutEngine {
    type Target = TaffyTree<LayoutEngineNodeContext>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for LayoutEngine {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LayoutSnapshot {
    x: i16,
    y: i16,
    width: u16,
    height: u16,
}

impl LayoutSnapshot {
    fn shifts_from(self, previous: Self) -> bool {
        // Width-only changes are not layout shifts for the terminal damage
        // backstop: the rebuilt canvas diff already catches visible cells and
        // clean cached blits should not become dirty just because skipped
        // descendants recomputed a different intrinsic width.
        self.x != previous.x || self.y != previous.y || self.height != previous.height
    }
}

struct Tree<'a> {
    layout_engine: LayoutEngine,
    wrapper_node_id: NodeId,
    root_component: InstantiatedComponent,
    root_component_props: AnyProps<'a>,
    system_context: SystemContext,
    root_view_event_context: crate::components::ViewFocusParentContext,
    prev_layout_snapshots: HashMap<NodeId, LayoutSnapshot>,
}

struct RenderOutput {
    canvas: Canvas,
    did_clear_terminal_output: bool,
    alternate_screen_changed: bool,
    phase_profile: RenderPhaseProfile,
}

impl<'a> Tree<'a> {
    fn new(mut props: AnyProps<'a>, helper: Box<dyn ComponentHelperExt>) -> Self {
        let mut layout_engine = LayoutEngine::new();
        let root_node_id = layout_engine
            .new_leaf_with_context(Style::default(), LayoutEngineNodeContext::default())
            .expect("we should be able to add the root");
        let wrapper_node_id = layout_engine
            .new_with_children(Style::default(), &[root_node_id])
            .expect("we should be able to add the root");
        Self {
            layout_engine,
            wrapper_node_id,
            root_component: InstantiatedComponent::new(root_node_id, props.borrow(), helper),
            root_component_props: props,
            system_context: SystemContext::new(),
            root_view_event_context: crate::components::ViewFocusParentContext::shared_root(),
            prev_layout_snapshots: HashMap::new(),
        }
    }

    fn render(
        &mut self,
        max_width: Option<usize>,
        mut terminal: Option<&mut Terminal<'_>>,
        profile_enabled: bool,
    ) -> RenderOutput {
        self.system_context.begin_render_frame();
        let update_start = profile_enabled.then(std::time::Instant::now);
        let exit_on_ctrl_c = terminal
            .as_deref()
            .map(Terminal::exit_on_ctrl_c)
            .unwrap_or(true);
        let mut wrapper_child_node_ids = vec![self.root_component.node_id()];
        let (did_clear_terminal_output, force_full_repaint, invalidate_prev_frame) = {
            let mut context = UpdateContext {
                terminal: terminal.as_deref_mut(),
                layout_engine: &mut self.layout_engine,
                did_clear_terminal_output: false,
                force_full_repaint: false,
                invalidate_prev_frame: false,
            };
            let mut component_context_stack = ContextStack::root(&mut self.system_context);
            // CC Ink dispatches hit-test/focus events from a single DOM root.
            // Seed an equivalent root event context for the whole iocraft tree so
            // transparent root components (Fragment, ContextProvider, etc.) share
            // one topmost click/hover registry instead of acting like independent
            // roots. The hovered set persists across frames like CC Ink's
            // `hoveredNodes`, while the per-frame hit registries are rebuilt below.
            self.root_view_event_context.begin_root_event_frame();
            component_context_stack.with_context(
                Some(Context::owned(ExitOnCtrlCContext(exit_on_ctrl_c))),
                |component_context_stack| {
                    component_context_stack.with_context(
                        Some(Context::owned(self.root_view_event_context.clone())),
                        |component_context_stack| {
                            self.root_component.update(
                                &mut context,
                                &mut wrapper_child_node_ids,
                                component_context_stack,
                                self.root_component_props.borrow(),
                            );
                        },
                    );
                },
            );
            (
                context.did_clear_terminal_output,
                context.force_full_repaint,
                context.invalidate_prev_frame,
            )
        };
        let update_duration = update_start.map_or(Duration::ZERO, |start| start.elapsed());
        let layout_start = profile_enabled.then(std::time::Instant::now);
        let alternate_screen_changed = terminal
            .as_deref_mut()
            .map(|term| {
                term.set_dynamic_alternate_screen(self.system_context.alternate_screen_request())
                    .unwrap()
            })
            .unwrap_or(false);
        let fullscreen_size = terminal.as_ref().and_then(|term| {
            if term.is_fullscreen() {
                term.size()
                    .map(|(cols, rows)| (cols as usize, rows as usize))
            } else {
                None
            }
        });
        self.layout_engine
            .set_children(self.wrapper_node_id, &wrapper_child_node_ids)
            .expect("we should be able to set the children");
        let mut wrapper_style = Style::default();
        if let Some(max_width) = max_width {
            // Mirror CC Ink's root layout contract: when rendering to a
            // terminal, the root is laid out against the terminal columns, so
            // percentage widths and right/center alignment resolve against the
            // viewport instead of shrink-wrapping to content.
            wrapper_style.size.width = taffy::style::Dimension::length(max_width as f32);
        }
        if let Some((_, rows)) = fullscreen_size {
            // Alt-screen's screen buffer is exactly terminalRows tall. Make the
            // root layout definite as well, so percentage heights resolve
            // against the viewport just like CC Ink's <AlternateScreen> Box.
            wrapper_style.size.height = taffy::style::Dimension::length(rows as f32);
        }
        self.layout_engine
            .set_style(self.wrapper_node_id, wrapper_style)
            .expect("we should be able to set the wrapper style");

        self.layout_engine
            .compute_layout_with_measure(
                self.wrapper_node_id,
                Size {
                    width: max_width
                        .map(|w| AvailableSpace::Definite(w as _))
                        .unwrap_or(AvailableSpace::MaxContent),
                    height: fullscreen_size
                        .map(|(_, rows)| AvailableSpace::Definite(rows as _))
                        .unwrap_or(AvailableSpace::MaxContent),
                },
                |known_dimensions, available_space, _node_id, node_context, style| {
                    match node_context.and_then(|cx| cx.measure_func.as_ref()) {
                        Some(f) => f(known_dimensions, available_space, style),
                        None => Size::ZERO,
                    }
                },
            )
            .expect("we should be able to compute the layout");

        debug_dump_layout_tree(&self.layout_engine, self.wrapper_node_id);
        let layout_shifted = self.update_layout_shift_state();
        let layout_duration = layout_start.map_or(Duration::ZERO, |start| start.elapsed());
        let draw_start = profile_enabled.then(std::time::Instant::now);
        let wrapper_layout = self
            .layout_engine
            .layout(self.wrapper_node_id)
            .expect("we should be able to get the wrapper layout");
        let canvas_width = fullscreen_size
            .map(|(cols, _)| cols)
            .unwrap_or(wrapper_layout.size.width as usize);
        let canvas_height = fullscreen_size
            .map(|(_, rows)| rows)
            .unwrap_or(wrapper_layout.size.height as usize);
        let mut canvas = Canvas::new(canvas_width, canvas_height);
        let mut deferred_no_select = Vec::new();
        let root_layout = self
            .layout_engine
            .layout(self.root_component.node_id())
            .expect("we should be able to get the root layout");
        {
            let mut drawer = ComponentDrawer {
                node_id: self.root_component.node_id(),
                node_position: Point {
                    x: root_layout.location.x as _,
                    y: root_layout.location.y as _,
                },
                node_size: Size {
                    width: root_layout.size.width as _,
                    height: root_layout.size.height as _,
                },
                clip_rect: Rect {
                    left: 0,
                    right: canvas_width as _,
                    top: 0,
                    bottom: canvas_height as _,
                },
                vertical_clip_active: false,
                skip_children: false,
                context: DrawContext {
                    layout_engine: &self.layout_engine,
                    canvas: &mut canvas,
                    deferred_no_select: &mut deferred_no_select,
                },
            };
            self.root_component.draw(&mut drawer);
            drawer.replay_deferred_no_select();
            drawer.clear_deferred_no_select();
        }
        if force_full_repaint {
            canvas.force_full_repaint();
        }
        if invalidate_prev_frame || layout_shifted {
            canvas.mark_damage(DamageRegion {
                x: 0,
                y: 0,
                width: canvas.width(),
                height: canvas.height(),
            });
        }
        let draw_duration = draw_start.map_or(Duration::ZERO, |start| start.elapsed());
        RenderOutput {
            canvas,
            did_clear_terminal_output,
            alternate_screen_changed,
            phase_profile: RenderPhaseProfile {
                update: update_duration,
                layout: layout_duration,
                draw: draw_duration,
            },
        }
    }

    fn update_layout_shift_state(&mut self) -> bool {
        let mut current = HashMap::new();
        self.collect_layout_snapshots(self.wrapper_node_id, &mut current);
        let shifted = !self.prev_layout_snapshots.is_empty()
            && (self.prev_layout_snapshots.len() != current.len()
                || current.iter().any(|(node_id, snapshot)| {
                    self.prev_layout_snapshots
                        .get(node_id)
                        .is_none_or(|previous| snapshot.shifts_from(*previous))
                }));
        self.prev_layout_snapshots = current;
        shifted
    }

    fn collect_layout_snapshots(&self, node_id: NodeId, out: &mut HashMap<NodeId, LayoutSnapshot>) {
        if let Ok(layout) = self.layout_engine.layout(node_id) {
            out.insert(
                node_id,
                LayoutSnapshot {
                    x: layout.location.x as i16,
                    y: layout.location.y as i16,
                    width: layout.size.width as u16,
                    height: layout.size.height as u16,
                },
            );
        }
        if let Ok(children) = self.layout_engine.children(node_id) {
            for child in children {
                self.collect_layout_snapshots(child, out);
            }
        }
    }

    async fn terminal_render_loop(
        &mut self,
        mut term: Terminal<'_>,
        throttle: Option<std::time::Duration>,
        mut frame_profile: Option<FrameProfileCallback<'_>>,
    ) -> io::Result<()> {
        term.start_event_stream()?;
        let mut prev_canvas: Option<Canvas> = None;
        let mut prev_terminal_size: Option<(u16, u16)> = None;
        let mut mouse_capture_enabled: Option<bool> = None;
        loop {
            // Self-healing: if the process was just resumed from suspension (Ctrl+Z
            // followed by `fg`), the shell has reset the terminal modes and overwritten
            // parts of the screen. Re-apply our modes and forget the previous canvas so
            // the next write is a full redraw rather than a row diff against content
            // that no longer exists.
            if term.take_resumed() {
                term.reinitialize_after_resume()?;
                prev_canvas = None;
                prev_terminal_size = None;
            }
            term.refresh_size();
            let terminal_size = term.size();
            let terminal_size_changed =
                prev_canvas.is_some() && prev_terminal_size != terminal_size;
            let profile_enabled = frame_profile.is_some();
            let frame_start = profile_enabled.then(std::time::Instant::now);
            let mut frame_phases = RenderFramePhases::default();
            let mut repaint = None;
            term.synchronized_update(|mut term| {
                let mut output = self.render(
                    terminal_size.map(|(w, _)| w as usize),
                    Some(&mut term),
                    profile_enabled,
                );
                let changed_cells = if profile_enabled {
                    count_changed_cells(prev_canvas.as_ref(), &output.canvas)
                } else {
                    0
                };
                debug_log_render_frame(
                    &output,
                    prev_canvas.as_ref(),
                    terminal_size,
                    terminal_size_changed,
                    changed_cells,
                );
                let should_repaint = output.did_clear_terminal_output
                    || output.alternate_screen_changed
                    || terminal_size_changed
                    || output.canvas.should_force_full_repaint()
                    || output.canvas.has_damage()
                    || prev_canvas
                        .as_ref()
                        .is_some_and(|canvas| canvas.has_damage())
                    || prev_canvas.as_ref() != Some(&output.canvas);
                let previous_damage = prev_canvas.as_ref().and_then(Canvas::damage_region);
                if should_repaint {
                    let reason = classify_debug_repaint_reason(
                        &output,
                        prev_canvas.as_ref(),
                        terminal_size_changed,
                    );
                    repaint = Some(DebugRepaintInfo {
                        reason,
                        damage: output.canvas.damage_region(),
                        previous_damage,
                        changed_cells,
                        canvas_width: output.canvas.width(),
                        canvas_height: output.canvas.height(),
                    });
                    let write_start = profile_enabled.then(std::time::Instant::now);
                    let prev =
                        if output.did_clear_terminal_output || output.alternate_screen_changed {
                            None
                        } else {
                            prev_canvas.as_ref()
                        };
                    term.write_canvas(prev, &output.canvas)?;
                    // Position (or hide) the physical cursor after the frame has been
                    // committed, so IMEs and screen readers can anchor to the caret of
                    // a focused text input.
                    term.position_cursor(output.canvas.cursor_declaration())?;
                    frame_phases.terminal_write =
                        write_start.map_or(Duration::ZERO, |start| start.elapsed());
                }
                frame_phases.update = output.phase_profile.update;
                frame_phases.layout = output.phase_profile.layout;
                frame_phases.draw = output.phase_profile.draw;
                frame_phases.changed_cells = changed_cells;
                frame_phases.canvas_width = output.canvas.width();
                frame_phases.canvas_height = output.canvas.height();
                output.canvas.clear_force_full_repaint();
                // Keep damage on the retained previous canvas. CC Ink's
                // diffEach unions prev.damage with next.damage so cleanup
                // frames still scan regions dirtied by the last render, while
                // Canvas equality ignores damage so this does not wake idle
                // renders by itself.
                prev_canvas = Some(output.canvas);
                if let Some(canvas) = prev_canvas.as_ref() {
                    term.set_event_cell_snapshot(canvas);
                }
                prev_terminal_size = terminal_size;
                Ok(())
            })?;
            if let (Some(callback), Some(start)) = (frame_profile.as_mut(), frame_start) {
                callback(RenderFrameProfile {
                    duration: start.elapsed(),
                    phases: frame_phases,
                    repaint,
                });
            }
            let last_frame = std::time::Instant::now();
            if let Some(requested) = self.system_context.mouse_capture() {
                if mouse_capture_enabled != Some(requested) {
                    if requested {
                        term.enable_mouse_capture()?;
                    } else {
                        term.disable_mouse_capture()?;
                    }
                    mouse_capture_enabled = Some(requested);
                }
            }
            if let Some(flags) = self.system_context.keyboard_enhancement_flags() {
                term.set_keyboard_enhancement_flags(flags)?;
            }
            if let Some(title) = self.system_context.terminal_title() {
                let _ = crate::ansi::terminal_title(term.render_output(), title);
                let _ = term.render_output().flush();
            }
            if self.system_context.should_exit() || term.received_ctrl_c() {
                break;
            }
            select(self.root_component.wait().boxed(), term.wait().boxed()).await;
            if term.take_resumed() {
                term.reinitialize_after_resume()?;
                prev_canvas = None;
                prev_terminal_size = None;
                continue;
            }
            term.resolve_pending_ctrl_c();
            if term.received_ctrl_c() {
                break;
            }
            // Frame throttling: after the first change, keep absorbing further changes
            // until the frame interval has elapsed, so high-frequency state updates
            // (animations, streaming output, progress ticks) coalesce into one frame.
            //
            // Crucially this keeps polling the component tree the whole time: component
            // futures (use_future et al) are driven by this loop, so a plain sleep
            // would freeze them and merely slow the app down instead of coalescing.
            // Input latency is bounded by the interval (≤ ~17ms at the default 60fps).
            if let Some(throttle) = throttle {
                let last = last_frame;
                let mut resumed_during_throttle = false;
                loop {
                    let remaining = throttle.saturating_sub(last.elapsed());
                    if remaining.is_zero() {
                        break;
                    }
                    let timed_out = {
                        let timer = futures_timer::Delay::new(remaining);
                        let timed_out = match futures::future::select(
                            timer,
                            select(self.root_component.wait().boxed(), term.wait().boxed()),
                        )
                        .await
                        {
                            futures::future::Either::Left(_) => true,
                            futures::future::Either::Right(_) => false,
                        };
                        if term.take_resumed() {
                            term.reinitialize_after_resume()?;
                            prev_canvas = None;
                            prev_terminal_size = None;
                            resumed_during_throttle = true;
                        }
                        term.resolve_pending_ctrl_c();
                        Ok::<bool, io::Error>(timed_out)
                    }?;
                    if resumed_during_throttle {
                        break;
                    }
                    if timed_out || term.received_ctrl_c() {
                        break;
                    }
                }
                if resumed_during_throttle {
                    continue;
                }
                if term.received_ctrl_c() {
                    break;
                }
            }
        }
        Ok(())
    }
}

fn count_changed_cells(prev: Option<&Canvas>, next: &Canvas) -> usize {
    let mut count = 0;
    if let Some(prev) = prev {
        prev.diff_each(next, |_| {
            count += 1;
            false
        });
    } else {
        let empty = Canvas::new(0, 0);
        empty.diff_each(next, |_| {
            count += 1;
            false
        });
    }
    count
}

fn debug_dump_layout_tree(layout_engine: &LayoutEngine, root_node_id: NodeId) {
    let Ok(path) = std::env::var("IOCRAFT_LAYOUT_DUMP") else {
        return;
    };
    if path.is_empty() {
        return;
    }
    let mut buf = String::new();
    fn rec(layout_engine: &LayoutEngine, node_id: NodeId, depth: usize, buf: &mut String) {
        if let Ok(layout) = layout_engine.layout(node_id) {
            let child_count = layout_engine
                .children(node_id)
                .map(|c| c.len())
                .unwrap_or(0);
            let style = layout_engine.style(node_id).ok();
            let _ = std::fmt::Write::write_fmt(
                buf,
                format_args!(
                    "{indent}{:?} x={:.1} y={:.1} w={:.1} h={:.1} children={} style={:?}\n",
                    node_id,
                    layout.location.x,
                    layout.location.y,
                    layout.size.width,
                    layout.size.height,
                    child_count,
                    style.map(|s| (
                        &s.display,
                        &s.flex_direction,
                        &s.size,
                        &s.min_size,
                        &s.max_size,
                        &s.gap,
                        &s.padding,
                        &s.margin,
                        s.flex_grow,
                        s.flex_shrink
                    )),
                    indent = "  ".repeat(depth),
                ),
            );
        }
        if depth >= 12 {
            return;
        }
        if let Ok(children) = layout_engine.children(node_id) {
            for child in children {
                rec(layout_engine, child, depth + 1, buf);
            }
        }
    }
    buf.push_str("--- layout ---\n");
    rec(layout_engine, root_node_id, 0, &mut buf);
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut file| std::io::Write::write_all(&mut file, buf.as_bytes()));
}

fn debug_log_render_frame(
    output: &RenderOutput,
    prev_canvas: Option<&Canvas>,
    terminal_size: Option<(u16, u16)>,
    terminal_size_changed: bool,
    changed_cells: usize,
) {
    let Ok(path) = std::env::var("IOCRAFT_FRAME_LOG") else {
        return;
    };

    let mut visible_rows = 0usize;
    let mut last_nonblank_row = 0usize;
    let canvas_text = output.canvas.to_string();
    for (idx, line) in canvas_text.lines().enumerate() {
        visible_rows = idx + 1;
        if !line.trim().is_empty() {
            last_nonblank_row = idx + 1;
        }
    }
    let trailing_blank_rows = visible_rows.saturating_sub(last_nonblank_row);

    let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    else {
        return;
    };

    use std::io::Write as _;
    let _ = writeln!(
        file,
        "canvas={}x{} term={:?} prev={} cursor={:?} changed={} clear={} alt={} resize={} last_nonblank={} trailing_blank={}",
        output.canvas.width(),
        output.canvas.height(),
        terminal_size,
        prev_canvas.map(Canvas::height).unwrap_or(0),
        output.canvas.cursor_declaration(),
        changed_cells,
        output.did_clear_terminal_output,
        output.alternate_screen_changed,
        terminal_size_changed,
        last_nonblank_row,
        trailing_blank_rows,
    );

    if trailing_blank_rows > 20 {
        if let Ok(dump_path) = std::env::var("IOCRAFT_FRAME_DUMP") {
            if let Ok(mut dump) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(dump_path)
            {
                let _ = writeln!(
                    dump,
                    "\n--- canvas {}x{} last_nonblank={} trailing_blank={} ---",
                    output.canvas.width(),
                    output.canvas.height(),
                    last_nonblank_row,
                    trailing_blank_rows,
                );
                let _ = writeln!(dump, "{}", canvas_text);
            }
        }
    }
}

fn classify_debug_repaint_reason(
    output: &RenderOutput,
    prev_canvas: Option<&Canvas>,
    terminal_size_changed: bool,
) -> DebugRepaintReason {
    if output.did_clear_terminal_output {
        DebugRepaintReason::TerminalCleared
    } else if output.alternate_screen_changed {
        DebugRepaintReason::AlternateScreenChanged
    } else if terminal_size_changed {
        DebugRepaintReason::TerminalResized
    } else if output.canvas.should_force_full_repaint() {
        DebugRepaintReason::ForceFullRepaint
    } else if output.canvas.has_damage() {
        DebugRepaintReason::CurrentDamage
    } else if prev_canvas.is_some_and(Canvas::has_damage) {
        DebugRepaintReason::PreviousDamage
    } else if prev_canvas.is_none() {
        DebugRepaintReason::FirstFrame
    } else {
        DebugRepaintReason::CanvasChanged
    }
}

pub(crate) fn render<E: ElementExt>(mut e: E, max_width: Option<usize>) -> Canvas {
    let h = e.helper();
    let mut tree = Tree::new(e.props_mut(), h);
    tree.render(max_width, None, false).canvas
}

pub(crate) async fn terminal_render_loop<E>(
    e: &mut E,
    term: Terminal<'_>,
    throttle: Option<std::time::Duration>,
    frame_profile: Option<FrameProfileCallback<'_>>,
) -> io::Result<()>
where
    E: ElementExt,
{
    let h = e.helper();
    let mut tree = Tree::new(e.props_mut(), h);
    tree.terminal_render_loop(term, throttle, frame_profile)
        .await
}

pub(crate) struct MockTerminalRenderLoop<'a> {
    output: MockTerminalOutputStream,
    render_loop: LocalBoxFuture<'a, io::Result<()>>,
    render_loop_is_done: bool,
}

impl Stream for MockTerminalRenderLoop<'_> {
    type Item = Canvas;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut task::Context<'_>) -> Poll<Option<Self::Item>> {
        let mut this = self.as_mut();

        if !this.render_loop_is_done && this.render_loop.poll_unpin(cx).is_ready() {
            this.render_loop_is_done = true;
        }

        this.output.poll_next_unpin(cx)
    }
}

pub(crate) fn mock_terminal_render_loop<'a, E>(
    e: &'a mut E,
    config: MockTerminalConfig,
) -> MockTerminalRenderLoop<'a>
where
    E: ElementExt + 'a,
{
    mock_terminal_render_loop_with_profile(e, config, None)
}

pub(crate) fn mock_terminal_render_loop_with_profile<'a, E>(
    e: &'a mut E,
    config: MockTerminalConfig,
    frame_profile: Option<FrameProfileCallback<'a>>,
) -> MockTerminalRenderLoop<'a>
where
    E: ElementExt + 'a,
{
    let (term, output) = Terminal::mock(config);
    MockTerminalRenderLoop {
        // No throttling for mock terminals: tests rely on deterministic, immediate
        // frame production.
        render_loop: terminal_render_loop(e, term, None, frame_profile).boxed_local(),
        render_loop_is_done: false,
        output,
    }
}

#[cfg(test)]
mod tests;
