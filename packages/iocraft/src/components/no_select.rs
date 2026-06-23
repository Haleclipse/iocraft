use crate::{component, components::View, element, AnyElement, Props};

/// Props for [`NoSelect`].
#[non_exhaustive]
#[derive(Default, Props)]
pub struct NoSelectProps<'a> {
    /// The children to render inside the non-selectable region.
    pub children: Vec<AnyElement<'a>>,

    /// Extend the exclusion zone from terminal column 0 through this wrapper's
    /// right edge for every row it occupies.
    ///
    /// This is useful for gutters rendered inside an indented container: a
    /// multi-row drag should not copy the container's leading indent on rows
    /// below the prefix.
    pub from_left_edge: bool,
}

/// Marks its contents as non-selectable in fullscreen text selection.
///
/// This is the iocraft counterpart to the CC Ink fork's `<NoSelect>` wrapper.
/// Cells inside this component are skipped by selection highlight, copy, search
/// highlight, URL fallback, and related screen-buffer operations while terminal
/// output remains unchanged.
#[component]
pub fn NoSelect<'a>(props: &mut NoSelectProps<'a>) -> impl Into<AnyElement<'a>> {
    element! {
        View(
            no_select: !props.from_left_edge,
            no_select_from_left_edge: props.from_left_edge,
        ) {
            #(props.children.iter_mut())
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::prelude::*;

    #[test]
    fn test_no_select_component_marks_child_region_without_output_changes() {
        let mut element = element! {
            NoSelect {
                Text(content: "gutter")
            }
        };
        let canvas = element.render(None);

        assert_eq!(canvas.to_string(), "gutter\n");
        for col in 0..6 {
            assert!(canvas.is_no_select(col, 0), "col {col} should be noSelect");
        }
        assert!(!canvas.is_no_select(6, 0));
    }

    #[test]
    fn test_no_select_from_left_edge_marks_indent_through_right_edge() {
        let mut element = element! {
            View(margin_left: 3) {
                NoSelect(from_left_edge: true) {
                    Text(content: "##")
                }
            }
        };
        let canvas = element.render(None);

        assert_eq!(canvas.to_string(), "   ##\n");
        for col in 0..5 {
            assert!(canvas.is_no_select(col, 0), "col {col} should be noSelect");
        }
        assert!(!canvas.is_no_select(5, 0));
    }
}
