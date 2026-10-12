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
use plaudon_domain::ids::{ClaimId, JobId, OutputId, RecordingId, SpeakerId, TranscriptRevisionId};
use plaudon_domain::media::Sha256Digest;
use plaudon_domain::time::Timestamp;
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
    /// Defaults the export surfaces start from.
    ///
    /// Defaulted on load so a preferences record written before EPIC O still
    /// deserializes instead of resetting every other setting.
    #[serde(default)]
    pub export_defaults: ExportDefaults,
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
    /// Cloud consent grants on record. Revoked grants stay listed so the
    /// history is visible (spec §4.4 #6, §16.3).
    pub cloud_consents: Vec<ConsentSummary>,
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

/// `resolve_media` request.
///
/// The native player needs a file to open; the archive layout is the
/// application layer's business, so the view asks for a recording by
/// identity and never composes a path of its own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolveMediaRequest {
    /// Recording whose archived media is wanted.
    pub recording_id: String,
}

/// `resolve_media` response: where the archived bytes of one recording are.
///
/// `Debug` is written by hand and prints no path: §15.4 keeps filesystem
/// paths out of anything that can be logged, and a derived `Debug` on a
/// struct whose whole purpose is to carry one would be the easiest way to
/// break that rule by accident.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaLocation {
    /// Recording the bytes belong to, echoed back so a late answer can be
    /// matched to the recording the view is showing now.
    pub recording_id: RecordingId,
    /// Absolute path of the archived file inside the library.
    pub path: String,
    /// Content type recorded for these bytes.
    pub content_type: String,
    /// Length of the archived file.
    pub byte_length: u64,
}

impl std::fmt::Debug for MediaLocation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MediaLocation")
            .field("recording_id", &self.recording_id)
            .field("path", &"<redacted>")
            .field("content_type", &self.content_type)
            .field("byte_length", &self.byte_length)
            .finish()
    }
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
    /// Cost carried by the job (spec §10.4). Local jobs are always `unknown`.
    pub cost: CostValue,
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

/* ----------------------------------------------------------------- sources */

/// The only user-intent tokens `plaud_connect` accepts.
///
/// Spec §7.1: every launch of the vendor tool follows an explicit user action.
/// The UI states which action it is acting for; Rust maps the token onto a
/// `&'static str` it controls and rejects anything else, so the string that
/// reaches the process runner is never frontend-supplied text.
pub const PLAUD_CONNECT_INTENT: &str = "connect_plaud_account";

/// `plaud_connect` request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaudConnectRequest {
    /// The user action that triggered the request; must be
    /// [`PLAUD_CONNECT_INTENT`].
    pub user_intent: String,
}

/// `plaud_list` request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaudListRequest {
    /// Cursor returned by the previous page, or `null` for the first page.
    pub cursor: Option<String>,
}

/// `plaud_import` request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaudImportRequest {
    /// External IDs from the last listing; any other ID rejects the request.
    pub external_ids: Vec<String>,
}

/// Machine-readable lifecycle state of the cloud connector.
pub type PlaudConnectorState = plaudon_source_plaud_cli::state::ConnectorState;

/// What the user should do next; the variants are the §5.4 corrective
/// actions, and the UI maps each to a control rather than composing text.
pub type PlaudNextAction = plaudon_source_plaud_cli::diagnostics::NextAction;

/// Which truthful disconnect wording applies (spec §4.2.7).
pub type PlaudDisconnectVariant = plaudon_source_plaud_cli::diagnostics::DisconnectCopy;

/// Compatibility verdict of the detected CLI version, in one word.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlaudCompatibility {
    /// No readable version.
    Unknown,
    /// The version is in the validated compatibility table.
    Tested,
    /// The version was read but never validated.
    Untested,
}

/// Persisted connector facts (spec §7.1), without any prose.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaudConnectorStatus {
    /// Lifecycle state.
    pub state: PlaudConnectorState,
    /// Version the tool reported, when readable.
    pub detected_version: Option<String>,
    /// Whether the version is in the validated compatibility table.
    pub version_validated: bool,
    /// Where the executable was found.
    pub program_origin: Option<String>,
    /// Stable code of the last error.
    pub last_error: Option<plaudon_domain::error::AppErrorCode>,
    /// Whether the last listing may not include every recording.
    pub listing_possibly_incomplete: bool,
    /// Whether the listing is read from an on-screen table.
    pub scraping_warning: bool,
    /// When the status last changed, Unix epoch milliseconds.
    pub updated_at_ms: i64,
}

/// The diagnostics panel payload (spec §4.2, §7.5, §7.6).
///
/// `headline`, `detail` and `warnings` are written in
/// `plaudon_source_plaud_cli::diagnostics` from a stable state or code and are
/// rendered verbatim by the UI; there is no vendor text to forward.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaudDiagnostics {
    /// Lifecycle state.
    pub state: PlaudConnectorState,
    /// One-line summary.
    pub headline: String,
    /// Explanation in the application's own words.
    pub detail: String,
    /// The concrete next step.
    pub next_action: PlaudNextAction,
    /// Version the tool reported, when readable.
    pub detected_version: Option<String>,
    /// Compatibility verdict.
    pub compatibility: PlaudCompatibility,
    /// Where the executable was found.
    pub program_origin: Option<String>,
    /// Stable code of the last error, for log correlation.
    pub last_error: Option<plaudon_domain::error::AppErrorCode>,
    /// Warnings to render alongside the panel.
    pub warnings: Vec<String>,
}

/// Disconnect wording for the detected release (spec §4.2.7).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaudDisconnectCopy {
    /// Which variant applies.
    pub variant: PlaudDisconnectVariant,
    /// Button label.
    pub label: String,
    /// Explanatory body.
    pub body: String,
}

/// Response of `plaud_probe`, `plaud_connect` and the connector half of
/// `plaud_disconnect`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaudConnectorSnapshot {
    /// Persisted connector facts.
    pub status: PlaudConnectorStatus,
    /// The diagnostics panel.
    pub diagnostics: PlaudDiagnostics,
    /// The disconnect control's wording.
    pub disconnect: PlaudDisconnectCopy,
}

/// Response of `plaud_disconnect`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaudDisconnectResponse {
    /// The wording that was true for what just happened.
    pub disconnect: PlaudDisconnectCopy,
    /// The connector after disconnecting.
    pub snapshot: PlaudConnectorSnapshot,
}

/// Whether a listing page may be partial (spec §7.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlaudListingCompleteness {
    /// Pagination has been observed for this release.
    Complete,
    /// Paging was never observed; more recordings may exist.
    PossiblyIncomplete,
}

/// One cloud recording as listed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaudItemSummary {
    /// Vendor identifier; the only handle `plaud_import` accepts.
    pub external_id: String,
    /// Name the account gives the recording. Advisory metadata.
    pub display_name: String,
    /// Capture time, when reported.
    pub captured_at: Option<String>,
    /// Duration, when reported.
    pub duration_ms: Option<u64>,
    /// Declared size in bytes, when reported. Untrusted until verified.
    pub declared_byte_length: Option<u64>,
    /// The library recording that already holds this item, if imported.
    pub imported_recording_id: Option<RecordingId>,
}

/// Response of `plaud_list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaudListResponse {
    /// The library source row for this connector; the handle `source_sync` and
    /// `source_reconcile` accept.
    pub source_id: String,
    /// Recordings on this page.
    pub items: Vec<PlaudItemSummary>,
    /// Cursor of the next page, or `null`.
    pub next_cursor: Option<String>,
    /// Completeness disclosure for the listing.
    pub completeness: PlaudListingCompleteness,
    /// Shown when the account looks empty. Not an error (spec §7.5).
    pub empty_account_advisory: Option<String>,
}

/// Per-ID result of `plaud_import`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum PlaudImportOutcome {
    /// The recording was downloaded, verified and committed.
    Imported {
        /// The listed identifier.
        external_id: String,
        /// The import job.
        job_id: JobId,
        /// The recording now in the library.
        recording_id: RecordingId,
    },
    /// The recording was already in the library; nothing changed.
    AlreadyImported {
        /// The listed identifier.
        external_id: String,
        /// The existing recording.
        recording_id: RecordingId,
    },
    /// The import failed; the job holds the same code.
    Failed {
        /// The listed identifier.
        external_id: String,
        /// The job that recorded the failure.
        job_id: JobId,
        /// §15.4 envelope with a stable code and no vendor text.
        error: ErrorEnvelope,
    },
}

/// Response of `plaud_import`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaudImportResponse {
    /// One outcome per requested ID, in request order.
    pub results: Vec<PlaudImportOutcome>,
}

/// What a watched folder does with the audio it imports (spec §4.3(7)).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WatchedFolderRule {
    /// Import the audio and stop there.
    ImportOnly,
    /// Import and queue local transcription; no egress.
    ImportAndQueueLocalStt,
    /// Import, and refuse every background cloud dispatch without consent.
    AskBeforeCloud,
}

