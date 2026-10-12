//! Wire DTOs for the Tauri command surface (spec §15.2).
//!
//! Every type here mirrors one interface in `apps/desktop/src/ipc/contract.ts`
//! field for field. The rules the frontend relies on:
//!
//! * objects use snake_case field names and enums snake_case values;
//! * an unknown value is `Option::None` and serializes as `null`; it is never
//!   replaced by a default (spec §8.6A: never fabricate duration, alignment or
//!   progress);
//! * timestamps are RFC 3339 strings rendered from
//!   [`plaudon_domain::time::Timestamp`];
//! * identifiers are the domain newtypes, which serialize as bare strings;
//! * `sha256` is [`Sha256Digest`], which serializes as lowercase hex.
//!
//! Request DTOs carry identifiers as `String` because they arrive from
//! JavaScript: parsing happens in [`crate::validate`] so a malformed id becomes
//! a typed `INVALID_ARGUMENT` envelope instead of a deserialization failure.

use std::collections::BTreeMap;

use plaudon_domain::capability::CapabilityStatus;
use plaudon_domain::error::ErrorEnvelope;
use plaudon_domain::ids::{JobId, OutputId, RecordingId, TranscriptRevisionId};
use plaudon_domain::media::Sha256Digest;
use plaudon_domain::transcript::AlignmentOrigin;
use serde::{Deserialize, Serialize};

/// Request payload of a command that takes no arguments.
///
/// The frontend always sends `{ request: {} }`, so the commands that the
/// contract types as `Record<string, never>` still take a request struct.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmptyRequest {}

/// Acknowledgement returned by secret-bearing commands.
///
/// The contract types this as `{ ok: true }`; no other shape is ever produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusResponse {
    /// Always `true`.
    pub ok: bool,
}

impl StatusResponse {
    /// The only value this type ever takes.
    #[must_use]
    pub const fn ok() -> Self {
        Self { ok: true }
    }
}

/* ------------------------------------------------------------ capabilities */

/// Pipeline stage an engine participates in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EngineStage {
    /// Speech to text.
    Stt,
    /// Text-to-text work such as summarization.
    Text,
}

/// One locally installed model of an engine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledModel {
    /// Engine-scoped model identifier.
    pub model_id: String,
    /// Name shown in the model picker.
    pub display_name: String,
    /// On-disk size of the installed files.
    pub size_bytes: u64,
    /// Whether the files are present right now.
    pub installed: bool,
}

/// One row of the engine inventory shown by the engine picker (spec §5.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineInventoryEntry {
    /// Stable engine id, for example `whisper.cpp`.
    pub engine_id: String,
    /// Name shown in the picker.
    pub display_name: String,
    /// Stage this engine serves.
    pub stage: EngineStage,
    /// Validation status; `unknown` until evidence exists.
    pub status: CapabilityStatus,
    /// Why the engine is unavailable, when it is.
    pub unavailable_reason: Option<String>,
    /// Models installed on this machine.
    pub installed_models: Vec<InstalledModel>,
    /// Bytes needed to install the smallest usable model, when known.
    pub download_bytes_required: Option<u64>,
    /// Whether using the engine requires network egress.
    pub requires_network: bool,
}

/// Free and total space of the volume holding the library.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageCapability {
    /// Absolute path of the library root.
    pub library_path: String,
    /// Free bytes on the volume.
    pub free_bytes: u64,
    /// Total bytes of the volume.
    pub total_bytes: u64,
}

/// Appearance preference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Appearance {
    /// Follow the operating system.
    System,
    /// Always light.
    Light,
    /// Always dark.
    Dark,
}

/// Contrast preference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Contrast {
    /// Standard contrast.
    Standard,
    /// Increased contrast.
    High,
}

/// Persisted user preferences.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Preferences {
    /// Whether first-run onboarding has been completed.
    pub onboarding_completed: bool,
    /// Whether local-only mode is enforced.
    pub local_only_mode: bool,
    /// Preferred speech-to-text engine, when the user chose one.
    pub default_stt_engine_id: Option<String>,
    /// Preferred text engine, when the user chose one.
    pub default_text_engine_id: Option<String>,
    /// Appearance preference.
    pub appearance: Appearance,
    /// Contrast preference.
    pub contrast: Contrast,
    /// Notification preferences.
    pub notifications: NotificationSettings,
}

/// Host operating system.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Platform {
    /// Apple macOS.
    Macos,
    /// Microsoft Windows.
    Windows,
    /// Linux.
    Linux,
}

/// Operating-system identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OsInfo {
    /// Host platform.
    pub platform: Platform,
    /// Version string as reported by the operating system.
    pub version: String,
}

/// Host hardware facts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HardwareInfo {
    /// Target architecture, for example `aarch64`.
    pub arch: String,
    /// Logical cores visible to the process.
    pub logical_cores: u32,
    /// Total physical memory in bytes.
    pub total_memory_bytes: u64,
}

/// State of an operating-system permission.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionState {
    /// The user granted the permission.
    Granted,
    /// The user denied the permission.
    Denied,
    /// The permission has never been requested.
    NotRequested,
}

