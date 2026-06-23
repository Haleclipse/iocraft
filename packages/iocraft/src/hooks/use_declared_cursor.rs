use crate::{ComponentDrawer, Hook, Hooks};

mod private {
    pub trait Sealed {}
    impl Sealed for crate::Hooks<'_, '_> {}
}

/// Hook for declaring where iocraft should park the physical terminal cursor
/// after a frame is rendered.
///
/// This is the Rust counterpart to the CC Ink fork's `useDeclaredCursor(...)`.
/// It lets custom input-like components anchor IME preedit text and
/// accessibility tools to their caret without owning terminal positioning.
pub trait UseDeclaredCursor<'a>: private::Sealed {
    /// Declares an active visible cursor at `column,line`, relative to the
    /// current component's canvas rect.
    fn use_declared_cursor(&mut self, line: isize, column: isize, active: bool);

    /// Declares a cursor at `column,line`, relative to the current component's
    /// canvas rect, with an explicit visibility flag.
    fn use_declared_cursor_with_visibility(
        &mut self,
        line: isize,
        column: isize,
        active: bool,
        visible: bool,
    );
}

impl UseDeclaredCursor<'_> for Hooks<'_, '_> {
    fn use_declared_cursor(&mut self, line: isize, column: isize, active: bool) {
        self.use_declared_cursor_with_visibility(line, column, active, true);
    }

    fn use_declared_cursor_with_visibility(
        &mut self,
        line: isize,
        column: isize,
        active: bool,
        visible: bool,
    ) {
        let hook = self.use_hook(UseDeclaredCursorImpl::default);
        hook.line = line;
        hook.column = column;
        hook.active = active;
        hook.visible = visible;
    }
}

#[derive(Default)]
struct UseDeclaredCursorImpl {
    line: isize,
    column: isize,
    active: bool,
    visible: bool,
}

impl Hook for UseDeclaredCursorImpl {
    fn pre_component_draw(&mut self, drawer: &mut ComponentDrawer) {
        if self.active {
            drawer
                .canvas()
                .declare_cursor(self.column, self.line, self.visible);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{canvas::CursorDeclaration, prelude::*};

    #[component]
    fn CursorOwner(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        hooks.use_declared_cursor(0, 2, true);
        element!(Text(content: "abcd"))
    }

    #[component]
    fn HiddenCursorOwner(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        hooks.use_declared_cursor_with_visibility(0, 1, true, false);
        element!(Text(content: "abcd"))
    }

    #[component]
    fn InactiveCursorOwner(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        hooks.use_declared_cursor(0, 2, false);
        element!(Text(content: "abcd"))
    }

    #[test]
    fn test_use_declared_cursor_declares_component_relative_cursor() {
        let mut element = element!(View(margin_left: 3) { CursorOwner });
        let canvas = element.render(None);
        assert_eq!(
            canvas.cursor_declaration(),
            Some(CursorDeclaration {
                x: 5,
                y: 0,
                visible: true,
            })
        );
    }

    #[test]
    fn test_use_declared_cursor_visibility_and_inactive_noop() {
        let mut hidden = element!(HiddenCursorOwner);
        let hidden = hidden.render(None);
        assert_eq!(
            hidden.cursor_declaration(),
            Some(CursorDeclaration {
                x: 1,
                y: 0,
                visible: false,
            })
        );

        let mut inactive = element!(InactiveCursorOwner);
        let inactive = inactive.render(None);
        assert_eq!(inactive.cursor_declaration(), None);
    }
}
