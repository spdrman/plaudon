//! What the desktop can ask the shell to show (parity rows C10 and J15).
//!
//! Three different things end here: a notification click, which arrives as a
//! `§15.3` `ui.activation_requested` event; a `plaudon://` deep link, which
//! the host parses with `plaudon_platform::activation`; and a document the
//! operating system asked the application to open, because the installer
//! claimed the `.plaudon` extension.
//!
//! The shell does not depend on `plaudon-platform` — it is the view layer and
//! must build with no operating system underneath it — so the host converts
//! its `Activation` into an [`ActivationTarget`] and its classified paths
//! into an [`OpenRequest`]. Both types are plain data: a test can build one
//! and drive the shell with no window, no dispatcher and no OS.

use plaudon_app::dto::UiEvent;
use plaudon_domain::ids::{JobId, RecordingId};

use super::route::Route;

/// Where an activation asks the shell to go.
///
/// The same three cases `plaudon_platform::activation::Activation` and
/// `plaudon_notifications::NotificationTarget` carry, named once more here so
/// that the view layer owns its own vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivationTarget {
    /// Open a recording.
    Recording(RecordingId),
    /// Open a job in the activity view.
    Job(JobId),
    /// Open the settings screen.
    Settings,
}

impl ActivationTarget {
    /// The route this target names.
    #[must_use]
    pub fn route(self) -> Route {
        match self {
            Self::Recording(recording_id) => Route::LibraryRecording {
                recording_id: recording_id.to_string(),
            },
            Self::Job(job_id) => Route::JobsJob {
                job_id: job_id.to_string(),
            },
            Self::Settings => Route::Settings,
        }
    }

    /// The target an `ui.activation_requested` event names, if it names one.
    ///
    /// A recording or job activation carries its id in the event's own
    /// columns. A settings activation has no id to carry, so the only thing
    /// that distinguishes it from an event naming nothing is the hint
    /// `plaudon_app::notify::publish_activation` writes. Reading it is not
    /// guessing: an event with no id and no hint still raises the window and
    /// leaves the user where they were.
    #[must_use]
    pub fn from_event(event: &UiEvent) -> Option<Self> {
        if let Some(recording_id) = event.recording_id {
            return Some(Self::Recording(recording_id));
        }
        if let Some(job_id) = event.job_id {
            return Some(Self::Job(job_id));
        }
        let kind = event
            .payload
            .get("target")
            .and_then(|target| target.get("kind"))
            .and_then(|kind| kind.as_str())?;
        (kind == "settings").then_some(Self::Settings)
    }
}

/// Documents the desktop asked the application to open (parity row C10).
///
/// Built by the host from `plaudon_platform::dragdrop::classify`, which is
/// the same validation a window drop goes through: every path in `audio` and
/// `archives` has already been resolved inside an approved root, and
/// `refused` carries the content-free reason codes of the ones that were
/// not, so the shell can say that something was refused without naming a
/// path it must not put on screen.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OpenRequest {
    /// Audio to import, as the import command takes it.
    pub audio: Vec<String>,
    /// Portable archives to restore from.
    pub archives: Vec<String>,
    /// Stable reason codes of the paths that were refused.
    pub refused: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use plaudon_app::dto::EventKind;

    fn event(payload: serde_json::Value) -> UiEvent {
        UiEvent {
            seq: 1,
            kind: EventKind::ActivationRequested,
            occurred_at: "2026-01-01T00:00:00Z".to_owned(),
            job_id: None,
            recording_id: None,
            payload: payload
                .as_object()
                .expect("an object payload")
                .clone()
                .into_iter()
                .collect(),
        }
    }

    #[test]
    fn an_event_naming_a_recording_names_a_recording() {
        let recording_id = RecordingId::new();
        let mut raised = event(serde_json::json!({}));
        raised.recording_id = Some(recording_id);
        assert_eq!(
            ActivationTarget::from_event(&raised),
            Some(ActivationTarget::Recording(recording_id))
        );
    }

    #[test]
    fn a_settings_activation_is_read_from_the_hint_it_was_published_with() {
        let raised = event(serde_json::json!({ "target": { "kind": "settings", "id": null } }));
        assert_eq!(
            ActivationTarget::from_event(&raised),
            Some(ActivationTarget::Settings)
        );
    }

    #[test]
    fn an_event_that_names_nothing_names_nothing() {
        assert_eq!(
            ActivationTarget::from_event(&event(serde_json::json!({}))),
            None
        );
        assert_eq!(
            ActivationTarget::from_event(&event(
                serde_json::json!({ "target": { "kind": "something-else" } })
            )),
            None
        );
    }

    #[test]
    fn every_target_has_a_route() {
        let recording_id = RecordingId::new();
        assert_eq!(
            ActivationTarget::Recording(recording_id).route(),
            Route::LibraryRecording {
                recording_id: recording_id.to_string()
            }
        );
        assert_eq!(ActivationTarget::Settings.route(), Route::Settings);
    }
}