/// Everything the shell knows about this machine (spec §15.2 `get_capabilities`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    /// Operating system identity.
    pub os: OsInfo,
    /// Hardware facts.
    pub hardware: HardwareInfo,
    /// Engine inventory, including unavailable engines and their reasons.
    pub engines: Vec<EngineInventoryEntry>,
    /// Library volume capacity.
    pub storage: StorageCapability,
    /// Integration statuses keyed by integration id.
    pub integrations: BTreeMap<String, CapabilityStatus>,
    /// Operating-system permissions keyed by permission id.
    pub permissions: BTreeMap<String, PermissionState>,
    /// Stored preferences.
    pub preferences: Preferences,
    /// Whether an OpenRouter key is present in the OS keychain.
    pub openrouter_key_present: bool,
}

/* --------------------------------------------------------------- recording */

/// Import lifecycle of a recording.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportStatus {
    /// Accepted but not started.
    Pending,
    /// Bytes are being copied into the archive.
    Importing,
    /// Committed to the archive.
    Imported,
    /// The import failed.
    Failed,
}

/// Transcription lifecycle of a recording.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptionStatus {
    /// No transcript has been requested.
    None,
    /// Queued for transcription.
    Queued,
    /// Transcription is running.
    Running,
    /// A transcript exists.
    Completed,
    /// Transcription failed.
    Failed,
    /// The transcript no longer matches the current media.
    Stale,
}

/// Where a recording came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    /// A file the user picked or dropped.
    ManualImport,
    /// The PLAUD cloud connector.
    PlaudCloud,
    /// A directly attached PLAUD device.
    PlaudDevice,
    /// A watched folder.
    WatchedFolder,
}

/// Source attribution shown in the library list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordingSourceRef {
    /// Source family.
    pub source_kind: SourceKind,
    /// Human-readable label.
    pub label: String,
}

/// Library list row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordingSummary {
    /// Recording identity.
    pub recording_id: RecordingId,
    /// Display title.
    pub title: String,
    /// When the audio was captured, when known.
    pub recorded_at: Option<String>,
    /// When the recording entered the library.
    pub imported_at: String,
    /// Media duration, or `null` when it has not been measured.
    pub duration_ms: Option<u64>,
    /// Source attribution.
    pub source: RecordingSourceRef,
    /// Import lifecycle.
    pub import_status: ImportStatus,
    /// Transcription lifecycle.
    pub transcription_status: TranscriptionStatus,
    /// Engine currently processing the recording, when one is.
    pub processing_engine: Option<String>,
    /// Last error recorded against the recording.
    pub last_error: Option<ErrorEnvelope>,
    /// User tags.
    pub tags: Vec<String>,
    /// Length of the archived bytes.
    pub byte_length: u64,
    /// Digest of the archived bytes.
    pub sha256: Sha256Digest,
}

/// Observed media facts for the detail view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordingMediaInfo {
    /// Codec reported for these bytes.
    pub observed_codec: String,
    /// Sample rate, when a decoder reported one.
    pub sample_rate_hz: Option<u32>,
    /// Channel count, when a decoder reported one.
    pub channels: Option<u32>,
    /// Original file name, kept as metadata only.
    pub original_file_name: String,
}

/// Kind of derived AI output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputKind {
    /// Executive summary.
    Summary,
    /// Action items.
    Actions,
    /// Decisions.
    Decisions,
}

/// A derived output of a transcript revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordingOutput {
    /// Output identity.
    pub output_id: OutputId,
    /// Output kind.
    pub kind: OutputKind,
    /// Revision the output was generated from.
    pub source_revision_id: TranscriptRevisionId,
    /// Creation instant.
    pub created_at: String,
    /// Whether the source revision has been superseded.
    pub stale: bool,
}

/// Library detail view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordingDetail {
    /// All summary fields, flattened exactly as the TypeScript `extends`.
    #[serde(flatten)]
    pub summary: RecordingSummary,
    /// Observed media facts.
    pub media: RecordingMediaInfo,
    /// Transcript revisions, newest last.
    pub revisions: Vec<TranscriptRevisionRef>,
    /// Current head revision, when one exists.
    pub current_revision_id: Option<TranscriptRevisionId>,
    /// Derived outputs.
    pub outputs: Vec<RecordingOutput>,
}

/// Sortable column of the library list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordingSortField {
    /// Capture instant.
    RecordedAt,
    /// Import instant.
    ImportedAt,
    /// Title.
    Title,
    /// Duration.
    DurationMs,
}

/// Sort direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SortDirection {
    /// Ascending.
    Asc,
    /// Descending.
    Desc,
}

/// Sort specification of the library list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordingSort {
    /// Column.
    pub field: RecordingSortField,
    /// Direction.
    pub direction: SortDirection,
}

/// Library list filter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordingFilter {
    /// Free-text query over the title.
    pub query: Option<String>,
    /// Import statuses to include; empty means all.
    pub import_status: Vec<ImportStatus>,
    /// Transcription statuses to include; empty means all.
    pub transcription_status: Vec<TranscriptionStatus>,
    /// Source kinds to include; empty means all.
    pub sources: Vec<String>,
    /// Tags to include; empty means all.
    pub tags: Vec<String>,
    /// Inclusive lower RFC 3339 bound.
    pub date_from: Option<String>,
    /// Inclusive upper RFC 3339 bound.
    pub date_to: Option<String>,
}

/// `list_recordings` request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListRecordingsRequest {
    /// Opaque cursor from the previous page.
    pub cursor: Option<String>,
    /// Page size.
    pub limit: u32,
    /// Filter.
    pub filter: RecordingFilter,
    /// Sort.
    pub sort: RecordingSort,
}

/// `list_recordings` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordingPage {
    /// Rows of this page.
    pub items: Vec<RecordingSummary>,
    /// Cursor of the next page, when more rows exist.
    pub next_cursor: Option<String>,
    /// Total matching rows at query time.
    pub total: u64,
}

