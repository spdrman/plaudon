//! The traits the command layer talks to.
//!
//! Commands never see SQL, the filesystem or a vendor SDK: they validate their
//! arguments and call a port. EPIC B's `crates/persistence` and EPIC C's
//! `crates/jobs` replace the implementations in [`crate::backend`] by swapping
//! what [`crate::build_ports`] constructs — no command and no UI change.

use std::path::PathBuf;

use async_trait::async_trait;
use plaudon_domain::claim::{Claim, ClaimState};
use plaudon_domain::error::AppError;
use plaudon_domain::ids::{ClaimId, JobId, OutputId, RecordingId};
use plaudon_domain::transcript::AlignmentOrigin;
use plaudon_persistence::intelligence::OutputHeader;
use plaudon_persistence::people::PersonDraft;

use crate::dto::SourceAddFolderRequest;
use crate::dto::{
    ActivityPage, Appearance, Capabilities, CloudBudget, CompleteOnboardingRequest, ConsentSummary,
    Contrast, DeviceAuthStatusDto, DeviceListingDto, DeviceProbeDto, DeviceScanDto,
    DeviceSyncPolicyDto, DuplicatePolicy, ExportDefaults, GetUsageResponse,
    GrantCloudConsentResponse, ImportFilesResponse, InspectModelPackResponse, InstallModelResponse,
    JobPage, JobSummary, ListLocalModelsResponse, ListModelsResponse, ListSourceArtifactsResponse,
    LocalOnlyModeResponse, NotificationSettings, NotificationSettingsResponse,
    PlaudConnectorSnapshot, PlaudDisconnectResponse, PlaudImportResponse, PlaudListResponse,
    Preferences, RecordingDetail, RecordingPage, RemoveLocalModelResponse,
    SaveTranscriptRevisionResponse, SetModelPreferenceResponse, SourceAddFolderResponse,
    SourceReconcileResponse, SourceSyncResponse, StartProcessingResponse, SyncTrigger,
    TranscriptPage, TranscriptRevisionRef, VerifyLocalModelResponse, VerifyModelResponse,
};
use crate::dto::{
    AssignClusterPersonResponse, ClusterSpeechDto, DeletePersonResponse,
    DeleteVoiceEnrollmentResponse, ForgetAllVoiceProfilesResponse, ListPeopleResponse,
    ListSpeakerSuggestionsResponse, ListVoiceExemplarsResponse, MergePeopleResponse, PersonDto,
    SpeakerSuggestionDecisionDto, SplitVoiceEnrollmentResponse, StartSpeakerMatchingResponse,
    VoiceEnrollmentDto, VoiceProfileSettingsDto,
};
use crate::secrets::SecretValue;
use crate::settings_presentation::UpdateStatus;
use crate::shortcuts::ShortcutBindings;
use crate::validate::{
    ValidatedBudget, ValidatedConsent, ValidatedExport, ValidatedGetTranscript,
    ValidatedListActivity, ValidatedListJobs, ValidatedListModels, ValidatedListRecordings,
    ValidatedPreference, ValidatedSaveRevision, ValidatedStartProcessing, ValidatedUsageRange,
    ValidatedVerifyModel,
};

/// Machine facts and stored preferences (`get_capabilities`).
#[async_trait]
pub trait CapabilitiesPort: Send + Sync {
    /// Observed capabilities of this machine.
    async fn capabilities(&self) -> Result<Capabilities, AppError>;
}

/// Media bytes resolved for the `plaudon-media` protocol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedMedia {
    /// Absolute path of the archived blob.
    pub path: PathBuf,
    /// MIME type derived from the stored extension.
    pub content_type: String,
    /// Length of the blob in bytes.
    pub byte_length: u64,
}

/// The recording library (`list_recordings`, `get_recording`).
#[async_trait]
pub trait LibraryPort: Send + Sync {
    /// One page of library rows.
    async fn list_recordings(
        &self,
        query: ValidatedListRecordings,
    ) -> Result<RecordingPage, AppError>;

    /// Full detail of one recording.
    async fn get_recording(&self, recording_id: RecordingId) -> Result<RecordingDetail, AppError>;

    /// Resolve the archived blob of a recording, for the media protocol.
    async fn resolve_media(&self, recording_id: RecordingId) -> Result<ResolvedMedia, AppError>;
}

/// Everything needed to record an engine's output as a new machine revision.
#[derive(Debug, Clone)]
pub struct MachineRevisionSpec {
    /// Recording the transcript belongs to.
    pub recording_id: RecordingId,
    /// Ordered segments, index 0..n.
    pub segments: Vec<MachineSegment>,
    /// Engine that produced the output.
    pub engine_id: String,
    /// Model that produced the output.
    pub model_id: String,
    /// Declared alignment origin for every segment.
    pub alignment_origin: AlignmentOrigin,
}

