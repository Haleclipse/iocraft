use crate::{
    components::{ErrorLocation, ErrorOverview},
    element, AnyElement, Component, ComponentUpdater, Hooks, Props,
};
use std::{
    any::Any,
    panic::{catch_unwind, AssertUnwindSafe},
    sync::{Arc, Mutex},
};

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

static PANIC_HOOK_LOCK: Mutex<()> = Mutex::new(());

#[derive(Clone, Debug)]
struct CapturedPanic {
    message: String,
    location: Option<ErrorLocation>,
}

fn panic_payload_message(payload: &(dyn Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        s.to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic".to_string()
    }
}

fn format_panic(payload: Box<dyn Any + Send>) -> String {
    panic_payload_message(&*payload)
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
        //
        // We temporarily install a silent panic hook so the default hook doesn't
        // print the panic message to stderr (which would corrupt the TUI output).
        // The previous hook is restored immediately after catch_unwind returns.
        let captured_panic = Arc::new(Mutex::new(None::<CapturedPanic>));
        let captured_for_hook = captured_panic.clone();
        // The panic hook is global. Serialize hook replacement so concurrent
        // ErrorBoundary instances/tests cannot restore each other's hook while
        // another subtree is still unwinding.
        let _hook_guard = PANIC_HOOK_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let prev_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let location = info.location().map(|location| ErrorLocation {
                file: location.file().to_string(),
                line: location.line() as usize,
                column: Some(location.column() as usize),
            });
            let captured = CapturedPanic {
                message: panic_payload_message(info.payload()),
                location,
            };
            *captured_for_hook
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(captured);
        }));
        let result = catch_unwind(AssertUnwindSafe(|| {
            updater.update_children(props.children.iter_mut(), None);
        }));
        std::panic::set_hook(prev_hook);

        match result {
            Ok(()) => {
                self.error = None;
            }
            Err(panic_payload) => {
                let captured = captured_panic
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .clone();
                let msg = captured
                    .as_ref()
                    .map(|panic| panic.message.clone())
                    .unwrap_or_else(|| format_panic(panic_payload));
                let location = captured.and_then(|panic| panic.location);
                self.error = Some(msg.clone());
                // Render a CC Ink-style fallback error display in place of the
                // crashed subtree, preserving the panic location when Rust's
                // panic hook provides one.
                let fallback = element! {
                    ErrorOverview(message: msg, location: location)
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
            output.contains("ERROR  boom"),
            "should render CC Ink-style error fallback: {output:?}"
        );
        assert!(
            output.contains("error_boundary.rs:"),
            "panic source location should be surfaced: {output:?}"
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
            output.contains("ERROR  boom"),
            "boundary catches: {output:?}"
        );
        assert!(
            output.contains("still here"),
            "sibling survives: {output:?}"
        );
    }
}
