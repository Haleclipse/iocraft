use crate::{strip_ansi::strip_ansi, Hooks, SystemContext};

use super::UseContext;

mod private {
    pub trait Sealed {}
    impl Sealed for crate::Hooks<'_, '_> {}
}

/// Declaratively set the terminal tab/window title.
///
/// This mirrors the CC Ink fork's `useTerminalTitle(...)`: ANSI escape
/// sequences are stripped before the title is handed to the terminal, and
/// `None` is a no-op that leaves any existing title untouched.
pub trait UseTerminalTitle: private::Sealed {
    /// Sets the terminal title for the current render pass.
    fn use_terminal_title<S>(&mut self, title: S)
    where
        S: Into<String>;

    /// Sets the terminal title when `title` is `Some`, or leaves it untouched
    /// when `None`.
    fn use_terminal_title_opt<S>(&mut self, title: Option<S>)
    where
        S: Into<String>;
}

impl UseTerminalTitle for Hooks<'_, '_> {
    fn use_terminal_title<S>(&mut self, title: S)
    where
        S: Into<String>,
    {
        self.use_terminal_title_opt(Some(title));
    }

    fn use_terminal_title_opt<S>(&mut self, title: Option<S>)
    where
        S: Into<String>,
    {
        let Some(title) = title else {
            return;
        };
        let clean = strip_ansi(&title.into()).into_owned();
        self.use_context_mut::<SystemContext>()
            .set_terminal_title(clean);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prelude::*;

    #[component]
    fn TitleProbe(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        hooks.use_terminal_title("\x1b[31mClean\x1b[0m Title");
        let title = hooks
            .use_context::<SystemContext>()
            .terminal_title()
            .unwrap_or("<none>")
            .to_string();
        element!(Text(content: title))
    }

    #[component]
    fn OptionalTitleProbe(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        hooks.use_terminal_title("Original");
        hooks.use_terminal_title_opt::<String>(None);
        let title = hooks
            .use_context::<SystemContext>()
            .terminal_title()
            .unwrap_or("<none>")
            .to_string();
        element!(Text(content: title))
    }

    #[test]
    fn test_use_terminal_title_strips_ansi_sequences() {
        assert_eq!(element!(TitleProbe).to_string(), "Clean Title\n");
    }

    #[test]
    fn test_use_terminal_title_none_is_noop() {
        assert_eq!(element!(OptionalTitleProbe).to_string(), "Original\n");
    }
}