/// One segment of engine output.
#[derive(Debug, Clone, PartialEq)]
pub struct MachineSegment {
    /// Normalized text.
    pub text: String,
    /// Start offset; must be `None` when the alignment origin is `none`.
    pub start_ms: Option<u64>,
    /// End offset; must be `None` when the alignment origin is `none`.
    pub end_ms: Option<u64>,
    /// Engine-scoped confidence, when the engine reported one.
    pub confidence: Option<f64>,
    /// Speaker label, when diarization ran.
    pub speaker_label: Option<String>,
}

impl MachineSegment {
    /// A text-only segment.
    #[must_use]
    pub fn text_only(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            start_ms: None,
            end_ms: None,
            confidence: None,
            speaker_label: None,
        }
    }

    /// A timed segment.
    #[must_use]
    pub fn timed(text: impl Into<String>, start_ms: u64, end_ms: u64) -> Self {
        Self {
            text: text.into(),
            start_ms: Some(start_ms),
            end_ms: Some(end_ms),
            confidence: None,
            speaker_label: None,
        }
    }
}

/// Transcript revisions (`get_transcript`, `save_transcript_revision`).
#[async_trait]
pub trait TranscriptPort: Send + Sync {
    /// Append an immutable machine revision. This is the entry point the STT
    /// EPIC calls when an engine finishes.
    async fn create_machine_revision(
        &self,
        spec: MachineRevisionSpec,
    ) -> Result<TranscriptRevisionRef, AppError>;

    /// One page of segments of a revision.
    async fn get_transcript(
        &self,
        query: ValidatedGetTranscript,
    ) -> Result<TranscriptPage, AppError>;

    /// Create a child revision carrying the edits. Returns
    /// [`plaudon_domain::error::AppErrorCode::Conflict`] when the base is no
    /// longer the head.
    async fn save_transcript_revision(
        &self,
        request: ValidatedSaveRevision,
    ) -> Result<SaveTranscriptRevisionResponse, AppError>;
}

/// The job table (`list_jobs`, `cancel_job`, `retry_job`).
#[async_trait]
pub trait JobsPort: Send + Sync {
    /// One page of jobs plus the per-state counts for the filter chips.
    async fn list_jobs(&self, query: ValidatedListJobs) -> Result<JobPage, AppError>;

    /// Request cancellation of a non-terminal job.
    async fn cancel_job(&self, job_id: JobId) -> Result<JobSummary, AppError>;

    /// Retry a job. A job whose retry may be billed again requires
    /// `cost_acknowledged`.
    async fn retry_job(
        &self,
        job_id: JobId,
        cost_acknowledged: bool,
    ) -> Result<JobSummary, AppError>;
}

/// The in-app activity history (`list_activity`).
#[async_trait]
pub trait ActivityPort: Send + Sync {
    /// One page of activity entries.
    async fn list_activity(&self, query: ValidatedListActivity) -> Result<ActivityPage, AppError>;
}

/// Manual import (`import_files`).
#[async_trait]
pub trait ImportPort: Send + Sync {
    /// Import every path, returning one outcome per path in request order.
    async fn import_files(
        &self,
        paths: Vec<String>,
        duplicate_policy: DuplicatePolicy,
    ) -> Result<ImportFilesResponse, AppError>;
}

/// Preferences and policy switches.
#[async_trait]
pub trait SettingsPort: Send + Sync {
    /// Stored preferences.
    async fn preferences(&self) -> Result<Preferences, AppError>;

    /// Replace the notification preferences.
    async fn set_notification_settings(
        &self,
        settings: NotificationSettings,
    ) -> Result<NotificationSettingsResponse, AppError>;

    /// Set local-only mode. The caller has already checked the confirmation.
    async fn set_local_only_mode(&self, enabled: bool) -> Result<LocalOnlyModeResponse, AppError>;

    /// Persist the onboarding result.
    async fn complete_onboarding(
        &self,
        request: CompleteOnboardingRequest,
    ) -> Result<Preferences, AppError>;

    /// Store the appearance and contrast choice (matrix row C1).
    async fn set_appearance(
        &self,
        appearance: Appearance,
        contrast: Contrast,
    ) -> Result<Preferences, AppError>;

    /// Store the default engine for each stage. `None` clears a preference.
    async fn set_engine_preferences(
        &self,
        default_stt_engine_id: Option<String>,
        default_text_engine_id: Option<String>,
    ) -> Result<Preferences, AppError>;

    /// Store the defaults the export surfaces start from.
    async fn set_export_defaults(&self, defaults: ExportDefaults) -> Result<Preferences, AppError>;

    /// The stored keyboard overrides (matrix row C9).
    async fn shortcuts(&self) -> Result<ShortcutBindings, AppError>;

