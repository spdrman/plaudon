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
];
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
];
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
];
/**
 * Fold an event stream into a resync decision.
 *
 * Returns the new watermark and whether the caller must refetch. A restart
 * (last === 0) always refetches, because the frontend holds no state.
 */
export function trackSequence(last, event) {
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
