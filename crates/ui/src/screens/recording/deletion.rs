//! Deleting a recording, with the disclosure the deletion contract requires
//! (matrix row C6; spec §4.6, §7.10, §13.5).
//!
//! The backend refuses to delete anything on a promise. It hands out a
//! disclosure — the title, the row counts, the media blobs and whether each
//! is shared with another recording, the bytes that will actually be freed,
//! and the sentences that say what deletion cannot reach — together with a
//! `confirmation_token` that is the digest of exactly that disclosure. The
//! service recomputes the digest when the confirmation arrives and fails
//! closed if anything changed in between.
//!
//! Three rules follow, and all three are load-bearing:
//!
//! * **The token is echoed verbatim.** This module never computes, parses,
//!   trims or synthesises one. If it is not the string the disclosure
//!   carried, the deletion is refused, and that is the point.
//! * **A refusal is not retried.** A mismatch means the library changed under
//!   the user, so the only honest next step is to fetch the disclosure again
//!   and let them read what it says now. The confirm control is withdrawn
//!   until they have.
//! * **The limits are rendered verbatim, all of them.** They are the part of
//!   the disclosure a user is most likely to be misled by, and summarising
//!   them would be the summary doing the misleading.
//!
//! Nothing here renders a filesystem path: the disclosure names blobs by
//! digest and quality label, which is all the UI ever learns about them.

use egui::{Align, Layout, RichText, Ui};
use plaudon_app::command::{AppCommand, AppResponse, CommandId};
use plaudon_app::dto::{
    DeleteRecordingRequest, DeleteRecordingResponse, DeletionCategoryDto, DeletionDisclosureDto,
    DeletionDisclosureRequest, DeletionMediaDto,
};
use plaudon_app::settings_presentation::format_bytes;
use plaudon_domain::error::{AppErrorCode, ErrorEnvelope};
use plaudon_domain::ids::RecordingId;

use crate::screens::library::list::Sender;

use super::artifacts::short_digest;

/// Label of the control that opens the disclosure.
pub const OPEN_LABEL: &str = "Delete recording…";

/// Label of the destructive confirmation.
pub const CONFIRM_LABEL: &str = "Delete permanently";

/// Label of the control that backs out.
pub const CANCEL_LABEL: &str = "Cancel";

/// Label of the control that fetches the disclosure again after a refusal.
pub const REFETCH_LABEL: &str = "Show what will be removed again";

/// What the modal says while the disclosure is being assembled.
pub const LOADING: &str = "Working out exactly what deleting this recording removes…";

/// What the modal says while the deletion itself is running.
pub const DELETING: &str = "Deleting…";

/// The warning a recording with a user note gets.
pub const ANNOTATION_WARNING: &str = "⚠ Your note on this recording is deleted with it.";

/// Heading over the category counts.
pub const REMOVED_HEADING: &str = "What will be removed";

/// Heading over the media rows.
pub const MEDIA_HEADING: &str = "Audio files";

/// Heading over the limit sentences.
pub const LIMITS_HEADING: &str = "What deleting cannot do";

/// What a refused confirmation means, in the user's terms.
///
/// The service recomputed the digest of the current disclosure and it no
/// longer matched the one the user read, so nothing was deleted. Re-sending
/// the same token would be refused for the same reason, so the only thing
/// offered is a fresh disclosure.
pub const MISMATCH_EXPLANATION: &str =
    "This recording changed while the list above was on screen, so the deletion was refused and \
     nothing was deleted. Fetch what will be removed again and read it before confirming.";

/// What any other failure means. Followed by the envelope's own message.
pub const FAILED_EXPLANATION: &str = "Nothing was deleted.";