    /// Replace the keyboard overrides. The caller has already validated them
    /// against the command registry.
    async fn set_shortcuts(&self, bindings: ShortcutBindings)
        -> Result<ShortcutBindings, AppError>;
}

/// Locally installed speech and text models (matrix row A13, spec §12).
///
/// Every method is explicitly user-initiated: nothing here runs on a timer,
/// and the install path cannot reach a network transport at all, because it
/// installs from a redistributable offline pack.
#[async_trait]
pub trait ModelsPort: Send + Sync {
    /// Models present on this machine, with the disk they occupy.
    async fn list_local_models(&self) -> Result<ListLocalModelsResponse, AppError>;

    /// Read a pack without installing anything, and report the licence and
    /// footprint gate for every model it offers.
    async fn inspect_pack(&self, pack_path: PathBuf) -> Result<InspectModelPackResponse, AppError>;

    /// Install one model from a pack. The caller has already checked that the
    /// licence was accepted and the footprint acknowledged.
    async fn install_from_pack(
        &self,
        pack_path: PathBuf,
        model_id: String,
        keep_offline: bool,
    ) -> Result<InstallModelResponse, AppError>;

    /// Delete an installed model. The caller has already checked the
    /// confirmation.
    async fn remove_local_model(
        &self,
        model_id: String,
    ) -> Result<RemoveLocalModelResponse, AppError>;

    /// Re-hash an installed model against what the registry recorded.
    async fn verify_local_model(
        &self,
        model_id: String,
    ) -> Result<VerifyLocalModelResponse, AppError>;

    /// Download one catalogue model under a grant for exactly that host and
    /// file (issue #303). The caller has already checked the licence and the
    /// footprint; the port checks Local-only Mode and the egress approval.
    async fn install_download(
        &self,
        request: crate::dto::InstallModelDownloadRequest,
    ) -> Result<InstallModelResponse, AppError>;

    /// Ask a running download to stop.
    async fn cancel_install(
        &self,
        model_id: String,
    ) -> Result<crate::dto::CancelModelInstallResponse, AppError>;

    /// Install a weights file the user already has, after its checksum
    /// matched the catalogue. Works with Local-only Mode on.
    async fn import_file(
        &self,
        path: PathBuf,
        model_id: Option<String>,
    ) -> Result<InstallModelResponse, AppError>;

    /// What this machine has of the PLAUD Cloud CLI toolchain. Reads the
    /// filesystem only.
    async fn detect_plaud_toolchain(&self) -> Result<crate::dto::PlaudToolchainStatus, AppError>;

    /// Run the guided `npm install` of the exercised PLAUD CLI release under
    /// a grant for exactly that registry and package.
    async fn install_plaud_cli(
        &self,
        request: crate::dto::InstallPlaudCliRequest,
    ) -> Result<crate::dto::PlaudToolchainStatus, AppError>;
}

/// What the update subsystem can say without making a request (task N2.2).
#[async_trait]
pub trait UpdatePort: Send + Sync {
    /// The installed version, the pinned endpoint and whether Local-only Mode
    /// suppresses checks. Never performs a network operation.
    async fn update_status(&self) -> Result<UpdateStatus, AppError>;
}

/// Transcript export (`export_recording`).
#[async_trait]
pub trait ExportPort: Send + Sync {
    /// Run the export as a job and return it.
    async fn export_recording(&self, request: ValidatedExport) -> Result<JobId, AppError>;
}

/// Recording sources: the PLAUD cloud connector and watched folders
/// (`plaud_*`, `source_*`, `list_source_artifacts`).
///
/// Commands never see the connector, the process runner or the folder service:
/// they validate their arguments and call this port, so Local-only Mode,
/// consent and the user-intent rule (spec §7.1, §22(15)) are enforced in one
/// place no matter which command reaches a source.
#[async_trait]
pub trait SourcesPort: Send + Sync {
    /// Connector status and diagnostics (`plaud_probe`).
    ///
    /// Locates the official CLI and reads its version while the connector has
    /// no usable binding; never signs in and never lists.
    async fn plaud_probe(&self) -> Result<PlaudConnectorSnapshot, AppError>;

    /// Discover the CLI, then hand authentication to it (`plaud_connect`).
    /// `user_intent` is the validated action the user took.
    async fn plaud_connect(
        &self,
        user_intent: &'static str,
    ) -> Result<PlaudConnectorSnapshot, AppError>;

    /// Drop the app's association (`plaud_disconnect`, spec §4.2.7).
    async fn plaud_disconnect(&self) -> Result<PlaudDisconnectResponse, AppError>;

    /// One page of the account's recordings (`plaud_list`).
    async fn plaud_list(&self, cursor: Option<String>) -> Result<PlaudListResponse, AppError>;

    /// Download, verify and commit listed recordings (`plaud_import`). An ID
    /// that was not in the last listing rejects the whole request.
    async fn plaud_import(
        &self,
        external_ids: Vec<String>,
    ) -> Result<PlaudImportResponse, AppError>;

