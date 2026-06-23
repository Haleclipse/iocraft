use super::{UseContext, UseInterval, UseState};
use crate::{Color, Hooks};
use std::time::{Duration, Instant};

mod private {
    pub trait Sealed {}
    impl Sealed for crate::Hooks<'_, '_> {}
}

const DEFAULT_NOTIFICATION_TIMEOUT: Duration = Duration::from_millis(8000);

/// Priority used by [`NotificationContext`] to pick the next queued toast.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NotificationPriority {
    /// Display as soon as possible, before any queued notification.
    Immediate,
    /// Display before medium and low notifications.
    High,
    /// Default notification priority.
    #[default]
    Medium,
    /// Display after higher-priority queued notifications.
    Low,
}

impl NotificationPriority {
    fn rank(self) -> u8 {
        match self {
            Self::Immediate => 0,
            Self::High => 1,
            Self::Medium => 2,
            Self::Low => 3,
        }
    }
}

/// A queued toast/notification.
///
/// This is a Rust counterpart to the CC app notification object used by
/// `useNotifications()`: it has a stable `key`, priority, optional timeout,
/// optional color, and a list of keys that should be invalidated when it is
/// added.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notification {
    /// Stable notification key. Non-immediate notifications with duplicate
    /// keys are ignored while an existing notification is current or queued.
    pub key: String,
    /// Text displayed by [`NotificationViewport`](crate::components::NotificationViewport).
    pub text: String,
    /// Optional display color for the notification text.
    pub color: Option<Color>,
    /// Queue priority.
    pub priority: NotificationPriority,
    /// Display timeout. `None` uses the CC default of eight seconds.
    pub timeout: Option<Duration>,
    /// Keys of notifications that should be removed from the current slot or
    /// queue when this notification is added.
    pub invalidates: Vec<String>,
}

impl Notification {
    /// Creates a text notification with the given key, text, and priority.
    pub fn new(
        key: impl Into<String>,
        text: impl Into<String>,
        priority: NotificationPriority,
    ) -> Self {
        Self {
            key: key.into(),
            text: text.into(),
            color: None,
            priority,
            timeout: None,
            invalidates: Vec::new(),
        }
    }

    /// Creates an immediate text notification.
    pub fn immediate(key: impl Into<String>, text: impl Into<String>) -> Self {
        Self::new(key, text, NotificationPriority::Immediate)
    }

    /// Creates a medium-priority text notification.
    pub fn medium(key: impl Into<String>, text: impl Into<String>) -> Self {
        Self::new(key, text, NotificationPriority::Medium)
    }

    /// Returns this notification with a display color.
    pub fn with_color(mut self, color: Color) -> Self {
        self.color = Some(color);
        self
    }

    /// Returns this notification with a display timeout.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Returns this notification with one additional invalidated key.
    pub fn invalidating(mut self, key: impl Into<String>) -> Self {
        self.invalidates.push(key.into());
        self
    }
}

#[derive(Clone, Debug)]
struct ActiveNotification {
    notification: Notification,
    shown_at: Instant,
}

#[derive(Clone, Debug, Default)]
struct NotificationRuntimeState {
    current: Option<ActiveNotification>,
    queue: Vec<Notification>,
}

impl NotificationRuntimeState {
    fn process_queue(&mut self) {
        if self.current.is_some() || self.queue.is_empty() {
            return;
        }
        let Some((idx, _)) = self
            .queue
            .iter()
            .enumerate()
            .min_by_key(|(_, notification)| notification.priority.rank())
        else {
            return;
        };
        let notification = self.queue.remove(idx);
        self.current = Some(ActiveNotification {
            notification,
            shown_at: Instant::now(),
        });
    }

    fn add(&mut self, notification: Notification) {
        if notification.priority == NotificationPriority::Immediate {
            let mut queue = Vec::new();
            if let Some(current) = self.current.take() {
                queue.push(current.notification);
            }
            queue.append(&mut self.queue);
            queue.retain(|item| {
                item.priority != NotificationPriority::Immediate
                    && !notification.invalidates.iter().any(|key| key == &item.key)
            });
            self.queue = queue;
            self.current = Some(ActiveNotification {
                notification,
                shown_at: Instant::now(),
            });
            return;
        }

        let duplicate = self
            .current
            .as_ref()
            .is_some_and(|current| current.notification.key == notification.key)
            || self.queue.iter().any(|item| item.key == notification.key);
        if duplicate {
            return;
        }

        let invalidates_current = self.current.as_ref().is_some_and(|current| {
            notification
                .invalidates
                .iter()
                .any(|key| key == &current.notification.key)
        });
        if invalidates_current {
            self.current = None;
        }
        self.queue.retain(|item| {
            item.priority != NotificationPriority::Immediate
                && !notification.invalidates.iter().any(|key| key == &item.key)
        });
        self.queue.push(notification);
        self.process_queue();
    }

    fn remove(&mut self, key: &str) {
        if self
            .current
            .as_ref()
            .is_some_and(|current| current.notification.key == key)
        {
            self.current = None;
        }
        self.queue.retain(|notification| notification.key != key);
        self.process_queue();
    }

    fn expire_current(&mut self) {
        let expired = self.current.as_ref().is_some_and(|current| {
            current.shown_at.elapsed()
                >= current
                    .notification
                    .timeout
                    .unwrap_or(DEFAULT_NOTIFICATION_TIMEOUT)
        });
        if expired {
            self.current = None;
            self.process_queue();
        }
    }
}