/// What the disclosure flow is doing.
#[derive(Debug, Default)]
enum Stage {
    /// No disclosure on screen.
    #[default]
    Closed,
    /// `get_deletion_disclosure` is in flight.
    Loading(CommandId),
    /// The disclosure is on screen, with the refusal that followed the last
    /// confirmation if there was one.
    Shown {
        disclosure: Box<DeletionDisclosureDto>,
        failure: Option<ErrorEnvelope>,
    },
    /// The disclosure the user confirmed is on screen and `delete_recording`
    /// is in flight. The disclosure is kept so a refusal can be shown
    /// against what the user actually read.
    Deleting {
        disclosure: Box<DeletionDisclosureDto>,
        pending: CommandId,
    },
    /// The deletion succeeded and the host has not been told yet.
    Deleted(Box<DeleteRecordingResponse>),
    /// The disclosure could not be fetched at all.
    Unavailable(ErrorEnvelope),
}

/// What a frame of the disclosure produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeletionEvent {
    /// The recording is gone. The string states what the backend reported it
    /// actually did, for the status line of the screen the user is about to
    /// be returned to.
    Deleted(String),
}

/// The deletion disclosure and its confirmation.
#[derive(Debug, Default)]
pub struct DeletionFlow {
    recording_id: Option<RecordingId>,
    stage: Stage,
}