    /// Register a watched folder (`source_add_folder`). The path is validated
    /// inside the port, against the approved roots, exactly like a manual
    /// import path.
    async fn source_add_folder(
        &self,
        request: SourceAddFolderRequest,
    ) -> Result<SourceAddFolderResponse, AppError>;

    /// Every watched folder this library stores, in registration order.
    ///
    /// This is not a published command and deliberately does not become one:
    /// it exists for [`crate::folder_pump::WatchedFolderPump`], which has to
    /// know what to sweep on its timer and must still know it after a
    /// restart, when nothing has called `source_add_folder` in this session.
    /// Reading the list through the port rather than through a command keeps
    /// the IPC surface the size it was while still letting the timer do its
    /// job.
    async fn watched_folder_ids(&self) -> Result<Vec<String>, AppError>;

    /// Run one sync of a source (`source_sync`).
    async fn source_sync(
        &self,
        source_id: uuid::Uuid,
        trigger: SyncTrigger,
    ) -> Result<SourceSyncResponse, AppError>;

    /// Reconcile a source against the library (`source_reconcile`).
    async fn source_reconcile(
        &self,
        source_id: uuid::Uuid,
    ) -> Result<SourceReconcileResponse, AppError>;

    /// Vendor exports found alongside a recording (`list_source_artifacts`).
    async fn list_source_artifacts(
        &self,
        recording_id: RecordingId,
    ) -> Result<ListSourceArtifactsResponse, AppError>;
}

/// Cross-recording search (spec §3.1, §9.8, §17.5).
#[async_trait]
pub trait SearchPort: Send + Sync {
    /// Search across all transcripts.
    async fn search(
        &self,
        query: &str,
        page: plaudon_persistence::search::SearchPage,
    ) -> Result<Vec<plaudon_app_core::search::SearchResult>, AppError>;
}

/// Speaker labelling and confirmation (spec §8.7, §17.6).
#[async_trait]
pub trait SpeakersPort: Send + Sync {
    /// List all speakers for a recording.
    async fn list_speakers(
        &self,
        recording_id: plaudon_domain::ids::RecordingId,
    ) -> Result<plaudon_app_core::speakers::SpeakerRegistry, AppError>;

    /// Rename a speaker.
    async fn rename_speaker(
        &self,
        recording_id: plaudon_domain::ids::RecordingId,
        speaker_id: plaudon_domain::ids::SpeakerId,
        name: &str,
        at: plaudon_domain::time::Timestamp,
    ) -> Result<plaudon_app_core::speakers::Speaker, AppError>;

    /// Confirm a speaker's identity.
    async fn confirm_speaker(
        &self,
        recording_id: plaudon_domain::ids::RecordingId,
        speaker_id: plaudon_domain::ids::SpeakerId,
        at: plaudon_domain::time::Timestamp,
    ) -> Result<plaudon_app_core::speakers::Speaker, AppError>;

    /// Reset a speaker to machine-assigned label.
    async fn reset_speaker(
        &self,
        recording_id: plaudon_domain::ids::RecordingId,
        speaker_id: plaudon_domain::ids::SpeakerId,
    ) -> Result<plaudon_app_core::speakers::Speaker, AppError>;

    /// Attribute one segment of a revision to a speaker row, or to nobody
    /// (spec §4.1, VP-031).
    ///
    /// The assignment is the user's, never the machine's: this writes what a
    /// human picked and infers nothing from the audio.
    async fn set_segment_speaker(
        &self,
        recording_id: plaudon_domain::ids::RecordingId,
        revision_id: plaudon_domain::ids::TranscriptRevisionId,
        seq_index: u32,
        speaker_id: Option<plaudon_domain::ids::SpeakerId>,
    ) -> Result<SegmentSpeakerAssignment, AppError>;
}

/// What a segment is attributed to after [`SpeakersPort::set_segment_speaker`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentSpeakerAssignment {
    /// Speaker row the segment now points at, `None` when the attribution was
    /// cleared.
    pub speaker_id: Option<plaudon_domain::ids::SpeakerId>,
    /// That row's current label, resolved for display.
    pub speaker_label: Option<String>,
}

/// Cloud AI over OpenRouter BYOK (spec §10, ADR-0022): model catalogue, usage
/// and budget, consent, model preferences and cost-bearing dispatch.
#[async_trait]
pub trait CloudPort: Send + Sync {
    /// The model picker for a stage.
    async fn list_models(&self, query: ValidatedListModels)
        -> Result<ListModelsResponse, AppError>;

    /// Ledger usage in a range, plus the current budget.
    async fn get_usage(&self, range: ValidatedUsageRange) -> Result<GetUsageResponse, AppError>;

