use core::{
    any::{Any, TypeId},
    cell::{Ref, RefCell, RefMut},
    mem,
};

/// The system context, which is always available to all components.
pub struct SystemContext {
    should_exit: bool,
    mouse_capture: Option<bool>,
    keyboard_enhancement_flags: Option<crate::KeyboardEnhancementFlags>,
    terminal_title: Option<String>,
}

impl SystemContext {
    pub(crate) fn new() -> Self {
        Self {
            should_exit: false,
            mouse_capture: None,
            keyboard_enhancement_flags: None,
            terminal_title: None,
        }
    }

    /// If called from a component that is being dynamically rendered, this will cause the render
    /// loop to exit and return to the caller after the current render pass.
    pub fn exit(&mut self) {
        self.should_exit = true;
    }

    pub(crate) fn should_exit(&self) -> bool {
        self.should_exit
    }

    /// Toggles mouse capture on the terminal. If called from a component that is being dynamically
    /// rendered, mouse capture will be enabled or disabled after the current render pass.
    pub fn set_mouse_capture(&mut self, enabled: bool) {
        self.mouse_capture = Some(enabled);
    }

    pub(crate) fn mouse_capture(&self) -> Option<bool> {
        self.mouse_capture
    }

    /// Requests a specific set of keyboard enhancement (kitty protocol) flags from the
    /// terminal, replacing the default of
    /// [`KeyboardEnhancementFlags::REPORT_EVENT_TYPES`](crate::KeyboardEnhancementFlags::REPORT_EVENT_TYPES).
    ///
    /// For example, adding `DISAMBIGUATE_ESCAPE_CODES` lets supporting terminals
    /// distinguish key combinations that are conflated in the legacy protocol (such as
    /// `Ctrl+I` vs `Tab`, or `Esc` vs escape sequences). The flags are applied after
    /// the current render pass; terminals without kitty protocol support ignore them.
    pub fn set_keyboard_enhancement_flags(&mut self, flags: crate::KeyboardEnhancementFlags) {
        self.keyboard_enhancement_flags = Some(flags);
    }

    pub(crate) fn keyboard_enhancement_flags(&self) -> Option<crate::KeyboardEnhancementFlags> {
        self.keyboard_enhancement_flags
    }

    /// Sets the terminal window title (OSC 0). The title is applied after the current
    /// render pass. Call this on every render to keep the title up-to-date, or once to
    /// set it and leave it — terminals retain the title until it is changed again.
    pub fn set_terminal_title(&mut self, title: impl Into<String>) {
        self.terminal_title = Some(title.into());
    }

    pub(crate) fn terminal_title(&self) -> Option<&str> {
        self.terminal_title.as_deref()
    }
}

/// A context that can be passed to components.
pub enum Context<'a> {
    /// Provides the context via a mutable reference. Children will be able to get mutable or
    /// immutable references to the context.
    Mut(&'a mut (dyn Any + Send + Sync)),
    /// Provides the context via an immutable reference. Children will not be able to get a mutable
    /// reference to the context.
    Ref(&'a (dyn Any + Send + Sync)),
    /// Provides the context via an owned value. Children will be able to get mutable or immutable
    /// references to the context.
    Owned(Box<dyn Any + Send + Sync>),
}

impl<'a> Context<'a> {
    /// Creates a new context from an owned value. Children will be able to get mutable or
    /// immutable references to the context.
    pub fn owned<T: Any + Send + Sync>(context: T) -> Self {
        Context::Owned(Box::new(context))
    }

    /// Creates a new context from a mutable reference. Children will be able to get mutable or
    /// immutable references to the context.
    pub fn from_mut<T: Any + Send + Sync>(context: &'a mut T) -> Self {
        Context::Mut(context)
    }

    /// Creates a new context from an immutable reference. Children will not be able to get a
    /// mutable reference to the context.
    pub fn from_ref<T: Any + Send + Sync>(context: &'a T) -> Self {
        Context::Ref(context)
    }

    #[doc(hidden)]
    pub fn downcast_ref<T: Any>(&self) -> Option<&T> {
        match self {
            Context::Mut(context) => context.downcast_ref::<T>(),
            Context::Ref(context) => context.downcast_ref::<T>(),
            Context::Owned(context) => context.downcast_ref::<T>(),
        }
    }

    #[doc(hidden)]
    pub fn downcast_mut<T: Any>(&mut self) -> Option<&mut T> {
        match self {
            Context::Mut(context) => context.downcast_mut::<T>(),
            Context::Ref(_) => None,
            Context::Owned(context) => context.downcast_mut::<T>(),
        }
    }

    fn value_type_id(&self) -> TypeId {
        match self {
            Context::Mut(context) => (&**context as &dyn Any).type_id(),
            Context::Ref(context) => (&**context as &dyn Any).type_id(),
            Context::Owned(context) => (&**context as &dyn Any).type_id(),
        }
    }

    #[doc(hidden)]
    pub fn borrow(&mut self) -> Context<'_> {
        match self {
            Context::Mut(context) => Context::Mut(*context),
            Context::Ref(context) => Context::Ref(*context),
            Context::Owned(context) => Context::Mut(&mut **context),
        }
    }
}