impl DeletionFlow {
    /// A flow with nothing open.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            recording_id: None,
            stage: Stage::Closed,
        }
    }

    /// Whether the disclosure is on screen in any of its states.
    #[must_use]
    pub const fn is_open(&self) -> bool {
        !matches!(self.stage, Stage::Closed)
    }

    /// The disclosure currently on screen, if one has arrived.
    #[must_use]
    pub fn disclosure(&self) -> Option<&DeletionDisclosureDto> {
        match &self.stage {
            Stage::Shown { disclosure, .. } | Stage::Deleting { disclosure, .. } => {
                Some(disclosure)
            }
            _ => None,
        }
    }

    /// Open the disclosure for `recording_id`.
    pub fn open(&mut self, recording_id: RecordingId, send: Sender<'_>) {
        self.recording_id = Some(recording_id);
        self.fetch(send);
    }

    /// Close without deleting anything, and forget the recording.
    pub fn close(&mut self) {
        *self = Self::new();
    }

    fn fetch(&mut self, send: Sender<'_>) {
        let Some(recording_id) = self.recording_id else {
            return;
        };
        let id = send(AppCommand::GetDeletionDisclosure(
            DeletionDisclosureRequest {
                recording_id: recording_id.to_string(),
            },
        ));
        self.stage = Stage::Loading(id);
    }

    /// Route a dispatcher answer. Returns whether it was ours.
    pub fn on_response(
        &mut self,
        id: CommandId,
        result: &Result<AppResponse, ErrorEnvelope>,
    ) -> bool {
        match std::mem::take(&mut self.stage) {
            Stage::Loading(pending) if pending == id => match result {
                Ok(AppResponse::GetDeletionDisclosure(disclosure)) => {
                    self.stage = Stage::Shown {
                        disclosure: Box::new(disclosure.clone()),
                        failure: None,
                    };
                    true
                }
                Err(envelope) => {
                    self.stage = Stage::Unavailable(envelope.clone());
                    true
                }
                Ok(_) => {
                    self.stage = Stage::Loading(pending);
                    false
                }
            },
            Stage::Deleting {
                disclosure,
                pending,
            } if pending == id => match result {
                Ok(AppResponse::DeleteRecording(response)) => {
                    self.stage = Stage::Deleted(Box::new(response.clone()));
                    true
                }
                Err(envelope) => {
                    self.stage = Stage::Shown {
                        disclosure,
                        failure: Some(envelope.clone()),
                    };
                    true
                }
                Ok(_) => {
                    self.stage = Stage::Deleting {
                        disclosure,
                        pending,
                    };
                    false
                }
            },
            other => {
                self.stage = other;
                false
            }
        }
    }

    /// Draw the disclosure, if it is open.
    pub fn ui(&mut self, ui: &mut Ui, send: Sender<'_>) -> Option<DeletionEvent> {
        if matches!(self.stage, Stage::Closed) {
            return None;
        }
        if matches!(self.stage, Stage::Deleted(_)) {
            let Stage::Deleted(response) = std::mem::take(&mut self.stage) else {
                unreachable!("the stage was just checked")
            };
            self.recording_id = None;
            return Some(DeletionEvent::Deleted(deletion_summary(&response)));
        }

        let mut action = None;
        let modal =
            egui::Modal::new(egui::Id::new("recording-deletion-disclosure")).show(ui.ctx(), |ui| {
                // A fixed width, not a minimum: the limit sentences are long
                // enough that an unconstrained modal would lay them out on
                // one line and push its own buttons past the window edge.
                ui.set_width(560.0);
                ui.label(RichText::new(self.title()).heading().strong());
                ui.add_space(8.0);
                // Half the viewport, so the heading above it and the confirm
                // control below it are on screen on a laptop display as well
                // as on a large one: a destructive confirmation whose buttons
                // are off screen is not one.
                let max_height = (ui.ctx().content_rect().height() * 0.5).max(240.0);
                egui::ScrollArea::vertical()
                    .max_height(max_height)
                    .auto_shrink([false, true])
                    .show(ui, |ui| self.body(ui));
                ui.add_space(12.0);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    action = self.footer(ui);
                });
            });

        match action {
            Some(Action::Confirm) => self.confirm(send),
            Some(Action::Refetch) => self.fetch(send),
            Some(Action::Cancel) => self.close(),
            // A deletion already in flight cannot be called back, so Escape
            // and a backdrop click do not pretend to cancel it.
            None if modal.should_close() && !matches!(self.stage, Stage::Deleting { .. }) => {
                self.close();
            }
            None => {}
        }
        None
    }

    /// Send the confirmation, carrying the token the disclosure arrived with
    /// and nothing else.
    fn confirm(&mut self, send: Sender<'_>) {
        let Stage::Shown { disclosure, .. } = std::mem::take(&mut self.stage) else {
            return;
        };
        let id = send(AppCommand::DeleteRecording(DeleteRecordingRequest {
            recording_id: disclosure.recording_id.clone(),
            confirmation_token: disclosure.confirmation_token.clone(),
        }));
        self.stage = Stage::Deleting {
            disclosure,
            pending: id,
        };
    }

    fn title(&self) -> String {
        match self.disclosure() {
            Some(disclosure) => format!("Delete “{}”?", disclosure.title),
            None => "Delete this recording?".to_owned(),
        }
    }

    fn body(&self, ui: &mut Ui) {
        match &self.stage {
            Stage::Loading(_) => {
                ui.label(LOADING);
            }
            Stage::Unavailable(envelope) => {
                alert(ui, &format!("{FAILED_EXPLANATION} {}", envelope.message));
                diagnostic(ui, envelope);
            }
            Stage::Shown {
                disclosure,
                failure,
            } => {
                disclosure_body(ui, disclosure);
                if let Some(envelope) = failure {
                    ui.add_space(12.0);
                    alert(ui, &refusal_message(envelope));
                    diagnostic(ui, envelope);
                }
            }
            Stage::Deleting { disclosure, .. } => {
                disclosure_body(ui, disclosure);
                ui.add_space(12.0);
                ui.label(DELETING);
            }
            Stage::Closed | Stage::Deleted(_) => {}
        }
    }

    fn footer(&self, ui: &mut Ui) -> Option<Action> {
        match &self.stage {
            // Nothing can be confirmed before the disclosure has arrived,
            // and nothing can be called back once the deletion is running.
            Stage::Deleting { .. } | Stage::Closed | Stage::Deleted(_) => None,
            Stage::Loading(_) => ui.button(CANCEL_LABEL).clicked().then_some(Action::Cancel),
            Stage::Unavailable(_) => {
                let refetch = ui.button(REFETCH_LABEL).clicked();
                let cancel = ui.button(CANCEL_LABEL).clicked();
                refetch
                    .then_some(Action::Refetch)
                    .or(cancel.then_some(Action::Cancel))
            }
            Stage::Shown { failure, .. } => {
                // After a refusal the token in hand is known to be stale, so
                // the destructive control is withdrawn until a fresh
                // disclosure has been read: there is no blind retry.
                let primary = if failure.is_some() {
                    ui.button(REFETCH_LABEL)
                        .clicked()
                        .then_some(Action::Refetch)
                } else {
                    danger_button(ui, CONFIRM_LABEL)
                        .clicked()
                        .then_some(Action::Confirm)
                };
                let cancel = ui.button(CANCEL_LABEL).clicked();
                primary.or(cancel.then_some(Action::Cancel))
            }
        }
    }
}