impl WatchedFolderRule {
    /// Stable wire string, equal to the stored column value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ImportOnly => "import_only",
            Self::ImportAndQueueLocalStt => "import_and_queue_local_stt",
            Self::AskBeforeCloud => "ask_before_cloud",
        }
    }
}

/* ------------------------------------------------- cloud (OpenRouter BYOK) */

/// Exact decimal money (spec §10.4). The amount is a plain decimal string.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MoneyAmount {
    /// Decimal digits, never a float.
    pub amount: String,
    /// ISO-4217 code, uppercase.
    pub currency: String,
}

/// How trustworthy a cost is (spec §10.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CostConfidence {
    /// No cost could be derived. Never rendered as zero.
    Unknown,
    /// A pre-dispatch or usage-derived estimate.
    Estimated,
    /// A provider-reported amount.
    Finalized,
}

/// A cost and its confidence. `amount` is `null` exactly when `unknown`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CostValue {
    /// Confidence class.
    pub confidence: CostConfidence,
    /// Amount, absent when unknown.
    pub amount: Option<MoneyAmount>,
}

impl CostValue {
    /// No derivable cost.
    #[must_use]
    pub const fn unknown() -> Self {
        Self {
            confidence: CostConfidence::Unknown,
            amount: None,
        }
    }
}

/// `source_add_folder` request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceAddFolderRequest {
    /// Folder obtained from the OS directory picker.
    pub path: String,
    /// Whether subdirectories are included.
    pub recursive: bool,
    /// What the folder may do with imported audio.
    pub rule: WatchedFolderRule,
}

/// `source_add_folder` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceAddFolderResponse {
    /// The watched folder's source ID.
    pub source_id: String,
    /// Rule now in force.
    pub rule: WatchedFolderRule,
    /// Whether subdirectories are included.
    pub recursive: bool,
}

/// Why a sync was started.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncTrigger {
    /// The user pressed a sync control.
    Manual,
    /// The periodic scheduler (watched folders only; the PLAUD cloud never
    /// syncs unattended, spec §7.1).
    Background,
}

/// `source_sync` request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceSyncRequest {
    /// A watched folder ID or the `source_id` from `plaud_list`.
    pub source_id: String,
    /// Why the sync runs.
    pub trigger: SyncTrigger,
}

/// `source_reconcile` request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceReconcileRequest {
    /// A watched folder ID or the `source_id` from `plaud_list`.
    pub source_id: String,
}

/// What kind of source a sync or reconcile ran against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncedSourceKind {
    /// A watched folder.
    WatchedFolder,
    /// The PLAUD cloud connector.
    PlaudCloud,
}

/// Counts from one watched-folder pass.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FolderSyncCounts {
    /// Entries the bounded sweep inspected.
    pub scanned: u32,
    /// Whether the sweep saw the whole folder.
    pub inventory_complete: bool,
    /// Recordings newly committed.
    pub imported: u32,
    /// Files already in the library.
    pub already_imported: u32,
    /// Files still changing or not yet local.
    pub deferred: u32,
    /// Files the user deleted locally, not resurrected.
    pub refused_tombstoned: u32,
    /// Files that failed validation.
    pub rejected: u32,
    /// Known files that are no longer present (marked unavailable).
    pub vanished: u32,
}

/// `source_sync` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceSyncResponse {
    /// The job that recorded the sync.
    pub job_id: JobId,
    /// The synced source.
    pub source_id: String,
    /// Kind of source.
    pub source_kind: SyncedSourceKind,
    /// Terminal state of the job.
    pub state: JobState,
    /// Folder counts; `null` for the PLAUD cloud.
    pub folder: Option<FolderSyncCounts>,
    /// Recordings the PLAUD listing returned; `null` for a folder.
    pub listed: Option<u32>,
    /// Recordings the sync committed to the library.
    pub imported_recording_ids: Vec<RecordingId>,
}

/// One reconciliation decision (spec §7.5, App. A.2), with stable names only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReconcileDecisionSummary {
    /// Vendor identifier of the item, when it has one.
    pub external_id: Option<String>,
    /// App. A.2 row, snake_case.
    pub event: String,
    /// Actions taken, snake_case.
    pub actions: Vec<String>,
}

/// `source_reconcile` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceReconcileResponse {
    /// The job that recorded the pass.
    pub job_id: JobId,
    /// The reconciled source.
    pub source_id: String,
    /// Kind of source.
    pub source_kind: SyncedSourceKind,
    /// Decisions for a PLAUD listing; empty for a folder, and empty for a
    /// listing that may be partial (nothing is recorded in that case).
    pub decisions: Vec<ReconcileDecisionSummary>,
    /// Whether the PLAUD listing was known to be complete; `null` for a
    /// folder. A partial listing cannot prove an item is gone, so it is not
    /// reconciled (spec §7.5).
    pub listing_complete: Option<bool>,
    /// Folder counts; `null` for the PLAUD cloud.
    pub folder: Option<FolderSyncCounts>,
}

/// `list_source_artifacts` request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListSourceArtifactsRequest {
    /// Recording whose vendor exports are listed.
    pub recording_id: String,
}

/// One vendor export found alongside a recording, with its provenance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceArtifactSummary {
    /// Row identity.
    pub artifact_id: String,
    /// `transcript`, `summary` or `tags`.
    pub kind: String,
    /// Vendor that produced it, for example `plaud`.
    pub source_vendor: String,
    /// Digest of the original exported bytes.
    pub content_sha256: String,
    /// Length of those bytes.
    pub byte_length: u64,
    /// Name the export used. Metadata only.
    pub original_filename: Option<String>,
    /// When it became a transcript revision; `null` while only recorded.
    pub imported_at: Option<String>,
    /// Mode that applied when it was seen.
    pub import_mode: String,
    /// When the app first recorded it.
    pub recorded_at: String,
}

/// `list_source_artifacts` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListSourceArtifactsResponse {
    /// Vendor exports, oldest first.
    pub artifacts: Vec<SourceArtifactSummary>,
}

/* ------------------------------------------------------------------ search */

/// Request for `search_library` (spec §3.1, §9.8, §17.5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchLibraryRequest {
    /// User query; tokenized and quoted before FTS5.
    pub query: String,
    /// Pagination cursor.
    pub cursor: Option<String>,
    /// Maximum results per page.
    pub limit: u32,
}

/// One search hit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchResultDto {
    /// Recording the hit belongs to.
    pub recording_id: RecordingId,
    /// Title of that recording.
    pub recording_title: String,
    /// Revision containing the match.
    pub transcript_revision_id: TranscriptRevisionId,
    /// Segment position.
    pub seq_index: u32,
    /// Excerpt around the match.
    pub excerpt: String,
    /// Seek position; `None` for text-only transcripts (spec §8.6A).
    pub seek_ms: Option<u64>,
    /// Alignment origin for the seek affordance label.
    pub alignment_origin: AlignmentOrigin,
}

/// Response for `search_library`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchLibraryResponse {
    /// Results for this page.
    pub results: Vec<SearchResultDto>,
    /// Cursor for the next page; `None` when exhausted.
    pub next_cursor: Option<String>,
}

/* ---------------------------------------------------------------- speakers */

/// Request for `list_speakers` (spec §8.7, §17.6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListSpeakersRequest {
    /// Recording identity.
    pub recording_id: String,
}

/// One speaker row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpeakerDto {
    /// Row identity.
    pub speaker_id: SpeakerId,
    /// Recording.
    pub recording_id: RecordingId,
    /// Current label.
    pub label: String,
    /// Whether confirmed as identity.
    pub confirmed: bool,
    /// Machine-assigned label.
    pub original_label: String,
    /// When renamed.
    pub renamed_at: Option<Timestamp>,
    /// When confirmed.
    pub confirmed_at: Option<Timestamp>,
}

/// Response for `list_speakers`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListSpeakersResponse {
    /// All speakers for the recording.
    pub speakers: Vec<SpeakerDto>,
}

/// Request for `rename_speaker`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenameSpeakerRequest {
    /// Recording identity.
    pub recording_id: String,
    /// Speaker identity.
    pub speaker_id: String,
    /// New name.
    pub name: String,
}

/// Request for `confirm_speaker`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfirmSpeakerRequest {
    /// Recording identity.
    pub recording_id: String,
    /// Speaker identity.
    pub speaker_id: String,
}

/// Request for `reset_speaker`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResetSpeakerRequest {
    /// Recording identity.
    pub recording_id: String,
    /// Speaker identity.
    pub speaker_id: String,
}

/// Response for speaker mutation operations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpeakerOperationResponse {
    /// Updated speaker.
    pub speaker: SpeakerDto,
}
/// Three-valued capability flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TriState {
    /// Observed available.
    Yes,
    /// Observed unavailable.
    No,
    /// Not observed.
    Unknown,
}