/// `get_recording` request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetRecordingRequest {
    /// Recording identity.
    pub recording_id: String,
}

/* -------------------------------------------------------------- transcript */

/// Whether a revision is engine output or a human edit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RevisionKind {
    /// Immutable engine output.
    Machine,
    /// A user edit of a previous revision.
    Human,
}

/// Revision header.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptRevisionRef {
    /// Revision identity.
    pub revision_id: TranscriptRevisionId,
    /// Machine output or human edit.
    pub kind: RevisionKind,
    /// Creation instant.
    pub created_at: String,
    /// Producing engine, when known.
    pub engine_id: Option<String>,
    /// Producing model, when known.
    pub model_id: Option<String>,
    /// Alignment origin of every segment in the revision.
    pub alignment_origin: AlignmentOrigin,
    /// Number of segments.
    pub segment_count: u32,
    /// Whether a newer revision exists.
    pub superseded: bool,
}

/// Engine-scoped confidence, never compared across engines (spec §8.5).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SegmentConfidence {
    /// Engine that produced the value.
    pub engine_id: String,
    /// Raw value, verbatim from the engine.
    pub value: f64,
}

/// Annotation kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnnotationKind {
    /// The engine reported low confidence.
    LowConfidence,
    /// The text matched a hallucination signature.
    PossibleHallucination,
    /// The engine marked the audio inaudible.
    Inaudible,
    /// The segment was edited by a human.
    Edited,
}

/// A segment annotation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptAnnotation {
    /// Annotation kind.
    pub kind: AnnotationKind,
    /// Why the annotation exists.
    pub reason: String,
    /// Segment the annotation applies to.
    pub segment_index: u32,
    /// Start offset, when the revision has alignment.
    pub start_ms: Option<u64>,
    /// End offset, when the revision has alignment.
    pub end_ms: Option<u64>,
}

/// One transcript segment on the wire.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TranscriptSegmentDto {
    /// Zero-based position.
    pub index: u32,
    /// Normalized text.
    pub text: String,
    /// Start offset, `null` when the revision has no alignment.
    pub start_ms: Option<u64>,
    /// End offset, `null` when the revision has no alignment.
    pub end_ms: Option<u64>,
    /// Alignment origin of this segment.
    pub alignment_origin: AlignmentOrigin,
    /// Engine-scoped confidence, when the engine reported one.
    pub confidence: Option<SegmentConfidence>,
    /// Speaker label, when diarization ran.
    pub speaker_label: Option<String>,
    /// Annotations attached to this segment.
    pub annotations: Vec<TranscriptAnnotation>,
}

/// `get_transcript` request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetTranscriptRequest {
    /// Revision identity.
    pub revision_id: String,
    /// Segment index to resume from.
    pub cursor: Option<u32>,
    /// Page size.
    pub limit: u32,
}

/// `get_transcript` response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TranscriptPage {
    /// Revision header.
    pub revision: TranscriptRevisionRef,
    /// Segments of this page.
    pub segments: Vec<TranscriptSegmentDto>,
    /// Index to resume from, when more segments exist.
    pub next_cursor: Option<u32>,
    /// Total segments in the revision.
    pub total_segments: u32,
}

/// One edited segment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SegmentEdit {
    /// Segment index being replaced.
    pub index: u32,
    /// Replacement text.
    pub text: String,
}

/// `save_transcript_revision` request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SaveTranscriptRevisionRequest {
    /// Recording the revision belongs to.
    pub recording_id: String,
    /// Revision the edits were made against.
    pub base_revision_id: String,
    /// Edits, one per changed segment.
    pub edits: Vec<SegmentEdit>,
}

/// `save_transcript_revision` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SaveTranscriptRevisionResponse {
    /// Identity of the new revision.
    pub revision_id: TranscriptRevisionId,
    /// Outputs whose source revision is now superseded.
    pub stale_output_ids: Vec<OutputId>,
}

/* -------------------------------------------------------------------- jobs */

/// Job lifecycle state: spec §14.1 coarse states plus §14.7 arbitration states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    /// Accepted, not started.
    Queued,
    /// Checking preconditions.
    Preflight,
    /// Blocked on a user decision.
    WaitingForUser,
    /// Blocked on network availability.
    WaitingForNetwork,
    /// Blocked on a model asset.
    WaitingForModel,
    /// Blocked on a source.
    WaitingForSource,
    /// Blocked on an authorization grant.
    WaitingForAuthorization,
    /// Running on this machine.
    RunningLocal,
    /// Ready to dispatch to a cloud provider.
    CloudReady,
    /// Bytes are in flight to a cloud provider.
    CloudSending,
    /// Dispatched but the outcome is unknown; possibly billed.
    CloudOutcomeUnknown,
    /// Verifying produced artifacts.
    Verifying,
    /// Committing the durable result.
    Committing,
    /// Cancellation requested, winding down.
    Cancelling,
    /// Finished with a durably committed result.
    Completed,
    /// Failed in a way that may be retried.
    FailedRetryable,
    /// Failed permanently.
    FailedTerminal,
    /// Cancelled.
    Cancelled,
}

