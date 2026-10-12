//! The transcript panel of the recording detail: `get_transcript` in, the
//! editor in the middle, `save_transcript_revision` out.
//!
//! The editing rules are not here — they are in `plaudon-transcript-doc`, and
//! the widget is [`crate::transcript::TranscriptEditor`]. What is here is the
//! wiring that only exists once the two meet a real backend:
//!
//! * **paging.** `get_transcript` answers in cursor pages; a document is
//!   opened only when the last page has arrived, so a 100,000-segment
//!   transcript never shows a half-built document that undo could not
//!   describe.
//! * **autosave.** The session decides *when* (debounce, maximum delay,
//!   backoff); this decides what happens then: one `save_transcript_revision`
//!   against the base revision, and the new revision id fed back so the next
//!   save is expressed against it.
//! * **conflict.** A `CONFLICT` envelope stops autosave and keeps the user's
//!   text. The two resolutions are the ones the React pane offered: reload
//!   the newest revision, or copy your version out first.
//! * **revisions.** A revision that is not the head opens read-only, exactly
//!   as `RevisionSwitcher` behaved.
//! * **sync.** The highlight follows the player through
//!   [`crate::player::SnapshotPosition`], and a timestamp chip the user
//!   activates comes back out as a seek for the player to carry out.
//!
//! Answers are matched by **content**, not by command id: a panel reports
//! commands rather than dispatching them, so it never learns the
//! [`plaudon_app::command::CommandId`] the host assigned. A transcript page
//! identifies its revision and its first segment, a speaker list identifies
//! its recording, and a save answer carries the new revision — which is more
//! than enough to tell a late answer from the one being waited for.

use egui::Ui;
use plaudon_app::command::{AppCommand, AppResponse};
use plaudon_app::dto::{
    GetTranscriptRequest, ListSpeakersRequest, ListSpeakersResponse, RecordingDetail, RevisionKind,
    SaveTranscriptRevisionRequest, SegmentEdit, SetSegmentSpeakerRequest, SpeakerDto,
    TranscriptPage, TranscriptRevisionRef, TranscriptSegmentDto,
};
use plaudon_domain::error::{AppErrorCode, ErrorEnvelope};
use plaudon_domain::ids::{RecordingId, SpeakerId, TranscriptRevisionId};
use plaudon_transcript_doc::autosave::SaveRequest;
use plaudon_transcript_doc::doc::{DocumentSource, SegmentId, SegmentSeed, TranscriptDocument};
use plaudon_transcript_doc::playback::PlaybackPosition;
use plaudon_transcript_doc::session::EditSession;

use crate::ai::AiOutput;
use crate::transcript::{ConflictChoice, SpeakerChoice, TranscriptEditor, TranscriptEditorState};

/// A segment reassignment waiting for its `set_segment_speaker` answer.
#[derive(Debug, Clone, Copy)]
struct PendingSpeaker {
    segment: SegmentId,
    seq_index: u32,
    revision_id: TranscriptRevisionId,
}

/// Segments asked for per page, matching the React pane's cursor pages.
pub const PAGE_SIZE: u32 = 200;

/// A transcript being read in, page by page.
#[derive(Debug)]
struct Loading {
    revision_id: TranscriptRevisionId,
    reference: TranscriptRevisionRef,
    segments: Vec<TranscriptSegmentDto>,
}

/// What the transcript panel holds between frames.
#[derive(Debug)]
pub struct TranscriptBinding {
    recording_id: Option<RecordingId>,
    /// The revision the user asked to see; `None` means "the head".
    chosen: Option<TranscriptRevisionId>,
    loading: Option<Loading>,
    loaded: Option<TranscriptRevisionId>,
    session: Option<EditSession>,
    state: TranscriptEditorState,
    speakers: Vec<SpeakerChoice>,
    speaker_ids: Vec<(String, SpeakerId)>,
    saving: bool,
    error: Option<ErrorEnvelope>,
    /// A position the user asked for by activating a timestamp.
    seek_to_ms: Option<u64>,
    /// Reassignments dispatched and not yet answered.
    pending_speakers: Vec<PendingSpeaker>,
    /// The last refusal of a reassignment, shown above the transcript until
    /// the next attempt.
    speaker_error: Option<ErrorEnvelope>,
}