/// Observed features of a model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelFeatures {
    /// Segment offsets.
    pub segments: TriState,
    /// Word offsets.
    pub word_timestamps: TriState,
    /// Speaker turns.
    pub diarization: TriState,
    /// Language detection.
    pub language_detection: TriState,
}

/// Pricing of a model as the catalog reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelPricing {
    /// Basis: `none`, `per_audio_second`, `per_token`, `per_request`, `other`.
    pub basis: String,
    /// Unit text; never invented by the UI.
    pub unit_label: String,
    /// Price per unit; `null` means unknown, never zero.
    pub amount_per_unit: Option<MoneyAmount>,
}

/// Resolved destination and retention posture of a model route.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelPrivacy {
    /// Relay the request passes through.
    pub relay: String,
    /// Downstream provider, `null` when undisclosed.
    pub downstream_provider: Option<String>,
    /// `zero_retention`, `limited_retention`, `provider_default` or `unknown`.
    pub retention: String,
    /// Days, for `limited_retention` only.
    pub retention_days: Option<u32>,
    /// Whether the resolved route documents zero retention.
    pub zero_retention_available: bool,
}

/// One model in the picker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelCard {
    /// Provider-scoped identifier.
    pub model_id: String,
    /// Display name.
    pub display_name: String,
    /// Engine id.
    pub engine: String,
    /// `multipart_stt`, `audio_chat`, `unsupported` or `unknown`.
    pub request_family: String,
    /// `verified` or `not_verified`.
    pub verification: String,
    /// Stable reason when not verified.
    pub verification_reason: Option<String>,
    /// Evidence backing `verified`.
    pub fixture_id: Option<String>,
    /// RFC 3339 instant of the evidence.
    pub validated_at: Option<String>,
    /// Observed features.
    pub features: ModelFeatures,
    /// Declared locales, `null` when unknown.
    pub locales: Option<Vec<String>>,
    /// Largest accepted payload.
    pub max_input_bytes: Option<u64>,
    /// Longest accepted media.
    pub max_duration_ms: Option<u64>,
    /// Pricing.
    pub pricing: ModelPricing,
    /// Privacy route.
    pub privacy: ModelPrivacy,
    /// Whether the model may be dispatched to right now.
    pub selectable: bool,
}

/// `list_models` request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListModelsRequest {
    /// Pipeline stage.
    pub stage: String,
    /// Engine filter; `null` lists every engine.
    pub engine: Option<String>,
}

/// `list_models` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListModelsResponse {
    /// Cards.
    pub models: Vec<ModelCard>,
    /// `live` or `pinned_allowlist`.
    pub catalog_source: String,
    /// When the live catalog was fetched.
    pub catalog_fetched_at: Option<String>,
    /// `offline`, `local_only_mode`, `no_key`, `catalog_error` or `null`.
    pub degraded_reason: Option<String>,
    /// Default model of the stage.
    pub default_model_id: Option<String>,
}

/// Spend totals; unknown attempts are counted, never zeroed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageTotals {
    /// Provider-reported spend.
    pub finalized: Option<MoneyAmount>,
    /// Estimated spend.
    pub estimated: Option<MoneyAmount>,
    /// Attempts without a derivable cost.
    pub unknown_count: u32,
}

/// Spend of one model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageByModel {
    /// Model.
    pub model_id: String,
    /// Attempts.
    pub attempts: u32,
    /// Provider-reported spend.
    pub finalized: Option<MoneyAmount>,
    /// Estimated spend.
    pub estimated: Option<MoneyAmount>,
    /// Attempts without a derivable cost.
    pub unknown_count: u32,
}

/// One ledger entry, without payload or secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageAttempt {
    /// Attempt id.
    pub attempt_id: String,
    /// Owning job.
    pub job_id: String,
    /// Model.
    pub model_id: String,
    /// Request family.
    pub request_family: String,
    /// Dispatch state.
    pub dispatch_state: String,
    /// Cost.
    pub cost: CostValue,
    /// Vendor request id.
    pub provider_request_id: Option<String>,
    /// Creation instant.
    pub created_at: String,
    /// Response instant.
    pub responded_at: Option<String>,
    /// Whether the attempt may already have been billed.
    pub possibly_billed: bool,
}

/// The local spending budget and the month's spend so far.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloudBudget {
    /// Per-job limit.
    pub per_job_limit: Option<MoneyAmount>,
    /// Monthly limit.
    pub monthly_limit: Option<MoneyAmount>,
    /// Finalized plus estimated spend this month.
    pub month_to_date: CostValue,
    /// Whether a limit is calculable here and therefore enforced.
    pub blocks_dispatch: bool,
    /// Fixed text: local budgets cannot prevent upstream charges.
    pub enforcement_note: String,
}

/// `get_usage` request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetUsageRequest {
    /// RFC 3339, inclusive.
    pub from: String,
    /// RFC 3339, exclusive.
    pub to: String,
}

/// `get_usage` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetUsageResponse {
    /// Echo of the range start.
    pub from: String,
    /// Echo of the range end.
    pub to: String,
    /// Totals.
    pub totals: UsageTotals,
    /// Per-model totals.
    pub by_model: Vec<UsageByModel>,
    /// Attempts in range.
    pub attempts: Vec<UsageAttempt>,
    /// Current budget.
    pub budget: CloudBudget,
}

/// `set_cloud_budget` request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetCloudBudgetRequest {
    /// Decimal string; `null` clears the limit.
    pub per_job_limit: Option<String>,
    /// Decimal string; `null` clears the limit.
    pub monthly_limit: Option<String>,
    /// Must be `USD`.
    pub currency: String,
}

/// Scope of a consent grant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsentScopeDto {
    /// `ask_every_time`, `recording` or `model_policy`.
    pub kind: String,
    /// Required for `recording`.
    pub recording_id: Option<String>,
}

/// `grant_cloud_consent` request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrantCloudConsentRequest {
    /// Purpose.
    pub purpose: String,
    /// Content type.
    pub content_type: String,
    /// Scope.
    pub scope: ConsentScopeDto,
    /// Model the grant covers.
    pub model_id: String,
    /// Downstream provider the user saw.
    pub downstream_provider: Option<String>,
    /// Whether documented zero retention is required.
    pub require_zero_retention: bool,
    /// Version of the privacy statement shown.
    pub disclosure_version: String,
}

/// `grant_cloud_consent` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrantCloudConsentResponse {
    /// Stable grant id.
    pub consent_id: String,
    /// Scope kind as stored.
    pub scope_kind: String,
    /// Grant instant.
    pub granted_at: String,
    /// Expiry, when one applies.
    pub expires_at: Option<String>,
}

/// A grant as listed in settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsentSummary {
    /// Stable grant id.
    pub consent_id: String,
    /// Purpose.
    pub purpose: String,
    /// Content type.
    pub content_type: String,
    /// Scope kind.
    pub scope_kind: String,
    /// Recording, for a recording scope.
    pub recording_id: Option<String>,
    /// Model.
    pub model_id: String,
    /// Downstream provider.
    pub downstream_provider: Option<String>,
    /// Retention the user required.
    pub required_retention: Option<String>,
    /// Privacy statement version.
    pub disclosure_version: String,
    /// Grant instant.
    pub granted_at: String,
    /// Expiry.
    pub expires_at: Option<String>,
    /// Revocation instant.
    pub revoked_at: Option<String>,
}

/// `revoke_cloud_consent` request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevokeCloudConsentRequest {
    /// Grant to revoke.
    pub consent_id: String,
}

/// Scope of a model preference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelPreferenceScope {
    /// `default` or `recording`.
    pub kind: String,
    /// Required for `recording`.
    pub recording_id: Option<String>,
}

/// `set_model_preference` request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetModelPreferenceRequest {
    /// Stage.
    pub stage: String,
    /// Scope.
    pub scope: ModelPreferenceScope,
    /// Engine.
    pub engine: String,
    /// Model; `null` clears a recording override.
    pub model_id: Option<String>,
}

/// `set_model_preference` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetModelPreferenceResponse {
    /// Stage.
    pub stage: String,
    /// `default` or `recording`.
    pub scope_kind: String,
    /// Recording, for a recording scope.
    pub recording_id: Option<String>,
    /// Engine.
    pub engine: String,
    /// Stored model.
    pub model_id: Option<String>,
    /// The model that would run now after precedence.
    pub effective_model: Option<ModelCard>,
}

/// One-shot model override of `start_processing`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelOverride {
    /// Engine.
    pub engine: String,
    /// Model.
    pub model_id: String,
}

/// `start_processing` request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartProcessingRequest {
    /// Recording to process.
    pub recording_id: String,
    /// Stage to run.
    pub stage: String,
    /// One-shot override; never changes stored preferences.
    pub model_override: Option<ModelOverride>,
    /// Acknowledges an unknown cost or a possibly-billed prior attempt.
    pub cost_acknowledged: bool,
    /// Re-run even when a result exists.
    pub rerun: bool,
}