    /// Replace the local spending budget.
    async fn set_cloud_budget(&self, budget: ValidatedBudget) -> Result<CloudBudget, AppError>;

    /// Record a consent grant.
    async fn grant_cloud_consent(
        &self,
        grant: ValidatedConsent,
    ) -> Result<GrantCloudConsentResponse, AppError>;

    /// Revoke a consent grant. Idempotent for a known id.
    async fn revoke_cloud_consent(&self, consent_id: String) -> Result<(), AppError>;

    /// Write a default or a per-recording model preference.
    async fn set_model_preference(
        &self,
        preference: ValidatedPreference,
    ) -> Result<SetModelPreferenceResponse, AppError>;

    /// Start a processing stage, enforcing budget and consent before any
    /// content leaves the machine.
    async fn start_processing(
        &self,
        request: ValidatedStartProcessing,
    ) -> Result<StartProcessingResponse, AppError>;

    /// Run the capability smoke test for a model (a real, cost-bearing call).
    async fn verify_model(
        &self,
        request: ValidatedVerifyModel,
    ) -> Result<VerifyModelResponse, AppError>;

    /// Every consent grant on record, for `get_capabilities`.
    async fn cloud_consents(&self) -> Result<Vec<ConsentSummary>, AppError>;
}

/// Direct PLAUD device access (spec §15.2 `device_*`, App. B.8).
///
/// Every method is read-only toward the device: there is no method that
/// deletes, clears, controls recording, writes settings or acknowledges a sync
/// (spec §7.9). Secrets enter through [`SecretValue`] and leave only as
/// presence flags.
#[async_trait]
pub trait DevicesPort: Send + Sync {
    /// Scan for advertising devices and merge the previously linked ones.
    ///
    /// A radio that cannot be used is reported in
    /// [`DeviceScanDto::radio`], never as an empty device list.
    async fn list_nearby(&self, timeout_ms: u64) -> Result<DeviceScanDto, AppError>;

    /// Establish model, firmware and authentication mode, then gate them
    /// against the compatibility register.
    async fn probe(
        &self,
        peripheral_id: &str,
        experimental_probe_approved: bool,
    ) -> Result<DeviceProbeDto, AppError>;

    /// Store the device credential in the OS vault. Returns status only.
    async fn authorize(
        &self,
        peripheral_id: &str,
        token_hex: SecretValue,
    ) -> Result<DeviceAuthStatusDto, AppError>;

    /// Remove the stored credential and the link.
    async fn revoke(&self, peripheral_id: &str) -> Result<DeviceAuthStatusDto, AppError>;

    /// The recordings the device lists right now, with transfer states.
    async fn list_recordings(&self, peripheral_id: &str) -> Result<DeviceListingDto, AppError>;

    /// Start read-only transfers of listed recordings. Returns one transfer
    /// identifier per recording, in request order.
    async fn import(
        &self,
        peripheral_id: &str,
        recording_ids: Vec<String>,
        local_only: bool,
    ) -> Result<Vec<String>, AppError>;

    /// End the session and cancel transfers in flight. Archived recordings are
    /// never touched.
    async fn disconnect(&self, peripheral_id: &str) -> Result<(), AppError>;

    /// Opt in or out of read-only transfer when the device is nearby.
    async fn sync_policy(
        &self,
        peripheral_id: &str,
        enable_when_nearby: bool,
    ) -> Result<DeviceSyncPolicyDto, AppError>;
}

/// Generated outputs and claim review (`list_outputs`, `review_claim`, `regenerate_output`).
#[async_trait]
pub trait OutputsPort: Send + Sync {
    /// Every generated output of a recording, with its claims.
    async fn list_outputs(
        &self,
        recording_id: RecordingId,
    ) -> Result<Vec<crate::dto::OutputDto>, AppError>;

    /// Record a human review decision, returning the updated claim state.
    async fn review_claim(
        &self,
        claim_id: ClaimId,
        accepted: bool,
        note: Option<String>,
    ) -> Result<ClaimState, AppError>;

    /// Trigger regeneration of an output.
    async fn regenerate_output(&self, output_id: OutputId) -> Result<JobId, AppError>;
}

/// One output with all its claims, for the review panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputWithClaims {
    /// Output header.
    pub header: OutputHeader,
    /// Claims with their grounding state.
    pub claims: Vec<ClaimWithFields>,
}

/// A claim with its rendered §9.2 fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimWithFields {
    /// The claim itself.
    pub claim: Claim,
    /// Rendered fields from the output body JSON.
    pub fields: ItemFields,
}

