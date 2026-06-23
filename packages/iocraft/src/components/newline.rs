use crate::{Component, ComponentUpdater, Hooks, Props};
use taffy::{geometry::Size, style::Dimension};

/// Props for [`Newline`].
#[derive(Props)]
pub struct NewlineProps {
    /// Number of blank terminal rows to reserve. Defaults to `1`.
    pub count: i32,
}

impl Default for NewlineProps {
    fn default() -> Self {
        Self { count: 1 }
    }
}

/// Adds one or more blank rows to a layout.
///
/// This is the iocraft counterpart to CC Ink's `<Newline count={...} />`.
/// iocraft text nodes are string-based rather than child-based, so this
/// component is used directly in normal component trees to create vertical
/// spacing without rendering visible cells.
#[derive(Default)]
pub struct Newline {
    count: u16,
}

impl Component for Newline {
    type Props<'a> = NewlineProps;

    fn new(_props: &Self::Props<'_>) -> Self {
        Self::default()
    }

    fn update(
        &mut self,
        props: &mut Self::Props<'_>,
        _hooks: Hooks,
        updater: &mut ComponentUpdater,
    ) {
        self.count = props.count.max(0) as u16;
        updater.set_layout_style(taffy::style::Style {
            size: Size {
                width: Dimension::length(0.0),
                height: Dimension::length(self.count as f32),
            },
            flex_shrink: 0.0,
            ..Default::default()
        });
    }
}

#[cfg(test)]
mod tests {
    use crate::prelude::*;

    #[test]
    fn test_newline_reserves_blank_rows() {
        let mut app = element! {
            View(flex_direction: FlexDirection::Column) {
                Text(content: "A")
                Newline(count: 2)
                Text(content: "B")
            }
        };
        let canvas = app.render(None);
        assert_eq!(canvas.to_string(), "A\n\n\nB\n");
    }

    #[test]
    fn test_newline_default_count_is_one() {
        let mut app = element! {
            View(flex_direction: FlexDirection::Column) {
                Text(content: "A")
                Newline
                Text(content: "B")
            }
        };
        let canvas = app.render(None);
        assert_eq!(canvas.to_string(), "A\n\nB\n");
    }
}