/// `start_processing` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartProcessingResponse {
    /// The job that was opened.
    pub job_id: String,
    /// Pre-dispatch estimate.
    pub estimated_cost: CostValue,
    /// `queued`, `consent_required` or `blocked`.
    pub dispatch: String,
    /// Explanation, `null` when queued.
    pub blocked_reason: Option<String>,
}

/// `verify_model` request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifyModelRequest {
    /// Model to verify.
    pub model_id: String,
    /// `transcription` or `text_generation`.
    pub task: String,
    /// Acknowledges an unknown cost.
    pub cost_acknowledged: bool,
}

/// `verify_model` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifyModelResponse {
    /// The refreshed card.
    pub model: ModelCard,
    /// Cost of the smoke test.
    pub cost: CostValue,
    /// Vendor request id.
    pub provider_request_id: Option<String>,
}

/* ----------------------------------------------------------------- devices */
//
// Spec §4.2A, §5.2 "PLAUD Devices", §15.2 `device_*`, App. B.8. Every label a
// device screen shows is produced here from the compatibility register or from
// a `*_ui_label()` helper in `plaudon_source_plaud_device`; the UI renders the
// strings and never composes a compatibility claim of its own.

/// Register status of a probed device profile.
///
/// Aliased here, like [`PlaudConnectorState`], so a user interface can name
/// the type of a DTO field without depending on a source crate directly.
pub type DeviceSupportStatus = plaudon_source_plaud_device::SupportStatus;

/// How sure the application is that it is talking to the device it means.
pub type DeviceIdentityAssurance = plaudon_source_plaud_device::IdentityAssurance;

/// Ledger state of one device transfer (App. B.6).
pub type DeviceTransferState = plaudon_source_plaud_device::TransferState;

/// The App. B.8 list sections.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceSection {
    /// Advertising now; not probed or not yet linked.
    Nearby,
    /// A live session authenticated with the stored device credential.
    Connected,
    /// Probed, supported by a named profile, but no credential is stored.
    NeedsAuthorization,
    /// Probed, but outside every validated profile or needing an
    /// authentication mode this build does not implement.
    UnsupportedFirmware,
    /// Linked before and not advertising right now.
    PreviouslyLinked,
}

/// Whether the radio could be consulted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RadioState {
    /// A scan ran; an empty device list means nothing was heard.
    Scanned,
    /// The operating system denied radio access; nothing was scanned.
    PermissionDenied,
    /// This build has no BLE radio; nothing was scanned.
    UnavailableInBuild,
}

/// Radio status of a scan, so "no devices nearby" is never confused with
/// "we could not look" (spec §5.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RadioStatusDto {
    /// What happened.
    pub state: RadioState,
    /// Stable error code when the radio could not be used.
    pub code: Option<String>,
    /// Plain-language explanation.
    pub message: String,
}

/// Whether a cloud route was involved in a device operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloudRoute {
    /// No cloud route was involved: the bytes travel device → this computer.
    NotUsed,
    /// A cloud route was involved.
    Used,
}

/// One row of the Devices list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceCandidateDto {
    /// Host-assigned peripheral identifier; stable on this host only.
    pub peripheral_id: String,
    /// List section.
    pub section: DeviceSection,
    /// Local name exactly as advertised. Display only; never an identity.
    pub advertised_name: Option<String>,
    /// Signal strength in dBm, when the host reports it.
    pub rssi_dbm: Option<i16>,
    /// Transport, for example `ble`.
    pub transport: String,
    /// Model, only after a probe established it.
    pub model: Option<String>,
    /// Firmware, only after a probe established it.
    pub firmware: Option<String>,
    /// Register status of the probed profile.
    pub support_status: plaudon_source_plaud_device::SupportStatus,
    /// Register-derived label of `support_status`.
    pub status_label: String,
    /// Identity assurance.
    pub identity: plaudon_source_plaud_device::IdentityAssurance,
    /// The exact phrase for `identity`.
    pub identity_label: String,
    /// Whether a device credential is stored in the OS vault.
    pub authorized: bool,
    /// Last time the device answered this application.
    pub last_contact_at: Option<String>,
}

/// `list_nearby_devices` request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListNearbyDevicesRequest {
    /// Scan duration in milliseconds.
    pub timeout_ms: u64,
}

/// `list_nearby_devices` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceScanDto {
    /// Whether the radio was consulted.
    pub radio: RadioStatusDto,
    /// Candidates plus previously linked devices.
    pub devices: Vec<DeviceCandidateDto>,
    /// When the scan finished; `null` if no scan ran.
    pub scanned_at: Option<String>,
}

/// What the gate decided for a probed profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceGateDto {
    /// Whether a transfer is permitted (only a `Supported` register cell).
    pub permits_transfer: bool,
    /// Whether the user may opt in to a read-only experimental probe.
    pub experimental_probe_available: bool,
    /// Whether an experimental probe was approved for this device.
    pub experimental_probe_approved: bool,
    /// Stable refusal code, when the gate refused or only offered a probe.
    pub refusal_code: Option<String>,
}

/// A capability a device detail page lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceCapability {
    /// Read-only listing of recordings on the device.
    ReadOnlyListing,
    /// Read-only transfer of a listed recording.
    ReadOnlyTransfer,
    /// Automatic transfer when the device is nearby.
    AutomaticSync,
}

/// One capability and whether it is available now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceCapabilityDto {
    /// Which capability.
    pub capability: DeviceCapability,
    /// Whether it may be used now.
    pub available: bool,
    /// Why it is not available, when it is not.
    pub reason: Option<String>,
}

/// Whether automatic sync may be advertised for a device (spec §4.2A.7).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceSyncPolicyDto {
    /// Host-assigned peripheral identifier.
    pub peripheral_id: String,
    /// Whether the interface may offer the option at all.
    pub may_advertise: bool,
    /// Whether the user opted in.
    pub enabled: bool,
    /// Whether the option would start on; always `false`.
    pub default_on: bool,
    /// Stable code of the audit outcome behind `may_advertise`.
    pub audit_code: String,
    /// Plain-language reason from the audit outcome.
    pub explanation: String,
}

/// `device_probe` request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceProbeRequest {
    /// Host-assigned peripheral identifier.
    pub peripheral_id: String,
    /// The user explicitly approved a read-only experimental probe.
    pub experimental_probe_approved: bool,
}

/// `device_probe` response: evidence about one device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceProbeDto {
    /// Host-assigned peripheral identifier.
    pub peripheral_id: String,
    /// Local name exactly as advertised.
    pub advertised_name: Option<String>,
    /// Transport, for example `ble`.
    pub transport: String,
    /// Model as probed; `null` when it could not be established.
    pub model: Option<String>,
    /// Firmware as probed; `null` when it could not be verified.
    pub firmware: Option<String>,
    /// Authentication mode the device offered; `null` when unknown.
    pub auth_mode: Option<String>,
    /// Battery level; `null` when the device did not report one.
    pub battery_percent: Option<u8>,
    /// Signal strength in dBm; `null` when unknown.
    pub rssi_dbm: Option<i16>,
    /// List section.
    pub section: DeviceSection,
    /// Register status.
    pub support_status: plaudon_source_plaud_device::SupportStatus,
    /// Register-derived label of `support_status`.
    pub status_label: String,
    /// `known_limitations` of the matched register row.
    pub known_limitations: Vec<String>,
    /// The gate's decision.
    pub gate: DeviceGateDto,
    /// Identity assurance.
    pub identity: plaudon_source_plaud_device::IdentityAssurance,
    /// The exact phrase for `identity`.
    pub identity_label: String,
    /// Capabilities and their availability.
    pub capabilities: Vec<DeviceCapabilityDto>,
    /// Label of the media this source yields, for example
    /// `Device BLE · compressed ASR`.
    pub source_label: String,
    /// Whether a cloud route is involved. Direct BLE never involves one.
    pub cloud_route: CloudRoute,
    /// Radio disclosures the user must read for this profile.
    pub disclosures: Vec<String>,
    /// How a credential for this device may be obtained.
    pub provisioning_note: Option<String>,
    /// Whether a credential is stored in the OS vault.
    pub authorized: bool,
    /// Last time the device answered this application.
    pub last_contact_at: Option<String>,
    /// Whether automatic sync may be advertised.
    pub sync: DeviceSyncPolicyDto,
}

/// `device_authorize` request.
///
/// Deliberately not `Debug`, not `Serialize` and not `Clone`: the token is
/// transported once, stored in the OS vault and never returned.
#[allow(missing_debug_implementations)]
#[derive(Deserialize)]
pub struct DeviceAuthorizeRequest {
    /// Host-assigned peripheral identifier.
    pub peripheral_id: String,
    /// Token the user already holds for their own device, hex encoded.
    pub token_hex: String,
}

/// `device_authorize` response: status only, never the token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceAuthStatusDto {
    /// Host-assigned peripheral identifier.
    pub peripheral_id: String,
    /// Whether a credential is stored in the OS vault.
    pub credential_stored: bool,
    /// Identity assurance. Storing a credential does not authenticate.
    pub identity: plaudon_source_plaud_device::IdentityAssurance,
    /// The exact phrase for `identity`.
    pub identity_label: String,
}