impl Default for TranscriptBinding {
    fn default() -> Self {
        Self::new()
    }
}

impl TranscriptBinding {
    /// A panel with nothing open.
    #[must_use]
    pub fn new() -> Self {
        Self {
            recording_id: None,
            chosen: None,
            loading: None,
            loaded: None,
            session: None,
            state: TranscriptEditorState::default(),
            speakers: Vec::new(),
            speaker_ids: Vec::new(),
            saving: false,
            error: None,
            seek_to_ms: None,
            pending_speakers: Vec::new(),
            speaker_error: None,
        }
    }

    /// The session, for a host that drives the editor directly and for
    /// tests.
    #[must_use]
    pub const fn session(&self) -> Option<&EditSession> {
        self.session.as_ref()
    }

    /// Whether a save is waiting for an answer.
    #[must_use]
    pub const fn is_saving(&self) -> bool {
        self.saving
    }

    /// Point the panel at a recording.
    pub fn open(&mut self, recording_id: RecordingId, out: &mut AiOutput) {
        if self.recording_id == Some(recording_id) {
            return;
        }
        *self = Self::new();
        self.recording_id = Some(recording_id);
        out.command(AppCommand::ListSpeakers(ListSpeakersRequest {
            recording_id: recording_id.to_string(),
        }));
    }

    /// Save anything still pending and forget the transcript.
    ///
    /// This is the shutdown path as well as the navigation path: an edit the
    /// debounce had not yet reached is dispatched here, which is the only
    /// reason it survives the window closing.
    pub fn close(&mut self, out: &mut AiOutput) {
        self.flush(out);
        *self = Self::new();
    }

    /// Dispatch a save now, if the session has unsaved text.
    pub fn flush(&mut self, out: &mut AiOutput) {
        let Some(session) = self.session.as_mut() else {
            return;
        };
        if let Some(request) = session.save_now() {
            self.saving = true;
            out.command(save_command(&request));
        }
    }

    /// The seek a timestamp chip asked for this frame, consumed once.
    pub fn take_seek(&mut self) -> Option<u64> {
        self.seek_to_ms.take()
    }

    /// Route a dispatcher answer. Returns whether it was this panel's.
    ///
    /// A speaker answer is this panel's *and* the outputs column's — the
    /// chips and the speaker list render the same rows — so the host hands
    /// the three labelling answers to both rather than to the first taker.
    pub fn on_response(
        &mut self,
        command: &'static str,
        result: &Result<AppResponse, ErrorEnvelope>,
        out: &mut AiOutput,
    ) -> bool {
        match command {
            "get_transcript" => self.on_page(result, out),
            "save_transcript_revision" => self.on_saved(result),
            "list_speakers" => self.on_speakers(result),
            "rename_speaker" | "confirm_speaker" | "reset_speaker" => {
                self.on_speaker_operation(result)
            }
            "set_segment_speaker" => self.on_segment_speaker(result),
            _ => false,
        }
    }

    fn on_page(&mut self, result: &Result<AppResponse, ErrorEnvelope>, out: &mut AiOutput) -> bool {
        let Some(loading) = self.loading.as_ref() else {
            return false;
        };
        match result {
            Ok(AppResponse::GetTranscript(page)) => {
                if page.revision.revision_id != loading.revision_id {
                    return false;
                }
                let expected = u32::try_from(loading.segments.len()).unwrap_or(u32::MAX);
                if page
                    .segments
                    .first()
                    .is_some_and(|first| first.index != expected)
                {
                    // A page out of order, or one that arrived twice.
                    return true;
                }
                self.absorb(page.clone(), out);
                true
            }
            Ok(_) => false,
            Err(envelope) => {
                self.error = Some(envelope.clone());
                self.loading = None;
                true
            }
        }
    }

