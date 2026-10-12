//! A thin port over `plaudon-notifications`, so the view never links a backend.
//!
//! `plaudon-notifications` already owns everything that matters about OS
//! notifications: the privacy-safe payload builder, the preference gate, the
//! permission gate, the per-call timeout and the panic containment
//! (migration plan §4.5 row J14). None of that is reimplemented here.
//!
//! What this module adds is a **seam**: [`NotificationPort`] exposes delivery
//! and the current OS permission, so a settings screen can say "macOS has
//! denied notifications" and a test can assert it without the notification
//! backend — and therefore `objc2`/WinRT — being linked into the test binary.
//! The value types are re-exported so a caller needs no second dependency.
//!
//! Permission reporting is the reason the trait exists at all: spec §5.5
//! requires the UI to explain suppression, and
//! [`plaudon_notifications::OsPermission`] is the only honest source for that.
//! A denied permission is shown, never worked around.
//!
//! Clicking a delivered notification comes back through
//! [`crate::activation::from_notification_token`].

use std::fmt;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use crate::lock;

pub use plaudon_notifications::{
    privacy, AttentionReason, BudgetScope, DeliveryOutcome, Dispatcher, NotificationCategory,
    NotificationContext, NotificationRequest, NotificationSettings, NotificationTarget, Notifier,
    OsPermission,
};

/// Deliver notifications and report the OS permission.
#[async_trait]
pub trait NotificationPort: Send + Sync + fmt::Debug {
    /// Hand one payload to the OS, subject to preferences and permission.
    ///
    /// Never fails and never blocks the caller's work: the outcome is a record
    /// of what the OS did, which the caller stores next to the activity entry.
    async fn deliver(&self, request: &NotificationRequest) -> DeliveryOutcome;

    /// The OS permission as last observed, without prompting.
    async fn permission(&self) -> OsPermission;

    /// Prompt for permission if the platform can prompt, and report the result.
    ///
    /// Called from the notification settings pane when the user turns the
    /// master switch on; everywhere else the dispatcher asks at most once, on
    /// the first delivery of an opted-in category.
    async fn request_permission(&self) -> OsPermission;
}

/// The real port: the shared [`Dispatcher`] the application layer built.
#[derive(Debug, Clone)]
pub struct DispatcherNotifications {
    dispatcher: Arc<Dispatcher>,
}

impl DispatcherNotifications {
    /// Wrap the dispatcher the application layer owns.
    #[must_use]
    pub const fn new(dispatcher: Arc<Dispatcher>) -> Self {
        Self { dispatcher }
    }

    /// The wrapped dispatcher, for callers that also need the preference API.
    #[must_use]
    pub fn dispatcher(&self) -> &Arc<Dispatcher> {
        &self.dispatcher
    }
}

#[async_trait]
impl NotificationPort for DispatcherNotifications {
    async fn deliver(&self, request: &NotificationRequest) -> DeliveryOutcome {
        self.dispatcher.notify(request).await
    }

    async fn permission(&self) -> OsPermission {
        self.dispatcher.os_permission().await
    }

    async fn request_permission(&self) -> OsPermission {
        self.dispatcher.request_os_permission().await
    }
}

/// In-memory port for tests: records payloads, reports a scripted permission.
///
/// This is *not* a second notification backend. When a test needs the real
/// gating rules it builds a [`Dispatcher`] over
/// `plaudon_notifications::testing::RecordingNotifier` and wraps it in
/// [`DispatcherNotifications`]; this fake exists for the much more common case
/// of a UI test that only cares what the screen was told.
#[derive(Debug)]
pub struct FakeNotifications {
    permission: Mutex<OsPermission>,
    permission_on_request: Mutex<Option<OsPermission>>,
    delivered: Mutex<Vec<NotificationRequest>>,
    prompts: Mutex<u32>,
}

impl FakeNotifications {
    /// A fake that reports `permission` and delivers everything it is given.
    #[must_use]
    pub fn new(permission: OsPermission) -> Self {
        Self {
            permission: Mutex::new(permission),
            permission_on_request: Mutex::new(None),
            delivered: Mutex::new(Vec::new()),
            prompts: Mutex::new(0),
        }
    }

    /// Script what a permission prompt resolves to.
    #[must_use]
    pub fn granting_on_request(self, permission: OsPermission) -> Self {
        *lock(&self.permission_on_request) = Some(permission);
        self
    }

    /// Change the reported permission, as the user would in system settings.
    pub fn set_permission(&self, permission: OsPermission) {
        *lock(&self.permission) = permission;
    }