/// Request carrying only a device identifier.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceIdRequest {
    /// Host-assigned peripheral identifier.
    pub peripheral_id: String,
}

/// How complete a device listing is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ListingCompleteness {
    /// A delta of unsynchronized files; files synced elsewhere may be absent.
    UnsyncedDelta,
    /// A delta, and the device returned fewer entries than it holds.
    UnsyncedDeltaTruncated,
    /// The completeness of the listing is unknown.
    Unknown,
}

/// Transfer state and progress of one on-device recording.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceTransferDto {
    /// Transfer identifier; `null` until a transfer was requested.
    pub transfer_id: Option<String>,
    /// Ledger state.
    pub state: plaudon_source_plaud_device::TransferState,
    /// `TransferState::ui_label`.
    pub state_label: String,
    /// Bytes received so far; `null` before any arrived.
    pub bytes_received: Option<u64>,
    /// Observed bandwidth in bytes per second; `null` until measurable.
    pub observed_bytes_per_sec: Option<u64>,
    /// An estimate of the seconds remaining; `null` when it cannot be
    /// estimated. It is never a promise.
    pub eta_seconds_estimate: Option<u64>,
    /// Whether the received bytes passed verification.
    pub verified: bool,
    /// Label of the media this transfer yields.
    pub source_label: String,
    /// Whether a cloud route was involved.
    pub cloud_route: CloudRoute,
}

/// One recording the device lists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceRecordingDto {
    /// Device-scoped recording identifier.
    pub recording_id: String,
    /// Display name the device reports. Advisory metadata only.
    pub display_name: String,
    /// Byte length the device declares. A claim, not a measurement.
    pub declared_byte_length: u64,
    /// Capture time the device reports.
    pub captured_at: Option<String>,
    /// Transfer state.
    pub transfer: DeviceTransferDto,
}

/// `device_list_recordings` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceListingDto {
    /// Host-assigned peripheral identifier.
    pub peripheral_id: String,
    /// Recordings the device lists right now.
    pub items: Vec<DeviceRecordingDto>,
    /// Number of entries the device returned.
    pub file_count: u32,
    /// Completeness qualifier.
    pub completeness: ListingCompleteness,
    /// Plain-language statement that the scan is not an exhaustive history.
    pub completeness_label: String,
    /// Identity assurance after the listing authenticated.
    pub identity: plaudon_source_plaud_device::IdentityAssurance,
    /// The exact phrase for `identity`.
    pub identity_label: String,
}

/// `device_import` request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceImportRequest {
    /// Host-assigned peripheral identifier.
    pub peripheral_id: String,
    /// Recordings from the device's own listing.
    pub recording_ids: Vec<String>,
    /// Process the result locally only.
    pub local_only: bool,
}

/// `device_import` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceImportDto {
    /// One transfer identifier per requested recording, in request order.
    pub transfer_ids: Vec<String>,
}

/// `device_disconnect` request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceDisconnectRequest {
    /// Host-assigned peripheral identifier.
    pub peripheral_id: String,
    /// Also remove the stored credential and the link.
    pub forget_authorization: bool,
}

/// `device_disconnect` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceDisconnectDto {
    /// Host-assigned peripheral identifier.
    pub peripheral_id: String,
    /// Always `true`: recordings already archived are never removed.
    pub local_archive_retained: bool,
    /// Whether the stored credential was removed.
    pub authorization_revoked: bool,
}

/// `device_sync_policy` request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceSyncPolicyRequest {
    /// Host-assigned peripheral identifier.
    pub peripheral_id: String,
    /// Opt in to read-only transfer when the device is nearby.
    pub enable_when_nearby: bool,
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
    /// Bytes of a local model install arrived (matrix row A13).
    #[serde(rename = "model.install_progress")]
    ModelInstallProgress,
    /// An operation needs consent.
    #[serde(rename = "consent.required")]
    ConsentRequired,
    /// A source needs the user to sign in again (spec §15.3).
    #[serde(rename = "source.auth_required")]
    SourceAuthRequired,
    /// A source sync, listing or import pass ended (spec §15.3).
    #[serde(rename = "source.sync_finished")]
    SourceSyncFinished,
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
    /// A device advertisement was heard (spec §15.3).
    #[serde(rename = "device.discovered")]
    DeviceDiscovered,
    /// A device session was authenticated.
    #[serde(rename = "device.connected")]
    DeviceConnected,
    /// A probe changed what is known about a device's compatibility.
    #[serde(rename = "device.compatibility_changed")]
    DeviceCompatibilityChanged,
    /// Bytes of a device transfer arrived or its state changed.
    #[serde(rename = "device.transfer_progress")]
    DeviceTransferProgress,
    /// A device transfer passed verification.
    #[serde(rename = "device.transfer_verified")]
    DeviceTransferVerified,
    /// A device session ended.
    #[serde(rename = "device.disconnected")]
    DeviceDisconnected,
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
        Self::ModelInstallProgress,
        Self::ConsentRequired,
        Self::SourceAuthRequired,
        Self::SourceSyncFinished,
        Self::LibraryChanged,
        Self::ActivityAdded,
        Self::ActivationRequested,
        Self::DeviceDiscovered,
        Self::DeviceConnected,
        Self::DeviceCompatibilityChanged,
        Self::DeviceTransferProgress,
        Self::DeviceTransferVerified,
        Self::DeviceDisconnected,
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
            Self::ModelInstallProgress => "model.install_progress",
            Self::ConsentRequired => "consent.required",
            Self::SourceAuthRequired => "source.auth_required",
            Self::SourceSyncFinished => "source.sync_finished",
            Self::LibraryChanged => "library.changed",
            Self::ActivityAdded => "activity.added",
            Self::ActivationRequested => "ui.activation_requested",
            Self::DeviceDiscovered => "device.discovered",
            Self::DeviceConnected => "device.connected",
            Self::DeviceCompatibilityChanged => "device.compatibility_changed",
            Self::DeviceTransferProgress => "device.transfer_progress",
            Self::DeviceTransferVerified => "device.transfer_verified",
            Self::DeviceDisconnected => "device.disconnected",
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

/* --------------------------------------------------------------- outputs */

/// Request for outputs of a recording.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListOutputsRequest {
    /// Recording whose outputs are requested.
    pub recording_id: String,
}

/// One anchor of a claim, with verification outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnchorDto {
    /// Zero-based segment index.
    pub seq_index: u32,
    /// Unicode character offset of the start.
    pub char_start: u32,
    /// Unicode character offset of the end.
    pub char_end: u32,
    /// Excerpt from the transcript.
    pub excerpt: String,
    /// Whether the anchor resolved to real text.
    pub verified: bool,
    /// Rejection code when verification failed.
    pub rejection_code: Option<String>,
    /// Matched text when verified.
    pub matched_text: Option<String>,
    /// Alignment origin.
    pub alignment_origin: String,
    /// Seek position in milliseconds, present only when the backend provided one.
    pub seek_ms: Option<u64>,
}

/// Per-kind rendered fields for an action item (spec 9.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionItemFields {
    /// Imperative verb phrase.
    pub verb: String,
    /// What is to be done.
    pub description: String,
    /// Owner name or the literal `"Unassigned"`.
    pub owner: String,
    /// Due date `YYYY-MM-DD` or the literal `"Not specified"`.
    pub due_date: String,
    /// Status: `open`, `in_progress`, `done`, `unknown`.
    pub status: String,
    /// Assertion: `directly_stated` or `model_interpretation`.
    pub assertion: String,
}

/// Per-kind rendered fields for a decision (spec 9.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecisionFields {
    /// The decision statement.
    pub statement: String,
    /// Participants, when evidenced.
    pub participants: Vec<String>,
    /// Time reference or the literal `"Not specified"`.
    pub time_reference: String,
    /// Assertion: `directly_stated` or `model_interpretation`.
    pub assertion: String,
}

/// Per-kind rendered fields for a risk (spec 9.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RiskFields {
    /// What the risk is.
    pub description: String,
    /// Consequence or the literal `"Not specified"`.
    pub consequence: String,
    /// Mitigation or the literal `"Not specified"`.
    pub mitigation: String,
}

/// Per-kind rendered fields for an open question (spec 9.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionFields {
    /// The question.
    pub question: String,
    /// Owner name or the literal `"Unassigned"`.
    pub owner: String,
}

/// Per-kind rendered fields for a topic heading (spec 9.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TopicFields {
    /// Heading.
    pub heading: String,
    /// First segment index covered, inclusive.
    pub first_segment: u32,
    /// Last segment index covered, inclusive.
    pub last_segment: u32,
}

