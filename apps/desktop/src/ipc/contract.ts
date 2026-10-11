/**
 * Typed Tauri command/event contract (spec §15).
 *
 * Every command name here appears in the §15.2 table except the two documented
 * additions recorded in docs/adr/ADR-0020-command-surface.md (`list_activity`,
 * `complete_onboarding`); `schemas/commands/*.schema.json` is the versioned
 * wire contract and `contract.schema.test.ts` fails CI on drift.
 */

/* ------------------------------------------------------------------ errors */

/**
 * §15.4 error envelope, serialized by `plaudon_domain::error::ErrorEnvelope`.
 * Never carries URLs, credentials, provider payloads or transcript text.
 * `action` is omitted by Rust when the code has no affordance.
 */
export interface ErrorEnvelope {
  code: ErrorCode;
  message: string;
  retryable: boolean;
  action?: ErrorAction | null;
  diagnostic_id: string;
}

/** `plaudon_domain::error::ErrorAction` (serde SCREAMING_SNAKE_CASE). */
export const ERROR_ACTIONS = [
  "OPEN_PLAUD_CONNECT",
  "OPEN_CLI_SETUP",
  "OPEN_SETTINGS",
  "OPEN_PERMISSION_SETTINGS",
  "RETRY_IMPORT",
  "REFRESH_SOURCE_LINK",
  "REVIEW_CONSENT",
  "FREE_DISK_SPACE",
  "REVIEW_LOCAL_ONLY_MODE",
  "OPEN_DEVICE_DIAGNOSTICS",
  "CHARGE_DEVICE",
  "OPEN_DIAGNOSTICS",
  "CHOOSE_ANOTHER_MODEL",
  "WAIT_AND_RETRY",
] as const;
export type ErrorAction = (typeof ERROR_ACTIONS)[number];

/**
 * `plaudon_domain::error::AppErrorCode` — a closed set. The UI maps codes to
 * corrective actions; an unrecognised code still renders its message and,
 * when `retryable`, a Retry control.
 */
export const ERROR_CODES = [
  "CLI_MISSING",
  "CLI_UNSUPPORTED",
  "AUTHENTICATION_REQUIRED",
  "AUTH_CANCELED",
  "SOURCE_UNAVAILABLE",
  "REMOTE_RATE_LIMITED",
  "LINK_EXPIRED",
  "ASSET_NOT_FOUND",
  "MALFORMED_OUTPUT",
  "DOWNLOAD_INTERRUPTED",
  "INTEGRITY_FAILED",
  "RADIO_PERMISSION_DENIED",
  "DEVICE_BUSY",
  "DEVICE_DISCONNECTED",
  "UNSUPPORTED_FIRMWARE",
  "AUTH_BOOTSTRAP_REQUIRED",
  "AUTH_REJECTED",
  "TRANSFER_INCOMPLETE",
  "UNSUPPORTED_CODEC",
  "NOT_PRESENT_ON_DEVICE",
  "DEVICE_POWER_LOW",
  "UNSUPPORTED_MEDIA",
  "MEDIA_VALIDATION_FAILED",
  "STORAGE_UNAVAILABLE",
  "STORAGE_FULL",
  "MIGRATION_FAILED",
  "RECORD_NOT_FOUND",
  "CONFLICT",
  "ARCHIVE_PATH_REJECTED",
  "CROSS_VOLUME_COMMIT",
  "CONSENT_REQUIRED",
  "LOCAL_ONLY_MODE",
  "POLICY_UNRESOLVED",
  "CAPABILITY_UNVERIFIED",
  "MODEL_UNAVAILABLE",
  "OUTCOME_UNKNOWN",
  "PROVIDER_REJECTED",
  "SCHEMA_VALIDATION_FAILED",
  "CONTEXT_TOO_LARGE",
  "CANCELED",
  "TIMEOUT",
  "INVALID_ARGUMENT",
  "CONFIRMATION_REQUIRED",
  "COST_ACKNOWLEDGEMENT_REQUIRED",
  "BUDGET_EXCEEDED",
  "SECRET_STORE_LOCKED",
  "EGRESS_DENIED",
  "COMMAND_NOT_AUTHORIZED",
  "HELPER_FAILED",
  "INTERNAL_ERROR",
] as const;
export type ErrorCode = (typeof ERROR_CODES)[number] | (string & {});

/* ------------------------------------------------------------ capabilities */

export type CapabilityStatus = "unknown" | "experimental" | "supported" | "unsupported";

