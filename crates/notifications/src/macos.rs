//! macOS Notification Center backend (spec 5.5).
//!
//! # Backend choice
//!
//! This module talks to **`UserNotifications.framework`** through the
//! `objc2`/`objc2-user-notifications` bindings rather than through a
//! convenience crate, for three reasons:
//!
//! * `UNUserNotificationCenter` is the only notification API Apple still
//!   supports. `mac-notification-sys` (and therefore `notify-rust` on macOS)
//!   drives the deprecated `NSUserNotification`, which Apple deprecated in
//!   macOS 11 and which has no permission model we can report truthfully —
//!   spec 5.5 requires the app to act "after OS permission/registration", and
//!   spec 22(13) requires completion events to stay visible when permission is
//!   absent, both of which need a real `authorizationStatus`.
//! * `objc2-user-notifications` is generated from the current SDK headers and
//!   is maintained alongside `objc2`, which the Tauri/`objc2` stack already
//!   pulls in; it adds no HTTP, mail or push dependency (see
//!   [`crate::deps_policy`]).
//! * Going through the framework directly lets us set
//!   `targetContentIdentifier`, which is what makes "notification clicks open
//!   the affected recording/job" work under the OS activation rules.
//!
//! # Bundling requirement
//!
//! `UNUserNotificationCenter::currentNotificationCenter` requires a *bundled*
//! application: it raises an Objective-C exception when the running process has
//! no bundle identifier (a bare `cargo run`/`cargo test` binary). Every entry
//! point therefore checks [`bundle_identifier`] first and reports
//! [`OsPermission::Unsupported`]/[`DeliveryOutcome::Unsupported`] instead, which
//! also keeps the test suite from ever reaching the real Notification Center.
//! The shipping app is a signed `.app` bundle, so this guard only fires for
//! development binaries.
//!
//! # Threading
//!
//! Objective-C objects and `block2` blocks are neither `Send` nor `Sync`, and a
//! completion handler must stay alive until the framework calls it. Each
//! operation therefore runs entirely inside one `spawn_blocking` closure: the
//! center, the block and the reply channel are created, used and dropped there,
//! and the async side only ever sees plain data. The bounded `recv_timeout`
//! guarantees the blocking thread is released even if a reply never arrives.

// The UserNotifications bindings are `unsafe` at the FFI boundary: the
// completion handlers hand us raw pointers that must be dereferenced to read
// the result. Every `unsafe` block below is a pointer-to-reference conversion
// inside such a callback.
#![allow(unsafe_code)]

use std::ptr::NonNull;
use std::sync::mpsc;
use std::time::Duration;

use async_trait::async_trait;
use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::Bool;
use objc2_foundation::{NSBundle, NSError, NSString};
use objc2_user_notifications::{
    UNAuthorizationOptions, UNAuthorizationStatus, UNMutableNotificationContent,
    UNNotificationRequest, UNNotificationSettings, UNUserNotificationCenter,
};

use crate::{DeliveryOutcome, NotificationRequest, Notifier, OsPermission};

/// Upper bound on how long a blocking notification thread waits for the
/// framework to call back before giving up and releasing the thread.
///
/// The dispatcher applies its own, shorter timeout to the whole call; this one
/// exists so a wedged `UserNotifications` XPC connection cannot leak threads.
const REPLY_TIMEOUT: Duration = Duration::from_secs(10);

/// Notification Center backend.
#[derive(Debug, Default, Clone, Copy)]
pub struct MacNotifier {
    _private: (),
}

impl MacNotifier {
    /// Create the backend. Cheap; all state lives in the OS.
    #[must_use]
    pub const fn new() -> Self {
        Self { _private: () }
    }
}

/// Bundle identifier of the running process, or `None` when the process is not
/// bundled and the user-notification API is unavailable.
#[must_use]
pub fn bundle_identifier() -> Option<String> {
    NSBundle::mainBundle()
        .bundleIdentifier()
        .map(|id| id.to_string())
}

/// The notification center, or `None` when this process may not use it.
fn center() -> Option<Retained<UNUserNotificationCenter>> {
    bundle_identifier().map(|_| UNUserNotificationCenter::currentNotificationCenter())
}

/// Map Apple's `UNAuthorizationStatus` onto our permission vocabulary.
///
/// Provisional and ephemeral authorizations do deliver notifications (quietly,
/// in the provisional case), so they count as granted.
fn map_status(status: UNAuthorizationStatus) -> OsPermission {
    match status {
        UNAuthorizationStatus::NotDetermined => OsPermission::NotRequested,
        UNAuthorizationStatus::Denied => OsPermission::Denied,
        UNAuthorizationStatus::Authorized
        | UNAuthorizationStatus::Provisional
        | UNAuthorizationStatus::Ephemeral => OsPermission::Granted,
        _ => OsPermission::Unsupported,
    }
}

/// Run one Objective-C interaction on a blocking thread.
///
/// `work` owns every Objective-C value it touches; only `T` crosses back.
async fn on_blocking_thread<T, F>(work: F) -> Option<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(work).await.ok()
}

#[async_trait]
impl Notifier for MacNotifier {
    async fn permission(&self) -> OsPermission {
        on_blocking_thread(|| {
            let Some(center) = center() else {
                return OsPermission::Unsupported;
            };

            let (tx, rx) = mpsc::channel::<UNAuthorizationStatus>();
            let handler = RcBlock::new(move |settings: NonNull<UNNotificationSettings>| {
                // Safety: Apple documents this argument as a valid, non-null
                // `UNNotificationSettings` for the duration of the callback.
                let status = unsafe { settings.as_ref() }.authorizationStatus();
                let _ = tx.send(status);
            });
            center.getNotificationSettingsWithCompletionHandler(&handler);

            rx.recv_timeout(REPLY_TIMEOUT)
                .map_or(OsPermission::Unsupported, map_status)
        })
        .await
        .unwrap_or(OsPermission::Unsupported)
    }