/// Rendered §9.2 fields of one item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ItemFields {
    /// Summary statement (no additional fields).
    SummaryStatement,
    /// Decision.
    Decision {
        /// The decision statement.
        statement: String,
        /// Participants.
        participants: Vec<String>,
        /// Time reference.
        time_reference: String,
        /// Assertion.
        assertion: String,
    },
    /// Action item.
    ActionItem {
        /// Verb.
        verb: String,
        /// Description.
        description: String,
        /// Owner.
        owner: String,
        /// Due date.
        due_date: String,
        /// Status.
        status: String,
        /// Assertion.
        assertion: String,
    },
    /// Risk.
    Risk {
        /// Description.
        description: String,
        /// Consequence.
        consequence: String,
        /// Mitigation.
        mitigation: String,
    },
    /// Question.
    Question {
        /// Question text.
        question: String,
        /// Owner.
        owner: String,
    },
    /// Topic.
    Topic {
        /// Heading.
        heading: String,
        /// First segment.
        first_segment: u32,
        /// Last segment.
        last_segment: u32,
    },
}

/// Original-audio and sanitized-derivative export (`export_media`, spec §4.6,
/// §7.8).
#[async_trait]
pub trait MediaExportPort: Send + Sync {
    /// Copy the stored bytes into a user-chosen folder, verifying the digest.
    async fn export_media(
        &self,
        recording_id: RecordingId,
        media_asset_id: Option<plaudon_domain::ids::MediaAssetId>,
        destination: crate::validate::ValidatedDirectory,
        include_sanitized_derivative: bool,
    ) -> Result<crate::dto::ExportMediaResponse, AppError>;
}

/// Deletion with disclosure and durable tombstones (spec §4.6, §7.10).
#[async_trait]
pub trait DeletionPort: Send + Sync {
    /// Exactly what a deletion would remove, and what it cannot reach.
    async fn deletion_disclosure(
        &self,
        recording_id: RecordingId,
    ) -> Result<crate::dto::DeletionDisclosureDto, AppError>;

    /// Delete the recording, given the confirmation token of the disclosure
    /// the user actually saw.
    async fn delete_recording(
        &self,
        recording_id: RecordingId,
        confirmation_token: String,
    ) -> Result<crate::dto::DeleteRecordingResponse, AppError>;
}

/// The portable archive (`create_portable_backup`, `inspect_restore_archive`,
/// `restore_portable_archive`, spec §13.6, §13.8).
#[async_trait]
pub trait ArchivePort: Send + Sync {
    /// Write a complete, restorable archive of the library.
    async fn create_portable_backup(
        &self,
        destination: crate::validate::ValidatedDirectory,
        password: Option<String>,
        sensitive_profile: bool,
        include_voice_profiles: bool,
    ) -> Result<crate::dto::CreateBackupResponse, AppError>;

    /// Validate an archive and report what it holds, writing nothing.
    async fn inspect_restore_archive(
        &self,
        archive: crate::validate::ValidatedArchiveFile,
        password: Option<String>,
    ) -> Result<crate::dto::InspectArchiveResponse, AppError>;

    /// Restore an archive into this library.
    async fn restore_portable_archive(
        &self,
        archive: crate::validate::ValidatedArchiveFile,
        password: Option<String>,
        mode: crate::dto::RestoreModeDto,
        on_conflict: crate::dto::ConflictPolicyDto,
        restore_tombstoned: bool,
    ) -> Result<crate::dto::RestoreArchiveResponse, AppError>;
}

/// Transcript tools, grounded question answering, study artifacts and the
/// per-recording conversations they are recorded in (spec §9.7, §9.8, matrix
/// row A11).
///
/// Every method is user-initiated and runs one tool. The port owns the three
/// decisions a command must not make for itself: which text model answers,
/// whether that route may be used right now, and whether an answer survived
/// anchor verification.
#[async_trait]
pub trait ToolsPort: Send + Sync {
    /// Registered tools at their highest version, with the route they would
    /// run on.
    async fn list_transcript_tools(
        &self,
        recording_id: Option<RecordingId>,
    ) -> Result<crate::dto::ListTranscriptToolsResponse, AppError>;

    /// Answer one question from one pinned transcript revision.
    ///
    /// A refusal outcome is a successful answer: only an infrastructure
    /// failure, a refused route or an invalid argument is an `AppError`.
    async fn ask_recording(
        &self,
        request: crate::validate::ValidatedAskRecording,
    ) -> Result<crate::dto::AskRecordingResponse, AppError>;

    /// Conversations recorded against one recording, newest first.
    async fn list_conversations(
        &self,
        recording_id: RecordingId,
    ) -> Result<crate::dto::ListConversationsResponse, AppError>;

    /// Generate and store one set of study artifacts.
    async fn generate_study_artifacts(
        &self,
        request: crate::validate::ValidatedGenerateStudyArtifacts,
    ) -> Result<crate::dto::GenerateStudyArtifactsResponse, AppError>;

    /// Study artifacts already stored for one recording.
    async fn list_study_artifacts(
        &self,
        recording_id: RecordingId,
    ) -> Result<crate::dto::ListStudyArtifactsResponse, AppError>;