export interface EngineInventoryEntry {
  /** Stable engine id, e.g. `whisper.cpp` or `apple.speech`. */
  engine_id: string;
  display_name: string;
  stage: "stt" | "text";
  status: CapabilityStatus;
  /** Why the engine is unavailable; never hidden from the picker (§5.2). */
  unavailable_reason: string | null;
  installed_models: InstalledModel[];
  /** Bytes required to install the smallest usable model, when known. */
  download_bytes_required: number | null;
  requires_network: boolean;
}

export interface InstalledModel {
  model_id: string;
  display_name: string;
  size_bytes: number;
  installed: boolean;
}

export interface StorageCapability {
  library_path: string;
  free_bytes: number;
  total_bytes: number;
}

export interface Preferences {
  onboarding_completed: boolean;
  local_only_mode: boolean;
  default_stt_engine_id: string | null;
  default_text_engine_id: string | null;
  appearance: "system" | "light" | "dark";
  contrast: "standard" | "high";
  notifications: NotificationSettings;
}

/** Observed state of an OS permission; `unsupported` means the OS has no such concept. */
export type OsPermissionState = "granted" | "denied" | "not_requested" | "unsupported";

export interface Capabilities {
  os: { platform: "macos" | "windows" | "linux"; version: string };
  hardware: { arch: string; logical_cores: number; total_memory_bytes: number };
  engines: EngineInventoryEntry[];
  storage: StorageCapability;
  /** Integration statuses keyed by integration id (plaud_cloud, openrouter, …). */
  integrations: Record<string, CapabilityStatus>;
  /** OS permissions the app has actually been granted. */
  permissions: Record<string, OsPermissionState>;
  preferences: Preferences;
  openrouter_key_present: boolean;
}

/* --------------------------------------------------------------- recording */

export type ImportStatus = "pending" | "importing" | "imported" | "failed";
export type TranscriptionStatus =
  | "none"
  | "queued"
  | "running"
  | "completed"
  | "failed"
  | "stale";

export interface RecordingSummary {
  recording_id: string;
  title: string;
  /** RFC3339 UTC. */
  recorded_at: string | null;
  imported_at: string;
  /** Null when the media has no measurable duration (§8.6A: never fabricate). */
  duration_ms: number | null;
  source: { source_kind: "manual_import" | "plaud_cloud" | "plaud_device" | "watched_folder"; label: string };
  import_status: ImportStatus;
  transcription_status: TranscriptionStatus;
  processing_engine: string | null;
  last_error: ErrorEnvelope | null;
  tags: string[];
  byte_length: number;
  sha256: string;
}

export interface RecordingDetail extends RecordingSummary {
  media: {
    observed_codec: string;
    sample_rate_hz: number | null;
    channels: number | null;
    /** Original path kept only for display; never used as an input path again. */
    original_file_name: string;
  };
  revisions: TranscriptRevisionRef[];
  current_revision_id: string | null;
  /** Summary outputs derived from a revision; stale when the revision is superseded. */
  outputs: RecordingOutput[];
}

export interface RecordingOutput {
  output_id: string;
  kind: "summary" | "actions" | "decisions";
  /** Revision the output was generated from. */
  source_revision_id: string;
  created_at: string;
  stale: boolean;
}

export type RecordingSortField = "recorded_at" | "imported_at" | "title" | "duration_ms";
export interface ListRecordingsRequest {
  cursor: string | null;
  limit: number;
  filter: RecordingFilter;
  sort: { field: RecordingSortField; direction: "asc" | "desc" };
}

export interface RecordingFilter {
  query: string | null;
  import_status: ImportStatus[];
  transcription_status: TranscriptionStatus[];
  sources: string[];
  tags: string[];
  /** Inclusive RFC3339 bounds on `recorded_at` (falling back to `imported_at`). */
  date_from: string | null;
  date_to: string | null;
}

export interface RecordingPage {
  items: RecordingSummary[];
  next_cursor: string | null;
  /** Total matching rows at query time; the UI labels it as a snapshot. */
  total: number;
}

/* -------------------------------------------------------------- transcript */

/** §8.6A: `none` means there is no timeline at all — the UI must not render one. */
export type AlignmentOrigin = "native" | "derived" | "none";

export interface TranscriptRevisionRef {
  revision_id: string;
  /** `machine` is the immutable engine output; `human` revisions are edits. */
  kind: "machine" | "human";
  created_at: string;
  engine_id: string | null;
  model_id: string | null;
  alignment_origin: AlignmentOrigin;
  segment_count: number;
  superseded: boolean;
}