    fn absorb(&mut self, page: TranscriptPage, out: &mut AiOutput) {
        let Some(loading) = self.loading.as_mut() else {
            return;
        };
        loading.segments.extend(page.segments);
        if let Some(cursor) = page.next_cursor {
            request_page(loading.revision_id, Some(cursor), out);
            return;
        }
        let loading = self.loading.take().expect("checked above");
        let Some(recording_id) = self.recording_id else {
            return;
        };
        let document = document_from(
            recording_id,
            &loading.reference,
            &loading.segments,
            &self.speaker_ids,
        );
        self.loaded = Some(loading.revision_id);
        self.session = Some(EditSession::new(document));
        self.state = TranscriptEditorState::default();
    }

    fn on_saved(&mut self, result: &Result<AppResponse, ErrorEnvelope>) -> bool {
        if !self.saving {
            return false;
        }
        self.saving = false;
        let Some(session) = self.session.as_mut() else {
            return true;
        };
        match result {
            Ok(AppResponse::SaveTranscriptRevision(response)) => {
                session.note_saved(response.revision_id);
                self.loaded = Some(response.revision_id);
            }
            Ok(_) => return false,
            Err(envelope) if envelope.code == AppErrorCode::Conflict => {
                session.note_save_conflict();
            }
            Err(envelope) => {
                self.error = Some(envelope.clone());
                session.note_save_failed();
            }
        }
        true
    }

    fn on_speakers(&mut self, result: &Result<AppResponse, ErrorEnvelope>) -> bool {
        let Ok(AppResponse::ListSpeakers(response)) = result else {
            return false;
        };
        let belongs = response
            .speakers
            .first()
            .is_none_or(|speaker| Some(speaker.recording_id) == self.recording_id);
        if !belongs {
            return false;
        }
        self.apply_speakers(response);
        true
    }

    /// One speaker row, patched in place.
    ///
    /// The answer to a rename, a confirmation or a reset is the stored row,
    /// so the chips are correct in the same frame without a refetch — and
    /// without a second round trip that could answer out of order.
    fn on_speaker_operation(&mut self, result: &Result<AppResponse, ErrorEnvelope>) -> bool {
        let speaker = match result {
            Ok(
                AppResponse::RenameSpeaker(answer)
                | AppResponse::ConfirmSpeaker(answer)
                | AppResponse::ResetSpeaker(answer),
            ) => &answer.speaker,
            _ => return false,
        };
        if Some(speaker.recording_id) != self.recording_id {
            return false;
        }
        let Some(slot) = self
            .speakers
            .iter()
            .position(|choice| choice.id == speaker.speaker_id)
        else {
            return false;
        };
        self.speakers[slot] = choice(speaker);
        self.speaker_ids = speaker_ids(&self.speakers);
        true
    }

    /// The answer to one reassignment.
    ///
    /// A refusal is not swallowed: the segment goes back to the attribution
    /// the database holds and the message is shown, because a chip that kept
    /// showing an assignment no store accepted would be a lie.
    fn on_segment_speaker(&mut self, result: &Result<AppResponse, ErrorEnvelope>) -> bool {
        match result {
            Ok(AppResponse::SetSegmentSpeaker(answer)) => {
                if Some(answer.recording_id) != self.recording_id {
                    return false;
                }
                let Some(slot) = self.pending_speakers.iter().position(|pending| {
                    pending.revision_id == answer.transcript_revision_id
                        && pending.seq_index == answer.seq_index
                }) else {
                    return false;
                };
                let pending = self.pending_speakers.remove(slot);
                if let Some(session) = self.session.as_mut() {
                    // Ignored deliberately: the only failure is a segment the
                    // document no longer holds, which is the case where there
                    // is nothing left to rebase.
                    let _ = session.note_speaker_persisted(pending.segment);
                }
                true
            }
            Err(envelope) => {
                // A refusal envelope carries no segment, so the oldest
                // unanswered reassignment is the one it belongs to: the panel
                // dispatches them in order and the dispatcher answers in
                // order.
                if self.pending_speakers.is_empty() {
                    return false;
                }
                let pending = self.pending_speakers.remove(0);
                if let Some(session) = self.session.as_mut() {
                    let _ = session.restore_persisted_speaker(pending.segment);
                }
                self.speaker_error = Some(envelope.clone());
                true
            }
            Ok(_) => false,
        }
    }

    fn apply_speakers(&mut self, response: &ListSpeakersResponse) {
        self.speakers = response.speakers.iter().map(choice).collect();
        self.speaker_ids = speaker_ids(&self.speakers);
    }

