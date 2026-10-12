//! OS notification delivery for committed activity entries (spec §5.5, §22(13)).
//!
//! Three rules shape this module:
//!
//! 1. **The activity entry is the record; the notification is a side effect.**
//!    The row is written inside the transaction that commits the work; the
//!    payload is handed to the OS strictly afterwards, and the row is updated
//!    with whatever the OS did.
//! 2. **Delivery never blocks or breaks a job.** Every notification runs on a
//!    detached task; a backend that fails, hangs or panics is contained by
//!    `plaudon_notifications::Dispatcher` and can only change the two
//!    delivery columns of an activity row.
//! 3. **No user content can reach the OS.** Payloads are built by
//!    [`plaudon_notifications::privacy::build`] from counts, classifications
//!    and typed ids; a title, a file name or a transcript excerpt cannot be
//!    named by its input type.

use std::sync::Arc;

use async_trait::async_trait;
use plaudon_domain::time::Timestamp;
use plaudon_notifications::privacy::{self, NotificationContext};
use plaudon_notifications::{
    DeliveryOutcome, NotificationRequest, NotificationSettings as CrateSettings, Notifier,
    OsPermission,
};
use sqlx::SqlitePool;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use crate::dto::{NotificationCategory, NotificationSettings, OsNotificationPermission};

/// Delivers notifications for committed activity entries.
#[derive(Debug, Clone)]
pub struct NotificationService {
    dispatcher: Arc<plaudon_notifications::Dispatcher>,
    pool: SqlitePool,
    in_flight: Arc<Mutex<Vec<JoinHandle<()>>>>,
}

impl NotificationService {
    /// Service over an existing dispatcher.
    #[must_use]
    pub fn new(dispatcher: Arc<plaudon_notifications::Dispatcher>, pool: SqlitePool) -> Self {
        Self { dispatcher, pool, in_flight: Arc::new(Mutex::new(Vec::new())) }
    }

    /// The dispatcher, for callers that need to inspect settings in tests.
    #[must_use]
    pub fn dispatcher(&self) -> &Arc<plaudon_notifications::Dispatcher> {
        &self.dispatcher
    }

    /// Hand a committed activity entry to the OS, without waiting for it.
    ///
    /// The returned handle exists so tests (and a future graceful shutdown)
    /// can drain delivery; the command path drops it, which detaches the task.
    pub async fn notify(
        &self,
        activity_id: String,
        context: NotificationContext,
        occurred_at: Timestamp,
    ) {
        let request = privacy::build(context, occurred_at);
        let dispatcher = Arc::clone(&self.dispatcher);
        let pool = self.pool.clone();
        let handle = tokio::spawn(async move {
            let outcome = dispatcher.notify(&request).await;
            record_outcome(&pool, &activity_id, &outcome).await;
        });

        let mut in_flight = self.in_flight.lock().await;
        in_flight.retain(|handle| !handle.is_finished());
        in_flight.push(handle);
    }

    /// Wait for every delivery started so far.
    ///
    /// Only tests and shutdown call this: the command path never waits for the
    /// operating system.
    pub async fn flush(&self) {
        let handles: Vec<JoinHandle<()>> = {
            let mut in_flight = self.in_flight.lock().await;
            std::mem::take(&mut *in_flight)
        };
        for handle in handles {
            // A panicking backend is contained by the dispatcher, but a
            // panicking task must still not propagate into a command.
            let _ = handle.await;
        }
    }

    /// The permission the OS reports right now, without prompting.
    pub async fn os_permission(&self) -> OsNotificationPermission {
        map_permission(self.dispatcher.os_permission().await)
    }

    /// Push new preferences into the dispatcher.
    ///
    /// When the user has just switched notifications on, this is the moment to
    /// ask the OS; otherwise the permission is only probed. Nothing prompts at
    /// startup (spec §5.5: notifications are opt-in).
    pub async fn apply_settings(
        &self,
        settings: &NotificationSettings,
    ) -> OsNotificationPermission {
        let crate_settings = to_crate_settings(settings);
        let wants_delivery = NotificationCategory::ALL
            .iter()
            .any(|category| crate_settings.allows(to_crate_category(*category)));
        self.dispatcher.set_settings(crate_settings).await;

        let observed = if wants_delivery {
            match self.dispatcher.os_permission().await {
                OsPermission::NotRequested => self.dispatcher.request_os_permission().await,
                other => other,
            }
        } else {
            self.dispatcher.os_permission().await
        };
        map_permission(observed)
    }
}

async fn record_outcome(pool: &SqlitePool, activity_id: &str, outcome: &DeliveryOutcome) {
    let delivered = outcome.delivered();
    let reason = outcome.suppression_reason();
    let result = sqlx::query(
        "UPDATE activity_events SET notification_delivered = ?2, \
         notification_suppressed_reason = ?3 WHERE id = ?1",
    )
    .bind(activity_id)
    .bind(i64::from(delivered))
    .bind(reason)
    .execute(pool)
    .await;
    if let Err(err) = result {
        // The entry itself is already durable; only the delivery columns are
        // affected, so this is logged and dropped.
        tracing::warn!(error = %err, "could not record a notification outcome");
    }
}

/// Map the crate's permission vocabulary onto the wire contract's.
#[must_use]
pub const fn map_permission(permission: OsPermission) -> OsNotificationPermission {
    match permission {
        OsPermission::Granted => OsNotificationPermission::Granted,
        OsPermission::Denied => OsNotificationPermission::Denied,
        OsPermission::NotRequested => OsNotificationPermission::NotRequested,
        OsPermission::Unsupported => OsNotificationPermission::Unsupported,
    }
}

