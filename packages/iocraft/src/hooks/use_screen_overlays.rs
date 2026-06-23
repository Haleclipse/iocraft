use super::{SearchHighlightContext, SelectionContext};
use crate::{ComponentDrawer, Hook, Hooks};

mod private {
    pub trait Sealed {}
    impl Sealed for crate::Hooks<'_, '_> {}
}

#[derive(Default)]
struct UseScreenOverlaysImpl {
    selection: SelectionContext,
    search_highlight: SearchHighlightContext,
}

impl Hook for UseScreenOverlaysImpl {
    fn post_component_draw(&mut self, drawer: &mut ComponentDrawer) {
        let canvas = drawer.root_canvas_mut();
        // CC Ink applies app-level overlays after rendering, in this order:
        // selection first, all query matches next, then the current positioned
        // match. Keeping that order centralized prevents hook-call order from
        // changing how overlapping selection/search cells resolve.
        self.selection.apply_overlay(canvas);
        self.search_highlight.apply_overlay(canvas);
    }
}

/// App-level post-render screen overlays.
///
/// This hook mirrors the CC Ink fork's `Ink.render()` overlay pipeline: apply
/// the fullscreen text-selection overlay first, then rendered-screen search
/// highlighting (including the current positioned match) on top. Use this on
/// the component that owns the fullscreen screen buffer instead of registering
/// the individual overlay hooks separately when both features are enabled.
pub trait UseScreenOverlays<'a>: private::Sealed {
    /// Applies selection and search overlays to the root canvas after the
    /// component subtree has drawn.
    fn use_screen_overlays(
        &mut self,
        selection: SelectionContext,
        search_highlight: SearchHighlightContext,
    );
}

impl UseScreenOverlays<'_> for Hooks<'_, '_> {
    fn use_screen_overlays(
        &mut self,
        selection: SelectionContext,
        search_highlight: SearchHighlightContext,
    ) {
        let hook = self.use_hook(UseScreenOverlaysImpl::default);
        hook.selection = selection;
        hook.search_highlight = search_highlight;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{prelude::*, SelectionController, TextMatchPosition};
    use crossterm::style::Colored;
    use futures::StreamExt;

    #[component]
    fn ScreenOverlaysApp(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let selection = create_selection_context(&mut hooks);
        let search = create_search_highlight_context(&mut hooks);

        if !selection.has_selection() {
            let mut controller = SelectionController::new();
            controller.selection_mut().start(0, 0);
            controller.selection_mut().update(3, 0);
            controller.selection_mut().finish();
            selection.set_controller(controller);
        }
        search.set_query("lazy");
        search.set_positions(
            vec![
                TextMatchPosition {
                    row: 0,
                    col: 0,
                    len: 4,
                },
                TextMatchPosition {
                    row: 0,
                    col: 5,
                    len: 4,
                },
            ],
            0,
            1,
        );

        hooks.use_screen_overlays(selection, search);
        system.exit();

        element!(Text(content: "lazy lazy"))
    }

    #[test]
    fn test_use_screen_overlays_applies_ink_selection_search_order() {
        let canvases: Vec<_> = smol::block_on(
            element!(ScreenOverlaysApp)
                .mock_terminal_render_loop(MockTerminalConfig::default())
                .collect(),
        );
        let canvas = canvases.last().unwrap();

        let selected_query = canvas.resolved_text_style(0, 0).unwrap();
        assert!(
            selected_query.invert,
            "query highlight should compose on top of selection"
        );
        let mut ansi = Vec::new();
        canvas.write_ansi(&mut ansi).unwrap();
        let ansi = String::from_utf8_lossy(&ansi);
        assert!(
            ansi.contains(&format!("{}", Colored::BackgroundColor(Color::Blue))),
            "selection background should survive query highlighting: {ansi:?}"
        );

        let current = canvas.resolved_text_style(5, 0).unwrap();
        assert!(current.underline);
        assert_eq!(current.weight, Weight::Bold);
        assert!(current.invert);
        assert_eq!(current.color, Some(Color::Yellow));
    }
}