    async fn request_permission(&self) -> OsPermission {
        on_blocking_thread(|| {
            let Some(center) = center() else {
                return OsPermission::Unsupported;
            };

            let (tx, rx) = mpsc::channel::<Result<bool, String>>();
            let handler = RcBlock::new(move |granted: Bool, error: *mut NSError| {
                // Safety: `error` is either null or a valid `NSError` owned by
                // the framework for the duration of the callback.
                let failure =
                    unsafe { error.as_ref() }.map(|err| err.localizedDescription().to_string());
                let _ = tx.send(failure.map_or_else(|| Ok(granted.as_bool()), Err));
            });
            center.requestAuthorizationWithOptions_completionHandler(
                UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound,
                &handler,
            );

            match rx.recv_timeout(REPLY_TIMEOUT) {
                Ok(Ok(true)) => OsPermission::Granted,
                Ok(Ok(false)) => OsPermission::Denied,
                // An authorization error means we cannot claim permission. The
                // app keeps working; the activity history stays authoritative.
                Ok(Err(detail)) => {
                    tracing::warn!(detail, "notification authorization request failed");
                    OsPermission::Denied
                }
                Err(_) => OsPermission::Unsupported,
            }
        })
        .await
        .unwrap_or(OsPermission::Unsupported)
    }

    async fn deliver(&self, req: &NotificationRequest) -> DeliveryOutcome {
        let title = req.title.clone();
        let body = req.body.clone();
        let token = req.target.map(|target| target.activation_token());
        let identifier = format!(
            "plaudon.{}.{}",
            req.category.as_str(),
            req.occurred_at.as_millis()
        );

        on_blocking_thread(move || {
            let Some(center) = center() else {
                return DeliveryOutcome::Unsupported;
            };

            let content = UNMutableNotificationContent::new();
            content.setTitle(&NSString::from_str(&title));
            content.setBody(&NSString::from_str(&body));
            if let Some(token) = token {
                // Read by the app delegate on activation to open the right view.
                content.setTargetContentIdentifier(Some(&NSString::from_str(&token)));
            }

            // `trigger: None` means "deliver immediately".
            let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
                &NSString::from_str(&identifier),
                &content,
                None,
            );

            let (tx, rx) = mpsc::channel::<Option<String>>();
            let handler = RcBlock::new(move |error: *mut NSError| {
                // Safety: as above, `error` is null or a valid `NSError`.
                let failure =
                    unsafe { error.as_ref() }.map(|err| err.localizedDescription().to_string());
                let _ = tx.send(failure);
            });
            center.addNotificationRequest_withCompletionHandler(&request, Some(&handler));

            match rx.recv_timeout(REPLY_TIMEOUT) {
                Ok(None) => DeliveryOutcome::Delivered,
                Ok(Some(detail)) => DeliveryOutcome::Failed(detail),
                Err(_) => {
                    DeliveryOutcome::Failed("Notification Center did not answer in time".to_owned())
                }
            }
        })
        .await
        .unwrap_or_else(|| {
            DeliveryOutcome::Failed("notification thread did not complete".to_owned())
        })
    }
}

#[cfg(test)]
mod tests {
    use plaudon_domain::time::Timestamp;

    use super::*;
    use crate::privacy::{self, NotificationContext};

    /// Tests run from an unbundled binary, so the guard must hold and no
    /// payload may reach the real Notification Center.
    #[tokio::test]
    async fn unbundled_processes_report_unsupported_without_touching_the_os() {
        if bundle_identifier().is_some() {
            // Running inside an app bundle (not how CI runs); refuse to deliver
            // a real notification from a test either way.
            return;
        }

        let notifier = MacNotifier::new();
        assert_eq!(notifier.permission().await, OsPermission::Unsupported);
        assert_eq!(
            notifier.request_permission().await,
            OsPermission::Unsupported
        );

        let req = privacy::build(
            NotificationContext::TranscriptionFinished {
                recordings: 1,
                recording: None,
            },
            Timestamp::EPOCH,
        );
        assert_eq!(notifier.deliver(&req).await, DeliveryOutcome::Unsupported);
    }

    #[test]
    fn authorization_statuses_map_onto_the_permission_vocabulary() {
        assert_eq!(
            map_status(UNAuthorizationStatus::NotDetermined),
            OsPermission::NotRequested
        );
        assert_eq!(
            map_status(UNAuthorizationStatus::Denied),
            OsPermission::Denied
        );
        assert_eq!(
            map_status(UNAuthorizationStatus::Authorized),
            OsPermission::Granted
        );
        assert_eq!(
            map_status(UNAuthorizationStatus::Provisional),
            OsPermission::Granted
        );
        assert_eq!(
            map_status(UNAuthorizationStatus::Ephemeral),
            OsPermission::Granted
        );
        assert_eq!(
            map_status(UNAuthorizationStatus(99)),
            OsPermission::Unsupported
        );
    }

    #[tokio::test]
    async fn blocking_work_is_handed_back_as_plain_data() {
        assert_eq!(on_blocking_thread(|| 7_u8).await, Some(7));
    }
}
