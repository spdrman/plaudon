//! Windows toast backend (spec 5.5).
//!
//! # Backend choice
//!
//! Delivery goes through **`tauri-winrt-notification`**, a thin wrapper over
//! `Windows.UI.Notifications.ToastNotificationManager`. The name is
//! misleading after the EPIC O cutover (ADR-0024): the crate is a standalone
//! WinRT toast wrapper published by the Tauri organisation, it does not pull
//! the Tauri framework, and nothing of a webview reaches this build. It
//! produces a real `WinRT` toast (no PowerShell shelling, no balloon-tip
//! fallback) and it depends only on the `windows` crate family — no HTTP,
//! mail or push dependency (see [`crate::deps_policy`]). Driving
//! `ToastNotificationManager` by hand would mean hand-building the toast XML
//! and the `TypedEventHandler` plumbing for no behavioural gain.
//!
//! # AUMID registration requirement
//!
//! `ToastNotificationManager::CreateToastNotifier` takes an
//! [AppUserModelID][aumid], and Windows silently drops toasts from an AUMID it
//! does not know. A shipping build must therefore either
//!
//! * install a Start-menu shortcut carrying the `System.AppUserModel.ID`
//!   property set to the same string (what the MSI/NSIS installer does), or
//! * call `SetCurrentProcessExplicitAppUserModelID` at startup with that string
//!   (what a portable/dev build does),
//!
//! and pass the identical value to [`WindowsNotifier::new`]. The desktop crate
//! owns that value; this module refuses to guess, and an empty AUMID reports
//! [`OsPermission::Unsupported`] rather than pretending to deliver.
//!
//! # Permission model
//!
//! Windows has no runtime notification permission prompt: a registered AUMID
//! may post toasts, and the user turns them off per app in Settings or Focus
//! Assist. There is no API in this dependency to read that state, so
//! [`Notifier::permission`] reports [`OsPermission::Granted`] for a configured
//! AUMID and a user-disabled app surfaces as a failed or silently discarded
//! delivery. Spec 14.5 already forbids treating OS delivery as the state of
//! record, so the in-app activity history remains the truthful one.
//!
//! [aumid]: https://learn.microsoft.com/en-us/windows/win32/shell/appids

use std::sync::Arc;

use async_trait::async_trait;
use tauri_winrt_notification::{Duration, Toast};

use crate::{DeliveryOutcome, NotificationRequest, NotificationTarget, Notifier, OsPermission};

/// Called when the user clicks a toast, with the target the payload carried.
pub type ActivationHandler = Arc<dyn Fn(Option<NotificationTarget>) + Send + Sync>;

/// Windows toast backend.
#[derive(Clone)]
pub struct WindowsNotifier {
    app_user_model_id: String,
    on_activated: Option<ActivationHandler>,
}

impl std::fmt::Debug for WindowsNotifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WindowsNotifier")
            .field("app_user_model_id", &self.app_user_model_id)
            .field("on_activated", &self.on_activated.is_some())
            .finish()
    }
}

impl WindowsNotifier {
    /// Create the backend for a registered AUMID (see the module docs).
    #[must_use]
    pub fn new(app_user_model_id: impl Into<String>) -> Self {
        Self {
            app_user_model_id: app_user_model_id.into(),
            on_activated: None,
        }
    }

    /// Route toast clicks back into the app (spec 5.5: "notification clicks
    /// open the affected recording/job, subject to OS activation rules").
    #[must_use]
    pub fn with_activation_handler(mut self, handler: ActivationHandler) -> Self {
        self.on_activated = Some(handler);
        self
    }

    /// Whether an AUMID was configured at all.
    fn is_registered(&self) -> bool {
        !self.app_user_model_id.trim().is_empty()
    }
}

#[async_trait]
impl Notifier for WindowsNotifier {
    async fn permission(&self) -> OsPermission {
        if self.is_registered() {
            OsPermission::Granted
        } else {
            OsPermission::Unsupported
        }
    }

    async fn request_permission(&self) -> OsPermission {
        // Nothing to prompt for on Windows; registration is an install-time fact.
        self.permission().await
    }

    async fn deliver(&self, req: &NotificationRequest) -> DeliveryOutcome {
        if !self.is_registered() {
            return DeliveryOutcome::Unsupported;
        }

        // Only plain data crosses onto the blocking thread; the toast and its
        // COM handlers are built and consumed there.
        let app_id = self.app_user_model_id.clone();
        let title = req.title.clone();
        let body = req.body.clone();
        let target = req.target;
        let handler = self.on_activated.clone();

        let shown = tokio::task::spawn_blocking(move || {
            let mut toast = Toast::new(&app_id)
                .title(&title)
                .text1(&body)
                .duration(Duration::Short);
            if let Some(handler) = handler {
                // The payload's own target is captured here rather than read
                // back from the activation arguments, so a body click and a
                // future button click route identically.
                toast = toast.on_activated(move |_action| {
                    handler(target);
                    Ok(())
                });
            }
            toast.show()
        })
        .await;

        match shown {
            Ok(Ok(())) => DeliveryOutcome::Delivered,
            Ok(Err(err)) => DeliveryOutcome::Failed(err.to_string()),
            Err(join) => DeliveryOutcome::Failed(format!("toast thread did not complete: {join}")),
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
            NotificationContext::TranscriptionFinished {
                recordings: 1,
                recording: None,
            },
            Timestamp::EPOCH,
        )
    }

    #[tokio::test]
    async fn an_unregistered_aumid_is_unsupported_and_never_posts_a_toast() {
        let notifier = WindowsNotifier::new("   ");
        assert_eq!(notifier.permission().await, OsPermission::Unsupported);
        assert_eq!(
            notifier.request_permission().await,
            OsPermission::Unsupported
        );
        assert_eq!(
            notifier.deliver(&request()).await,
            DeliveryOutcome::Unsupported
        );
    }

    #[tokio::test]
    async fn a_registered_aumid_reports_granted() {
        let notifier = WindowsNotifier::new("com.plaudon.desktop");
        assert_eq!(notifier.permission().await, OsPermission::Granted);
    }
}
