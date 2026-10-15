//! Vendor exports found alongside a recording (matrix row D17).
//!
//! A PLAUD recording often arrives with the vendor's own transcript, summary
//! or tag file next to the audio. The application records every one of them
//! with its provenance, whether or not it was ever turned into a transcript
//! revision, and this panel is where a user can see that provenance.
//!
//! Two honesty rules shape the rendering:
//!
//! * `imported_at` is `null` while an export has only been *recorded*. That
//!   is a different fact from "imported a moment ago", so it gets its own
//!   sentence rather than a blank cell or a substituted date.
//! * the digest is shown as the same twelve-character fingerprint the
//!   provenance table uses: enough to compare two exports by eye, not a value
//!   to retype.
//!
//! No filesystem path is rendered. `original_filename` is the name the export
//! carried, which the backend stores as metadata only; where the bytes live
//! is never part of a UI string.

use egui::{RichText, Ui};
use plaudon_app::command::{AppCommand, AppResponse, CommandId};
use plaudon_app::dto::{ListSourceArtifactsRequest, SourceArtifactSummary};
use plaudon_app::settings_presentation::format_bytes;
use plaudon_domain::error::ErrorEnvelope;
use plaudon_domain::ids::RecordingId;

use crate::ai::fact;
use crate::screens::library::format::format_timestamp;
use crate::screens::library::list::Sender;
use crate::widgets::error_notice::{error_notice, ErrorNoticeAction};

use super::DetailIntent;

/// Name of the detail region that lists the vendor exports.
pub const PANEL_NAME: &str = "Source artifacts";

/// What a recording with no vendor exports says.
pub const NO_ARTIFACTS: &str = "No vendor exports are recorded for this recording.";

/// Why that is not an error.
pub const NO_ARTIFACTS_DETAIL: &str =
    "The source provided no transcript, summary or tag file alongside the audio.";

/// What an export that was recorded but never promoted to a revision says.
///
/// `imported_at` is `null` in exactly this case, and the distinction matters:
/// the bytes were seen and fingerprinted, but nothing in the transcript came
/// from them.
pub const NOT_IMPORTED: &str =
    "Not imported: this export was recorded alongside the audio and has not become a transcript \
     revision.";

/// What the panel says while the list is in flight.
pub const LOADING: &str = "Loading vendor exports…";

/// What failed, in the words of the view rather than of the envelope.
const LOAD_FAILED: &str = "Vendor exports could not be loaded.";

/// The vendor exports of the open recording.
#[derive(Debug, Default)]
pub struct SourceArtifacts {
    recording_id: Option<RecordingId>,
    pending: Option<CommandId>,
    artifacts: Option<Vec<SourceArtifactSummary>>,
    error: Option<ErrorEnvelope>,
}