    /// Draw the panel.
    pub fn ui(
        &mut self,
        ui: &mut Ui,
        detail: &RecordingDetail,
        playback: &dyn PlaybackPosition,
        out: &mut AiOutput,
    ) {
        self.revision_bar(ui, detail);

        let Some(revision_id) = self.chosen.or(detail.current_revision_id) else {
            ui.label(
                "This recording has no transcript yet. Start one from the processing controls.",
            );
            return;
        };
        if self.loaded != Some(revision_id) && self.loading.is_none() && self.error.is_none() {
            if let Some(reference) = detail
                .revisions
                .iter()
                .find(|candidate| candidate.revision_id == revision_id)
            {
                self.loading = Some(Loading {
                    revision_id,
                    reference: reference.clone(),
                    segments: Vec::new(),
                });
                request_page(revision_id, None, out);
            }
        }

        if let Some(envelope) = self.error.clone() {
            ui.label(&envelope.message);
            ui.label(format!("Diagnostic id {}", envelope.diagnostic_id));
            if ui.button("Try again").clicked() {
                self.error = None;
                self.loaded = None;
            }
            return;
        }

        if let Some(envelope) = self.speaker_error.clone() {
            ui.label(format!("Speaker not changed: {}", envelope.message));
            ui.label(format!("Diagnostic id {}", envelope.diagnostic_id));
            if ui.button("Dismiss").clicked() {
                self.speaker_error = None;
            }
        }

        let Some(session) = self.session.as_mut() else {
            ui.label("Reading the transcript…");
            return;
        };

        let output = TranscriptEditor::new("recording-transcript", session, &mut self.state)
            .speakers(&self.speakers)
            .playback(playback)
            .show(ui);

        if let Some(position_ms) = output.seek_to_ms {
            self.seek_to_ms = Some(position_ms);
        }
        if let Some(text) = output.copied {
            ui.ctx().copy_text(text);
        }
        if let Some(request) = output.save_request {
            if !self.saving {
                self.saving = true;
                out.command(save_command(&request));
            }
        }
        if let Some(change) = output.speaker_change {
            if let (Some(recording_id), Some(revision_id)) = (self.recording_id, self.loaded) {
                self.speaker_error = None;
                self.pending_speakers.push(PendingSpeaker {
                    segment: change.segment,
                    seq_index: change.seq_index,
                    revision_id,
                });
                out.command(AppCommand::SetSegmentSpeaker(SetSegmentSpeakerRequest {
                    recording_id: recording_id.to_string(),
                    transcript_revision_id: revision_id.to_string(),
                    seq_index: change.seq_index,
                    speaker_id: change.speaker_id.map(|id| id.to_string()),
                }));
            }
        }
        match output.conflict_choice {
            Some(ConflictChoice::ReloadNewest) => {
                // Drop the session and read the head again; the user's text
                // is theirs to copy first, which the other choice does.
                self.chosen = None;
                self.loaded = None;
                self.session = None;
            }
            Some(ConflictChoice::CopyMyChanges) => {
                let text = session.document().segments().iter().fold(
                    String::new(),
                    |mut text, segment| {
                        text.push_str(&segment.text());
                        text.push('\n');
                        text
                    },
                );
                ui.ctx().copy_text(text);
            }
            None => {}
        }
    }

    /// The revision switcher: every revision, the head marked as such.
    fn revision_bar(&mut self, ui: &mut Ui, detail: &RecordingDetail) {
        if detail.revisions.len() < 2 {
            return;
        }
        let head = detail.current_revision_id;
        let selected = self.chosen.or(head);
        ui.horizontal_wrapped(|ui| {
            for revision in &detail.revisions {
                let is_head = Some(revision.revision_id) == head;
                let label = match (revision.kind, is_head) {
                    (RevisionKind::Machine, true) => "Original (machine), current".to_owned(),
                    (RevisionKind::Machine, false) => "Original (machine)".to_owned(),
                    (RevisionKind::Human, true) => "Edited, current".to_owned(),
                    (RevisionKind::Human, false) => format!("Edited {}", revision.created_at),
                };
                if ui
                    .selectable_label(selected == Some(revision.revision_id), label)
                    .clicked()
                    && selected != Some(revision.revision_id)
                {
                    self.chosen = Some(revision.revision_id);
                    self.loaded = None;
                    self.session = None;
                    self.loading = None;
                }
            }
        });
        if selected != head {
            ui.label("This revision is not the current one, so it is read-only.");
        }
    }
}