/// What the footer asked for this frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Confirm,
    Refetch,
    Cancel,
}

/// A destructive action button: the label itself carries the danger colour,
/// so the warning is on the words and not on a shape alone.
fn danger_button(ui: &mut Ui, label: &str) -> egui::Response {
    ui.add(egui::Button::new(
        RichText::new(label).color(ui.visuals().error_fg_color),
    ))
}

/// The whole disclosure, in the order a user has to read it.
fn disclosure_body(ui: &mut Ui, disclosure: &DeletionDisclosureDto) {
    ui.label(format!(
        "This permanently deletes “{}” from this library. It cannot be undone.",
        disclosure.title
    ));

    if disclosure.has_annotation {
        ui.add_space(8.0);
        ui.label(RichText::new(ANNOTATION_WARNING).color(ui.visuals().warn_fg_color));
    }

    ui.add_space(12.0);
    ui.label(RichText::new(REMOVED_HEADING).strong());
    if disclosure.categories.is_empty() {
        ui.label("No database rows are removed.");
    } else {
        for category in &disclosure.categories {
            ui.label(category_line(category));
        }
    }

    if !disclosure.media.is_empty() {
        ui.add_space(12.0);
        ui.label(RichText::new(MEDIA_HEADING).strong());
        for media in &disclosure.media {
            ui.label(media_line(media));
        }
    }

    ui.add_space(12.0);
    ui.label(format!(
        "Storage freed: {}",
        format_bytes(Some(disclosure.bytes_to_free))
    ));

    if !disclosure.limits.is_empty() {
        ui.add_space(12.0);
        ui.label(RichText::new(LIMITS_HEADING).strong());
        // Verbatim, every one of them. These sentences are the contract's
        // own words about what deletion cannot reach; shortening one would
        // be the interface overstating what it is about to do.
        for limit in &disclosure.limits {
            ui.label(limit);
        }
    }
}

/// `Transcript revisions: 3`.
#[must_use]
pub fn category_line(category: &DeletionCategoryDto) -> String {
    format!("{}: {}", category.label, category.count)
}

/// One media blob and its fate, including why a shared blob survives.
#[must_use]
pub fn media_line(media: &DeletionMediaDto) -> String {
    let fate = if media.shared_with_other_recordings {
        "kept — another recording uses the same audio"
    } else if media.bytes_will_be_removed {
        "removed from disk"
    } else {
        "kept — these bytes are not removed by this deletion"
    };
    format!(
        "{} ({}, {}) — {fate}",
        media.quality_label,
        format_bytes(Some(media.byte_length)),
        short_digest(&media.sha256),
    )
}

/// Why the confirmation was refused.
///
/// A digest mismatch is its own situation and gets its own sentence; every
/// other failure states that nothing was deleted and then the envelope's own
/// vendor-neutral message.
#[must_use]
pub fn refusal_message(envelope: &ErrorEnvelope) -> String {
    if envelope.code == AppErrorCode::ConfirmationRequired {
        MISMATCH_EXPLANATION.to_owned()
    } else {
        format!("{FAILED_EXPLANATION} {}", envelope.message)
    }
}