#[doc(hidden)]
pub struct ContextStack<'a> {
    contexts: Vec<(TypeId, RefCell<Context<'a>>)>,
}

impl<'a> ContextStack<'a> {
    pub(crate) fn root(root_context: &'a mut (dyn Any + Send + Sync)) -> Self {
        let type_id = (&*root_context as &dyn Any).type_id();
        Self {
            contexts: vec![(type_id, RefCell::new(Context::Mut(root_context)))],
        }
    }

    pub(crate) fn with_context<'b, F>(&'b mut self, context: Option<Context<'b>>, f: F)
    where
        F: FnOnce(&mut ContextStack),
    {
        if let Some(context) = context {
            // SAFETY: Mutable references to this struct are invariant over 'a, so in order to
            // append a shorter-lived context, we need to transmute 'a to the shorter lifetime.
            //
            // This is only safe because we don't allow any other changes to the stack, and we
            // revert the stack right after the call.
            let type_id = context.value_type_id();
            let shorter_lived_self =
                unsafe { mem::transmute::<&mut Self, &mut ContextStack<'b>>(self) };
            shorter_lived_self
                .contexts
                .push((type_id, RefCell::new(context)));
            f(shorter_lived_self);
            shorter_lived_self.contexts.pop();
        } else {
            f(self);
        }
    }

    pub fn get_context<T: Any>(&self) -> Option<Ref<'_, T>> {
        for (type_id, context) in self.contexts.iter().rev() {
            if *type_id != TypeId::of::<T>() {
                continue;
            }
            let Ok(context) = context.try_borrow() else {
                return None;
            };
            if let Ok(ret) = Ref::filter_map(context, |context| context.downcast_ref::<T>()) {
                return Some(ret);
            }
        }
        None
    }

    pub fn get_context_mut<T: Any>(&self) -> Option<RefMut<'_, T>> {
        for (type_id, context) in self.contexts.iter().rev() {
            if *type_id != TypeId::of::<T>() {
                continue;
            }
            let Ok(context) = context.try_borrow_mut() else {
                return None;
            };
            if let Ok(ret) = RefMut::filter_map(context, |context| context.downcast_mut::<T>()) {
                return Some(ret);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_value_type_id_tracks_wrapped_type() {
        let by_ref = String::from("ref");
        let mut by_mut = String::from("mut");
        assert_eq!(
            Context::from_ref(&by_ref).value_type_id(),
            TypeId::of::<String>()
        );
        assert_eq!(
            Context::from_mut(&mut by_mut).value_type_id(),
            TypeId::of::<String>()
        );
        assert_eq!(
            Context::owned(String::from("owned")).value_type_id(),
            TypeId::of::<String>()
        );
    }

    #[test]
    fn borrowed_inner_context_does_not_fall_back_to_outer_same_type() {
        let mut outer = String::from("outer");
        let mut inner = String::from("inner");
        let mut stack = ContextStack::root(&mut outer);
        stack.with_context(Some(Context::from_mut(&mut inner)), |stack| {
            let _inner_borrow = stack.get_context_mut::<String>().unwrap();
            assert!(stack.get_context::<String>().is_none());
        });
    }
}