/// Map a wire category onto the notification crate's.
#[must_use]
pub const fn to_crate_category(
    category: NotificationCategory,
) -> plaudon_notifications::NotificationCategory {
    match category {
        NotificationCategory::ImportFinished => {
            plaudon_notifications::NotificationCategory::ImportFinished
        }
        NotificationCategory::TranscriptionFinished => {
            plaudon_notifications::NotificationCategory::TranscriptionFinished
        }
        NotificationCategory::RequiresAttention => {
            plaudon_notifications::NotificationCategory::RequiresAttention
        }
        NotificationCategory::CloudBudgetExceeded => {
            plaudon_notifications::NotificationCategory::CloudBudgetExceeded
        }
    }
}

/// Map the stored preferences onto the notification crate's settings.
#[must_use]
pub fn to_crate_settings(settings: &NotificationSettings) -> CrateSettings {
    let mut crate_settings = CrateSettings::uniform(false).with_enabled(settings.enabled);
    for (category, opted_in) in &settings.categories {
        crate_settings = crate_settings.with_category(to_crate_category(*category), *opted_in);
    }
    crate_settings
}

/// Backend for platforms with no supported notification API.
///
/// Spec §5.5 names macOS Notification Center and Windows toasts and nothing
/// else; on any other platform the shell reports `unsupported` rather than
/// pretending a notification was delivered.
#[derive(Debug, Default, Clone, Copy)]
pub struct UnsupportedNotifier;

#[async_trait]
impl Notifier for UnsupportedNotifier {
    async fn permission(&self) -> OsPermission {
        OsPermission::Unsupported
    }

    async fn request_permission(&self) -> OsPermission {
        OsPermission::Unsupported
    }

    async fn deliver(&self, _req: &NotificationRequest) -> DeliveryOutcome {
        DeliveryOutcome::Unsupported
    }
}

/// Callback invoked when the user activates a notification.
pub type ActivationHandler = Arc<dyn Fn(Option<NotificationTarget>) + Send + Sync>;

/// The platform backend for this host.
///
/// `on_activated` routes a click back into the app. macOS carries the target
/// in `targetContentIdentifier` and the application delegate calls
/// [`activation_target`] with it; Windows hands it to the toast backend.
#[must_use]
pub fn platform_notifier(on_activated: ActivationHandler) -> Arc<dyn Notifier> {
    #[cfg(target_os = "macos")]
    {
        let _ = on_activated;
        Arc::new(plaudon_notifications::macos::MacNotifier::new())
    }
    #[cfg(target_os = "windows")]
    {
        Arc::new(
            plaudon_notifications::windows::WindowsNotifier::new("app.plaudon.desktop")
                .with_activation_handler(on_activated),
        )
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = on_activated;
        Arc::new(UnsupportedNotifier)
    }
}

/// Parse an activation token the OS handed back.
///
/// A token this build did not write is ignored: the user is never navigated
/// somewhere because a foreign process raised a notification.
#[must_use]
pub fn activation_target(token: &str) -> Option<NotificationTarget> {
    let target = NotificationTarget::from_activation_token(token);
    if target.is_none() {
        tracing::debug!("ignored an unrecognized notification activation token");
    }
    target
}

/// Announce a notification activation to the UI.
///
/// Uses the same publisher, and therefore the same monotonic sequence and
/// post-commit discipline, as every other event.
pub async fn publish_activation(events: &EventPublisher, target: NotificationTarget) {
    let mut draft = EventDraft::new(EventKind::ActivationRequested, Timestamp::now()).with_hint(
        "target",
        serde_json::json!({ "kind": target.kind(), "id": target.id() }),
    );
    match target {
        NotificationTarget::Recording(recording_id) => {
            draft = draft.with_recording(recording_id);
        }
        NotificationTarget::Job(job_id) => draft = draft.with_job(job_id),
        NotificationTarget::Settings => {}
    }
    events.publish(draft).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::default_preferences;

    #[test]
    fn settings_round_trip_into_the_notification_crate() {
        let mut preferences = default_preferences();
        preferences.notifications.enabled = true;
        preferences
            .notifications
            .categories
            .insert(NotificationCategory::ImportFinished, true);

        let crate_settings = to_crate_settings(&preferences.notifications);
        assert!(crate_settings.allows(plaudon_notifications::NotificationCategory::ImportFinished));
        assert!(
            !crate_settings
                .allows(plaudon_notifications::NotificationCategory::TranscriptionFinished),
            "a category the user did not opt into stays off"
        );
    }

    #[test]
    fn every_permission_maps_onto_the_wire_contract() {
        assert_eq!(map_permission(OsPermission::Granted), OsNotificationPermission::Granted);
        assert_eq!(map_permission(OsPermission::Denied), OsNotificationPermission::Denied);
        assert_eq!(
            map_permission(OsPermission::NotRequested),
            OsNotificationPermission::NotRequested
        );
        assert_eq!(
            map_permission(OsPermission::Unsupported),
            OsNotificationPermission::Unsupported
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_unsupported_host_never_claims_delivery() {
        let notifier = UnsupportedNotifier;
        assert_eq!(notifier.permission().await, OsPermission::Unsupported);
        let request = privacy::build(
            NotificationContext::ImportFinished { imported: 1, failed: 0, recording: None },
            Timestamp::EPOCH,
        );
        let outcome = notifier.deliver(&request).await;
        assert!(!outcome.delivered());
        assert_eq!(outcome.suppression_reason(), Some("unsupported"));
    }
}