impl JobState {
    /// Every state, in the order published by `JOB_STATES` in `contract.ts`.
    pub const ALL: &'static [Self] = &[
        Self::Queued,
        Self::Preflight,
        Self::WaitingForUser,
        Self::WaitingForNetwork,
        Self::WaitingForModel,
        Self::WaitingForSource,
        Self::WaitingForAuthorization,
        Self::RunningLocal,
        Self::CloudReady,
        Self::CloudSending,
        Self::CloudOutcomeUnknown,
        Self::Verifying,
        Self::Committing,
        Self::Cancelling,
        Self::Completed,
        Self::FailedRetryable,
        Self::FailedTerminal,
        Self::Cancelled,
    ];

    /// Stable wire string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Preflight => "preflight",
            Self::WaitingForUser => "waiting_for_user",
            Self::WaitingForNetwork => "waiting_for_network",
            Self::WaitingForModel => "waiting_for_model",
            Self::WaitingForSource => "waiting_for_source",
            Self::WaitingForAuthorization => "waiting_for_authorization",
            Self::RunningLocal => "running_local",
            Self::CloudReady => "cloud_ready",
            Self::CloudSending => "cloud_sending",
            Self::CloudOutcomeUnknown => "cloud_outcome_unknown",
            Self::Verifying => "verifying",
            Self::Committing => "committing",
            Self::Cancelling => "cancelling",
            Self::Completed => "completed",
            Self::FailedRetryable => "failed_retryable",
            Self::FailedTerminal => "failed_terminal",
            Self::Cancelled => "cancelled",
        }
    }

    /// Parse a stored or requested state string.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|state| state.as_str() == value)
    }

    /// Whether no further transition is possible.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::FailedTerminal | Self::Cancelled
        )
    }
}

/// Job family.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
    /// Copy a file into the archive.
    Import,
    /// Produce a transcript.
    Transcribe,
    /// Produce a summary.
    Summarize,
    /// Write an export file.
    Export,
    /// Install a model.
    ModelInstall,
    /// Synchronize a source.
    SourceSync,
}

impl JobKind {
    /// Every kind.
    pub const ALL: &'static [Self] = &[
        Self::Import,
        Self::Transcribe,
        Self::Summarize,
        Self::Export,
        Self::ModelInstall,
        Self::SourceSync,
    ];

    /// Stable wire string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Import => "import",
            Self::Transcribe => "transcribe",
            Self::Summarize => "summarize",
            Self::Export => "export",
            Self::ModelInstall => "model_install",
            Self::SourceSync => "source_sync",
        }
    }

    /// Parse a stored or requested kind string.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|kind| kind.as_str() == value)
    }
}

/// Target family of a job's produced output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputLinkKind {
    /// A recording in the library.
    Recording,
    /// An export job.
    Export,
}

/// Where the UI should navigate for a job's output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputLink {
    /// Target family.
    pub kind: OutputLinkKind,
    /// Target identity.
    pub id: String,
}

/// Jobs list row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobSummary {
    /// Job identity.
    pub job_id: JobId,
    /// Job family.
    pub kind: JobKind,
    /// Lifecycle state.
    pub state: JobState,
    /// Current stage, when the job reports one.
    pub stage: Option<String>,
    /// Progress in `0..=1`, `null` when the stage cannot report a ratio.
    pub progress: Option<f64>,
    /// Creation instant.
    pub created_at: String,
    /// Last transition instant.
    pub updated_at: String,
    /// First execution instant, when the job has started.
    pub started_at: Option<String>,
    /// Terminal instant, when the job has finished.
    pub finished_at: Option<String>,
    /// Recording the job belongs to, when one exists.
    pub recording_id: Option<RecordingId>,
    /// Display label.
    pub label: String,
    /// Persisted failure envelope.
    pub error: Option<ErrorEnvelope>,
    /// Whether retrying may incur a new provider charge.
    pub cost_acknowledgement_required: bool,
    /// Provider request id preserved for uncertain outcomes.
    pub provider_request_id: Option<String>,
    /// Navigation target of the produced output.
    pub output_link: Option<OutputLink>,
}

/// `list_jobs` request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListJobsRequest {
    /// Opaque cursor from the previous page.
    pub cursor: Option<String>,
    /// Page size.
    pub limit: u32,
    /// States to include; empty means all.
    pub states: Vec<JobState>,
    /// Kinds to include; empty means all.
    pub kinds: Vec<JobKind>,
}

/// `list_jobs` response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobPage {
    /// Rows of this page.
    pub items: Vec<JobSummary>,
    /// Cursor of the next page, when more rows exist.
    pub next_cursor: Option<String>,
    /// Counts per state at query time.
    pub state_counts: BTreeMap<JobState, u64>,
}

/// `retry_job` request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetryJobRequest {
    /// Job identity.
    pub job_id: String,
    /// Explicit acknowledgement that a retry may be billed again.
    pub cost_acknowledged: bool,
}

/// `cancel_job` request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CancelJobRequest {
    /// Job identity.
    pub job_id: String,
}

/* ------------------------------------------------------------------ import */

/// What to do when the imported bytes already exist in the archive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DuplicatePolicy {
    /// Report the duplicate and wait for the user.
    Ask,
    /// Create a second recording for the same bytes.
    ImportAnyway,
    /// Return the existing recording.
    OpenExisting,
    /// Do nothing.
    Skip,
}

/// `import_files` request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportFilesRequest {
    /// Paths obtained from the OS picker or a window drop event.
    pub paths: Vec<String>,
    /// Duplicate handling.
    pub duplicate_policy: DuplicatePolicy,
}

