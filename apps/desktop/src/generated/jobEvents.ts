// GENERATED FILE — DO NOT EDIT BY HAND.
//
// Source of truth: schemas/events/job-events.v1.schema.json
// Regenerate with: scripts/generate-job-event-types.mjs
//
// Typed job/activity events delivered from the Rust job engine
// (crates/jobs, SPEC v1.3 §15.3). Event delivery is NOT durable persistence:
// on a sequence gap or a restart, refetch authoritative state via `list_jobs`.

/** Contract version carried by every event. */
export const UI_EVENT_CONTRACT_VERSION = 1;

/** Every persisted job state (SPEC v1.3 §14.1 ∪ §14.7). */
export const JOB_STATES = [
  'queued',
  'preflight',
  'waiting_for_user',
  'waiting_for_network',
  'waiting_for_model',
  'waiting_for_source',
  'waiting_for_authorization',
  'running_local',
  'cloud_ready',
  'cloud_sending',
  'cloud_outcome_unknown',
  'verifying',
  'committing',
  'completed',
  'failed_retryable',
  'failed_terminal',
  'cancelling',
  'cancelled',
] as const;

export type JobState = (typeof JOB_STATES)[number];

/** Coarse UI bucket for a job state. */
export type JobPhase = 'pending' | 'waiting' | 'active' | 'finishing' | 'terminal';

/** Deterministic retry classification (SPEC v1.3 §14.6). */
export type RetryClass =
  | 'automatic_retry'
  | 'user_retry_with_acknowledgement'
  | 'resume_when_available'
  | 'terminal';

/** Why a job is parked in a waiting state. */
export const WAIT_REASONS = [
  'insufficient_disk',
  'insufficient_memory',
  'model_asset_missing',
  'source_unavailable',
  'power_saver',
  'network_unavailable',
  'keychain_locked',
  'user_decision_required',
  'consent_required',
  'system_suspend',
] as const;

export type WaitReason = (typeof WAIT_REASONS)[number];

/** How a source item changed relative to our record (SPEC v1.3 §7.5). */
export type SourceChange =
  | 'new'
  | 'unchanged'
  | 'metadata_changed'
  | 'audio_changed'
  | 'remote_trashed'
  | 'removed_from_listing';

/** Event names emitted to the frontend (SPEC v1.3 §15.3). */
export const JOB_EVENT_KINDS = [
  'job.updated',
  'job.failed',
  'recording.imported',
  'recording.source_updated',
  'recording.deleted',
  'transcript.completed',
  'tool.completed',
  'model.availability_changed',
  'consent.required',
  'library.changed',
] as const;

export type JobEventKind = (typeof JOB_EVENT_KINDS)[number];

export interface JobUpdatedPayload {
  type: 'job_updated';
  state: JobState;
  previous_state: JobState;
  wait_reason?: WaitReason;
  /** True when content may already have been accepted and billed. */
  possibly_billed: boolean;
}

export interface JobFailedPayload {
  type: 'job_failed';
  state: JobState;
  /** Stable AppErrorCode from the shared closed set (§15.4). */
  code: string;
  retryable: boolean;
}

export interface RecordingImportedPayload {
  type: 'recording_imported';
  media_sha256: string;
  byte_length: number;
}

export interface RecordingSourceUpdatedPayload {
  type: 'recording_source_updated';
  change: SourceChange;
}

export interface RecordingDeletedPayload {
  type: 'recording_deleted';
}

export interface TranscriptCompletedPayload {
  type: 'transcript_completed';
  transcript_revision: string;
  segment_count: number;
}

export interface ToolCompletedPayload {
  type: 'tool_completed';
  tool: string;
  output_id: string;
}

export interface ModelAvailabilityChangedPayload {
  type: 'model_availability_changed';
  engine: string;
  available: boolean;
}

export interface ConsentRequiredPayload {
  type: 'consent_required';
  purpose: string;
}

export interface LibraryChangedPayload {
  type: 'library_changed';
}

export type JobEventPayload =
  | JobUpdatedPayload
  | JobFailedPayload
  | RecordingImportedPayload
  | RecordingSourceUpdatedPayload
  | RecordingDeletedPayload
  | TranscriptCompletedPayload
  | ToolCompletedPayload
  | ModelAvailabilityChangedPayload
  | ConsentRequiredPayload
  | LibraryChangedPayload;

export interface JobEvent {
  version: typeof UI_EVENT_CONTRACT_VERSION;
  /** Durable, strictly monotonic and gap-free local sequence number. */
  seq: number;
  kind: JobEventKind;
  job_id?: string;
  recording_id?: string;
  /** Milliseconds since the Unix epoch, UTC. */
  occurred_at: number;
  payload: JobEventPayload;
}

/** One row of the Jobs view, returned by `list_jobs`. */
export interface JobSummary {
  job_id: string;
  job_type: string;
  state: JobState;
  phase: JobPhase;
  retry_class: RetryClass;
  wait_reason?: WaitReason;
  retry_count: number;
  recording_id?: string;
  last_error_code?: string;
  created_at: number;
  started_at?: number;
  finished_at?: number;
  possibly_billed: boolean;
  is_reprocess: boolean;
  output_count: number;
}

/** A page of job summaries plus the event watermark at query time. */
export interface JobHistoryPage {
  jobs: JobSummary[];
  total: number;
  /** Highest durable event sequence when this page was read. */
  watermark_seq: number;
}

/** One entry in the in-app activity history (§5.5). */
export interface ActivityEntry {
  seq: number;
  kind: JobEventKind;
  job_id?: string;
  recording_id?: string;
  occurred_at: number;
  completion_notice: boolean;
}

/**
 * Fold an event stream into a resync decision.
 *
 * Returns the new watermark and whether the caller must refetch. A restart
 * (last === 0) always refetches, because the frontend holds no state.
 */
export function trackSequence(
  last: number,
  event: Pick<JobEvent, 'seq'>,
): { watermark: number; mustRefetch: boolean } {
  if (last === 0) {
    return { watermark: event.seq, mustRefetch: true };
  }
  if (event.seq <= last) {
    // Replay of an already-seen event: idempotent, no refetch needed.
    return { watermark: last, mustRefetch: false };
  }
  // The sequence is gap-free by construction, so any hole means the frontend
  // missed an event and must refetch rather than patch its local cache.
  return { watermark: event.seq, mustRefetch: event.seq !== last + 1 };
}
