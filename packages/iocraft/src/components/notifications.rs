use crate::{
    component,
    components::{ContextProvider, Fragment, Text, View},
    element,
    hooks::{create_notification_context, UseNotificationExpiry, UseNotifications},
    AnyElement, Color, Context, Hooks, Props,
};

/// Props for [`NotificationProvider`].
#[non_exhaustive]
#[derive(Default, Props)]
pub struct NotificationProviderProps<'a> {
    /// Children that should be able to access [`NotificationContext`](crate::hooks::NotificationContext).
    pub children: Vec<AnyElement<'a>>,
}

/// Provides a CC-style notification queue to descendants.
///
/// This mirrors the stateful provider behind Claude Code's `useNotifications()`:
/// descendants call [`UseNotifications::use_notifications`] to enqueue or
/// remove toasts, and a [`NotificationViewport`] renders the current item.
#[component]
pub fn NotificationProvider<'a>(
    mut hooks: Hooks,
    props: &mut NotificationProviderProps<'a>,
) -> impl Into<AnyElement<'a>> {
    let notifications = create_notification_context(&mut hooks);

    element! {
        ContextProvider(value: Context::owned(notifications)) {
            #(props.children.iter_mut())
        }
    }
}

/// Props for [`NotificationViewport`].
#[non_exhaustive]
#[derive(Default, Props)]
pub struct NotificationViewportProps {
    /// Prefix inserted before notification text, for example `"✓ "`.
    pub prefix: Option<String>,
    /// Fallback color used when the notification itself has no color.
    pub color: Option<Color>,
}

/// Renders the current notification and expires it on its timeout.
///
/// Mount this component anywhere under [`NotificationProvider`]. When no
/// notification is active it renders as a zero-height fragment; when active it
/// renders one line with `no_select` metadata so selection copy skips toast UI.
#[component]
pub fn NotificationViewport(
    mut hooks: Hooks,
    props: &NotificationViewportProps,
) -> impl Into<AnyElement<'static>> {
    let notifications = hooks.use_notifications();
    hooks.use_notification_expiry(notifications);

    let rows: Vec<AnyElement<'static>> = notifications
        .current_notification()
        .map(|notification| {
            let prefix = props.prefix.clone().unwrap_or_default();
            let color = notification.color.or(props.color).unwrap_or(Color::Grey);
            element! {
                View(height: 1, no_select: true) {
                    Text(color: color, content: format!("{prefix}{}", notification.text))
                }
            }
            .into_any()
        })
        .into_iter()
        .collect();

    element! {
        Fragment {
            #(rows)
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::prelude::*;
    use futures::StreamExt;

    #[component]
    fn NotificationProbe(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let notifications = hooks.use_notifications();

        if notifications.current_notification().is_none() {
            notifications.add_notification(Notification::new(
                "low",
                "queued low",
                NotificationPriority::Low,
            ));
            notifications.add_notification(Notification::immediate("immediate", "right now"));
        }
        system.exit();

        element! {
            View(flex_direction: FlexDirection::Column) {
                NotificationViewport(prefix: Some("! ".to_string()))
                Text(content: format!("queued={}", notifications.queued_len()))
            }
        }
    }

    #[test]
    fn test_notification_provider_immediate_preempts_current_and_renders_viewport() {
        let canvases: Vec<_> = smol::block_on(
            element! {
                NotificationProvider {
                    NotificationProbe
                }
            }
            .mock_terminal_render_loop(MockTerminalConfig::default())
            .collect(),
        );
        let output = canvases.last().unwrap().to_string();
        assert!(
            output.contains("! right now"),
            "immediate toast should render: {output:?}"
        );
        assert!(
            output.contains("queued=1"),
            "preempted low toast should be requeued: {output:?}"
        );
    }
}
