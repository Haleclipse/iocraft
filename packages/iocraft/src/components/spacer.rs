use crate::{Component, ComponentUpdater, Hooks, Props};
use taffy::style::Style;

/// Props for [`Spacer`].
#[derive(Default, Props)]
pub struct SpacerProps;

/// Flexible empty space that expands along the parent layout's main axis.
///
/// This mirrors CC Ink's `<Spacer />`, which is shorthand for a box with
/// `flexGrow: 1`. Use it between siblings to push them apart without rendering
/// any visible cells.
#[derive(Default)]
pub struct Spacer;

impl Component for Spacer {
    type Props<'a> = SpacerProps;

    fn new(_props: &Self::Props<'_>) -> Self {
        Self
    }

    fn update(
        &mut self,
        _props: &mut Self::Props<'_>,
        _hooks: Hooks,
        updater: &mut ComponentUpdater,
    ) {
        updater.set_layout_style_if_changed(Style {
            flex_grow: 1.0,
            ..Default::default()
        });
    }
}

#[cfg(test)]
mod tests {
    use crate::prelude::*;

    #[test]
    fn test_spacer_expands_along_row_main_axis() {
        let mut app = element! {
            View(width: 6, flex_direction: FlexDirection::Row) {
                Text(content: "A")
                Spacer
                Text(content: "B")
            }
        };
        let canvas = app.render(None);
        assert_eq!(canvas.to_string(), "A    B\n");
    }
}
