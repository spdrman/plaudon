//! The outputs column of the recording detail: every AI surface this build
//! has, bound to the commands that feed them.
//!
//! The panels themselves ([`crate::ai`]) are pure: they render borrowed DTOs
//! and return [`AiIntent`](crate::ai::AiIntent)s. This is the state that has
//! to exist once they face a real engine — which responses they need, when
//! to ask again, and which job is "the transcription job for this
//! recording".
//!
//! Two rules are worth naming because they are easy to lose in wiring:
//!
//! * **nothing polls.** Every refetch here is caused by the user, by a §15.3
//!   event or by a resynchronisation. There is no timer.
//! * **nothing is filtered into existence.** `list_jobs` has no
//!   per-recording filter, so the page is filtered here by the recording id
//!   the rows carry; a row without one is not this recording's and is not
//!   shown as if it were.

use egui::Ui;
use plaudon_app::cloud_presentation::ProcessingStage;
use plaudon_app::command::{AppCommand, AppResponse};
use plaudon_app::dto::{
    Capabilities, EmptyRequest, EventKind, ExportMediaResponse, JobKind, JobPage, JobSummary,
    ListJobsRequest, ListModelsRequest, ListModelsResponse, ListOutputsRequest,
    ListOutputsResponse, ListPeopleRequest, ListSpeakerSuggestionsRequest, ListSpeakersResponse,
    RecordingDetail, SpeakerDto, SpeakerSuggestionDto, UiEvent, VoiceProfileSettingsDto,
};
use plaudon_domain::error::ErrorEnvelope;
use plaudon_domain::ids::RecordingId;
use plaudon_domain::transcript::AlignmentOrigin;

use crate::ai::{
    AiIntent, AiOutput, ExportMenu, OutputsViewer, ProcessingHistory, RecordingContext,
    ReviewPanel, SummarizationPanel, TranscriptionPanel, VoiceContext,
};
use crate::screens::people::{picker_person, PickerPerson};
use crate::widgets::error_notice::{error_notice, ErrorNoticeAction};
use plaudon_app::job_presentation::ControlIntent;

/// Jobs asked for per page of the processing history.
pub const JOB_PAGE_SIZE: u32 = 50;

/// Suggestions asked for per recording. One cluster has at most one pending
/// suggestion per person, so a recording's panel never needs a page.
pub const SUGGESTION_PAGE_SIZE: u32 = 100;

/// Which tab of the outputs column is on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputsTab {
    /// Choosing an engine and running transcription.
    Transcription,
    /// Choosing a summary stage and running it.
    Summarize,
    /// Reading the produced summary, actions and decisions.
    Outputs,
    /// Reviewing claims against their anchors.
    Review,
    /// Every job that ran for this recording.
    History,
    /// Transcript and media export.
    Export,
}

impl OutputsTab {
    /// Every tab, in the order they are offered.
    pub const ALL: [Self; 6] = [
        Self::Transcription,
        Self::Summarize,
        Self::Outputs,
        Self::Review,
        Self::History,
        Self::Export,
    ];

    /// The tab's label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Transcription => "Transcription",
            Self::Summarize => "Summarize",
            Self::Outputs => "Outputs",
            Self::Review => "Review",
            Self::History => "History",
            Self::Export => "Export",
        }
    }
}

/// The AI panels of one recording, and the responses that feed them.
#[derive(Debug)]
pub struct AiBinding {
    recording_id: Option<RecordingId>,
    tab: OutputsTab,
    transcription: TranscriptionPanel,
    summarize: SummarizationPanel,
    outputs_viewer: OutputsViewer,
    review: ReviewPanel,
    history: ProcessingHistory,
    export: ExportMenu,
    capabilities: Option<Capabilities>,
    models: Option<ListModelsResponse>,
    /// The `(stage, engine)` the held catalogue answers for.
    models_for: Option<(String, Option<String>)>,
    outputs: Option<ListOutputsResponse>,
    speakers: Option<ListSpeakersResponse>,
    jobs: Option<JobPage>,
    media_export: Option<ExportMediaResponse>,
    /// The roster, as the person picker offers it (VP-210).
    people: Vec<PickerPerson>,
    /// The voice-profile settings. `None` until they have been read, which
    /// reads as matching off.
    voice_settings: Option<VoiceProfileSettingsDto>,
    /// Pending suggestions for this recording.
    suggestions: Vec<SpeakerSuggestionDto>,
    error: Option<ErrorEnvelope>,
}