/// Per-path result of an import request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ImportOutcome {
    /// The file was accepted and an import job ran for it.
    Accepted {
        /// The requested path.
        path: String,
        /// The job that performed the import.
        job_id: JobId,
    },
    /// The bytes already exist and the policy asked for a decision.
    Duplicate {
        /// The requested path.
        path: String,
        /// Digest of the requested bytes.
        sha256: Sha256Digest,
        /// Recording that already holds these bytes.
        existing_recording_id: RecordingId,
        /// Title of that recording.
        existing_title: String,
    },
    /// The bytes already exist and nothing new was created.
    Skipped {
        /// The requested path.
        path: String,
        /// Digest of the requested bytes.
        sha256: Sha256Digest,
        /// Recording that already holds these bytes.
        existing_recording_id: RecordingId,
    },
    /// The path was rejected before anything was copied.
    Rejected {
        /// The requested path.
        path: String,
        /// Why it was rejected.
        error: ErrorEnvelope,
    },
}

/// `import_files` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportFilesResponse {
    /// One result per requested path, in request order.
    pub results: Vec<ImportOutcome>,
}

/* ------------------------------------------------------------------ export */

/// Export file format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportFormat {
    /// Plain text.
    Txt,
    /// Markdown.
    Markdown,
    /// JSON.
    Json,
    /// SubRip captions. Available only when the revision has real timestamps.
    Srt,
    /// WebVTT captions. Available only when the revision has real timestamps.
    Vtt,
}

impl ExportFormat {
    /// The `crates/export` format this DTO selects.
    #[must_use]
    pub const fn to_export(self) -> plaudon_export::ExportFormat {
        match self {
            Self::Txt => plaudon_export::ExportFormat::Txt,
            Self::Markdown => plaudon_export::ExportFormat::Markdown,
            Self::Json => plaudon_export::ExportFormat::Json,
            Self::Srt => plaudon_export::ExportFormat::Srt,
            Self::Vtt => plaudon_export::ExportFormat::Vtt,
        }
    }

    /// Stable wire string, also used for `export_events.format`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        self.to_export().as_str()
    }

    /// Whether this format needs real segment times (spec §4.6, §8.6A).
    #[must_use]
    pub const fn needs_timestamps(self) -> bool {
        matches!(self, Self::Srt | Self::Vtt)
    }
}

/// `export_recording` request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportRecordingRequest {
    /// Recording identity.
    pub recording_id: String,
    /// Revision to render.
    pub revision_id: String,
    /// Output format.
    pub format: ExportFormat,
    /// Destination chosen through the OS save dialog.
    pub destination: String,
}

/// `export_recording` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportRecordingResponse {
    /// The export job.
    pub job_id: JobId,
}

/* -------------------------------------------------- audio export (J1.3) */

/// `export_media` request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportMediaRequest {
    /// Recording whose audio to export.
    pub recording_id: String,
    /// Candidate to export. Defaults to the preferred one.
    pub media_asset_id: Option<String>,
    /// Folder chosen through the OS folder picker.
    pub destination_directory: String,
    /// Also produce a sanitized derivative, on explicit request (spec §7.8).
    #[serde(default)]
    pub include_sanitized_derivative: bool,
}

/// One file an audio export produced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportedMediaFile {
    /// File name written into the chosen folder.
    pub file_name: String,
    /// Digest of the written bytes.
    pub sha256: String,
    /// Size in bytes.
    pub byte_length: u64,
    /// Whether this is the immutable original or a produced derivative.
    pub is_original: bool,
    /// Honest, source-relative description of what the file is.
    pub label: String,
    /// Metadata chunks removed, for a sanitized derivative.
    pub stripped_metadata: Vec<String>,
}

/// `export_media` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportMediaResponse {
    /// The exported original.
    pub original: ExportedMediaFile,
    /// The sanitized derivative, when one was requested and possible.
    pub sanitized: Option<ExportedMediaFile>,
    /// Why no derivative was produced, when one was requested.
    pub sanitized_refusal: Option<String>,
}

/* ------------------------------------------------------ deletion (J1.4) */

/// One media blob a deletion will or will not remove.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeletionMediaDto {
    /// Digest of the stored bytes.
    pub sha256: String,
    /// Size in bytes.
    pub byte_length: u64,
    /// Honest fidelity label.
    pub quality_label: String,
    /// Whether another recording also references these exact bytes.
    pub shared_with_other_recordings: bool,
    /// Whether the bytes themselves will be removed from disk.
    pub bytes_will_be_removed: bool,
}

/// One category of rows a deletion will remove.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeletionCategoryDto {
    /// User-facing category name.
    pub label: String,
    /// How many rows will be removed.
    pub count: u64,
}

/// `get_deletion_disclosure` response: exactly what deletion will remove.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeletionDisclosureDto {
    /// Disclosure contract version.
    pub disclosure_version: String,
    /// Recording identity.
    pub recording_id: String,
    /// Title as currently stored.
    pub title: String,
    /// Whether a user note will be removed.
    pub has_annotation: bool,
    /// Audio that will be removed, or kept because it is shared.
    pub media: Vec<DeletionMediaDto>,
    /// Row counts by category, in display order.
    pub categories: Vec<DeletionCategoryDto>,
    /// Bytes that will actually be freed.
    pub bytes_to_free: u64,
    /// What this deletion cannot reach.
    pub limits: Vec<String>,
    /// Digest the confirmation must carry back.
    pub confirmation_token: String,
}

/// `get_deletion_disclosure` request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeletionDisclosureRequest {
    /// Recording identity.
    pub recording_id: String,
}