/// Copyable handle to the current notification queue.
///
/// Use [`UseNotifications::use_notifications`] inside a
/// [`NotificationProvider`](crate::components::NotificationProvider), then call
/// [`Self::add_notification`] or [`Self::remove_notification`] from event
/// handlers. Outside a provider the handle is disabled and all mutation methods
/// are no-ops.
#[derive(Clone, Copy)]
pub struct NotificationContext {
    state: Option<super::State<NotificationRuntimeState>>,
}

impl Default for NotificationContext {
    fn default() -> Self {
        Self::disabled()
    }
}

impl NotificationContext {
    /// Creates a disabled no-op notification context.
    pub fn disabled() -> Self {
        Self { state: None }
    }

    fn new(state: super::State<NotificationRuntimeState>) -> Self {
        Self { state: Some(state) }
    }

    /// Returns whether this handle is backed by a live provider.
    pub fn is_enabled(&self) -> bool {
        self.state.is_some()
    }

    fn with_ref<R>(&self, f: impl FnOnce(&NotificationRuntimeState) -> R) -> Option<R> {
        let state = self.state?;
        let guard = state.try_read()?;
        Some(f(&guard))
    }

    fn with_mut<R>(&self, f: impl FnOnce(&mut NotificationRuntimeState) -> R) -> Option<R> {
        let mut state = self.state?;
        let mut guard = state.try_write()?;
        Some(f(&mut guard))
    }

    /// Adds a notification to the queue.
    pub fn add_notification(&self, notification: Notification) {
        self.with_mut(|state| state.add(notification));
    }

    /// Removes the current or queued notification with `key`.
    pub fn remove_notification(&self, key: &str) {
        self.with_mut(|state| state.remove(key));
    }

    /// Expires the current notification when its timeout has elapsed.
    pub fn expire_current(&self) {
        self.with_mut(NotificationRuntimeState::expire_current);
    }

    /// Returns the current notification, if one is displayed.
    pub fn current_notification(&self) -> Option<Notification> {
        self.with_ref(|state| {
            state
                .current
                .as_ref()
                .map(|current| current.notification.clone())
        })
        .flatten()
    }

    /// Returns the number of queued notifications waiting behind the current one.
    pub fn queued_len(&self) -> usize {
        self.with_ref(|state| state.queue.len()).unwrap_or(0)
    }
}

/// Creates a notification context owned by the current component.
pub fn create_notification_context(hooks: &mut Hooks<'_, '_>) -> NotificationContext {
    NotificationContext::new(hooks.use_state(NotificationRuntimeState::default))
}

/// Hook for accessing the nearest [`NotificationProvider`](crate::components::NotificationProvider).
pub trait UseNotifications: private::Sealed {
    /// Returns the nearest notification context, or a disabled no-op handle
    /// when no provider is mounted.
    fn use_notifications(&mut self) -> NotificationContext;
}

impl UseNotifications for Hooks<'_, '_> {
    fn use_notifications(&mut self) -> NotificationContext {
        self.try_use_context::<NotificationContext>()
            .map(|context| *context)
            .unwrap_or_else(NotificationContext::disabled)
    }
}

/// Periodically expires the current notification while one is displayed.
pub trait UseNotificationExpiry: private::Sealed {
    /// Wires the CC-style timeout processor for `notifications`.
    fn use_notification_expiry(&mut self, notifications: NotificationContext);
}

impl UseNotificationExpiry for Hooks<'_, '_> {
    fn use_notification_expiry(&mut self, notifications: NotificationContext) {
        let active = notifications.current_notification().is_some();
        self.use_interval(
            move || notifications.expire_current(),
            active.then_some(Duration::from_millis(100)),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notification_runtime_immediate_preempts_and_requeues_current() {
        let mut state = NotificationRuntimeState::default();
        state.add(Notification::new("low", "low", NotificationPriority::Low));
        assert_eq!(state.current.as_ref().unwrap().notification.key, "low");
        state.add(Notification::immediate("now", "now"));
        assert_eq!(state.current.as_ref().unwrap().notification.key, "now");
        assert_eq!(
            state
                .queue
                .iter()
                .map(|n| n.key.as_str())
                .collect::<Vec<_>>(),
            vec!["low"]
        );
    }

    #[test]
    fn notification_runtime_invalidates_current_and_queue() {
        let mut state = NotificationRuntimeState::default();
        state.add(Notification::new("a", "a", NotificationPriority::Low));
        state.add(Notification::new("b", "b", NotificationPriority::Low));
        state.add(
            Notification::new("c", "c", NotificationPriority::High)
                .invalidating("a")
                .invalidating("b"),
        );
        assert_eq!(state.current.as_ref().unwrap().notification.key, "c");
        assert!(state.queue.is_empty());
    }

    #[test]
    fn notification_runtime_expires_current_and_promotes_next() {
        let mut state = NotificationRuntimeState::default();
        state.add(Notification::medium("short", "short").with_timeout(Duration::from_millis(1)));
        state.add(Notification::new("next", "next", NotificationPriority::Low));
        state.current.as_mut().unwrap().shown_at = Instant::now() - Duration::from_millis(10);
        state.expire_current();
        assert_eq!(state.current.as_ref().unwrap().notification.key, "next");
        assert!(state.queue.is_empty());
    }
}
