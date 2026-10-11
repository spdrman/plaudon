//! Native macOS and Windows notifications.
//!
//! Spec 5.5 limits completion notices to **macOS Notification Center** and
//! **Windows toast notifications**, explicitly ruling out SMTP, push services
//! and browser notification infrastructure. Spec 14.5 adds that notifications
//! must never block jobs and that OS delivery is never a source of truth, and
//! spec 22(13) makes "privacy safe, nonblocking, and every completion event
//! still visible in the app regardless of OS permission" a release gate.
//!
//! The crate therefore exposes four things and nothing else:
//!
//! * the [`Notifier`] port plus the small value types it speaks,
//! * [`privacy`] — the only supported way to build a [`NotificationRequest`],
//!   which by construction cannot carry free text,
//! * [`dispatcher::Dispatcher`] — preference gate, permission gate, and a
//!   containment boundary around the platform backend,
//! * [`testing::RecordingNotifier`] — the in-memory double used by this crate's
//!   tests and by the desktop crate.
//!
//! The in-app activity history (the actual record of what happened) lives in
//! the desktop/persistence layer. This crate only reports what the OS did with
//! a payload; callers persist the outcome, they never derive state from it.

use std::collections::BTreeMap;

use async_trait::async_trait;
use plaudon_domain::ids::{JobId, RecordingId};
use plaudon_domain::time::Timestamp;
use serde::{Deserialize, Serialize};

pub mod deps_policy;
pub mod dispatcher;
pub mod privacy;
pub mod testing;

#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(target_os = "windows")]
pub mod windows;

pub use dispatcher::Dispatcher;
pub use privacy::{AttentionReason, BudgetScope, NotificationContext};

/// Cargo package name of this crate, used by the workspace layout conformance test.
pub const PACKAGE_NAME: &str = "plaudon-notifications";

/// Version of the inter-module contract exposed by this crate (spec 6.6).
///
/// Capability and contract payloads are versioned independently of the
/// application release, so consumers can detect an unknown contract instead of
/// assuming an unavailable feature is present.
pub const CONTRACT_VERSION: u32 = 1;

/// The four completion notices named in spec 5.5.
///
/// The wire form matches `NOTIFICATION_CATEGORIES` in
/// `apps/desktop/src/ipc/contract.ts`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationCategory {
    /// An import batch finished (spec 7.4).
    ImportFinished,
    /// A transcription job reached `Completed` (spec 14.7).
    TranscriptionFinished,
    /// A job needs a decision: authorization, consent, retry or disk space.
    RequiresAttention,
    /// Cloud spending passed the configured limit and cloud work is paused.
    CloudBudgetExceeded,
}

impl NotificationCategory {
    /// Every category, in spec 5.5 order.
    pub const ALL: [Self; 4] = [
        Self::ImportFinished,
        Self::TranscriptionFinished,
        Self::RequiresAttention,
        Self::CloudBudgetExceeded,
    ];

    /// Stable wire identifier, identical to the serde representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ImportFinished => "import_finished",
            Self::TranscriptionFinished => "transcription_finished",
            Self::RequiresAttention => "requires_attention",
            Self::CloudBudgetExceeded => "cloud_budget_exceeded",
        }
    }
}

/// What a notification click should open, subject to OS activation rules (spec 5.5).
///
/// This is the in-process representation. Mapping it onto the
/// `ActivityEntry.target` wire shape is the desktop crate's job; [`Self::kind`]
/// and [`Self::id`] exist so that mapping needs no `match` on a foreign enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationTarget {
    /// Open a recording.
    Recording(RecordingId),
    /// Open a job in the activity/jobs view.
    Job(JobId),
    /// Open the notification settings pane.
    Settings,
}

impl NotificationTarget {
    /// Wire `kind` discriminator.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Recording(_) => "recording",
            Self::Job(_) => "job",
            Self::Settings => "settings",
        }
    }

    /// Canonical identifier string, or `None` for targets that have no id.
    #[must_use]
    pub fn id(&self) -> Option<String> {
        match self {
            Self::Recording(id) => Some(id.to_string()),
            Self::Job(id) => Some(id.to_string()),
            Self::Settings => None,
        }
    }

    /// Opaque, user-content-free token handed to the OS so that an activation
    /// can be routed back to this target.
    ///
    /// macOS carries it in `UNNotificationContent.targetContentIdentifier`,
    /// Windows in the toast activation handler. It contains an identifier at
    /// most, never a title or a path.
    #[must_use]
    pub fn activation_token(&self) -> String {
        match self.id() {
            Some(id) => format!("{}:{id}", self.kind()),
            None => self.kind().to_owned(),
        }
    }

    /// Parse a token produced by [`Self::activation_token`]; `None` when the
    /// OS handed back something we did not write.
    #[must_use]
    pub fn from_activation_token(token: &str) -> Option<Self> {
        match token.split_once(':') {
            Some(("recording", id)) => id.parse().ok().map(Self::Recording),
            Some(("job", id)) => id.parse().ok().map(Self::Job),
            Some(_) => None,
            None => (token == "settings").then_some(Self::Settings),
        }
    }
}

