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
  /** Stable engine id, e.g. `whisper.cpp` or `apple-speech`. */
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
  /**
   * Cloud consent grants on record (§4.4 #6, §16.3). Revoked grants stay in
   * the list with `revoked_at` set so settings can show what was withdrawn.
   */
  cloud_consents: ConsentSummary[];
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
  /**
   * §10.4: cost carried by the job. Local jobs are always `unknown`; an
   * estimate is never promoted to a finalized amount by the UI.
   */
  cost: CostValue;
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

/* ----------------------------------------------------------------- sources */

/** `plaudon_source_plaud_cli::state::ConnectorState` (serde snake_case). */
export type PlaudConnectorState =
  | "unconfigured"
  | "cli_detected"
  | "auth_required"
  | "authenticated"
  | "enumerating"
  | "ready"
  | "expired_auth"
  | "incompatible_version"
  | "error";

/**
 * `plaudon_source_plaud_cli::diagnostics::NextAction`. Each kind maps to one
 * control in the UI; the sentence that explains it comes from Rust.
 */
export type PlaudNextAction =
  | { kind: "none" }
  | { kind: "install_cli" }
  | { kind: "install_tested_version"; usable_versions: string[] }
  | { kind: "sign_in" }
  | { kind: "retry" }
  | { kind: "wait_and_retry" }
  | { kind: "review_local_only_mode" }
  | { kind: "import_manually" }
  | { kind: "check_account_and_region" };

export type PlaudCompatibility = "unknown" | "tested" | "untested";

export interface PlaudConnectorStatus {
  state: PlaudConnectorState;
  detected_version: string | null;
  version_validated: boolean;
  program_origin: string | null;
  last_error: ErrorCode | null;
  listing_possibly_incomplete: boolean;
  scraping_warning: boolean;
  updated_at_ms: number;
}

/** `headline`, `detail` and `warnings` are rendered verbatim (§7.6). */
export interface PlaudDiagnostics {
  state: PlaudConnectorState;
  headline: string;
  detail: string;
  next_action: PlaudNextAction;
  detected_version: string | null;
  compatibility: PlaudCompatibility;
  program_origin: string | null;
  last_error: ErrorCode | null;
  warnings: string[];
}

/** §4.2.7: which truthful wording applies. */
export type PlaudDisconnectVariant = "association_only" | "with_vendor_logout";

export interface PlaudDisconnectCopy {
  variant: PlaudDisconnectVariant;
  label: string;
  body: string;
}

export interface PlaudConnectorSnapshot {
  status: PlaudConnectorStatus;
  diagnostics: PlaudDiagnostics;
  disconnect: PlaudDisconnectCopy;
}

export interface PlaudDisconnectResponse {
  disconnect: PlaudDisconnectCopy;
  snapshot: PlaudConnectorSnapshot;
}

export interface PlaudItemSummary {
  external_id: string;
  display_name: string;
  captured_at: string | null;
  duration_ms: number | null;
  declared_byte_length: number | null;
  imported_recording_id: string | null;
}

export type PlaudListingCompleteness = "complete" | "possibly_incomplete";

export interface PlaudListResponse {
  source_id: string;
  items: PlaudItemSummary[];
  next_cursor: string | null;
  completeness: PlaudListingCompleteness;
  empty_account_advisory: string | null;
}

export type PlaudImportOutcome =
  | { outcome: "imported"; external_id: string; job_id: string; recording_id: string }
  | { outcome: "already_imported"; external_id: string; recording_id: string }
  | { outcome: "failed"; external_id: string; job_id: string; error: ErrorEnvelope };

export interface PlaudImportResponse {
  results: PlaudImportOutcome[];
}

export type WatchedFolderRule = "import_only" | "import_and_queue_local_stt" | "ask_before_cloud";
export type SyncTrigger = "manual" | "background";
export type SyncedSourceKind = "watched_folder" | "plaud_cloud";

export interface SourceAddFolderRequest {
  path: string;
  recursive: boolean;
  rule: WatchedFolderRule;
}

export interface SourceAddFolderResponse {
  source_id: string;
  rule: WatchedFolderRule;
  recursive: boolean;
}