export interface TranscriptSegment {
  index: number;
  text: string;
  /** Absent (null) when alignment_origin is `none`; never defaulted to 0. */
  start_ms: number | null;
  end_ms: number | null;
  alignment_origin: AlignmentOrigin;
  /** Engine-specific confidence; never compared across engines (§8.5). */
  confidence: { engine_id: string; value: number } | null;
  speaker_label: string | null;
  annotations: TranscriptAnnotation[];
}

export interface TranscriptAnnotation {
  kind: "low_confidence" | "possible_hallucination" | "inaudible" | "edited";
  reason: string;
  segment_index: number;
  start_ms: number | null;
  end_ms: number | null;
}

export interface GetTranscriptRequest {
  revision_id: string;
  cursor: number | null;
  limit: number;
}

export interface TranscriptPage {
  revision: TranscriptRevisionRef;
  segments: TranscriptSegment[];
  next_cursor: number | null;
  total_segments: number;
}

export interface SegmentEdit {
  index: number;
  text: string;
}

export interface SaveTranscriptRevisionRequest {
  recording_id: string;
  base_revision_id: string;
  edits: SegmentEdit[];
}

export interface SaveTranscriptRevisionResponse {
  revision_id: string;
  /** Outputs whose source revision is now superseded (§4.5 regeneration hint). */
  stale_output_ids: string[];
}

/* ------------------------------------------------------------------- jobs */

/**
 * §14.1 coarse states + §14.7 arbitration states, as persisted by
 * `plaudon_jobs::state::JobState` (serde snake_case).
 */
export const JOB_STATES = [
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
] as const;
export type JobState = (typeof JOB_STATES)[number];

export type JobKind =
  | "import"
  | "transcribe"
  | "summarize"
  | "export"
  | "model_install"
  | "source_sync";

export interface JobSummary {
  job_id: string;
  kind: JobKind;
  state: JobState;
  /** Human-readable current stage, e.g. "hashing", "decoding". */
  stage: string | null;
  /** 0..1, or null when the stage cannot report a ratio. */
  progress: number | null;
  created_at: string;
  updated_at: string;
  started_at: string | null;
  finished_at: string | null;
  recording_id: string | null;
  /** Title for display; falls back to the job kind when no recording exists yet. */
  label: string;
  error: ErrorEnvelope | null;
  /** True when a retry may incur a new provider charge (§14.3, §14.7). */
  cost_acknowledgement_required: boolean;
  /** Provider request id preserved for uncertain outcomes; never a URL. */
  provider_request_id: string | null;
  /** Link target for the produced output, resolved by the UI router. */
  output_link: { kind: "recording" | "export"; id: string } | null;
}

export interface ListJobsRequest {
  cursor: string | null;
  limit: number;
  states: JobState[];
  kinds: JobKind[];
}

export interface JobPage {
  items: JobSummary[];
  next_cursor: string | null;
  /** Counts per state at query time, for the Jobs filter chips. */
  state_counts: Partial<Record<JobState, number>>;
}

export interface RetryJobRequest {
  job_id: string;
  /** Required when `cost_acknowledgement_required` is true (§18.7 #9). */
  cost_acknowledged: boolean;
}

/* ----------------------------------------------------------------- import */

export type DuplicatePolicy = "ask" | "import_anyway" | "open_existing" | "skip";

export interface ImportFilesRequest {
  /** Paths obtained from the OS picker or a window drop event; validated in Rust. */
  paths: string[];
  duplicate_policy: DuplicatePolicy;
}

export type ImportOutcome =
  | { outcome: "accepted"; path: string; job_id: string }
  | {
      outcome: "duplicate";
      path: string;
      sha256: string;
      existing_recording_id: string;
      existing_title: string;
    }
  | { outcome: "skipped"; path: string; sha256: string; existing_recording_id: string }
  | { outcome: "rejected"; path: string; error: ErrorEnvelope };

export interface ImportFilesResponse {
  results: ImportOutcome[];
}

/* ------------------------------------------------------------------ export */

export type ExportFormat = "txt" | "markdown" | "json";

export interface ExportRecordingRequest {
  recording_id: string;
  revision_id: string;
  format: ExportFormat;
  /** Destination chosen through the OS save dialog. */
  destination: string;
}

export interface ExportRecordingResponse {
  job_id: string;
}

/* ----------------------------------------------------- settings / activity */

export const NOTIFICATION_CATEGORIES = [
  "import_finished",
  "transcription_finished",
  "requires_attention",
  "cloud_budget_exceeded",
] as const;
export type NotificationCategory = (typeof NOTIFICATION_CATEGORIES)[number];