/// A fully built, privacy-safe notification payload.
///
/// [`privacy::build`] is the only supported constructor: it takes structured
/// context (counts, category, target id) rather than strings, which is what
/// keeps transcript text, recording titles and file paths off lock screens.
/// Nothing in the application composes `title`/`body` by hand; the fields are
/// public only so that the desktop layer can copy them into an activity entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotificationRequest {
    /// Which of the four spec 5.5 notices this is.
    pub category: NotificationCategory,
    /// Short generic headline.
    pub title: String,
    /// Generic body: counts and categories only, never user content.
    pub body: String,
    /// What a click opens, if anything.
    pub target: Option<NotificationTarget>,
    /// When the underlying event happened, not when the OS displayed it.
    pub occurred_at: Timestamp,
}

/// What actually happened to one [`NotificationRequest`].
///
/// Callers record this next to the activity entry; they never treat it as the
/// state of the underlying job (spec 14.5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryOutcome {
    /// The OS accepted the payload.
    Delivered,
    /// The master switch is off, or the category is not opted in.
    SuppressedByPreference,
    /// The user denied the OS notification permission.
    SuppressedByOsPermission,
    /// This platform has no supported notification backend.
    Unsupported,
    /// The backend errored, panicked or did not answer in time.
    ///
    /// The payload is an internal detail for logs and diagnostics; the UI shows
    /// [`Self::suppression_reason`] instead.
    Failed(String),
}

impl DeliveryOutcome {
    /// Whether the OS accepted the payload, for `ActivityEntry.notification_delivered`.
    #[must_use]
    pub const fn delivered(&self) -> bool {
        matches!(self, Self::Delivered)
    }

    /// Stable, user-content-free reason code for
    /// `ActivityEntry.notification_suppressed_reason`; `None` when delivered.
    #[must_use]
    pub const fn suppression_reason(&self) -> Option<&'static str> {
        match self {
            Self::Delivered => None,
            Self::SuppressedByPreference => Some("suppressed_by_preference"),
            Self::SuppressedByOsPermission => Some("suppressed_by_os_permission"),
            Self::Unsupported => Some("unsupported"),
            Self::Failed(_) => Some("failed"),
        }
    }
}

/// The OS-level notification permission as last observed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OsPermission {
    /// The user authorized notifications.
    Granted,
    /// The user denied notifications; we suppress without retrying prompts.
    Denied,
    /// No prompt has been shown yet.
    NotRequested,
    /// The platform has no notification backend we support.
    Unsupported,
}

/// Port implemented by each platform backend and by the test double.
///
/// Implementations are expected to be cheap to clone behind an [`std::sync::Arc`]
/// and must tolerate being called from any tokio worker thread.
#[async_trait]
pub trait Notifier: Send + Sync {
    /// Current permission state, without prompting the user.
    async fn permission(&self) -> OsPermission;

    /// Prompt for permission if the platform supports prompting, then report
    /// the resulting state.
    async fn request_permission(&self) -> OsPermission;

    /// Hand one payload to the OS.
    async fn deliver(&self, req: &NotificationRequest) -> DeliveryOutcome;
}

/// Per-category opt-in preferences (spec 5.5).
///
/// Minimal default: **everything off**. `enabled` is the master switch and every
/// category is individually opt-in, so a fresh install delivers no OS
/// notification until onboarding (or the settings pane) records an explicit
/// choice. The in-app activity history is unaffected by these preferences.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotificationSettings {
    /// Master switch; `false` suppresses every category.
    pub enabled: bool,
    /// Per-category opt-in. A missing entry counts as not opted in.
    pub categories: BTreeMap<NotificationCategory, bool>,
}

impl NotificationSettings {
    /// All four categories present and set to `value`, with the master switch
    /// set to `value` as well.
    #[must_use]
    pub fn uniform(value: bool) -> Self {
        Self {
            enabled: value,
            categories: NotificationCategory::ALL
                .into_iter()
                .map(|category| (category, value))
                .collect(),
        }
    }

