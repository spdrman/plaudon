//! Preference gate, permission gate and containment boundary.
//!
//! Spec 14.5: "The app will display notifications on completion without
//! blocking jobs or relying on OS delivery as a source of truth." Spec 22(13)
//! turns that into a release gate.
//!
//! [`Dispatcher::notify`] is therefore **fire-and-forget from the caller's
//! point of view**: it returns a [`DeliveryOutcome`] for the activity log and
//! never an error, never propagates a backend panic, and never waits longer
//! than [`Dispatcher::timeout`] on a platform API. A job that just committed
//! its result must be able to call it on the completion path without any risk
//! of being blocked or unwound by the OS notification stack.
//!
//! The returned outcome is a *record of what the OS did*, not application
//! state. The committed job row and the in-app activity entry remain the state
//! of record whether delivery succeeded, was suppressed, or failed.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::RwLock;

use crate::{DeliveryOutcome, NotificationRequest, NotificationSettings, Notifier, OsPermission};

/// How long any single backend call may take before it is abandoned.
///
/// Both macOS and Windows answer in milliseconds when healthy; a multi-second
/// stall means the notification subsystem is wedged, and the completion path
/// must not wait for it.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);

/// Gates a [`Notifier`] behind user preferences and OS permission.
pub struct Dispatcher {
    notifier: Arc<dyn Notifier>,
    settings: RwLock<NotificationSettings>,
    timeout: Duration,
}

impl std::fmt::Debug for Dispatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `dyn Notifier` is a platform backend; its identity is not debuggable
        // and would add nothing to a log line.
        f.debug_struct("Dispatcher")
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}

impl Dispatcher {
    /// Build a dispatcher over `notifier` with the user's current preferences.
    #[must_use]
    pub fn new(notifier: Arc<dyn Notifier>, settings: NotificationSettings) -> Self {
        Self {
            notifier,
            settings: RwLock::new(settings),
            timeout: DEFAULT_TIMEOUT,
        }
    }

    /// Override the per-call backend timeout (tests use a short one).
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// The per-call backend timeout.
    #[must_use]
    pub const fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Current preferences.
    pub async fn settings(&self) -> NotificationSettings {
        self.settings.read().await.clone()
    }

    /// Replace preferences, e.g. from the `set_notification_settings` command.
    pub async fn set_settings(&self, settings: NotificationSettings) {
        *self.settings.write().await = settings;
    }

    /// Ask the backend for the current OS permission, contained the same way as
    /// delivery. A backend that panics or stalls reports
    /// [`OsPermission::Unsupported`] rather than failing the caller.
    pub async fn os_permission(&self) -> OsPermission {
        let notifier = Arc::clone(&self.notifier);
        contain(self.timeout, "permission probe", async move {
            notifier.permission().await
        })
        .await
        .unwrap_or_else(|detail| {
            tracing::warn!(detail, "notification permission probe failed");
            OsPermission::Unsupported
        })
    }

    /// Prompt for OS permission, contained the same way as delivery.
    pub async fn request_os_permission(&self) -> OsPermission {
        let notifier = Arc::clone(&self.notifier);
        contain(self.timeout, "permission prompt", async move {
            notifier.request_permission().await
        })
        .await
        .unwrap_or_else(|detail| {
            tracing::warn!(detail, "notification permission prompt failed");
            OsPermission::Unsupported
        })
    }

