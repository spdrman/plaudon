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

use crate::dto::SourceAddFolderRequest;
use crate::dto::{
    ActivityPage, Capabilities, CloudBudget, CompleteOnboardingRequest, ConsentSummary,
    DeviceAuthStatusDto, DeviceListingDto, DeviceProbeDto, DeviceScanDto, DeviceSyncPolicyDto,
    DuplicatePolicy, GetUsageResponse, GrantCloudConsentResponse, ImportFilesResponse, JobPage,
    JobSummary, ListModelsResponse, ListSourceArtifactsResponse, LocalOnlyModeResponse,
    NotificationSettings, NotificationSettingsResponse, PlaudConnectorSnapshot,
    PlaudDisconnectResponse, PlaudImportResponse, PlaudListResponse, Preferences, RecordingDetail,
    RecordingPage, SaveTranscriptRevisionResponse, SetModelPreferenceResponse,
    SourceAddFolderResponse, SourceReconcileResponse, SourceSyncResponse, StartProcessingResponse,
    SyncTrigger, TranscriptPage, TranscriptRevisionRef, VerifyModelResponse,
};
use crate::secrets::SecretValue;
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