impl SourceArtifacts {
    /// Nothing loaded and nothing in flight.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            recording_id: None,
            pending: None,
            artifacts: None,
            error: None,
        }
    }

    /// The exports that have arrived, if the list answered.
    #[must_use]
    pub fn artifacts(&self) -> Option<&[SourceArtifactSummary]> {
        self.artifacts.as_deref()
    }

    /// A recording was opened: forget the previous one and ask for its
    /// exports.
    pub fn open(&mut self, recording_id: RecordingId, send: Sender<'_>) {
        self.recording_id = Some(recording_id);
        self.artifacts = None;
        self.error = None;
        self.fetch(send);
    }

    /// Forget everything: the detail view is being left.
    pub fn close(&mut self) {
        *self = Self::new();
    }

    /// Ask for the list again. Used on open, on retry and on the §15.3
    /// events that can add an export to a recording already on screen.
    pub fn fetch(&mut self, send: Sender<'_>) {
        let Some(recording_id) = self.recording_id else {
            return;
        };
        self.pending = Some(send(AppCommand::ListSourceArtifacts(
            ListSourceArtifactsRequest {
                recording_id: recording_id.to_string(),
            },
        )));
    }

    /// Route a dispatcher answer. Returns whether it was ours.
    pub fn on_response(
        &mut self,
        id: CommandId,
        result: &Result<AppResponse, ErrorEnvelope>,
    ) -> bool {
        if self.pending != Some(id) {
            return false;
        }
        self.pending = None;
        match result {
            Ok(AppResponse::ListSourceArtifacts(response)) => {
                self.error = None;
                self.artifacts = Some(response.artifacts.clone());
            }
            Ok(_) => return false,
            Err(envelope) => self.error = Some(envelope.clone()),
        }
        true
    }

    /// Draw the panel. Returns the one intent it cannot act on itself.
    pub fn ui(&mut self, ui: &mut Ui, send: Sender<'_>) -> Option<DetailIntent> {
        if let Some(envelope) = self.error.clone() {
            return match error_notice(ui, &envelope, LOAD_FAILED) {
                Some(ErrorNoticeAction::Retry) => {
                    self.fetch(send);
                    None
                }
                Some(
                    ErrorNoticeAction::OpenSettings
                    | ErrorNoticeAction::InstallSpeechModel
                    | ErrorNoticeAction::ImportModelFile,
                ) => Some(DetailIntent::OpenSettings),
                None => None,
            };
        }

        let Some(artifacts) = self.artifacts.as_deref() else {
            ui.label(LOADING);
            return None;
        };

        if artifacts.is_empty() {
            ui.label(NO_ARTIFACTS);
            ui.label(
                RichText::new(NO_ARTIFACTS_DETAIL)
                    .small()
                    .color(ui.visuals().weak_text_color()),
            );
            return None;
        }

        ui.label(
            RichText::new(count_line(artifacts.len()))
                .small()
                .color(ui.visuals().weak_text_color()),
        );
        for artifact in artifacts {
            egui::Frame::group(ui.style()).show(ui, |ui| artifact_facts(ui, artifact));
        }
        None
    }
}

/// How many exports there are, and the order they are in.
#[must_use]
pub fn count_line(count: usize) -> String {
    if count == 1 {
        "1 vendor export.".to_owned()
    } else {
        format!("{count} vendor exports, oldest first.")
    }
}

/// One export, as the fact rows the rest of the detail view uses.
fn artifact_facts(ui: &mut Ui, artifact: &SourceArtifactSummary) {
    fact(ui, "Kind", &artifact.kind);
    fact(ui, "Vendor", &artifact.source_vendor);
    match &artifact.original_filename {
        Some(name) => fact(ui, "Original file", name),
        None => fact(ui, "Original file", "The export carried no file name."),
    }
    fact(ui, "Size", &format_bytes(Some(artifact.byte_length)));
    fact(
        ui,
        "Content hash (SHA-256)",
        &short_digest(&artifact.content_sha256),
    );
    fact(ui, "Import mode", &artifact.import_mode);
    fact(
        ui,
        "Recorded",
        &format_timestamp(Some(&artifact.recorded_at)),
    );
    match &artifact.imported_at {
        Some(imported_at) => fact(ui, "Imported", &format_timestamp(Some(imported_at))),
        None => {
            ui.label(NOT_IMPORTED);
        }
    }
}

/// The twelve-character fingerprint the provenance table shows, for a digest
/// that arrives as a string rather than as a parsed
/// [`Sha256Digest`](plaudon_domain::media::Sha256Digest).
///
/// Same rule as [`crate::screens::library::format::short_hash`]: a value to
/// compare by eye, never one to retype. A digest shorter than the cut is
/// rendered whole rather than padded, because inventing characters in a hash
/// is worse than showing a short one.
#[must_use]
pub fn short_digest(hex: &str) -> String {
    match hex.char_indices().nth(12) {
        Some((cut, _)) => format!("{}…", &hex[..cut]),
        None => hex.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_digest_is_cut_to_the_same_fingerprint_as_the_provenance_table() {
        let hex = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        assert_eq!(short_digest(hex), "0123456789ab…");
    }

    #[test]
    fn a_short_digest_is_shown_whole_rather_than_padded() {
        assert_eq!(short_digest("abc"), "abc");
    }

    #[test]
    fn the_count_line_states_the_ordering_once_there_is_an_order() {
        assert_eq!(count_line(1), "1 vendor export.");
        assert_eq!(count_line(3), "3 vendor exports, oldest first.");
    }
}