    /// Set one category's opt-in, returning `self` for chaining.
    #[must_use]
    pub fn with_category(mut self, category: NotificationCategory, value: bool) -> Self {
        self.categories.insert(category, value);
        self
    }

    /// Set the master switch, returning `self` for chaining.
    #[must_use]
    pub fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Whether `category` may be delivered to the OS right now.
    #[must_use]
    pub fn allows(&self, category: NotificationCategory) -> bool {
        self.enabled && self.categories.get(&category).copied().unwrap_or(false)
    }
}

impl Default for NotificationSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            categories: NotificationCategory::ALL
                .into_iter()
                .map(|category| (category, false))
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_name_matches_cargo() {
        assert_eq!(PACKAGE_NAME, env!("CARGO_PKG_NAME"));
    }

    #[test]
    fn categories_use_the_typescript_wire_names() {
        let wire: Vec<String> = NotificationCategory::ALL
            .iter()
            .map(|category| {
                serde_json::to_string(category).unwrap_or_else(|_| "<unserializable>".to_owned())
            })
            .collect();
        assert_eq!(
            wire,
            vec![
                "\"import_finished\"",
                "\"transcription_finished\"",
                "\"requires_attention\"",
                "\"cloud_budget_exceeded\"",
            ]
        );
        for category in NotificationCategory::ALL {
            assert_eq!(
                format!("\"{}\"", category.as_str()),
                serde_json::to_string(&category).unwrap_or_default()
            );
        }
    }

    #[test]
    fn default_settings_are_fully_opt_in() {
        let settings = NotificationSettings::default();
        assert!(!settings.enabled);
        for category in NotificationCategory::ALL {
            assert!(!settings.allows(category));
            assert_eq!(settings.categories.get(&category), Some(&false));
        }
    }

    #[test]
    fn master_switch_overrides_category_opt_in() {
        let settings = NotificationSettings::uniform(true).with_enabled(false);
        assert!(!settings.allows(NotificationCategory::ImportFinished));

        let settings = NotificationSettings::uniform(true)
            .with_category(NotificationCategory::ImportFinished, false);
        assert!(!settings.allows(NotificationCategory::ImportFinished));
        assert!(settings.allows(NotificationCategory::RequiresAttention));
    }

    #[test]
    fn outcomes_map_to_the_activity_entry_fields() {
        assert!(DeliveryOutcome::Delivered.delivered());
        assert_eq!(DeliveryOutcome::Delivered.suppression_reason(), None);
        assert!(!DeliveryOutcome::SuppressedByPreference.delivered());
        assert_eq!(
            DeliveryOutcome::SuppressedByPreference.suppression_reason(),
            Some("suppressed_by_preference")
        );
        assert_eq!(
            DeliveryOutcome::SuppressedByOsPermission.suppression_reason(),
            Some("suppressed_by_os_permission")
        );
        assert_eq!(
            DeliveryOutcome::Unsupported.suppression_reason(),
            Some("unsupported")
        );
        assert_eq!(
            DeliveryOutcome::Failed("backend exploded".to_owned()).suppression_reason(),
            Some("failed")
        );
    }

    #[test]
    fn targets_expose_wire_kind_and_id() {
        let recording = RecordingId::new();
        let target = NotificationTarget::Recording(recording);
        assert_eq!(target.kind(), "recording");
        assert_eq!(target.id(), Some(recording.to_string()));

        let job = JobId::new();
        let target = NotificationTarget::Job(job);
        assert_eq!(target.kind(), "job");
        assert_eq!(target.id(), Some(job.to_string()));

        assert_eq!(NotificationTarget::Settings.kind(), "settings");
        assert_eq!(NotificationTarget::Settings.id(), None);
    }

    #[test]
    fn activation_tokens_round_trip() {
        for target in [
            NotificationTarget::Recording(RecordingId::new()),
            NotificationTarget::Job(JobId::new()),
            NotificationTarget::Settings,
        ] {
            let token = target.activation_token();
            assert_eq!(
                NotificationTarget::from_activation_token(&token),
                Some(target)
            );
        }
    }

    #[test]
    fn unknown_activation_tokens_are_rejected() {
        for token in [
            "",
            "recording",
            "recording:not-a-uuid",
            "job:",
            "nonsense:1",
            "Settings",
        ] {
            assert_eq!(
                NotificationTarget::from_activation_token(token),
                None,
                "{token:?}"
            );
        }
    }
}