    /// Deliver `req` if preferences and OS permission allow it.
    ///
    /// Order of checks, per spec 5.5: the user's master switch and per-category
    /// opt-in first, then the OS permission, then the backend. Checking
    /// preferences first means an opted-out category never triggers a
    /// permission prompt or touches the platform API at all.
    ///
    /// Never panics, never blocks indefinitely and never returns an error:
    /// every failure mode maps to a [`DeliveryOutcome`] for the activity log.
    pub async fn notify(&self, req: &NotificationRequest) -> DeliveryOutcome {
        if !self.settings.read().await.allows(req.category) {
            return DeliveryOutcome::SuppressedByPreference;
        }

        let outcome = match self.os_permission().await {
            OsPermission::Granted => self.deliver(req).await,
            OsPermission::Denied => DeliveryOutcome::SuppressedByOsPermission,
            OsPermission::Unsupported => DeliveryOutcome::Unsupported,
            // The user opted in to this category, so the first delivery is the
            // natural moment to ask the OS. We ask once and honour the answer.
            OsPermission::NotRequested => match self.request_os_permission().await {
                OsPermission::Granted => self.deliver(req).await,
                OsPermission::Unsupported => DeliveryOutcome::Unsupported,
                OsPermission::Denied | OsPermission::NotRequested => {
                    DeliveryOutcome::SuppressedByOsPermission
                }
            },
        };

        tracing::debug!(
            category = req.category.as_str(),
            delivered = outcome.delivered(),
            reason = outcome.suppression_reason(),
            "notification dispatched"
        );
        outcome
    }

    async fn deliver(&self, req: &NotificationRequest) -> DeliveryOutcome {
        let notifier = Arc::clone(&self.notifier);
        let payload = req.clone();
        contain(self.timeout, "delivery", async move {
            notifier.deliver(&payload).await
        })
        .await
        .unwrap_or_else(DeliveryOutcome::Failed)
    }
}