export interface NotificationSettings {
  /** Master switch; categories are opt-in individually (§5.5). */
  enabled: boolean;
  categories: Record<NotificationCategory, boolean>;
}

export interface NotificationSettingsResponse {
  settings: NotificationSettings;
  /** What the OS actually permits right now. */
  os_permission: OsPermissionState;
}

export interface LocalOnlyModeResponse {
  enabled: boolean;
  /** Effective enforced policy description from the egress gate. */
  description: string;
  /** Hosts that remain reachable (empty in local-only mode). */
  allowed_hosts: string[];
}

export interface ActivityEntry {
  activity_id: string;
  category: NotificationCategory;
  occurred_at: string;
  title: string;
  /** Privacy-safe body; same text used for the OS notification. */
  body: string;
  /** Whether the OS notification was actually delivered. */
  notification_delivered: boolean;
  notification_suppressed_reason: string | null;
  target: { kind: "recording" | "job" | "settings"; id: string } | null;
  read: boolean;
}

export interface ListActivityRequest {
  cursor: string | null;
  limit: number;
}

export interface ActivityPage {
  items: ActivityEntry[];
  next_cursor: string | null;
  unread_count: number;
}

export interface CompleteOnboardingRequest {
  default_stt_engine_id: string | null;
  default_text_engine_id: string | null;
  local_only_mode: boolean;
  notifications: NotificationSettings;
}

export interface SetOpenRouterKeyRequest {
  /** Transported once, never returned (§15.2). */
  key: string;
}

/** Commands return status only for secret-bearing operations. */
export interface StatusResponse {
  ok: true;
}

/* ------------------------------------------------------------ command map */

export interface CommandMap {
  get_capabilities: { req: Record<string, never>; res: Capabilities };
  list_recordings: { req: ListRecordingsRequest; res: RecordingPage };
  import_files: { req: ImportFilesRequest; res: ImportFilesResponse };
  get_recording: { req: { recording_id: string }; res: RecordingDetail };
  get_transcript: { req: GetTranscriptRequest; res: TranscriptPage };
  save_transcript_revision: {
    req: SaveTranscriptRevisionRequest;
    res: SaveTranscriptRevisionResponse;
  };
  list_jobs: { req: ListJobsRequest; res: JobPage };
  cancel_job: { req: { job_id: string }; res: JobSummary };
  retry_job: { req: RetryJobRequest; res: JobSummary };
  export_recording: { req: ExportRecordingRequest; res: ExportRecordingResponse };
  set_notification_settings: {
    req: { settings: NotificationSettings };
    res: NotificationSettingsResponse;
  };
  set_local_only_mode: { req: { enabled: boolean; confirmed: boolean }; res: LocalOnlyModeResponse };
  set_openrouter_key: { req: SetOpenRouterKeyRequest; res: StatusResponse };
  remove_openrouter_key: { req: Record<string, never>; res: StatusResponse };
  list_activity: { req: ListActivityRequest; res: ActivityPage };
  complete_onboarding: { req: CompleteOnboardingRequest; res: Preferences };
}

export type CommandName = keyof CommandMap;

/** Commands quoted verbatim from the §15.2 table. */
export const SPEC_COMMANDS: readonly CommandName[] = [
  "get_capabilities",
  "list_recordings",
  "import_files",
  "get_recording",
  "get_transcript",
  "save_transcript_revision",
  "list_jobs",
  "cancel_job",
  "retry_job",
  "export_recording",
  "set_notification_settings",
  "set_local_only_mode",
  "set_openrouter_key",
  "remove_openrouter_key",
];

/** Additions reviewed in ADR-0020 because §15.2 has no read path for them. */
export const ADR_0020_COMMANDS: readonly CommandName[] = ["list_activity", "complete_onboarding"];

export const ALL_COMMANDS: readonly CommandName[] = [...SPEC_COMMANDS, ...ADR_0020_COMMANDS];

/* ----------------------------------------------------------------- events */

/** §15.3 event names. Payloads carry a monotonically increasing `seq`. */
export const EVENT_KINDS = [
  "job.updated",
  "job.failed",
  "recording.imported",
  "recording.source_updated",
  "transcript.completed",
  "tool.completed",
  "model.availability_changed",
  "consent.required",
  "library.changed",
  "activity.added",
] as const;
export type EventKind = (typeof EVENT_KINDS)[number];

export interface UiEvent {
  seq: number;
  kind: EventKind;
  occurred_at: string;
  job_id: string | null;
  recording_id: string | null;
  /** Opaque, non-authoritative hint payload; the store is the source of truth. */
  payload: Record<string, unknown>;
}