/// `delete_recording` request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeleteRecordingRequest {
    /// Recording identity.
    pub recording_id: String,
    /// The `confirmation_token` from the disclosure the user saw.
    pub confirmation_token: String,
}

/// `delete_recording` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeleteRecordingResponse {
    /// Journal identity of the deletion.
    pub deletion_id: String,
    /// Blobs unlinked from the archive.
    pub blobs_removed: u64,
    /// Blobs kept because another recording references them.
    pub blobs_retained_shared: u64,
    /// Bytes freed.
    pub bytes_freed: u64,
    /// Whether a durable tombstone exists.
    pub tombstoned: bool,
}

/* ----------------------------------------------- portable archive (J2.x) */

/// `create_portable_backup` request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateBackupRequest {
    /// Folder chosen through the OS folder picker.
    pub destination_directory: String,
    /// Password. Absent means a plaintext archive, which the UI must disclose.
    pub password: Option<String>,
    /// Use the heavier Argon2id profile.
    #[serde(default)]
    pub sensitive_profile: bool,
}

/// `create_portable_backup` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateBackupResponse {
    /// File name written into the chosen folder.
    pub file_name: String,
    /// Whether the body is encrypted.
    pub encrypted: bool,
    /// The sentence the UI must show about protection (spec §13.6).
    pub protection_disclosure: String,
    /// Number of entries, excluding the manifest.
    pub entry_count: u64,
    /// Sum of entry byte lengths.
    pub total_byte_length: u64,
    /// Archive file size.
    pub file_byte_length: u64,
    /// Manifest integrity digest.
    pub integrity_digest: String,
    /// What the digest proves, and what it does not.
    pub integrity_note: String,
    /// Manifest schema version.
    pub manifest_schema_version: String,
    /// Settings that do not travel with the archive.
    pub nonportable_settings: Vec<String>,
}

/// `inspect_restore_archive` request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InspectArchiveRequest {
    /// Archive chosen through the OS open dialog.
    pub archive_path: String,
    /// Password, when the archive is encrypted.
    pub password: Option<String>,
}

/// One restore conflict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestoreConflictDto {
    /// Archive entry path.
    pub entry_path: String,
    /// Why it would be skipped.
    pub reason: String,
}

/// `inspect_restore_archive` response: a dry run that writes nothing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InspectArchiveResponse {
    /// Whether the body is encrypted.
    pub encrypted: bool,
    /// Manifest schema version.
    pub manifest_schema_version: String,
    /// Container format version.
    pub format_version: u32,
    /// When the archive was produced.
    pub created_at: String,
    /// Operating system the archive was produced on.
    pub source_os: String,
    /// Entry counts by namespace.
    pub entries_by_namespace: Vec<DeletionCategoryDto>,
    /// Sum of entry byte lengths.
    pub total_byte_length: u64,
    /// Whether every payload digest matched the manifest.
    pub payloads_verified: bool,
    /// Manifest integrity digest.
    pub integrity_digest: String,
    /// What the digest proves, and what it does not.
    pub integrity_note: String,
    /// Settings that did not travel with the archive.
    pub nonportable_settings: Vec<String>,
    /// Records that already exist in this library.
    pub conflicts: Vec<RestoreConflictDto>,
    /// Whether this library currently has no recordings.
    pub target_is_empty: bool,
}

/// How a restore should treat the target library.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RestoreModeDto {
    /// Refuse unless the library has no recordings.
    NewLibrary,
    /// Add to this library, reporting conflicts.
    Merge,
}

/// What a restore should do about an existing record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictPolicyDto {
    /// Keep the library's version and report the conflict.
    Skip,
    /// Abort the whole restore without changing the library.
    Fail,
}

/// `restore_portable_archive` request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestoreArchiveRequest {
    /// Archive chosen through the OS open dialog.
    pub archive_path: String,
    /// Password, when the archive is encrypted.
    pub password: Option<String>,
    /// New library or merge.
    pub mode: RestoreModeDto,
    /// What to do about existing records.
    pub on_conflict: ConflictPolicyDto,
    /// Whether a recording the user deleted locally may be restored.
    #[serde(default)]
    pub restore_tombstoned: bool,
}

/// `restore_portable_archive` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestoreArchiveResponse {
    /// Recordings inserted.
    pub recordings_restored: u64,
    /// Transcript revisions inserted.
    pub transcripts_restored: u64,
    /// Generated outputs inserted.
    pub outputs_restored: u64,
    /// Sources inserted.
    pub sources_restored: u64,
    /// Media blobs committed into the archive.
    pub media_committed: u64,
    /// Media blobs already present and reused.
    pub media_reused: u64,
    /// Records that were not applied.
    pub conflicts: Vec<RestoreConflictDto>,
}

/* ----------------------------------------------------- settings / activity */

/// Notification category (spec §5.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationCategory {
    /// An import finished.
    ImportFinished,
    /// A transcription finished.
    TranscriptionFinished,
    /// Something needs the user.
    RequiresAttention,
    /// The cloud budget was exhausted.
    CloudBudgetExceeded,
}

impl NotificationCategory {
    /// Every category.
    pub const ALL: &'static [Self] = &[
        Self::ImportFinished,
        Self::TranscriptionFinished,
        Self::RequiresAttention,
        Self::CloudBudgetExceeded,
    ];

    /// Stable wire string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ImportFinished => "import_finished",
            Self::TranscriptionFinished => "transcription_finished",
            Self::RequiresAttention => "requires_attention",
            Self::CloudBudgetExceeded => "cloud_budget_exceeded",
        }
    }

    /// Parse a stored or requested category string.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|category| category.as_str() == value)
    }
}