/// Rendered §9.2 fields of a claim, discriminated by kind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RenderedClaimFields {
    /// Summary statement; no additional fields beyond `text`.
    SummaryStatement,
    /// Decision.
    Decision(DecisionFields),
    /// Action item.
    ActionItem(ActionItemFields),
    /// Risk.
    Risk(RiskFields),
    /// Open question.
    Question(QuestionFields),
    /// Topic heading.
    Topic(TopicFields),
}

/// One claim with its grounding and review state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimDto {
    /// Claim identity.
    pub claim_id: ClaimId,
    /// JSON Pointer into the output body.
    pub item_path: String,
    /// Claim text (display text for the reviewer).
    pub text: String,
    /// Verification state: `source_linked`, `human_confirmed`, `unverified`.
    pub state: String,
    /// Whether the claim requires human review.
    pub review_required: bool,
    /// Reasons the claim is marked for review.
    pub review_reasons: Vec<String>,
    /// Entailment check result: `supported`, `not_supported`, `not_checked`.
    pub entailment: String,
    /// Category: `general`, `financial_amount`, `obligation`, `deadline`, `safety_commitment`.
    pub category: String,
    /// Rendered §9.2 fields.
    pub fields: RenderedClaimFields,
    /// Source anchors.
    pub anchors: Vec<AnchorDto>,
}

/// Coverage of the input transcript.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoverageDto {
    /// Coverage state: `full` or `model_reported_partial`.
    pub state: String,
    /// Chunk indices when partial.
    pub chunks: Option<Vec<u32>>,
}

/// Staleness of a stored output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StaleDto {
    /// When it was marked stale, RFC 3339.
    pub stale_at: String,
    /// Why.
    pub reason: String,
}

/// One generated output header with its claims.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputDto {
    /// Output identity.
    pub output_id: OutputId,
    /// Recording the output belongs to.
    pub recording_id: RecordingId,
    /// Transcript revision it read.
    pub transcript_revision_id: TranscriptRevisionId,
    /// Output type, e.g. `meeting_summary`.
    pub output_type: String,
    /// Template identity.
    pub template_id: String,
    /// Template version.
    pub template_version: String,
    /// Pipeline version.
    pub pipeline_version: String,
    /// Lifecycle status: `complete` or `needs_review`.
    pub status: String,
    /// Coverage of the transcript.
    pub coverage: CoverageDto,
    /// Staleness, when superseded.
    pub stale: Option<StaleDto>,
    /// Creation instant, RFC 3339.
    pub created_at: String,
    /// Claims with their grounding state.
    pub claims: Vec<ClaimDto>,
}

/// Response of `list_outputs`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListOutputsResponse {
    /// The outputs of the recording, newest last.
    pub outputs: Vec<OutputDto>,
}

/// Request to review a claim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewClaimRequest {
    /// Output the claim belongs to.
    pub output_id: String,
    /// Claim being reviewed.
    pub claim_id: String,
    /// Whether the reviewer accepted the claim.
    pub accepted: bool,
    /// Optional review note.
    pub note: Option<String>,
}

/// Response of `review_claim`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewClaimResponse {
    /// Updated claim state: `human_confirmed` or `unverified`.
    pub state: String,
}

/// Request to regenerate an output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegenerateOutputRequest {
    /// Output to regenerate.
    pub output_id: String,
    /// Explicit user acknowledgement.
    pub acknowledged: bool,
}

/// Response of `regenerate_output`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegenerateOutputResponse {
    /// Job identity of the regeneration run.
    pub job_id: JobId,
}

/* ------------------------------------------- settings additions (O2.4) */

/// `set_appearance` request (matrix row C1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetAppearanceRequest {
    /// Requested appearance.
    pub appearance: Appearance,
    /// Requested contrast.
    pub contrast: Contrast,
}

/// `set_engine_preferences` request.
///
/// Both fields are explicit: `None` clears the preference, which is how a
/// user goes back to letting the application choose.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetEnginePreferencesRequest {
    /// Preferred speech-to-text engine.
    pub default_stt_engine_id: Option<String>,
    /// Preferred text engine, used for summarization.
    pub default_text_engine_id: Option<String>,
}

/// Defaults the export surfaces start from.
///
/// Stored in [`Preferences`] so the setting is configured in exactly one
/// place; the export menu seeds its format and destination from it rather
/// than hard-coding plain text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportDefaults {
    /// Format the export menu opens on.
    pub format: ExportFormat,
    /// Folder the save dialog opens in, when the user chose one.
    pub destination_directory: Option<String>,
    /// Whether speaker labels are written by default.
    pub include_speaker_labels: bool,
    /// Whether timestamps are written by default.
    pub include_timestamps: bool,
}

impl Default for ExportDefaults {
    fn default() -> Self {
        Self {
            format: ExportFormat::Markdown,
            destination_directory: None,
            include_speaker_labels: true,
            include_timestamps: true,
        }
    }
}

/// `set_export_defaults` request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetExportDefaultsRequest {
    /// The defaults to store.
    pub defaults: ExportDefaults,
}

/// `set_shortcuts` request (matrix row C9).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetShortcutsRequest {
    /// The overrides to store. Validated against `specs` before anything is
    /// written, so a conflicting map is refused rather than half-applied.
    pub bindings: crate::shortcuts::ShortcutBindings,
    /// The command registry the overrides are validated against. The shell
    /// owns the registry, so it travels with the request instead of being
    /// guessed at here.
    pub specs: Vec<crate::shortcuts::ShortcutSpec>,
}

/* ------------------------------------------------- local models (A13) */

/// How a locally installed model got here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelOriginDto {
    /// Installed from a redistributable offline pack.
    Pack,
    /// Downloaded from a networked source.
    Download,
}

/// One model present on this machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalModelDto {
    /// Model identity.
    pub model_id: String,
    /// Name shown in the manager.
    pub display_name: String,
    /// Engine that loads these weights.
    pub engine: String,
    /// Weight version.
    pub version: String,
    /// Size of the installed files.
    pub size_bytes: u64,
    /// SPDX identifier of the licence the weights ship under.
    pub licence_spdx_id: String,
    /// Where it came from.
    pub origin: ModelOriginDto,
    /// Whether it is marked "keep downloaded for offline use".
    pub keep_offline: bool,
}

/// Disk taken by the model root.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelDiskUsageDto {
    /// Bytes of installed weights.
    pub installed_bytes: u64,
    /// Bytes left behind in staging by an interrupted transfer.
    pub staging_bytes: u64,
}

/// `list_local_models` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListLocalModelsResponse {
    /// Models installed right now.
    pub models: Vec<LocalModelDto>,
    /// Disk the model root occupies.
    pub disk: ModelDiskUsageDto,
    /// Absolute path of the model root, shown as a storage fact.
    pub model_root: String,
    /// Bytes free on the volume holding the model root, when it could be
    /// measured.
    pub available_disk_bytes: Option<u64>,
    /// Physical memory this machine reports, when it could be measured.
    pub total_ram_bytes: Option<u64>,
    /// Where models come from in this build, stated so the absence of a
    /// download button is explained rather than mysterious.
    pub source_note: String,
}

/// `inspect_model_pack` request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InspectModelPackRequest {
    /// Pack directory chosen through the OS folder picker.
    pub pack_path: String,
}

/// One model offered by a pack, with everything the approval gate needs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelOfferDto {
    /// The licence and footprint gate.
    pub gate: crate::settings_presentation::ModelInstallGate,
    /// Whether this machine already has it.
    pub already_installed: bool,
    /// Languages the model states it covers. Empty means "not stated", which
    /// the view shows as unknown rather than as "all".
    pub languages: Vec<String>,
    /// Engine that loads these weights.
    pub engine: String,
    /// Weight version.
    pub version: String,
}

/// `inspect_model_pack` response: a dry run that installs nothing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InspectModelPackResponse {
    /// Pack that was inspected.
    pub pack_path: String,
    /// Digest of the pack manifest, so two inspections can be compared.
    pub artifact_digest: String,
    /// Pack schema version.
    pub schema_version: u32,
    /// Models the pack offers.
    pub offers: Vec<ModelOfferDto>,
    /// Bytes free on the volume holding the model root, when measurable.
    pub available_disk_bytes: Option<u64>,
    /// Physical memory this machine reports, when measurable.
    pub total_ram_bytes: Option<u64>,
}

/// `install_model_from_pack` request.
///
/// The approval flags are not decoration: the command refuses without them,
/// so an install cannot happen because a view forgot to show the disclosure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallModelFromPackRequest {
    /// Pack directory the model comes from.
    pub pack_path: String,
    /// Model to install.
    pub model_id: String,
    /// The user accepted the model's licence, named by its SPDX identifier.
    pub licence_accepted_spdx_id: String,
    /// The user acknowledged the disk and memory footprint.
    pub footprint_acknowledged: bool,
    /// Mark the model "keep downloaded for offline use".
    #[serde(default)]
    pub keep_offline: bool,
}

