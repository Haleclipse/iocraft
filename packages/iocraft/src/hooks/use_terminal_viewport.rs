use crate::{ComponentDrawer, Hook, Hooks};

mod private {
    pub trait Sealed {}
    impl Sealed for crate::Hooks<'_, '_> {}
}

/// Visibility of a component relative to the terminal's live viewport.
///
/// This mirrors the CC Ink fork's `useTerminalViewport()` entry. Visibility is
/// computed from the component's canvas rect after layout/draw; changing it does
/// not by itself schedule another render. Components that re-render for other
/// reasons can use the latest value to pause offscreen work.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TerminalViewportEntry {
    /// Whether the component intersects the terminal viewport.
    pub is_visible: bool,
}

impl Default for TerminalViewportEntry {
    fn default() -> Self {
        Self { is_visible: true }
    }
}

/// Hook for checking whether the current component is inside the terminal
/// viewport.
pub trait UseTerminalViewport<'a>: private::Sealed {
    /// Returns the current component's terminal-viewport entry.
    ///
    /// This uses [`UseTerminalSize`](crate::hooks::UseTerminalSize) for the row
    /// count. If the row count is unavailable/zero, the component is treated as
    /// visible so tests/non-TTY rendering do not accidentally disable content.
    fn use_terminal_viewport(&mut self) -> TerminalViewportEntry;

    /// Same as [`UseTerminalViewport::use_terminal_viewport`], but with an
    /// explicit terminal row count. This is useful for tests or custom viewport
    /// owners.
    fn use_terminal_viewport_with_rows(&mut self, rows: u16) -> TerminalViewportEntry;
}

impl UseTerminalViewport<'_> for Hooks<'_, '_> {
    fn use_terminal_viewport(&mut self) -> TerminalViewportEntry {
        let (_, rows) = crate::hooks::UseTerminalSize::use_terminal_size(self);
        self.use_terminal_viewport_with_rows(rows)
    }

    fn use_terminal_viewport_with_rows(&mut self, rows: u16) -> TerminalViewportEntry {
        let hook = self.use_hook(UseTerminalViewportImpl::default);
        hook.terminal_rows = rows;
        hook.entry
    }
}

#[derive(Default)]
struct UseTerminalViewportImpl {
    entry: TerminalViewportEntry,
    terminal_rows: u16,
}

impl Hook for UseTerminalViewportImpl {
    fn pre_component_draw(&mut self, drawer: &mut ComponentDrawer) {
        let rows = self.terminal_rows as isize;
        if rows <= 0 {
            self.entry = TerminalViewportEntry { is_visible: true };
            return;
        }

        let position = drawer.canvas_position();
        let size = drawer.size();
        let absolute_top = position.y as isize;
        let bottom = absolute_top + size.height as isize;
        let screen_height = drawer.root_canvas_mut().height() as isize;

        // Match CC Ink/log-update's cursor-restore scroll model. If the live
        // canvas is taller than the terminal viewport, restoring the cursor at
        // frame end can push one additional row into native scrollback; content
        // at that boundary should be considered offscreen as well.
        let cursor_restore_scroll = if screen_height > rows { 1 } else { 0 };
        let viewport_top = (screen_height - rows).max(0) + cursor_restore_scroll;
        let viewport_bottom = viewport_top + rows;
        self.entry = TerminalViewportEntry {
            is_visible: bottom > viewport_top && absolute_top < viewport_bottom,
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prelude::*;
    use futures::StreamExt;

    #[component]
    fn TopProbe(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let entry = hooks.use_terminal_viewport_with_rows(3);
        element!(Text(content: format!("top={}", entry.is_visible)))
    }

    #[component]
    fn BottomProbe(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let entry = hooks.use_terminal_viewport_with_rows(3);
        element!(Text(content: format!("bottom={}", entry.is_visible)))
    }

    #[component]
    fn ViewportProbeApp(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let mut tick = hooks.use_state(|| 0u8);
        if tick.get() < 2 {
            tick += 1;
        } else {
            system.exit();
        }

        element! {
            View(flex_direction: FlexDirection::Column) {
                TopProbe
                Text(content: "row 1")
                Text(content: "row 2")
                Text(content: "row 3")
                Text(content: "row 4")
                Text(content: "row 5")
                BottomProbe
                Text(content: "row 7")
            }
        }
    }

    #[test]
    fn test_use_terminal_viewport_matches_scrollback_boundary() {
        let canvases: Vec<_> = smol::block_on(
            element!(ViewportProbeApp)
                .mock_terminal_render_loop(MockTerminalConfig::default())
                .collect(),
        );
        let rendered = canvases.last().unwrap().to_string();
        assert!(
            rendered.starts_with("top=false"),
            "top row should be treated as native scrollback: {rendered:?}"
        );
        assert!(
            rendered.contains("bottom=true"),
            "bottom probe should remain in the live viewport: {rendered:?}"
        );
    }
}