/// One speaker row as the chips present it.
///
/// The arguments handed to the honesty rule are the same ones
/// `plaudon-export` hands it for the same row — current label, typed name,
/// confirmation — so a chip and a bundle say the same thing. The machine
/// label rides along so a renamed-but-unconfirmed row can still show which
/// cluster the name is attached to.
fn choice(speaker: &SpeakerDto) -> SpeakerChoice {
    SpeakerChoice {
        id: speaker.speaker_id,
        label: speaker.label.clone(),
        machine_label: speaker.original_label.clone(),
        name: speaker.name.clone(),
        confirmed: speaker.confirmed,
    }
}

/// The label-to-row index the segments are matched through.
///
/// A segment carries whatever `speakers.label` held when its page was read,
/// which a rename moves, so both the current and the machine label resolve
/// to the row. Without that, a page that arrives after a rename loses its
/// attribution and the chip reads "No speaker" for a segment that has one.
fn speaker_ids(speakers: &[SpeakerChoice]) -> Vec<(String, SpeakerId)> {
    let mut pairs = Vec::with_capacity(speakers.len());
    for speaker in speakers {
        pairs.push((speaker.label.clone(), speaker.id));
        if speaker.machine_label != speaker.label {
            pairs.push((speaker.machine_label.clone(), speaker.id));
        }
    }
    pairs
}

/// Ask for one page of a transcript.
fn request_page(revision_id: TranscriptRevisionId, cursor: Option<u32>, out: &mut AiOutput) {
    out.command(AppCommand::GetTranscript(GetTranscriptRequest {
        revision_id: revision_id.to_string(),
        cursor,
        limit: PAGE_SIZE,
    }));
}

/// The command one autosave is carried out with.
fn save_command(request: &SaveRequest) -> AppCommand {
    AppCommand::SaveTranscriptRevision(SaveTranscriptRevisionRequest {
        recording_id: request.recording_id.to_string(),
        base_revision_id: request.base_revision_id.to_string(),
        edits: request
            .edits
            .iter()
            .map(|(index, text)| SegmentEdit {
                index: *index,
                text: text.clone(),
            })
            .collect(),
    })
}

/// Turn the paged DTOs into a document the editor can open.
fn document_from(
    recording_id: RecordingId,
    revision: &TranscriptRevisionRef,
    segments: &[TranscriptSegmentDto],
    speakers: &[(String, SpeakerId)],
) -> TranscriptDocument {
    let seeds = segments
        .iter()
        .map(|segment| SegmentSeed {
            seq_index: segment.index,
            start_ms: segment.start_ms,
            end_ms: segment.end_ms,
            text: segment.text.clone(),
            // The row identity is on the wire since VP-030; the label match
            // stays as the fallback for a page read before the column was
            // published, where the label is all there is.
            speaker_id: segment
                .speaker_id
                .as_ref()
                .and_then(|id| id.parse::<SpeakerId>().ok())
                .or_else(|| {
                    segment.speaker_label.as_ref().and_then(|label| {
                        speakers
                            .iter()
                            .find(|(known, _)| known == label)
                            .map(|(_, id)| *id)
                    })
                }),
            // `get_transcript` sends a confidence value and the engine that
            // produced it, but not the scale it is on. §8.5 forbids reading
            // a value without its scale and a scale cannot be invented, so
            // the document carries none; the annotations the backend did
            // send carry the meaning instead.
            confidence: None,
            // Word alignments are not on the wire for this command.
            words: Vec::new(),
        })
        .collect();
    TranscriptDocument::new(DocumentSource {
        recording_id,
        revision_id: revision.revision_id,
        alignment_origin: revision.alignment_origin,
        duration_ms: None,
        editable: !revision.superseded,
        segments: seeds,
    })
}