/// `install_model_from_pack` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallModelResponse {
    /// The installed model.
    pub model: LocalModelDto,
    /// Bytes written by this attempt.
    pub transferred_bytes: u64,
    /// Disk the model root now occupies.
    pub disk: ModelDiskUsageDto,
}

/// `remove_local_model` request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoveLocalModelRequest {
    /// Model to delete.
    pub model_id: String,
    /// Explicit confirmation; without it nothing is deleted.
    pub confirmed: bool,
}

/// `remove_local_model` response.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoveLocalModelResponse {
    /// Bytes freed.
    pub bytes_freed: u64,
    /// Disk the model root now occupies.
    pub disk: ModelDiskUsageDto,
}

/// `verify_local_model` request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifyLocalModelRequest {
    /// Model to re-hash.
    pub model_id: String,
}

/// `verify_local_model` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifyLocalModelResponse {
    /// Model that was checked.
    pub model_id: String,
    /// Whether the bytes on disk still match the recorded digest and size.
    pub intact: bool,
    /// Observed size.
    pub observed_bytes: u64,
    /// Recorded size.
    pub expected_bytes: u64,
    /// What was found, in the user's terms.
    pub detail: String,
}

/* ------------------------------------------------- transcript tools (A11) */

/// One registered transcript tool at one version (spec §9.7).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptToolDto {
    /// Stable tool identity.
    pub tool_id: String,
    /// Version of this definition. A definition is immutable; an edit appends.
    pub version: u32,
    /// What the tool produces, as a stable wire string.
    pub kind: String,
    /// Name shown to the user.
    pub display_name: String,
    /// The task instruction the tool dispatches. Transcript text is never part
    /// of it (spec §9.6).
    pub instruction: String,
    /// Version label of the instruction, recorded on every run.
    pub prompt_version: String,
    /// Version label of the output schema, recorded on every run.
    pub output_schema_version: String,
    /// Where the definition came from: `built_in` or `user_edit`.
    pub origin: String,
    /// Whether the tool is outside the default meeting product (spec §9.2).
    pub optional: bool,
    /// Whether the resolved text model satisfies this tool's requirements.
    pub offerable: bool,
    /// Why it is not offerable, when it is not. `None` when it is.
    pub unavailable_reason: Option<String>,
}

/// The text model a tool run would use, as resolved right now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextRouteDto {
    /// Engine identifier.
    pub engine: String,
    /// Model identifier, absent when the engine names no model.
    pub model_id: Option<String>,
    /// Whether running this route sends content off the device.
    pub is_cloud: bool,
    /// Whether the route can run at all right now.
    pub available: bool,
    /// Why it cannot, when it cannot.
    pub unavailable_reason: Option<String>,
}

/// `list_transcript_tools` request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListTranscriptToolsRequest {
    /// Recording whose per-recording model override applies, when one exists.
    pub recording_id: Option<String>,
}

/// `list_transcript_tools` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListTranscriptToolsResponse {
    /// Every tool at its highest version.
    pub tools: Vec<TranscriptToolDto>,
    /// The text route these tools would run on, when one resolves.
    pub text_model: Option<TextRouteDto>,
}

/// An anchor the model produced that did not verify against the revision.
///
/// Surfaced rather than hidden: a grounding failure the user cannot see is a
/// grounding failure that looks like success (spec §9.6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RejectedAnchorDto {
    /// Zero-based segment index the anchor claimed.
    pub seq_index: u32,
    /// Claimed start offset, in Unicode scalar values.
    pub char_start: u32,
    /// Claimed end offset, in Unicode scalar values.
    pub char_end: u32,
    /// The excerpt the model quoted.
    pub excerpt: String,
    /// Stable rejection discriminator, e.g. `excerpt_mismatch`.
    pub rejection_code: String,
}

/// `ask_recording` request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AskRecordingRequest {
    /// Recording the question is about.
    pub recording_id: String,
    /// The user's question.
    pub question: String,
    /// Transcript revision to answer from; the head revision when absent.
    pub revision_id: Option<String>,
    /// Per-run model override, with the same meaning as `start_processing`'s.
    pub model_override: Option<ModelOverride>,
    /// Explicit acknowledgement that the charge cannot be estimated (§10.4).
    pub cost_acknowledged: bool,
}

/// `ask_recording` response.
///
/// The three outcomes of spec §9.8 are data, not errors: a question the
/// recording cannot answer is a legitimate answer the user must be able to
/// read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AskRecordingResponse {
    /// Conversation the exchange was recorded in.
    pub conversation_id: String,
    /// Revision the answer is pinned to.
    pub transcript_revision_id: String,
    /// `answered`, `insufficient_evidence` or `not_supported_by_recording`.
    pub outcome: String,
    /// The answer text.
    pub text: String,
    /// Anchors that verified against the pinned revision.
    pub anchors: Vec<AnchorDto>,
    /// Anchors that did not verify, with the reason.
    pub rejected_anchors: Vec<RejectedAnchorDto>,
    /// Segment indices that were retrieved and dispatched.
    pub retrieved_segments: Vec<u32>,
    /// Whether anything left this device for this question.
    pub dispatched: bool,
    /// The run that produced the answer, when a model was dispatched to.
    pub tool_run_id: Option<String>,
    /// Tool identity that answered.
    pub tool_id: String,
    /// Tool version that answered.
    pub tool_version: u32,
}

/// `list_conversations` request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListConversationsRequest {
    /// Recording whose conversations are requested.
    pub recording_id: String,
}

/// One recorded turn of a conversation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationTurnDto {
    /// Message identity.
    pub message_id: String,
    /// Position in the conversation.
    pub seq_index: u32,
    /// `user` or `assistant`.
    pub role: String,
    /// Message text.
    pub text: String,
    /// Outcome of an assistant turn; absent on a user turn.
    pub outcome: Option<String>,
    /// Verified anchors of an assistant turn.
    pub anchors: Vec<AnchorDto>,
    /// Run that produced an assistant turn, when one was dispatched.
    pub tool_run_id: Option<String>,
    /// When the turn was recorded.
    pub created_at: String,
}

/// One per-recording conversation, pinned to one transcript revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationDto {
    /// Conversation identity.
    pub conversation_id: String,
    /// Recording it belongs to.
    pub recording_id: String,
    /// Revision every answer in it was produced from.
    pub transcript_revision_id: String,
    /// Whether that revision is still the head revision.
    pub revision_is_current: bool,
    /// When the conversation was opened.
    pub created_at: String,
    /// Turns in order.
    pub turns: Vec<ConversationTurnDto>,
}

/// `list_conversations` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListConversationsResponse {
    /// Conversations, newest first.
    pub conversations: Vec<ConversationDto>,
}

/// One flashcard of a study artifact set (spec §9.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlashcardDto {
    /// Prompt side.
    pub front: String,
    /// Answer side.
    pub back: String,
    /// Citations into the pinned revision.
    pub anchors: Vec<AnchorDto>,
}

/// One quiz item with its answer key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuizItemDto {
    /// The question.
    pub question: String,
    /// Offered answers.
    pub options: Vec<String>,
    /// Zero-based index of the correct option.
    pub answer_index: u32,
    /// Citations into the pinned revision.
    pub anchors: Vec<AnchorDto>,
}

/// One term and the definition the recording gives for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DefinitionDto {
    /// The term.
    pub term: String,
    /// The definition as the recording states it.
    pub definition: String,
    /// Citations into the pinned revision.
    pub anchors: Vec<AnchorDto>,
}

/// A validated set of study artifacts.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StudyArtifactsDto {
    /// Flashcards.
    pub flashcards: Vec<FlashcardDto>,
    /// Quiz items.
    pub quiz: Vec<QuizItemDto>,
    /// Definitions.
    pub definitions: Vec<DefinitionDto>,
}

/// An item that was refused, named by its JSON Pointer path.
///
/// Spec §9.5 refuses a malformed item rather than dropping it quietly, so the
/// path and the reason travel to the interface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RejectedStudyItemDto {
    /// JSON Pointer of the refused item.
    pub path: String,
    /// Stable reason discriminator.
    pub reason: String,
    /// What was wrong, in the user's terms.
    pub detail: String,
}

/// `generate_study_artifacts` request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GenerateStudyArtifactsRequest {
    /// Recording to study.
    pub recording_id: String,
    /// Tool to run: one of the optional study tools.
    pub tool_id: String,
    /// Revision to generate from; the head revision when absent.
    pub revision_id: Option<String>,
    /// Per-run model override.
    pub model_override: Option<ModelOverride>,
    /// Explicit acknowledgement of an unknown charge (§10.4).
    pub cost_acknowledged: bool,
}

/// `generate_study_artifacts` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GenerateStudyArtifactsResponse {
    /// Tool identity that ran.
    pub tool_id: String,
    /// Tool version that ran.
    pub tool_version: u32,
    /// Revision the artifacts are pinned to.
    pub transcript_revision_id: String,
    /// Accepted artifacts.
    pub artifacts: StudyArtifactsDto,
    /// Items that were refused, with their paths.
    pub rejected: Vec<RejectedStudyItemDto>,
    /// Stored `ai_outputs` rows, one per artifact kind that had items.
    pub output_ids: Vec<String>,
    /// The run that produced them.
    pub tool_run_id: Option<String>,
    /// Whether anything left this device.
    pub dispatched: bool,
}

