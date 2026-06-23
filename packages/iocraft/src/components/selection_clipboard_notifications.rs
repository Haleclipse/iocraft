use crate::{
    component,
    components::Fragment,
    element,
    hooks::{
        NotificationContext, SelectionClipboardPath, SelectionContext, StdoutHandle,
        UseNotifications, UseOutput, UseSelection,
    },
    AnyElement, Hooks, Props,
};

/// Props for [`SelectionClipboardNotifications`].
#[non_exhaustive]
#[derive(Default, Props)]
pub struct SelectionClipboardNotificationsProps<'a> {
    /// Children rendered inside this wrapper. Copy-on-select runs after they draw
    /// so the retained root canvas contains the selected text.
    pub children: Vec<AnyElement<'a>>,
    /// Selection context to observe. Defaults to the nearest selection context.
    pub selection: Option<SelectionContext>,
    /// Stdout handle used for OSC 52 clipboard writes. Defaults to this component's output handle.
    pub stdout: Option<StdoutHandle>,
    /// Notification context used for copy toasts. Defaults to the nearest provider.
    pub notifications: Option<NotificationContext>,
    /// Whether copy feedback is active. Defaults to `true`.
    pub active: Option<bool>,
    /// Toast wording/transport label. Defaults to OSC 52, iocraft's clipboard path.
    pub clipboard_path: Option<SelectionClipboardPath>,
}

/// Wires fullscreen selection copy to clipboard notifications.
///
/// This small component extracts the CC app-layer combination used by
/// `ScrollKeybindingHandler`: copy-on-select writes the clipboard and shows a
/// toast without clearing the highlight, while legacy Ctrl+C copies and clears
/// an active selection. The component renders no visible UI; mount a
/// [`NotificationViewport`](crate::components::NotificationViewport) under a
/// [`NotificationProvider`](crate::components::NotificationProvider) to display
/// the toast.
#[component]
pub fn SelectionClipboardNotifications<'a>(
    mut hooks: Hooks,
    props: &mut SelectionClipboardNotificationsProps<'a>,
) -> impl Into<AnyElement<'a>> {
    let (default_stdout, _) = hooks.use_output();
    let selection = props.selection.unwrap_or_else(|| hooks.use_selection());
    let stdout = props.stdout.clone().unwrap_or(default_stdout);
    let notifications = props
        .notifications
        .unwrap_or_else(|| hooks.use_notifications());
    hooks.use_selection_copy_notifications(
        selection,
        stdout,
        notifications,
        props.active.unwrap_or(true),
        props.clipboard_path.unwrap_or_default(),
    );

    element! {
        Fragment {
            #(props.children.iter_mut())
        }
    }
}