impl Default for AiBinding {
    fn default() -> Self {
        Self::new()
    }
}

impl AiBinding {
    /// A binding with nothing fetched.
    #[must_use]
    pub fn new() -> Self {
        Self {
            recording_id: None,
            tab: OutputsTab::Transcription,
            transcription: TranscriptionPanel::default(),
            summarize: SummarizationPanel::default(),
            outputs_viewer: OutputsViewer::default(),
            review: ReviewPanel::default(),
            history: ProcessingHistory::default(),
            export: ExportMenu::default(),
            capabilities: None,
            models: None,
            models_for: None,
            outputs: None,
            speakers: None,
            jobs: None,
            media_export: None,
            people: Vec::new(),
            voice_settings: None,
            suggestions: Vec::new(),
            error: None,
        }
    }

    /// The roster this recording's pickers offer, for a test.
    #[must_use]
    pub fn people(&self) -> &[PickerPerson] {
        &self.people
    }

    /// Pending suggestions for this recording, for a test.
    #[must_use]
    pub fn suggestions(&self) -> &[SpeakerSuggestionDto] {
        &self.suggestions
    }

    /// The capability report, once it has arrived.
    #[must_use]
    pub const fn capabilities(&self) -> Option<&Capabilities> {
        self.capabilities.as_ref()
    }

    /// Whether Local-only Mode is enforced.
    ///
    /// Unknown capabilities read as enforced: a claim that cloud work is
    /// allowed has to come from the report, never from its absence (§4.3).
    #[must_use]
    pub fn local_only(&self) -> bool {
        self.capabilities
            .as_ref()
            .is_none_or(|capabilities| capabilities.preferences.local_only_mode)
    }

    /// Point the panels at a recording and ask for what they need.
    pub fn open(&mut self, recording_id: RecordingId, out: &mut AiOutput) {
        if self.recording_id == Some(recording_id) {
            return;
        }
        let capabilities = self.capabilities.take();
        let people = std::mem::take(&mut self.people);
        let voice_settings = self.voice_settings.take();
        *self = Self::new();
        // The roster and the feature's state are the library's, not this
        // recording's, so they are kept; everything else is read again.
        self.people = people;
        self.voice_settings = voice_settings;
        // The machine inventory does not change with the recording, so it is
        // kept; everything else is per recording and is read again.
        self.capabilities = capabilities;
        self.recording_id = Some(recording_id);
        self.refresh(out);
    }

    /// Forget the recording.
    pub fn close(&mut self) {
        let capabilities = self.capabilities.take();
        *self = Self::new();
        self.capabilities = capabilities;
    }

    /// Ask for everything this recording's panels render.
    pub fn refresh(&mut self, out: &mut AiOutput) {
        let Some(recording_id) = self.recording_id else {
            return;
        };
        if self.capabilities.is_none() {
            out.command(AppCommand::GetCapabilities);
        }
        out.command(AppCommand::ListOutputs(ListOutputsRequest {
            recording_id: recording_id.to_string(),
        }));
        out.command(AppCommand::ListJobs(ListJobsRequest {
            cursor: None,
            limit: JOB_PAGE_SIZE,
            states: Vec::new(),
            kinds: Vec::new(),
        }));
        // The roster is the assignment vocabulary of every picker in this
        // recording, and it works with biometric matching off (VP-011).
        out.command(AppCommand::ListPeople(ListPeopleRequest { query: None }));
        out.command(AppCommand::GetVoiceProfileSettings(EmptyRequest {}));
        if self.matching_on() {
            out.command(self.suggestions_command());
        }
    }

    /// Whether biometric matching is on. Unknown settings read as off.
    #[must_use]
    pub fn matching_on(&self) -> bool {
        self.voice_settings
            .as_ref()
            .is_some_and(|settings| settings.enabled)
    }