/// `list_study_artifacts` request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListStudyArtifactsRequest {
    /// Recording whose stored study artifacts are requested.
    pub recording_id: String,
}

/// One stored set of study artifacts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StudyArtifactSetDto {
    /// Output row the set was stored as.
    pub output_id: String,
    /// Tool that produced it.
    pub tool_id: String,
    /// Version of that tool.
    pub tool_version: u32,
    /// Revision the citations resolve against.
    pub transcript_revision_id: String,
    /// Whether that revision is still the head revision.
    pub revision_is_current: bool,
    /// When it was produced.
    pub created_at: String,
    /// The artifacts themselves.
    pub artifacts: StudyArtifactsDto,
}

/// `list_study_artifacts` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListStudyArtifactsResponse {
    /// Stored sets, newest first.
    pub sets: Vec<StudyArtifactSetDto>,
}

/// The consent and routing posture a run happened under.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsentPolicyDto {
    /// Whether this run was permitted to leave the device.
    pub egress_allowed: bool,
    /// Privacy class of the route, as a stable wire string.
    pub privacy_class: String,
    /// Consent grant the run was authorised by, when a grant applied.
    pub consent_id: Option<String>,
}

/// One recorded tool run (spec §9.7, §13.3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolRunDto {
    /// Run identity.
    pub run_id: String,
    /// Recording the run belongs to.
    pub recording_id: String,
    /// Revision the run read.
    pub transcript_revision_id: String,
    /// Tool identity.
    pub tool_id: String,
    /// Tool version, which resolves to the exact definition that ran.
    pub tool_version: u32,
    /// Engine that executed it.
    pub engine: String,
    /// Model identity.
    pub model_id: String,
    /// Model version, when the engine reported one.
    pub model_version: Option<String>,
    /// Request contract used.
    pub request_family: String,
    /// Prompt version recorded with the run.
    pub prompt_version: String,
    /// Output schema version recorded with the run.
    pub schema_version: String,
    /// Hash of the dispatched input.
    pub input_sha256: String,
    /// Hash of the accepted output, when the run produced one.
    pub output_sha256: Option<String>,
    /// `succeeded`, `rejected`, `failed` or `canceled`.
    pub status: String,
    /// Consent posture at dispatch time.
    pub consent_policy: ConsentPolicyDto,
    /// Cost, with unknown never rendered as zero (§13.2).
    pub cost: CostValue,
    /// When the run opened.
    pub created_at: String,
    /// When it finished, when it did.
    pub finished_at: Option<String>,
}

/// `list_tool_runs` request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListToolRunsRequest {
    /// Recording whose runs are requested.
    pub recording_id: String,
}

/// `list_tool_runs` response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ListToolRunsResponse {
    /// Runs, newest first.
    pub runs: Vec<ToolRunDto>,
}

/* ------------------------------------------------------- templates (A12) */

/// One template, as the Templates screen lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemplateSummaryDto {
    /// Template identity.
    pub template_id: String,
    /// Current version.
    pub version: String,
    /// Name shown to the user.
    pub display_name: String,
    /// What the template is for.
    pub purpose: String,
    /// Whether it ships with the application and cannot be edited in place.
    pub built_in: bool,
    /// Built-in a user copy derives from.
    pub base_id: Option<String>,
    /// Version of that built-in at the time of the copy.
    pub base_version: Option<String>,
    /// Number of extraction fields it declares.
    pub field_count: u32,
    /// How many stored versions exist, including the current one.
    pub version_count: u32,
}

/// One extraction field of a template.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemplateFieldDto {
    /// Field key inside the output body.
    pub key: String,
    /// Claim kind items of this field produce.
    pub kind: String,
    /// Heading shown in the UI and in exports.
    pub heading: String,
    /// Minimum resolving anchors per item; zero means anchors are optional.
    pub min_anchors: u32,
    /// Maximum anchors an item may carry.
    pub max_anchors: u32,
    /// Maximum items the model may return.
    pub max_items: u32,
    /// Maximum length of any single string of an item.
    pub max_text_chars: u32,
}

/// The stage instructions of a template.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemplateInstructionsDto {
    /// Role and refusal rules shared by every stage.
    pub system: String,
    /// Instruction for summarizing one section.
    pub section_summary: String,
    /// Instruction for consolidating section summaries.
    pub synthesis: String,
    /// Instruction for structured extraction.
    pub extraction: String,
}

/// How a template's output is rendered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemplateRenderingDto {
    /// Field keys in presentation order.
    pub order: Vec<String>,
    /// Text shown when a field has no items.
    pub empty_placeholder: String,
    /// Whether the renderer shows an anchor chip per item.
    pub show_anchors: bool,
}

/// One template in full, including the document the editor edits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemplateDocumentDto {
    /// Template identity.
    pub template_id: String,
    /// Version of this document.
    pub version: String,
    /// Name shown to the user.
    pub display_name: String,
    /// What the template is for.
    pub purpose: String,
    /// Whether it ships with the application.
    pub built_in: bool,
    /// Built-in a user copy derives from.
    pub base_id: Option<String>,
    /// Version of that built-in at the time of the copy.
    pub base_version: Option<String>,
    /// Prompt version recorded on every output.
    pub prompt_version: String,
    /// Output schema version recorded on every output.
    pub output_schema_version: String,
    /// Stage instructions.
    pub instructions: TemplateInstructionsDto,
    /// Extraction fields in declaration order.
    pub fields: Vec<TemplateFieldDto>,
    /// Rendering rules.
    pub rendering: TemplateRenderingDto,
    /// The canonical JSON document, which the editor edits and `save_template`
    /// accepts back.
    pub document: String,
}

/// `list_templates` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListTemplatesResponse {
    /// Built-ins first, then user copies, each in identity order.
    pub templates: Vec<TemplateSummaryDto>,
}

/// `get_template` request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetTemplateRequest {
    /// Template to read.
    pub template_id: String,
    /// Stored version to read; the current one when absent.
    pub version: Option<String>,
}

/// `create_template_copy` request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateTemplateCopyRequest {
    /// Built-in the copy derives from.
    pub base_id: String,
    /// Identity of the new copy: a lowercase identifier.
    pub new_id: String,
    /// Name for the copy; the base's name with a suffix when absent.
    pub display_name: Option<String>,
}

/// `save_template` request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SaveTemplateRequest {
    /// Template being edited. Must be a user copy.
    pub template_id: String,
    /// The edited document.
    pub document: String,
    /// Version the editor started from, for conflict detection.
    pub expected_version: Option<String>,
}

/// `save_template` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SaveTemplateResponse {
    /// The saved document at its new version.
    pub template: TemplateDocumentDto,
    /// Version that was superseded, when there was one.
    pub previous_version: Option<String>,
}

/// One problem found in a template document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemplateProblemDto {
    /// Stable problem discriminator.
    pub code: String,
    /// What is wrong, in the user's terms. Never echoes the document.
    pub message: String,
}

/// `validate_template` request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidateTemplateRequest {
    /// Document to check. Nothing is stored.
    pub document: String,
}

/// `validate_template` response.
///
/// A document that fails validation is data, not an error: the editor lists
/// every problem next to the field that caused it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidateTemplateResponse {
    /// Whether the document would be accepted by `save_template`.
    pub valid: bool,
    /// Problems, in the order they were found.
    pub problems: Vec<TemplateProblemDto>,
}

/// `list_template_versions` request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListTemplateVersionsRequest {
    /// Template whose history is requested.
    pub template_id: String,
}

/// One stored version of a template.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemplateVersionDto {
    /// Version label.
    pub version: String,
    /// When it was stored, for a version that exists as a file. A built-in is
    /// compiled into the binary and has no stored time, so this is `None`
    /// rather than a fabricated instant.
    pub created_at: Option<String>,
    /// Whether it is the version the template resolves to now.
    pub is_current: bool,
}

/// `list_template_versions` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListTemplateVersionsResponse {
    /// Versions, newest first.
    pub versions: Vec<TemplateVersionDto>,
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
        // The twelve §15.3 names this build emits, the ADR-0020 activation
        // event, the six §15.3 `device.*` names and `model.install_progress`,
        // which EPIC O task O2.4 added for the local-model installer.
        assert_eq!(EventKind::ALL.len(), 20);
        for name in [
            "device.discovered",
            "device.connected",
            "device.compatibility_changed",
            "device.transfer_progress",
            "device.transfer_verified",
            "device.disconnected",
        ] {
            assert!(
                EventKind::ALL.iter().any(|kind| kind.as_str() == name),
                "{name} is not an event kind"
            );
        }
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
