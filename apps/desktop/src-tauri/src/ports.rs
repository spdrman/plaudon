//! The traits the command layer talks to.
//!
//! Commands never see SQL, the filesystem or a vendor SDK: they validate their
//! arguments and call a port. EPIC B's `crates/persistence` and EPIC C's
//! `crates/jobs` replace the implementations in [`crate::backend`] by swapping
//! what [`crate::build_ports`] constructs — no command and no UI change.

use std::path::PathBuf;

use async_trait::async_trait;
use plaudon_domain::error::AppError;
use plaudon_domain::ids::{JobId, RecordingId};
use plaudon_domain::transcript::AlignmentOrigin;

use crate::dto::SourceAddFolderRequest;
use crate::dto::{
    ActivityPage, Capabilities, CompleteOnboardingRequest, DuplicatePolicy, ImportFilesResponse,
    JobPage, JobSummary, ListSourceArtifactsResponse, LocalOnlyModeResponse, NotificationSettings,
    NotificationSettingsResponse, PlaudConnectorSnapshot, PlaudDisconnectResponse,
    PlaudImportResponse, PlaudListResponse, Preferences, RecordingDetail, RecordingPage,
    SaveTranscriptRevisionResponse, SourceAddFolderResponse, SourceReconcileResponse,
    SourceSyncResponse, SyncTrigger, TranscriptPage, TranscriptRevisionRef,
};
use crate::validate::{
    ValidatedExport, ValidatedGetTranscript, ValidatedListActivity, ValidatedListJobs,
    ValidatedListRecordings, ValidatedSaveRevision,
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