    /// Recorded tool runs of one recording, newest first.
    async fn list_tool_runs(
        &self,
        recording_id: RecordingId,
    ) -> Result<crate::dto::ListToolRunsResponse, AppError>;
}

/// Output templates: the shipped built-ins and the user's editable copies
/// (spec §9.3, matrix row A12).
///
/// A built-in is never edited in place and never shadowed. An edit to a user
/// copy stores a new version and keeps the previous one, so an output that
/// recorded `(template_id, template_version)` still resolves to the document
/// that produced it.
#[async_trait]
pub trait TemplatesPort: Send + Sync {
    /// Every template this installation can run.
    async fn list_templates(&self) -> Result<crate::dto::ListTemplatesResponse, AppError>;

    /// One template at one version; the current version when none is named.
    async fn get_template(
        &self,
        template_id: String,
        version: Option<String>,
    ) -> Result<crate::dto::TemplateDocumentDto, AppError>;

    /// Create an editable copy of a built-in.
    async fn create_template_copy(
        &self,
        base_id: String,
        new_id: String,
        display_name: Option<String>,
    ) -> Result<crate::dto::TemplateDocumentDto, AppError>;

    /// Store an edited user copy as a new version.
    async fn save_template(
        &self,
        template_id: String,
        document: String,
        expected_version: Option<String>,
    ) -> Result<crate::dto::SaveTemplateResponse, AppError>;

    /// Check a document without storing anything.
    async fn validate_template(
        &self,
        document: String,
    ) -> Result<crate::dto::ValidateTemplateResponse, AppError>;

    /// Stored versions of one template, newest first.
    async fn list_template_versions(
        &self,
        template_id: String,
    ) -> Result<crate::dto::ListTemplateVersionsResponse, AppError>;
}

/// The people roster: the one master speaker list (`VOICE_PROFILES_SPEC` §9A,
/// VP-010, VP-011).
///
/// Ungated on purpose. Every method here works in a library where biometric
/// matching was never turned on, where the OS credential store is locked, and
/// where no speaker-embedding model exists — because a typed name is ordinary
/// personal data and the roster is the assignment vocabulary of the whole app
/// (§12.2A). Nothing in this trait can reach an embedder: the only type that
/// authorises that is `plaudon_app_core::voice_profiles::ConsentProof`, which
/// no method here produces or accepts.
#[async_trait]
pub trait PeoplePort: Send + Sync {
    /// The roster, optionally filtered, with per-person counts and the
    /// same-name groups a screen may offer as merge candidates (VP-214).
    async fn list_people(&self, query: Option<&str>) -> Result<ListPeopleResponse, AppError>;

    /// Add a roster entry.
    async fn create_person(&self, draft: &PersonDraft) -> Result<PersonDto, AppError>;

    /// Edit a roster entry's name, aliases and plain contacts.
    async fn update_person(
        &self,
        person_id: plaudon_domain::ids::PersonId,
        draft: &PersonDraft,
    ) -> Result<PersonDto, AppError>;

    /// Merge two entries, or disclose what that would move (VP-213c).
    async fn merge_people(
        &self,
        loser: plaudon_domain::ids::PersonId,
        survivor: plaudon_domain::ids::PersonId,
        apply: bool,
    ) -> Result<MergePeopleResponse, AppError>;

    /// Delete an entry, or disclose what that would reach (VP-080).
    async fn delete_person(
        &self,
        person_id: plaudon_domain::ids::PersonId,
        apply: bool,
    ) -> Result<DeletePersonResponse, AppError>;

    /// Assign one cluster of one recording to a person, or clear it (VP-211).
    async fn assign_cluster_person(
        &self,
        assignment: &ClusterAssignment,
    ) -> Result<AssignClusterPersonResponse, AppError>;
}

/// A validated `assign_cluster_person` (VP-210, VP-211).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClusterAssignment {
    /// Recording the cluster belongs to. Nothing beyond it is touched.
    pub recording_id: RecordingId,
    /// Speaker row standing for the cluster.
    pub speaker_id: plaudon_domain::ids::SpeakerId,
    /// Person to assign, or `None` for "Unknown/none".
    pub person_id: Option<plaudon_domain::ids::PersonId>,
    /// Whether to overwrite segments that were attributed individually.
    pub overwrite_segment_exceptions: bool,
}

/// A validated enrolment or exemplar addition (VP-094).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnrolmentRequest {
    /// Person the speech is attributed to.
    pub person_id: plaudon_domain::ids::PersonId,
    /// Recording the speech is in.
    pub recording_id: RecordingId,
    /// Cluster within that recording's latest diarization run.
    pub cluster_key: String,
    /// The attestation, for an enrolment. `None` extends an existing one,
    /// which was attested when it was created.
    pub attestation: Option<plaudon_app_core::voice_profiles::EnrolmentAttestation>,
}