    /// `list_speaker_suggestions` for the open recording.
    fn suggestions_command(&self) -> AppCommand {
        AppCommand::ListSpeakerSuggestions(ListSpeakerSuggestionsRequest {
            recording_id: self.recording_id.map(|id| id.to_string()),
            limit: Some(SUGGESTION_PAGE_SIZE),
        })
    }

    /// Route a dispatcher answer. Returns whether it was this binding's.
    pub fn on_response(
        &mut self,
        command: &'static str,
        result: &Result<AppResponse, ErrorEnvelope>,
        out: &mut AiOutput,
    ) -> bool {
        for follow_up in self
            .outputs_viewer
            .speaker_panel_mut()
            .on_response(command, result)
        {
            out.command(follow_up);
        }
        match result {
            Ok(AppResponse::ListPeople(people)) => {
                self.people = people.people.iter().map(picker_person).collect();
                return true;
            }
            Ok(
                AppResponse::GetVoiceProfileSettings(settings)
                | AppResponse::SetVoiceProfilesEnabled(settings),
            ) => {
                let was_on = self.matching_on();
                self.voice_settings = Some(settings.clone());
                if settings.enabled {
                    if !was_on {
                        out.command(self.suggestions_command());
                    }
                } else {
                    // Matching is off: there is no suggestion to have an
                    // opinion about, and the panel must not show one (VP-215).
                    self.suggestions.clear();
                }
                return true;
            }
            Ok(AppResponse::ListSpeakerSuggestions(suggestions)) => {
                self.suggestions = suggestions
                    .suggestions
                    .iter()
                    .filter(|row| Some(row.recording_id) == self.recording_id)
                    .cloned()
                    .collect();
                return true;
            }
            Ok(AppResponse::AssignClusterPerson(answer)) => {
                // The answer is the stored row, so the panel and the chips
                // are right in this frame; only the suggestions that stood
                // against this cluster are now stale.
                self.patch_speaker(&answer.speaker);
                if self.matching_on() {
                    out.command(self.suggestions_command());
                }
                return true;
            }
            Ok(
                AppResponse::ConfirmSpeakerSuggestion(decision)
                | AppResponse::RejectSpeakerSuggestion(decision),
            ) => {
                if let Some(speaker) = decision.speaker.as_ref() {
                    self.patch_speaker(speaker);
                }
                if self.matching_on() {
                    out.command(self.suggestions_command());
                }
                return true;
            }
            Ok(AppResponse::EnrollPersonVoice(_)) => {
                // An enrolment changes future matching only; nothing on
                // screen is stale (VP-213e).
                return true;
            }
            Ok(AppResponse::GetClusterSpeech(_)) => {
                // The clean-speech measurement of the enrolment meter; the
                // panel has already taken it (VP-094).
                return true;
            }
            _ => {}
        }
        match result {
            Ok(AppResponse::GetCapabilities(capabilities)) => {
                self.capabilities = Some(capabilities.clone());
                true
            }
            Ok(AppResponse::ListModels(models)) => {
                self.models = Some(models.clone());
                true
            }
            Ok(AppResponse::ListOutputs(outputs)) => {
                self.outputs = Some(outputs.clone());
                true
            }
            Ok(AppResponse::ListSpeakers(speakers)) => {
                self.speakers = Some(speakers.clone());
                true
            }
            Ok(
                AppResponse::RenameSpeaker(answer)
                | AppResponse::ConfirmSpeaker(answer)
                | AppResponse::ResetSpeaker(answer),
            ) => {
                self.patch_speaker(&answer.speaker);
                // The answer is the stored row, so the refusal that was on
                // screen no longer describes anything.
                self.error = None;
                true
            }
            Ok(AppResponse::ListJobs(page)) => {
                self.jobs = Some(page.clone());
                true
            }
            Ok(AppResponse::ExportMedia(response)) => {
                self.media_export = Some(response.clone());
                true
            }
            Err(envelope) if is_ours(command) => {
                self.error = Some(envelope.clone());
                true
            }
            _ => false,
        }
    }

