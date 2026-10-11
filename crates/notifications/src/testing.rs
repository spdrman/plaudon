//! In-memory [`Notifier`] double.
//!
//! Tests must never hand a payload to the real Notification Center, so this
//! double is the only `Notifier` used anywhere in the crate's test suite. It is
//! public (not `#[cfg(test)]`) because the desktop crate needs it to test the
//! `set_notification_settings` command and the activity history without
//! touching the OS.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::Mutex;

use crate::{DeliveryOutcome, NotificationRequest, Notifier, OsPermission};

/// What the double does when [`Notifier::deliver`] is called.
#[derive(Debug, Clone)]
pub enum Behavior {
    /// Record the payload and report [`DeliveryOutcome::Delivered`].
    Deliver,
    /// Record the payload and report [`DeliveryOutcome::Failed`] with this detail.
    Fail(String),
    /// Panic, to prove the dispatcher contains a broken backend.
    Panic,
    /// Sleep for this long, to prove the dispatcher's timeout works.
    Hang(Duration),
}

/// A [`Notifier`] that captures deliveries and simulates any [`OsPermission`].
#[derive(Debug)]
pub struct RecordingNotifier {
    permission: Mutex<OsPermission>,
    /// Permission reported after [`Notifier::request_permission`] resolves.
    granted_on_request: Mutex<OsPermission>,
    behavior: Mutex<Behavior>,
    delivered: Mutex<Vec<NotificationRequest>>,
    permission_checks: Mutex<u32>,
    permission_prompts: Mutex<u32>,
}

impl RecordingNotifier {
    /// A notifier whose permission is already `Granted` and that delivers.
    #[must_use]
    pub fn granted() -> Arc<Self> {
        Self::with_permission(OsPermission::Granted)
    }

    /// A notifier reporting `permission`, delivering when allowed.
    ///
    /// `request_permission` resolves to the same value unless
    /// [`Self::granting_on_request`] changes it.
    #[must_use]
    pub fn with_permission(permission: OsPermission) -> Arc<Self> {
        Arc::new(Self {
            permission: Mutex::new(permission),
            granted_on_request: Mutex::new(permission),
            behavior: Mutex::new(Behavior::Deliver),
            delivered: Mutex::new(Vec::new()),
            permission_checks: Mutex::new(0),
            permission_prompts: Mutex::new(0),
        })
    }

    /// Make `request_permission` resolve to `outcome` and update the stored
    /// permission, the way an OS prompt would.
    pub async fn granting_on_request(self: &Arc<Self>, outcome: OsPermission) -> Arc<Self> {
        *self.granted_on_request.lock().await = outcome;
        Arc::clone(self)
    }

    /// Replace the delivery behavior.
    pub async fn set_behavior(self: &Arc<Self>, behavior: Behavior) -> Arc<Self> {
        *self.behavior.lock().await = behavior;
        Arc::clone(self)
    }

    /// Payloads handed to [`Notifier::deliver`], oldest first.
    pub async fn deliveries(&self) -> Vec<NotificationRequest> {
        self.delivered.lock().await.clone()
    }

    /// How many times [`Notifier::permission`] was called.
    pub async fn permission_checks(&self) -> u32 {
        *self.permission_checks.lock().await
    }

    /// How many times [`Notifier::request_permission`] was called.
    pub async fn permission_prompts(&self) -> u32 {
        *self.permission_prompts.lock().await
    }
}

#[async_trait]
impl Notifier for RecordingNotifier {
    async fn permission(&self) -> OsPermission {
        *self.permission_checks.lock().await += 1;
        *self.permission.lock().await
    }

    async fn request_permission(&self) -> OsPermission {
        *self.permission_prompts.lock().await += 1;
        let outcome = *self.granted_on_request.lock().await;
        *self.permission.lock().await = outcome;
        outcome
    }

    async fn deliver(&self, req: &NotificationRequest) -> DeliveryOutcome {
        let behavior = self.behavior.lock().await.clone();
        match behavior {
            Behavior::Deliver => {
                self.delivered.lock().await.push(req.clone());
                DeliveryOutcome::Delivered
            }
            Behavior::Fail(detail) => {
                self.delivered.lock().await.push(req.clone());
                DeliveryOutcome::Failed(detail)
            }
            Behavior::Panic => {
                self.delivered.lock().await.push(req.clone());
                // Deliberate: proves the dispatcher contains a broken backend.
                #[allow(clippy::panic)]
                {
                    panic!("simulated notification backend crash");
                }
            }
            Behavior::Hang(duration) => {
                self.delivered.lock().await.push(req.clone());
                tokio::time::sleep(duration).await;
                DeliveryOutcome::Delivered
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use plaudon_domain::time::Timestamp;

    use super::*;
    use crate::privacy::{self, NotificationContext};

    fn request() -> NotificationRequest {
        privacy::build(
            NotificationContext::TranscriptionFinished { recordings: 1, recording: None },
            Timestamp::EPOCH,
        )
    }

    #[tokio::test]
    async fn captures_deliveries_and_counts_probes() {
        let notifier = RecordingNotifier::granted();
        assert_eq!(notifier.permission().await, OsPermission::Granted);
        assert_eq!(notifier.deliver(&request()).await, DeliveryOutcome::Delivered);
        assert_eq!(notifier.deliveries().await, vec![request()]);
        assert_eq!(notifier.permission_checks().await, 1);
        assert_eq!(notifier.permission_prompts().await, 0);
    }

    #[tokio::test]
    async fn prompting_updates_the_reported_permission() {
        let notifier = RecordingNotifier::with_permission(OsPermission::NotRequested);
        let notifier = notifier.granting_on_request(OsPermission::Denied).await;
        assert_eq!(notifier.request_permission().await, OsPermission::Denied);
        assert_eq!(notifier.permission().await, OsPermission::Denied);
        assert_eq!(notifier.permission_prompts().await, 1);
    }
}