    /// Every payload the fake accepted, in order.
    #[must_use]
    pub fn delivered(&self) -> Vec<NotificationRequest> {
        lock(&self.delivered).clone()
    }

    /// How many times permission was prompted for.
    #[must_use]
    pub fn prompt_count(&self) -> u32 {
        *lock(&self.prompts)
    }
}

#[async_trait]
impl NotificationPort for FakeNotifications {
    async fn deliver(&self, request: &NotificationRequest) -> DeliveryOutcome {
        match *lock(&self.permission) {
            OsPermission::Granted => {
                lock(&self.delivered).push(request.clone());
                DeliveryOutcome::Delivered
            }
            OsPermission::Denied | OsPermission::NotRequested => {
                DeliveryOutcome::SuppressedByOsPermission
            }
            OsPermission::Unsupported => DeliveryOutcome::Unsupported,
        }
    }

    async fn permission(&self) -> OsPermission {
        *lock(&self.permission)
    }

    async fn request_permission(&self) -> OsPermission {
        *lock(&self.prompts) += 1;
        if let Some(resolved) = *lock(&self.permission_on_request) {
            *lock(&self.permission) = resolved;
        }
        *lock(&self.permission)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use plaudon_domain::ids::RecordingId;
    use plaudon_domain::time::Timestamp;
    use plaudon_notifications::testing::RecordingNotifier;

    use super::*;

    fn request() -> NotificationRequest {
        privacy::build(
            NotificationContext::TranscriptionFinished {
                recordings: 1,
                recording: Some(RecordingId::new()),
            },
            Timestamp::EPOCH,
        )
    }

    #[tokio::test]
    async fn the_fake_delivers_and_reports_its_permission() {
        let port = FakeNotifications::new(OsPermission::Granted);
        assert_eq!(port.permission().await, OsPermission::Granted);

        let payload = request();
        assert_eq!(port.deliver(&payload).await, DeliveryOutcome::Delivered);
        assert_eq!(port.delivered(), vec![payload]);
    }

    #[tokio::test]
    async fn a_denied_permission_suppresses_delivery_and_is_visible() {
        let port = FakeNotifications::new(OsPermission::Denied);
        assert_eq!(
            port.deliver(&request()).await,
            DeliveryOutcome::SuppressedByOsPermission
        );
        assert!(
            port.delivered().is_empty(),
            "a denied permission must deliver nothing"
        );
        assert_eq!(port.permission().await, OsPermission::Denied);
        assert_eq!(
            port.deliver(&request())
                .await
                .suppression_reason()
                .expect("a suppression must state a reason"),
            "suppressed_by_os_permission"
        );
    }

    #[tokio::test]
    async fn an_unsupported_platform_is_reported_as_unsupported() {
        let port = FakeNotifications::new(OsPermission::Unsupported);
        assert_eq!(port.deliver(&request()).await, DeliveryOutcome::Unsupported);
        assert!(!DeliveryOutcome::Unsupported.delivered());
    }

    #[tokio::test]
    async fn prompting_for_permission_updates_what_is_reported() {
        let port = FakeNotifications::new(OsPermission::NotRequested)
            .granting_on_request(OsPermission::Granted);
        assert_eq!(
            port.deliver(&request()).await,
            DeliveryOutcome::SuppressedByOsPermission
        );
        assert_eq!(port.request_permission().await, OsPermission::Granted);
        assert_eq!(port.prompt_count(), 1);
        assert_eq!(port.permission().await, OsPermission::Granted);
        assert_eq!(port.deliver(&request()).await, DeliveryOutcome::Delivered);
    }

    #[tokio::test]
    async fn the_real_port_delegates_to_the_dispatcher() {
        let notifier = RecordingNotifier::with_permission(OsPermission::Granted);
        let dispatcher = Arc::new(
            Dispatcher::new(
                Arc::clone(&notifier) as Arc<dyn Notifier>,
                NotificationSettings::uniform(true),
            )
            .with_timeout(Duration::from_secs(1)),
        );
        let port = DispatcherNotifications::new(Arc::clone(&dispatcher));

        assert_eq!(port.permission().await, OsPermission::Granted);
        assert_eq!(port.deliver(&request()).await, DeliveryOutcome::Delivered);
        assert_eq!(notifier.deliveries().await.len(), 1);
        assert!(Arc::ptr_eq(port.dispatcher(), &dispatcher));
    }

    #[tokio::test]
    async fn the_port_is_usable_as_a_trait_object() {
        let port: Arc<dyn NotificationPort> =
            Arc::new(FakeNotifications::new(OsPermission::Granted));
        assert_eq!(port.deliver(&request()).await, DeliveryOutcome::Delivered);
    }
}