    /// Replace one row of the held speaker list with the stored row a
    /// labelling command answered with.
    ///
    /// Patching rather than refetching: the answer *is* what `list_speakers`
    /// would say, so a second round trip would only add a window in which
    /// the screen shows the old name.
    fn patch_speaker(&mut self, speaker: &SpeakerDto) {
        let Some(held) = self.speakers.as_mut() else {
            return;
        };
        if let Some(row) = held
            .speakers
            .iter_mut()
            .find(|row| row.speaker_id == speaker.speaker_id)
        {
            *row = speaker.clone();
        }
    }

    /// Route a §15.3 event: refetch what it invalidates, nothing else.
    pub fn on_ui_event(&mut self, event: &UiEvent, out: &mut AiOutput) {
        let concerns_us = event
            .recording_id
            .is_none_or(|id| Some(id) == self.recording_id);
        if !concerns_us {
            return;
        }
        match event.kind {
            EventKind::JobUpdated | EventKind::JobFailed => {
                out.command(AppCommand::ListJobs(ListJobsRequest {
                    cursor: None,
                    limit: JOB_PAGE_SIZE,
                    states: Vec::new(),
                    kinds: Vec::new(),
                }));
            }
            EventKind::TranscriptCompleted | EventKind::ToolCompleted => {
                if let Some(recording_id) = self.recording_id {
                    out.command(AppCommand::ListOutputs(ListOutputsRequest {
                        recording_id: recording_id.to_string(),
                    }));
                }
            }
            EventKind::ModelAvailabilityChanged | EventKind::ConsentRequired => {
                out.command(AppCommand::GetCapabilities);
                self.models_for = None;
            }
            EventKind::PersonChanged => {
                out.command(AppCommand::ListPeople(ListPeopleRequest { query: None }));
            }
            EventKind::VoiceProfileSuggestionsReady if self.matching_on() => {
                out.command(self.suggestions_command());
            }
            _ => {}
        }
    }

    /// Draw the outputs column.
    pub fn ui(&mut self, ui: &mut Ui, detail: &RecordingDetail, now_ms: i128, out: &mut AiOutput) {
        ui.horizontal_wrapped(|ui| {
            for tab in OutputsTab::ALL {
                if ui.selectable_label(self.tab == tab, tab.label()).clicked() {
                    self.tab = tab;
                }
            }
        });

        if let Some(envelope) = self.error.clone() {
            // The shared notice, so an envelope's own corrective controls are
            // offered here too: for "no speech model is installed", install
            // and import straight from the error (issue #305).
            match error_notice(ui, &envelope, "") {
                Some(ErrorNoticeAction::InstallSpeechModel) => {
                    out.push(AiIntent::Navigate(ControlIntent::InstallSpeechModel));
                }
                Some(ErrorNoticeAction::ImportModelFile) => out.push(AiIntent::PickModelFile),
                Some(ErrorNoticeAction::OpenSettings) => {
                    out.push(AiIntent::Navigate(ControlIntent::OpenSettings));
                }
                Some(ErrorNoticeAction::Retry) => self.error = None,
                None => {}
            }
        }

        // The catalogue is asked for before the borrowed context exists:
        // the panels read `capabilities` by reference for the rest of the
        // frame, and nothing may mutate this binding after that point.
        match self.tab {
            OutputsTab::Transcription => {
                self.ensure_models(ProcessingStage::Transcribe.as_str(), out);
            }
            OutputsTab::Summarize => {
                let stage = self.summarize.stage().as_str().to_owned();
                self.ensure_models(&stage, out);
            }
            OutputsTab::Outputs | OutputsTab::Review | OutputsTab::History | OutputsTab::Export => {
            }
        }
        let jobs = self.recording_jobs(detail.summary.recording_id);
        let history_page = self.history_page(detail.summary.recording_id);

        let title = detail.summary.title.clone();
        let recording_id = detail.summary.recording_id.to_string();
        let revision_id = detail.current_revision_id.map(|id| id.to_string());
        let alignment_origin = detail
            .current_revision_id
            .and_then(|head| {
                detail
                    .revisions
                    .iter()
                    .find(|revision| revision.revision_id == head)
            })
            .map_or(AlignmentOrigin::None, |revision| revision.alignment_origin);
        let context = RecordingContext {
            recording_id: &recording_id,
            duration_ms: detail.summary.duration_ms,
            revision_id: revision_id.as_deref(),
            alignment_origin,
            title: &title,
            capabilities: self.capabilities.as_ref(),
        };

        let panel_output = match self.tab {
            OutputsTab::Transcription => {
                let job = latest(&jobs, JobKind::Transcribe);
                self.transcription
                    .ui(ui, context, self.models.as_ref(), job, now_ms)
            }
            OutputsTab::Summarize => {
                let job = latest(&jobs, JobKind::Summarize);
                self.summarize.ui(
                    ui,
                    context,
                    self.models.as_ref(),
                    self.outputs.as_ref(),
                    job,
                    now_ms,
                )
            }
            OutputsTab::Outputs => {
                // Borrowed for the rest of the frame: nothing may mutate the
                // binding after this point, which is the same rule the model
                // catalogue above follows.
                let voice = VoiceContext {
                    people: &self.people,
                    settings: self.voice_settings.as_ref(),
                    suggestions: &self.suggestions,
                };
                self.outputs_viewer.ui(
                    ui,
                    context,
                    self.outputs.as_ref(),
                    self.speakers.as_ref(),
                    voice,
                    now_ms,
                )
            }
            OutputsTab::Review => self.review.ui(ui, context, self.outputs.as_ref()),
            OutputsTab::History => self.history.ui(ui, history_page.as_ref(), now_ms),
            OutputsTab::Export => self.export.ui(ui, context, self.media_export.as_ref()),
        };
        out.extend(panel_output);
    }