/// What the deletion actually did, from the response and from nothing else.
#[must_use]
pub fn deletion_summary(response: &DeleteRecordingResponse) -> String {
    let removed = if response.blobs_removed == 1 {
        "1 audio file removed".to_owned()
    } else {
        format!("{} audio files removed", response.blobs_removed)
    };
    let mut parts = vec![removed];
    if response.blobs_retained_shared > 0 {
        parts.push(if response.blobs_retained_shared == 1 {
            "1 kept because another recording shares it".to_owned()
        } else {
            format!(
                "{} kept because other recordings share them",
                response.blobs_retained_shared
            )
        });
    }
    parts.push(format!(
        "{} freed",
        format_bytes(Some(response.bytes_freed))
    ));
    parts.push(
        if response.tombstoned {
            "a tombstone was recorded so a later import does not bring it back"
        } else {
            "no tombstone was recorded"
        }
        .to_owned(),
    );
    format!("Recording deleted: {}.", parts.join(", "))
}

/// A failure sentence an assistive technology announces as an alert.
fn alert(ui: &mut Ui, message: &str) {
    let response =
        ui.label(RichText::new(format!("✕ {message}")).color(ui.visuals().error_fg_color));
    let announced = message.to_owned();
    response.widget_info(|| {
        let mut info = egui::WidgetInfo::labeled(egui::WidgetType::Label, true, &announced);
        info.current_text_value = Some(announced.clone());
        info
    });
    let label = message.to_owned();
    ui.ctx().accesskit_node_builder(response.id, |node| {
        node.set_role(egui::accesskit::Role::Alert);
        node.set_label(label);
    });
}

/// The identifier a support request quotes, as every other failure shows it.
fn diagnostic(ui: &mut Ui, envelope: &ErrorEnvelope) {
    ui.label(
        RichText::new(format!("Diagnostic id: {}", envelope.diagnostic_id))
            .small()
            .color(ui.visuals().weak_text_color()),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response(
        removed: u64,
        retained: u64,
        freed: u64,
        tombstoned: bool,
    ) -> DeleteRecordingResponse {
        DeleteRecordingResponse {
            deletion_id: "01932f00-0000-7000-8000-000000000000".to_owned(),
            blobs_removed: removed,
            blobs_retained_shared: retained,
            bytes_freed: freed,
            tombstoned,
        }
    }

    #[test]
    fn the_summary_reports_every_field_of_the_response() {
        let summary = deletion_summary(&response(2, 1, 3_145_728, true));
        assert!(summary.contains("2 audio files removed"), "{summary}");
        assert!(
            summary.contains("1 kept because another recording shares it"),
            "{summary}"
        );
        assert!(summary.contains("3.0 MB freed"), "{summary}");
        assert!(summary.contains("a tombstone was recorded"), "{summary}");
    }

    #[test]
    fn a_deletion_without_a_tombstone_says_so_rather_than_staying_silent() {
        let summary = deletion_summary(&response(1, 0, 0, false));
        assert!(summary.contains("1 audio file removed"), "{summary}");
        assert!(summary.contains("no tombstone was recorded"), "{summary}");
        assert!(
            !summary.contains("kept because"),
            "nothing was retained, so nothing is claimed: {summary}"
        );
    }

    #[test]
    fn a_shared_blob_says_it_survives_and_why() {
        let line = media_line(&DeletionMediaDto {
            sha256: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".to_owned(),
            byte_length: 2048,
            quality_label: "Original recording".to_owned(),
            shared_with_other_recordings: true,
            bytes_will_be_removed: false,
        });
        assert!(
            line.contains("kept — another recording uses the same audio"),
            "{line}"
        );
        assert!(line.contains("0123456789ab…"), "{line}");
    }

    #[test]
    fn a_mismatch_is_explained_as_a_change_rather_than_as_a_retryable_fault() {
        use plaudon_domain::error::{AppError, DiagnosticLog};
        use plaudon_domain::time::Timestamp;
        let mut log = DiagnosticLog::new();
        let envelope = AppError::new(AppErrorCode::ConfirmationRequired, "digest mismatch")
            .to_envelope(&mut log, Timestamp::now());
        assert_eq!(refusal_message(&envelope), MISMATCH_EXPLANATION);
    }
}