/// Notification preferences; categories are opt-in individually.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotificationSettings {
    /// Master switch.
    pub enabled: bool,
    /// Per-category switches; every category is always present.
    pub categories: BTreeMap<NotificationCategory, bool>,
}

/// `set_notification_settings` request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetNotificationSettingsRequest {
    /// Requested settings.
    pub settings: NotificationSettings,
}

/// What the operating system permits right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OsNotificationPermission {
    /// Notifications may be delivered.
    Granted,
    /// The user refused notifications.
    Denied,
    /// Permission has never been requested.
    NotRequested,
    /// This platform has no notification permission concept.
    Unsupported,
}

/// `set_notification_settings` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotificationSettingsResponse {
    /// Stored settings.
    pub settings: NotificationSettings,
    /// Current operating-system permission.
    pub os_permission: OsNotificationPermission,
}

/// `set_local_only_mode` request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetLocalOnlyModeRequest {
    /// Requested mode.
    pub enabled: bool,
    /// Explicit confirmation of the policy change.
    pub confirmed: bool,
}

/// `set_local_only_mode` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalOnlyModeResponse {
    /// Effective mode.
    pub enabled: bool,
    /// Description of the enforced policy.
    pub description: String,
    /// Hosts that remain reachable.
    pub allowed_hosts: Vec<String>,
}

/// Navigation target of an activity entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityTargetKind {
    /// A recording.
    Recording,
    /// A job.
    Job,
    /// The settings screen.
    Settings,
}

/// Where an activity entry points.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityTarget {
    /// Target family.
    pub kind: ActivityTargetKind,
    /// Target identity.
    pub id: String,
}

/// One in-app activity entry (spec §5.5: kept even when notifications are off).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityEntry {
    /// Entry identity.
    pub activity_id: String,
    /// Category this entry would notify under.
    pub category: NotificationCategory,
    /// When the underlying event was committed.
    pub occurred_at: String,
    /// Short title.
    pub title: String,
    /// Privacy-safe body.
    pub body: String,
    /// Whether an OS notification was actually delivered.
    pub notification_delivered: bool,
    /// Why no notification was delivered, when none was.
    pub notification_suppressed_reason: Option<String>,
    /// Navigation target.
    pub target: Option<ActivityTarget>,
    /// Whether the user has seen the entry.
    pub read: bool,
}

/// `list_activity` request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListActivityRequest {
    /// Opaque cursor from the previous page.
    pub cursor: Option<String>,
    /// Page size.
    pub limit: u32,
}

/// `list_activity` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityPage {
    /// Rows of this page.
    pub items: Vec<ActivityEntry>,
    /// Cursor of the next page, when more rows exist.
    pub next_cursor: Option<String>,
    /// Unread entries at query time.
    pub unread_count: u64,
}

/// `complete_onboarding` request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompleteOnboardingRequest {
    /// Chosen speech-to-text engine, when the user chose one.
    pub default_stt_engine_id: Option<String>,
    /// Chosen text engine, when the user chose one.
    pub default_text_engine_id: Option<String>,
    /// Whether local-only mode should be enforced.
    pub local_only_mode: bool,
    /// Notification preferences.
    pub notifications: NotificationSettings,
}

/// `set_openrouter_key` request.
///
/// Deliberately not `Debug`, not `Serialize` and not `Clone`: the key is
/// transported once and must not reach a log line or a response.
#[allow(missing_debug_implementations)]
#[derive(Deserialize)]
pub struct SetOpenRouterKeyRequest {
    /// The key as entered by the user.
    pub key: String,
}

/* ------------------------------------------------------------------ events */

/// §15.3 event names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum EventKind {
    /// A job transitioned.
    #[serde(rename = "job.updated")]
    JobUpdated,
    /// A job failed.
    #[serde(rename = "job.failed")]
    JobFailed,
    /// A recording was committed to the archive.
    #[serde(rename = "recording.imported")]
    RecordingImported,
    /// Source-side metadata of a recording changed.
    #[serde(rename = "recording.source_updated")]
    RecordingSourceUpdated,
    /// A transcript revision was committed.
    #[serde(rename = "transcript.completed")]
    TranscriptCompleted,
    /// A transcript tool run finished.
    #[serde(rename = "tool.completed")]
    ToolCompleted,
    /// The set of available models changed.
    #[serde(rename = "model.availability_changed")]
    ModelAvailabilityChanged,
    /// An operation needs consent.
    #[serde(rename = "consent.required")]
    ConsentRequired,
    /// The library listing changed.
    #[serde(rename = "library.changed")]
    LibraryChanged,
    /// An activity entry was appended.
    #[serde(rename = "activity.added")]
    ActivityAdded,
    /// The user clicked an OS notification and the app should navigate.
    ///
    /// The only event name outside §15.3; recorded in ADR-0020 next to the
    /// two command additions.
    #[serde(rename = "ui.activation_requested")]
    ActivationRequested,
}

