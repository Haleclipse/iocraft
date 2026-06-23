use crate::{
    hooks::{State, UseState, UseTerminalEvents},
    ComponentUpdater, Hook, Hooks, TerminalEvent,
};
use crossterm::terminal;

mod private {
    pub trait Sealed {}
    impl Sealed for crate::Hooks<'_, '_> {}
}

/// `UseTerminalSize` is a hook that returns the current terminal size.
///
/// This mirrors CC Ink's `TerminalSizeContext`: during a render loop it syncs
/// from the render-owned terminal size even before a resize event is received,
/// so tests, fullscreen layouts, and terminal-viewport hooks see the same size
/// that the renderer uses for that frame.
pub trait UseTerminalSize: private::Sealed {
    /// Returns the current terminal size as a tuple of `(width, height)`.
    fn use_terminal_size(&mut self) -> (u16, u16);
}

impl UseTerminalSize for Hooks<'_, '_> {
    fn use_terminal_size(&mut self) -> (u16, u16) {
        let mut size = self.use_state(|| terminal::size().unwrap_or((0, 0)));
        let hook = self.use_hook(|| UseTerminalSizeImpl { size });
        hook.size = size;
        self.use_terminal_events(move |event| {
            if let TerminalEvent::Resize(width, height) = event {
                size.set((width, height));
            }
        });
        size.get()
    }
}

struct UseTerminalSizeImpl {
    size: State<(u16, u16)>,
}

impl Hook for UseTerminalSizeImpl {
    fn post_component_update(&mut self, updater: &mut ComponentUpdater) {
        let Some(size) = updater.terminal_mut().and_then(|terminal| terminal.size()) else {
            return;
        };
        if self.size.get() != size {
            self.size.set(size);
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::prelude::*;
    use futures::stream::StreamExt;
    use macro_rules_attribute::apply;
    use smol_macros::test;

    #[component]
    fn MyComponent(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let (width, height) = hooks.use_terminal_size();

        if width == 100 && height == 40 {
            system.exit();
        }

        element! {
            Text(content: format!("{}x{}", width, height))
        }
    }

    #[apply(test!)]
    async fn test_use_terminal_size() {
        let actual = element!(MyComponent)
            .mock_terminal_render_loop(MockTerminalConfig::with_events(futures::stream::iter(
                vec![TerminalEvent::Resize(100, 40)],
            )))
            .map(|c| c.to_string())
            .collect::<Vec<_>>()
            .await;
        assert_eq!(actual.last().unwrap(), "100x40\n");
    }

    #[component]
    fn MockSizeComponent(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let (width, height) = hooks.use_terminal_size();
        if (width, height) == (20, 3) {
            system.exit();
        }
        element!(Text(content: format!("{}x{}", width, height)))
    }

    #[apply(test!)]
    async fn test_use_terminal_size_syncs_render_loop_terminal_size_without_resize() {
        let actual = element!(MockSizeComponent)
            .mock_terminal_render_loop(MockTerminalConfig::default().with_size(20, 3))
            .map(|c| c.to_string())
            .collect::<Vec<_>>()
            .await;
        assert_eq!(actual.last().unwrap(), "20x3\n");
    }
}