/// A validated `split_voice_enrollment` (§3.5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitRequest {
    /// Enrolment to split.
    pub person_id: plaudon_domain::ids::PersonId,
    /// Exemplars to move.
    pub exemplar_ids: Vec<plaudon_domain::ids::VoiceExemplarId>,
    /// Where they go.
    pub target: SplitTarget,
    /// The attestation for the target person.
    pub attestation: plaudon_app_core::voice_profiles::EnrolmentAttestation,
}

/// Who a split's exemplars move to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SplitTarget {
    /// An entry already on the roster.
    Existing(plaudon_domain::ids::PersonId),
    /// A new entry to create with this display name.
    New(String),
}

/// The biometric tier: the consent gate, enrolments and suggestions
/// (`VOICE_PROFILES_SPEC` §8, §12.2).
///
/// Every method that could lead to an embedding first reads the stored gate
/// and refuses when it is closed, so "off by default" (VP-003) and "no embed
/// call is reachable while ungated or while matching is off" (VP-011, VP-215)
/// are properties of this implementation rather than of its callers.
#[async_trait]
pub trait VoiceProfilesPort: Send + Sync {
    /// The feature's state: the toggle, the gate, the disclosure and the
    /// counts a settings block shows (VP-090).
    async fn voice_profile_settings(&self) -> Result<VoiceProfileSettingsDto, AppError>;

    /// Turn biometric matching on — which requires accepting the current
    /// disclosure — or off (VP-120, VP-121).
    async fn set_voice_profiles_enabled(
        &self,
        enabled: bool,
        accepted_disclosure_version: Option<&str>,
    ) -> Result<VoiceProfileSettingsDto, AppError>;

    /// Attach an enrolment to a roster person from one cluster (VP-094).
    async fn enroll_person_voice(
        &self,
        request: &EnrolmentRequest,
    ) -> Result<VoiceEnrollmentDto, AppError>;

    /// Extend an enrolment from another recording's cluster (§6.5).
    async fn add_voice_exemplar(
        &self,
        request: &EnrolmentRequest,
    ) -> Result<VoiceEnrollmentDto, AppError>;

    /// Split an enrolment's exemplars by provenance onto another person.
    async fn split_voice_enrollment(
        &self,
        request: &SplitRequest,
    ) -> Result<SplitVoiceEnrollmentResponse, AppError>;

    /// One enrolment's exemplars: ids, provenance and durations, never the
    /// vectors, so a user can see what a profile is made of and split it
    /// (VP-091, §3.5).
    async fn list_voice_exemplars(
        &self,
        person_id: plaudon_domain::ids::PersonId,
    ) -> Result<ListVoiceExemplarsResponse, AppError>;

    /// The clean speech one cluster offers, for the enrolment meter (VP-094).
    async fn cluster_speech(
        &self,
        recording_id: RecordingId,
        cluster_key: &str,
    ) -> Result<ClusterSpeechDto, AppError>;

    /// Delete one person's voiceprint, or disclose what that destroys. The
    /// roster entry stays (VP-080, VP-123).
    async fn delete_voice_enrollment(
        &self,
        person_id: plaudon_domain::ids::PersonId,
        apply: bool,
        reason: Option<&str>,
    ) -> Result<DeleteVoiceEnrollmentResponse, AppError>;

    /// "Forget everyone": every enrolment, exemplar, suggestion and the key
    /// (VP-095). The roster survives.
    async fn forget_all_voice_profiles(
        &self,
        apply: bool,
    ) -> Result<ForgetAllVoiceProfilesResponse, AppError>;

    /// Pending suggestions for one recording, or the global review queue.
    async fn list_speaker_suggestions(
        &self,
        recording_id: Option<RecordingId>,
        limit: u32,
    ) -> Result<ListSpeakerSuggestionsResponse, AppError>;

    /// Confirm a suggestion: assign the person through the assignment path.
    async fn confirm_speaker_suggestion(
        &self,
        suggestion_id: plaudon_domain::ids::SpeakerSuggestionId,
        overwrite_segment_exceptions: bool,
    ) -> Result<SpeakerSuggestionDecisionDto, AppError>;

    /// Reject a suggestion, optionally naming who it actually was (VP-216).
    async fn reject_speaker_suggestion(
        &self,
        suggestion_id: plaudon_domain::ids::SpeakerSuggestionId,
        corrected_person_id: Option<plaudon_domain::ids::PersonId>,
        overwrite_segment_exceptions: bool,
    ) -> Result<SpeakerSuggestionDecisionDto, AppError>;

    /// Queue the §10 match job for an explicit selection of recordings
    /// (VP-007).
    async fn start_speaker_matching(
        &self,
        recording_ids: &[RecordingId],
    ) -> Result<StartSpeakerMatchingResponse, AppError>;
}