export interface FolderSyncCounts {
  scanned: number;
  inventory_complete: boolean;
  imported: number;
  already_imported: number;
  deferred: number;
  refused_tombstoned: number;
  rejected: number;
  vanished: number;
}

export interface SourceSyncResponse {
  job_id: string;
  source_id: string;
  source_kind: SyncedSourceKind;
  state: JobState;
  folder: FolderSyncCounts | null;
  listed: number | null;
  imported_recording_ids: string[];
}

export interface ReconcileDecisionSummary {
  external_id: string | null;
  event: string;
  actions: string[];
}

export interface SourceReconcileResponse {
  job_id: string;
  source_id: string;
  source_kind: SyncedSourceKind;
  decisions: ReconcileDecisionSummary[];
  listing_complete: boolean | null;
  folder: FolderSyncCounts | null;
}

export interface SourceArtifactSummary {
  artifact_id: string;
  kind: string;
  source_vendor: string;
  content_sha256: string;
  byte_length: number;
  original_filename: string | null;
  imported_at: string | null;
  import_mode: string;
  recorded_at: string;
}

export interface ListSourceArtifactsResponse {
  artifacts: SourceArtifactSummary[];

/* ------------------------------------------------------------------ search */

export interface SearchLibraryRequest {
  query: string;
  cursor: string | null;
  limit: number;
}

export interface SearchResult {
  recording_id: string;
  recording_title: string;
  transcript_revision_id: string;
  seq_index: number;
  excerpt: string;
  /** Null for text-only transcripts (\u00a78.6A: never fabricate seek times). */
  seek_ms: number | null;
  alignment_origin: "native" | "derived" | "none";
}

export interface SearchLibraryResponse {
  results: SearchResult[];
  next_cursor: string | null;
}

/* ---------------------------------------------------------------- speakers */

export interface ListSpeakersRequest {
  recording_id: string;
}

export interface Speaker {
  speaker_id: string;
  recording_id: string;
  label: string;
  confirmed: boolean;
  original_label: string;
  renamed_at: string | null;
  confirmed_at: string | null;
}

export interface ListSpeakersResponse {
  speakers: Speaker[];
}

export interface RenameSpeakerRequest {
  recording_id: string;
  speaker_id: string;
  name: string;
}

export interface ConfirmSpeakerRequest {
  recording_id: string;
  speaker_id: string;
}

export interface ResetSpeakerRequest {
  recording_id: string;
  speaker_id: string;
}

export interface SpeakerOperationResponse {
  speaker: Speaker;
}

/* ------------------------------------------------- cloud (OpenRouter BYOK) */

/**
 * Exact decimal money. `amount` is a plain decimal string so no rounding is
 * introduced between Rust and the UI; it is never parsed into a float for
 * display arithmetic.
 */
export interface MoneyAmount {
  amount: string;
  /** ISO-4217, uppercase. */
  currency: string;
}

/** §10.4: an estimate is never shown or stored as an actual amount. */
export type CostConfidence = "unknown" | "estimated" | "finalized";

export interface CostValue {
  confidence: CostConfidence;
  /** `null` exactly when `confidence` is `unknown`; never substituted with 0. */
  amount: MoneyAmount | null;
}

/** §8.6: which OpenRouter request contract a model actually implements. */
export type RequestFamily = "multipart_stt" | "audio_chat" | "unsupported" | "unknown";

/** §8.6A / §10.2: `verified` means our capability smoke test passed. */
export type Verification = "verified" | "not_verified";

export type TriState = "yes" | "no" | "unknown";

export type RetentionPosture =
  | "zero_retention"
  | "limited_retention"
  | "provider_default"
  | "unknown";

/** §9.1 pipeline stages a model preference or a re-run can target. */
export const PROCESSING_STAGES = [
  "transcribe",
  "diarize",
  "normalize",
  "section_summary",
  "consolidated_summary",
  "structured_extraction",
  "citation_verification",
  "tool_run",
] as const;
export type ProcessingStage = (typeof PROCESSING_STAGES)[number];

export interface ModelPricing {
  basis: "none" | "per_audio_second" | "per_token" | "per_request" | "other";
  /** Unit text taken from provider metadata; never invented by the UI. */
  unit_label: string;
  /** `null` means the price is unknown. It is never rendered as 0. */
  amount_per_unit: MoneyAmount | null;
}

export interface ModelPrivacy {
  /** The relay the request passes through, e.g. `openrouter`. */
  relay: string;
  /** `null` means the downstream provider was not disclosed; never guessed. */
  downstream_provider: string | null;
  retention: RetentionPosture;
  /** Only meaningful for `limited_retention`. */
  retention_days: number | null;
  zero_retention_available: boolean;
}

/** Stable machine reason a model is not verified (§8.6A); `null` when verified. */
export type VerificationReason =
  | "not_in_allowlist"
  | "no_observed_exchange"
  | "catalog_only"
  | "privacy_route_unresolved"
  | "no_request_builder"
  | "user_supplied_custom_id";

export interface ModelCard {
  model_id: string;
  display_name: string;
  engine: string;
  request_family: RequestFamily;
  verification: Verification;
  verification_reason: VerificationReason | null;
  /** Evidence backing `verified`. */
  fixture_id: string | null;
  /** RFC3339 instant the evidence was produced. */
  validated_at: string | null;
  features: {
    segments: TriState;
    word_timestamps: TriState;
    diarization: TriState;
    language_detection: TriState;
  };
  /** `null` means the locale coverage is unknown, not "none". */
  locales: string[] | null;
  max_input_bytes: number | null;
  max_duration_ms: number | null;
  pricing: ModelPricing;
  privacy: ModelPrivacy;
  /** True only when the model may actually be dispatched to right now. */
  selectable: boolean;
}

export interface ListModelsRequest {
  stage: ProcessingStage;
  /** `null` lists every engine that can serve the stage. */
  engine: string | null;
}

export interface ListModelsResponse {
  models: ModelCard[];
  /** §10.1: discovery failure degrades to the pinned allowlist, never to empty. */
  catalog_source: "live" | "pinned_allowlist";
  catalog_fetched_at: string | null;
  degraded_reason: "offline" | "local_only_mode" | "no_key" | "catalog_error" | null;
  /** Per-task default for `stage`. */
  default_model_id: string | null;
}

export type DispatchState =
  | "not_sent"
  | "sending"
  | "accepted_by_provider"
  | "outcome_unknown"
  | "completed"
  | "failed_before_send";

export interface UsageTotals {
  finalized: MoneyAmount | null;
  estimated: MoneyAmount | null;
  /** Attempts with no derivable cost; never folded into a 0 amount. */
  unknown_count: number;
}

export interface UsageByModel extends UsageTotals {
  model_id: string;
  attempts: number;
}

/** §10.7 inference attempt ledger entry, without any payload or secret. */
export interface UsageAttempt {
  attempt_id: string;
  job_id: string;
  model_id: string;
  request_family: RequestFamily;
  dispatch_state: DispatchState;
  cost: CostValue;
  provider_request_id: string | null;
  created_at: string;
  responded_at: string | null;
  possibly_billed: boolean;
}

export interface CloudBudget {
  per_job_limit: MoneyAmount | null;
  monthly_limit: MoneyAmount | null;
  /** Finalized plus estimated spend for the current month. */
  month_to_date: CostValue;
  /** True when a limit is locally calculable and therefore enforceable here. */
  blocks_dispatch: boolean;
  /** §10.4: fixed text; local budgets cannot prevent upstream charges. */
  enforcement_note: string;
}

export interface GetUsageRequest {
  /** RFC3339, inclusive. */
  from: string;
  /** RFC3339, exclusive. */
  to: string;
}

export interface GetUsageResponse {
  from: string;
  to: string;
  totals: UsageTotals;
  by_model: UsageByModel[];
  attempts: UsageAttempt[];
  budget: CloudBudget;
}

export interface SetCloudBudgetRequest {
  /** Decimal string; `null` clears the limit. */
  per_job_limit: string | null;
  monthly_limit: string | null;
  /** Must be `USD` in V1. */
  currency: string;
}

export type ConsentPurpose =
  | "transcription"
  | "summarization"
  | "transcript_tool"
  | "credential_validation";

export type ConsentContentType = "audio_bytes" | "transcript_text" | "metadata_only";

/** §4.4 #6: the three scopes offered before a cloud operation. */
export type ConsentScopeKind = "ask_every_time" | "recording" | "model_policy";

export interface ConsentScope {
  kind: ConsentScopeKind;
  /** Required for `recording`; `null` otherwise. */
  recording_id: string | null;
}

export interface GrantCloudConsentRequest {
  purpose: ConsentPurpose;
  content_type: ConsentContentType;
  scope: ConsentScope;
  model_id: string;
  downstream_provider: string | null;
  require_zero_retention: boolean;
  /** Version of the privacy statement the user agreed to (§4.4 #5). */
  disclosure_version: string;
}

export interface GrantCloudConsentResponse {
  consent_id: string;
  scope_kind: string;
  granted_at: string;
  expires_at: string | null;
}

export interface ConsentSummary {
  consent_id: string;
  purpose: ConsentPurpose;
  content_type: ConsentContentType;
  scope_kind: ConsentScopeKind;
  recording_id: string | null;
  model_id: string;
  downstream_provider: string | null;
  required_retention: RetentionPosture | null;
  disclosure_version: string;
  granted_at: string;
  expires_at: string | null;
  revoked_at: string | null;
}

export interface ModelPreferenceScope {
  kind: "default" | "recording";
  recording_id: string | null;
}

export interface SetModelPreferenceRequest {
  stage: ProcessingStage;
  scope: ModelPreferenceScope;
  engine: string;
  /** `null` clears a recording override; it never clears the global default. */
  model_id: string | null;
}

export interface SetModelPreferenceResponse {
  stage: ProcessingStage;
  scope_kind: "default" | "recording";
  recording_id: string | null;
  engine: string;
  model_id: string | null;
  /** The model that would run now for this stage/recording after precedence. */
  effective_model: ModelCard | null;
}

export interface StartProcessingRequest {
  recording_id: string;
  stage: ProcessingStage;
  /** One-shot override; §10.2 forbids it from changing stored preferences. */
  model_override: { engine: string; model_id: string } | null;
  /** Required when a prior attempt is `outcome_unknown` or a budget warning applies. */
  cost_acknowledged: boolean;
  /** Re-run only this stage even when a result already exists. */
  rerun: boolean;
}

export interface StartProcessingResponse {
  job_id: string;
  estimated_cost: CostValue;
  dispatch: "queued" | "consent_required" | "blocked";
  /** Machine-independent explanation from Rust; `null` when `queued`. */
  blocked_reason: string | null;
}

/** The two request builders a capability check can exercise (§8.6, §10.3). */
export type VerificationTask = "transcription" | "text_generation";

/**
 * ADR-0022: `verified` requires an observed exchange, so the only way a model
 * can become verified is to actually send it a short bundled fixture clip.
 * That is a real, consented, billable request — never a silent background one.
 */
export interface VerifyModelRequest {
  model_id: string;
  task: VerificationTask;
  /** The check costs money, so it is never run without explicit approval. */
  cost_acknowledged: boolean;
}

export interface VerifyModelResponse {
  /** The model after the exchange; `verified` only when the check passed. */
  model: ModelCard;
  /** What the check itself cost; `unknown` when no cost could be derived. */
  cost: CostValue;
  provider_request_id: string | null;
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
  search_library: { req: SearchLibraryRequest; res: SearchLibraryResponse };
  list_speakers: { req: ListSpeakersRequest; res: ListSpeakersResponse };
  rename_speaker: { req: RenameSpeakerRequest; res: SpeakerOperationResponse };
  confirm_speaker: { req: ConfirmSpeakerRequest; res: SpeakerOperationResponse };
  reset_speaker: { req: ResetSpeakerRequest; res: SpeakerOperationResponse };
  complete_onboarding: { req: CompleteOnboardingRequest; res: Preferences };
  plaud_probe: { req: Record<string, never>; res: PlaudConnectorSnapshot };
  plaud_connect: { req: { user_intent: "connect_plaud_account" }; res: PlaudConnectorSnapshot };
  plaud_disconnect: { req: Record<string, never>; res: PlaudDisconnectResponse };
  plaud_list: { req: { cursor: string | null }; res: PlaudListResponse };
  plaud_import: { req: { external_ids: string[] }; res: PlaudImportResponse };
  source_add_folder: { req: SourceAddFolderRequest; res: SourceAddFolderResponse };
  source_sync: { req: { source_id: string; trigger: SyncTrigger }; res: SourceSyncResponse };
  source_reconcile: { req: { source_id: string }; res: SourceReconcileResponse };
  list_source_artifacts: { req: { recording_id: string }; res: ListSourceArtifactsResponse };
  list_models: { req: ListModelsRequest; res: ListModelsResponse };
  get_usage: { req: GetUsageRequest; res: GetUsageResponse };
  set_cloud_budget: { req: SetCloudBudgetRequest; res: CloudBudget };
  grant_cloud_consent: { req: GrantCloudConsentRequest; res: GrantCloudConsentResponse };
  revoke_cloud_consent: { req: { consent_id: string }; res: StatusResponse };
  set_model_preference: { req: SetModelPreferenceRequest; res: SetModelPreferenceResponse };
  start_processing: { req: StartProcessingRequest; res: StartProcessingResponse };
  verify_model: { req: VerifyModelRequest; res: VerifyModelResponse };
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
  "list_models",
  "start_processing",
  "grant_cloud_consent",
  "revoke_cloud_consent",
  "get_usage",
];

/** Additions reviewed in ADR-0020 because §15.2 has no read path for them. */
export const ADR_0020_COMMANDS: readonly CommandName[] = ["list_activity", "complete_onboarding"];

/** EPIC K source commands, quoted from the §15.2 table. */
export const EPIC_K_COMMANDS: readonly CommandName[] = [
  "plaud_probe",
  "plaud_connect",
  "plaud_disconnect",
  "plaud_list",
  "plaud_import",
  "source_add_folder",
  "source_sync",
  "source_reconcile",
  "list_source_artifacts",
];

/** EPIC M extensions: cross-recording search (§3.1, §9.8) and speaker labelling (§8.7). */
export const EPIC_M_COMMANDS: readonly CommandName[] = [
  "search_library",
  "list_speakers",
  "rename_speaker",
  "confirm_speaker",
  "reset_speaker",
];

/**
 * EPIC G additions. §15.2 lists no write path for the local spending budget,
 * for the per-task default model, or for the capability check that is the only
 * thing able to make a model `verified` at all (ADR-0022) — yet §10.2, §10.4
 * and §8.6A all require them. They are named here so the drift test still
 * covers them.
 */
export const CLOUD_ADDED_COMMANDS: readonly CommandName[] = [
  "set_cloud_budget",
  "set_model_preference",
  "verify_model",
];

export const ALL_COMMANDS: readonly CommandName[] = [
  ...SPEC_COMMANDS,
  ...ADR_0020_COMMANDS,
  ...EPIC_K_COMMANDS,
  ...EPIC_M_COMMANDS,
  ...CLOUD_ADDED_COMMANDS,
];

/* ----------------------------------------------------------------- events */

/**
 * §15.3 event names, plus `ui.activation_requested` (ADR-0020) which the Rust
 * side emits when the user clicks an OS notification. Payloads carry a
 * monotonically increasing `seq`.
 */
export const EVENT_KINDS = [
  "job.updated",
  "job.failed",
  "recording.imported",
  "recording.source_updated",
  "transcript.completed",
  "tool.completed",
  "model.availability_changed",
  "consent.required",
  "source.auth_required",
  "source.sync_finished",
  "library.changed",
  "activity.added",
  "ui.activation_requested",
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

/** Target carried by `ui.activation_requested` when a notification is clicked. */
export interface ActivationTarget {
  kind: "recording" | "job" | "settings";
  id: string;
}

/** Narrows an event payload to an activation target; returns null when absent. */
export function activationTargetOf(event: UiEvent): ActivationTarget | null {
  if (event.kind !== "ui.activation_requested") return null;
  const target = event.payload.target;
  if (!target || typeof target !== "object") return null;
  if (!("kind" in target) || !("id" in target)) return null;
  const kind = target.kind;
  const id = target.id;
  if (typeof id !== "string") return null;
  if (kind !== "recording" && kind !== "job" && kind !== "settings") return null;
  return { kind, id };
}