/// Run `future` on its own task so a panic unwinds there instead of here, and
/// abandon it if it outlives `limit`.
///
/// `tokio::spawn` is the containment mechanism: a panicking backend surfaces as
/// a `JoinError` instead of unwinding the caller, and a wedged backend is
/// aborted rather than awaited forever.
async fn contain<T, F>(limit: Duration, label: &'static str, future: F) -> Result<T, String>
where
    F: Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    let mut handle = tokio::spawn(future);
    match tokio::time::timeout(limit, &mut handle).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(join)) if join.is_panic() => Err(format!("notification {label} panicked")),
        Ok(Err(join)) => Err(format!("notification {label} did not complete: {join}")),
        Err(_elapsed) => {
            handle.abort();
            Err(format!("notification {label} exceeded {limit:?}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use plaudon_domain::time::Timestamp;

    use super::*;
    use crate::privacy::{self, AttentionReason, BudgetScope, NotificationContext};
    use crate::testing::{Behavior, RecordingNotifier};
    use crate::{NotificationCategory, NotificationTarget};

    fn request(category: NotificationCategory) -> NotificationRequest {
        let context = match category {
            NotificationCategory::ImportFinished => NotificationContext::ImportFinished {
                imported: 2,
                failed: 0,
                recording: None,
            },
            NotificationCategory::TranscriptionFinished => {
                NotificationContext::TranscriptionFinished {
                    recordings: 1,
                    recording: None,
                }
            }
            NotificationCategory::RequiresAttention => NotificationContext::RequiresAttention {
                jobs: 1,
                reason: AttentionReason::AuthorizationRequired,
                job: None,
            },
            NotificationCategory::CloudBudgetExceeded => NotificationContext::CloudBudgetExceeded {
                scope: BudgetScope::Daily,
                paused_jobs: 1,
            },
        };
        privacy::build(context, Timestamp::from_millis(1_700_000_000_000))
    }

    fn short(notifier: Arc<RecordingNotifier>, settings: NotificationSettings) -> Dispatcher {
        Dispatcher::new(notifier, settings).with_timeout(Duration::from_millis(150))
    }

    #[tokio::test]
    async fn master_switch_off_suppresses_everything() {
        let notifier = RecordingNotifier::granted();
        let dispatcher = short(
            Arc::clone(&notifier),
            NotificationSettings::uniform(true).with_enabled(false),
        );

        for category in NotificationCategory::ALL {
            assert_eq!(
                dispatcher.notify(&request(category)).await,
                DeliveryOutcome::SuppressedByPreference
            );
        }
        assert!(
            notifier.deliveries().await.is_empty(),
            "no payload may reach the backend"
        );
        assert_eq!(
            notifier.permission_checks().await,
            0,
            "preference gate runs first"
        );
    }

    #[tokio::test]
    async fn only_opted_in_categories_are_delivered() {
        let notifier = RecordingNotifier::granted();
        let settings = NotificationSettings::uniform(false)
            .with_enabled(true)
            .with_category(NotificationCategory::RequiresAttention, true);
        let dispatcher = short(Arc::clone(&notifier), settings);

        assert_eq!(
            dispatcher
                .notify(&request(NotificationCategory::ImportFinished))
                .await,
            DeliveryOutcome::SuppressedByPreference
        );
        assert_eq!(
            dispatcher
                .notify(&request(NotificationCategory::RequiresAttention))
                .await,
            DeliveryOutcome::Delivered
        );

        let delivered = notifier.deliveries().await;
        assert_eq!(delivered.len(), 1);
        assert_eq!(
            delivered[0].category,
            NotificationCategory::RequiresAttention
        );
    }

    #[tokio::test]
    async fn missing_category_entry_counts_as_opted_out() {
        let notifier = RecordingNotifier::granted();
        let settings = NotificationSettings {
            enabled: true,
            categories: std::collections::BTreeMap::new(),
        };
        let dispatcher = short(Arc::clone(&notifier), settings);

        assert_eq!(
            dispatcher
                .notify(&request(NotificationCategory::ImportFinished))
                .await,
            DeliveryOutcome::SuppressedByPreference
        );
        assert!(
            notifier.deliveries().await.is_empty(),
            "no payload may reach the backend"
        );
    }

    #[tokio::test]
    async fn denied_os_permission_suppresses_without_delivering() {
        let notifier = RecordingNotifier::with_permission(OsPermission::Denied);
        let dispatcher = short(Arc::clone(&notifier), NotificationSettings::uniform(true));

        assert_eq!(
            dispatcher
                .notify(&request(NotificationCategory::ImportFinished))
                .await,
            DeliveryOutcome::SuppressedByOsPermission
        );
        assert!(
            notifier.deliveries().await.is_empty(),
            "a denied OS never sees a payload"
        );
        assert_eq!(
            notifier.permission_prompts().await,
            0,
            "a denial is not re-prompted"
        );
    }

    #[tokio::test]
    async fn unsupported_platform_reports_unsupported() {
        let notifier = RecordingNotifier::with_permission(OsPermission::Unsupported);
        let dispatcher = short(Arc::clone(&notifier), NotificationSettings::uniform(true));

        assert_eq!(
            dispatcher
                .notify(&request(NotificationCategory::ImportFinished))
                .await,
            DeliveryOutcome::Unsupported
        );
        assert!(
            notifier.deliveries().await.is_empty(),
            "an unsupported OS never sees a payload"
        );
    }

    #[tokio::test]
    async fn not_requested_prompts_once_and_honours_the_answer() {
        let granting = RecordingNotifier::with_permission(OsPermission::NotRequested)
            .granting_on_request(OsPermission::Granted)
            .await;
        let dispatcher = short(Arc::clone(&granting), NotificationSettings::uniform(true));
        assert_eq!(
            dispatcher
                .notify(&request(NotificationCategory::TranscriptionFinished))
                .await,
            DeliveryOutcome::Delivered
        );
        assert_eq!(granting.permission_prompts().await, 1);

        let refusing = RecordingNotifier::with_permission(OsPermission::NotRequested)
            .granting_on_request(OsPermission::Denied)
            .await;
        let dispatcher = short(Arc::clone(&refusing), NotificationSettings::uniform(true));
        assert_eq!(
            dispatcher
                .notify(&request(NotificationCategory::TranscriptionFinished))
                .await,
            DeliveryOutcome::SuppressedByOsPermission
        );
        assert!(
            refusing.deliveries().await.is_empty(),
            "a refused prompt delivers nothing"
        );
    }

    #[tokio::test]
    async fn backend_failure_is_reported_not_propagated() {
        let notifier = RecordingNotifier::granted()
            .set_behavior(Behavior::Fail("Notification Center refused".to_owned()))
            .await;
        let dispatcher = short(Arc::clone(&notifier), NotificationSettings::uniform(true));

        let outcome = dispatcher
            .notify(&request(NotificationCategory::ImportFinished))
            .await;
        assert_eq!(
            outcome,
            DeliveryOutcome::Failed("Notification Center refused".to_owned())
        );
        assert!(!outcome.delivered());
        assert_eq!(outcome.suppression_reason(), Some("failed"));
    }

    #[tokio::test]
    async fn panicking_backend_is_contained() {
        let notifier = RecordingNotifier::granted()
            .set_behavior(Behavior::Panic)
            .await;
        let dispatcher = short(Arc::clone(&notifier), NotificationSettings::uniform(true));

        let outcome = dispatcher
            .notify(&request(NotificationCategory::ImportFinished))
            .await;
        assert!(matches!(&outcome, DeliveryOutcome::Failed(detail) if detail.contains("panicked")));

        // The dispatcher is still usable afterwards.
        let notifier = RecordingNotifier::granted();
        let dispatcher = short(Arc::clone(&notifier), NotificationSettings::uniform(true));
        assert_eq!(
            dispatcher
                .notify(&request(NotificationCategory::ImportFinished))
                .await,
            DeliveryOutcome::Delivered
        );
    }

    #[tokio::test]
    async fn hanging_backend_times_out_instead_of_blocking_the_caller() {
        let notifier = RecordingNotifier::granted()
            .set_behavior(Behavior::Hang(Duration::from_secs(30)))
            .await;
        let dispatcher = short(Arc::clone(&notifier), NotificationSettings::uniform(true));

        let started = std::time::Instant::now();
        let outcome = dispatcher
            .notify(&request(NotificationCategory::ImportFinished))
            .await;
        let elapsed = started.elapsed();

        assert!(matches!(&outcome, DeliveryOutcome::Failed(detail) if detail.contains("exceeded")));
        assert!(
            elapsed < Duration::from_secs(5),
            "caller waited {elapsed:?}"
        );
    }

    #[tokio::test]
    async fn settings_can_be_replaced_at_runtime() {
        let notifier = RecordingNotifier::granted();
        let dispatcher = short(Arc::clone(&notifier), NotificationSettings::default());
        assert_eq!(
            dispatcher
                .notify(&request(NotificationCategory::ImportFinished))
                .await,
            DeliveryOutcome::SuppressedByPreference
        );

        dispatcher
            .set_settings(NotificationSettings::uniform(true))
            .await;
        assert!(dispatcher
            .settings()
            .await
            .allows(NotificationCategory::ImportFinished));
        assert_eq!(
            dispatcher
                .notify(&request(NotificationCategory::ImportFinished))
                .await,
            DeliveryOutcome::Delivered
        );
    }

    #[tokio::test]
    async fn delivered_payloads_keep_their_target_and_event_time() {
        let recording = plaudon_domain::ids::RecordingId::new();
        let notifier = RecordingNotifier::granted();
        let dispatcher = short(Arc::clone(&notifier), NotificationSettings::uniform(true));
        let req = privacy::build(
            NotificationContext::ImportFinished {
                imported: 1,
                failed: 0,
                recording: Some(recording),
            },
            Timestamp::from_millis(99),
        );

        assert_eq!(dispatcher.notify(&req).await, DeliveryOutcome::Delivered);
        let delivered = notifier.deliveries().await;
        assert_eq!(
            delivered[0].target,
            Some(NotificationTarget::Recording(recording))
        );
        assert_eq!(delivered[0].occurred_at, Timestamp::from_millis(99));
    }
}