impl EventKind {
    /// Every event name, in the order published by `EVENT_KINDS`.
    pub const ALL: &'static [Self] = &[
        Self::JobUpdated,
        Self::JobFailed,
        Self::RecordingImported,
        Self::RecordingSourceUpdated,
        Self::TranscriptCompleted,
        Self::ToolCompleted,
        Self::ModelAvailabilityChanged,
        Self::ConsentRequired,
        Self::LibraryChanged,
        Self::ActivityAdded,
        Self::ActivationRequested,
    ];

    /// Stable wire string, which is also the Tauri event name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::JobUpdated => "job.updated",
            Self::JobFailed => "job.failed",
            Self::RecordingImported => "recording.imported",
            Self::RecordingSourceUpdated => "recording.source_updated",
            Self::TranscriptCompleted => "transcript.completed",
            Self::ToolCompleted => "tool.completed",
            Self::ModelAvailabilityChanged => "model.availability_changed",
            Self::ConsentRequired => "consent.required",
            Self::LibraryChanged => "library.changed",
            Self::ActivityAdded => "activity.added",
            Self::ActivationRequested => "ui.activation_requested",
        }
    }
}

/// An event delivered to the UI. The store, not the payload, is authoritative.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UiEvent {
    /// Strictly increasing sequence number drawn after the database commit.
    pub seq: u64,
    /// Event name.
    pub kind: EventKind,
    /// When the committed change happened.
    pub occurred_at: String,
    /// Job the event belongs to, when one exists.
    pub job_id: Option<JobId>,
    /// Recording the event belongs to, when one exists.
    pub recording_id: Option<RecordingId>,
    /// Opaque, non-authoritative hint payload.
    pub payload: serde_json::Map<String, serde_json::Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn job_states_match_the_published_list() {
        let names: Vec<&str> = JobState::ALL.iter().map(|state| state.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "queued",
                "preflight",
                "waiting_for_user",
                "waiting_for_network",
                "waiting_for_model",
                "waiting_for_source",
                "waiting_for_authorization",
                "running_local",
                "cloud_ready",
                "cloud_sending",
                "cloud_outcome_unknown",
                "verifying",
                "committing",
                "cancelling",
                "completed",
                "failed_retryable",
                "failed_terminal",
                "cancelled",
            ]
        );
        assert_eq!(JobState::ALL.len(), 18);
    }

    #[test]
    fn job_state_serialization_agrees_with_as_str() {
        for state in JobState::ALL {
            let json = serde_json::to_string(state).unwrap();
            assert_eq!(json, format!("\"{}\"", state.as_str()));
            assert_eq!(JobState::parse(state.as_str()), Some(*state));
        }
    }

    #[test]
    fn event_kinds_use_dotted_names() {
        for kind in EventKind::ALL {
            let json = serde_json::to_string(kind).unwrap();
            assert_eq!(json, format!("\"{}\"", kind.as_str()));
        }
        // The ten §15.3 names plus the ADR-0020 activation event.
        assert_eq!(EventKind::ALL.len(), 11);
    }

    #[test]
    fn unknown_values_serialize_as_null_and_are_never_defaulted() {
        let segment = TranscriptSegmentDto {
            index: 0,
            text: "hello".into(),
            start_ms: None,
            end_ms: None,
            alignment_origin: AlignmentOrigin::None,
            confidence: None,
            speaker_label: None,
            annotations: Vec::new(),
        };
        let json = serde_json::to_value(&segment).unwrap();
        assert_eq!(json["start_ms"], serde_json::Value::Null);
        assert_eq!(json["end_ms"], serde_json::Value::Null);
        assert_eq!(json["confidence"], serde_json::Value::Null);
        assert_eq!(json["speaker_label"], serde_json::Value::Null);
    }

    #[test]
    fn import_outcomes_are_internally_tagged_by_outcome() {
        let outcome = ImportOutcome::Rejected {
            path: "/tmp/x.wav".into(),
            error: plaudon_domain::error::AppError::new(
                plaudon_domain::error::AppErrorCode::UnsupportedMedia,
                "detail",
            )
            .to_envelope(
                &mut plaudon_domain::error::DiagnosticLog::new(),
                plaudon_domain::time::Timestamp::EPOCH,
            ),
        };
        let json = serde_json::to_value(&outcome).unwrap();
        assert_eq!(json["outcome"], "rejected");
        assert_eq!(json["path"], "/tmp/x.wav");
    }

    #[test]
    fn recording_detail_flattens_its_summary() {
        let summary = RecordingSummary {
            recording_id: RecordingId::new(),
            title: "t".into(),
            recorded_at: None,
            imported_at: "1970-01-01T00:00:00Z".into(),
            duration_ms: None,
            source: RecordingSourceRef {
                source_kind: SourceKind::ManualImport,
                label: "Manual import".into(),
            },
            import_status: ImportStatus::Imported,
            transcription_status: TranscriptionStatus::None,
            processing_engine: None,
            last_error: None,
            tags: Vec::new(),
            byte_length: 3,
            sha256: Sha256Digest::of(b"abc"),
        };
        let detail = RecordingDetail {
            summary,
            media: RecordingMediaInfo {
                observed_codec: "wav".into(),
                sample_rate_hz: None,
                channels: None,
                original_file_name: "a.wav".into(),
            },
            revisions: Vec::new(),
            current_revision_id: None,
            outputs: Vec::new(),
        };
        let json = serde_json::to_value(&detail).unwrap();
        assert!(
            json.get("recording_id").is_some(),
            "summary fields are flattened"
        );
        assert_eq!(json["media"]["original_file_name"], "a.wav");
        assert_eq!(json["duration_ms"], serde_json::Value::Null);
    }

    #[test]
    fn status_response_is_always_ok_true() {
        assert_eq!(
            serde_json::to_value(StatusResponse::ok()).unwrap()["ok"],
            true
        );
    }
}
