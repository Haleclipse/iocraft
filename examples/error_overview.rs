//! Demonstrates the CC Ink-style error overlay used by `ErrorBoundary`.
//!
//! Running this example intentionally panics inside a child component. The
//! boundary catches it and renders a red `ERROR` badge, source location, and a
//! source excerpt instead of letting the panic corrupt terminal output.

use iocraft::prelude::*;

#[component]
fn RiskyWidget() -> impl Into<AnyElement<'static>> {
    if std::env::var_os("IOCRAFT_ERROR_OVERVIEW_OK").is_none() {
        panic!("simulated component failure");
    }

    element!(Text(content: "set IOCRAFT_ERROR_OVERVIEW_OK=1 to skip the demo panic"))
}

fn main() {
    element! {
        ErrorBoundary {
            RiskyWidget
        }
    }
    .print();
}
