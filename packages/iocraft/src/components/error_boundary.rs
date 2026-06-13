use crate::{
    components::{BorderStyle, Text, View},
    element, AnyElement, Color, Component, ComponentUpdater, Hooks, Props, Weight,
};
use std::panic::{catch_unwind, AssertUnwindSafe};

/// The props which can be passed to the [`ErrorBoundary`] component.
#[non_exhaustive]
#[derive(Default, Props)]
pub struct ErrorBoundaryProps<'a> {
    /// The children to render. If any child panics during update, the boundary
    /// catches it and renders an error message instead.
    pub children: Vec<AnyElement<'a>>,
}

/// `ErrorBoundary` catches panics in its child subtree and renders an error
/// message instead of letting the panic crash the entire TUI.
///
/// This is iocraft's equivalent of ink's `<ErrorBoundary>` (and React's error
/// boundary pattern). When a child component panics during the update phase,
/// the boundary captures the panic payload, replaces the subtree with a
/// readable error display, and lets the rest of the application continue.
///
/// # Limitations
///
/// - Only catches panics during the **update** phase (component function
///   execution + `update_children`). Panics during layout computation or the
///   draw phase are not caught — those are framework-internal and would leave
///   state inconsistent if swallowed.
/// - Uses `std::panic::catch_unwind`, which cannot catch `panic = "abort"` or
///   panics from FFI. In practice, iocraft components written in safe Rust will
///   produce catchable panics.
/// - After catching a panic, the child subtree is replaced wholesale — there is
///   no recovery or retry. To reset, the parent must unmount and remount the
///   boundary (e.g. by toggling a key).
///
/// # Example
///
/// ```
/// # use iocraft::prelude::*;
/// #[component]
/// fn Risky() -> impl Into<AnyElement<'static>> {
///     // This would normally crash the entire app:
///     // panic!("something went wrong");
///     element! { Text(content: "all good") }
/// }
///
/// # fn _example() -> impl Into<AnyElement<'static>> {
/// element! {
///     ErrorBoundary {
///         Risky
///     }
/// }
/// # }
/// ```
#[derive(Default)]
pub struct ErrorBoundary {
    error: Option<String>,
}

fn format_panic(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        s.to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic".to_string()
    }
}

impl Component for ErrorBoundary {
    type Props<'a> = ErrorBoundaryProps<'a>;

    fn new(_props: &Self::Props<'_>) -> Self {
        Self::default()
    }

    fn update(
        &mut self,
        props: &mut Self::Props<'_>,
        _hooks: Hooks,
        updater: &mut ComponentUpdater,
    ) {
        // Attempt to update the child subtree. If any child panics, we catch it
        // and display the error instead.
        let children = std::mem::take(&mut props.children);
        let result = catch_unwind(AssertUnwindSafe(|| {
            updater.update_children(children, None);
        }));

        match result {
            Ok(()) => {
                self.error = None;
            }
            Err(panic_payload) => {
                let msg = format_panic(panic_payload);
                self.error = Some(msg.clone());
                // Render a fallback error display in place of the crashed subtree.
                let fallback = element! {
                    View(
                        border_style: BorderStyle::Round,
                        border_color: Color::Red,
                    ) {
                        Text(
                            content: format!("Error: {msg}"),
                            color: Color::Red,
                            weight: Weight::Bold,
                        )
                    }
                };
                updater.update_children(std::iter::once(fallback), None);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::prelude::*;

    #[component]
    fn Safe() -> impl Into<AnyElement<'static>> {
        element!(Text(content: "safe"))
    }

    #[component]
    fn Exploding() -> impl Into<AnyElement<'static>> {
        if true {
            panic!("boom");
        }
        element!(View)
    }

    #[test]
    fn test_error_boundary_passes_through_when_no_panic() {
        let output = element! {
            ErrorBoundary {
                Safe
            }
        }
        .to_string();
        assert!(output.contains("safe"), "should render child: {output:?}");
    }

    #[test]
    fn test_error_boundary_catches_child_panic() {
        let output = element! {
            ErrorBoundary {
                Exploding
            }
        }
        .to_string();
        assert!(
            output.contains("Error: boom"),
            "should render error fallback: {output:?}"
        );
    }

    #[test]
    fn test_error_boundary_isolates_from_siblings() {
        let output = element! {
            View(flex_direction: FlexDirection::Column) {
                ErrorBoundary {
                    Exploding
                }
                Text(content: "still here")
            }
        }
        .to_string();
        assert!(
            output.contains("Error: boom"),
            "boundary catches: {output:?}"
        );
        assert!(
            output.contains("still here"),
            "sibling survives: {output:?}"
        );
    }
}