    /// Ask for the model catalogue of `stage` when it is not the one held.
    fn ensure_models(&mut self, stage: &str, out: &mut AiOutput) {
        let engine = self.transcription.engine_id().map(str::to_owned);
        let key = (stage.to_owned(), engine.clone());
        if self.models_for.as_ref() == Some(&key) {
            return;
        }
        self.models_for = Some(key);
        out.command(AppCommand::ListModels(ListModelsRequest {
            stage: stage.to_owned(),
            engine,
        }));
    }

    /// The jobs of this recording, newest first, out of the page held.
    fn recording_jobs(&self, recording_id: RecordingId) -> Vec<JobSummary> {
        let Some(page) = self.jobs.as_ref() else {
            return Vec::new();
        };
        let mut rows: Vec<JobSummary> = page
            .items
            .iter()
            .filter(|job| job.recording_id == Some(recording_id))
            .cloned()
            .collect();
        // `created_at` is the backend's own RFC 3339 instant; comparing the
        // strings orders them because the format is fixed-width UTC.
        rows.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        rows
    }

    /// The processing history page, filtered to this recording.
    ///
    /// The per-state counts are the backend's and are of the whole page, so
    /// they are dropped rather than recomputed: a count this process derived
    /// is not a count the backend reported.
    fn history_page(&self, recording_id: RecordingId) -> Option<JobPage> {
        let page = self.jobs.as_ref()?;
        Some(JobPage {
            items: self.recording_jobs(recording_id),
            next_cursor: page.next_cursor.clone(),
            state_counts: std::collections::BTreeMap::new(),
        })
    }
}

/// Whether a failed command was one of this binding's.
fn is_ours(command: &'static str) -> bool {
    matches!(
        command,
        "get_capabilities"
            | "list_models"
            | "list_outputs"
            | "list_jobs"
            | "list_speakers"
            | "rename_speaker"
            | "confirm_speaker"
            | "reset_speaker"
            | "export_media"
            | "list_people"
            | "get_voice_profile_settings"
            | "list_speaker_suggestions"
            | "assign_cluster_person"
            | "confirm_speaker_suggestion"
            | "reject_speaker_suggestion"
            | "enroll_person_voice"
            | "get_cluster_speech"
            | "export_recording"
            | "start_processing"
            | "review_claim"
            | "regenerate_output"
    )
}

/// The newest job of `kind`, if the recording has one.
fn latest(jobs: &[JobSummary], kind: JobKind) -> Option<&JobSummary> {
    jobs.iter().find(|job| job.kind == kind)
}
