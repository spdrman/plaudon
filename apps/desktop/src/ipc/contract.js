/**
 * Typed Tauri command/event contract (spec §15).
 *
 * Every command name here appears in the §15.2 table except the two documented
 * additions recorded in docs/adr/ADR-0020-command-surface.md (`list_activity`,
 * `complete_onboarding`); `schemas/commands/*.schema.json` is the versioned
 * wire contract and `contract.schema.test.ts` fails CI on drift.
 */
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
];
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
];
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
];
/* ----------------------------------------------------- settings / activity */
export const NOTIFICATION_CATEGORIES = [
    "import_finished",
    "transcription_finished",
    "requires_attention",
    "cloud_budget_exceeded",
];
/** Commands quoted verbatim from the §15.2 table. */
export const SPEC_COMMANDS = [
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
export const ADR_0020_COMMANDS = ["list_activity", "complete_onboarding"];
/** EPIC K source commands, quoted from the §15.2 table. */
export const EPIC_K_COMMANDS = [
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
export const ALL_COMMANDS = [
    ...SPEC_COMMANDS,
    ...ADR_0020_COMMANDS,
    ...EPIC_K_COMMANDS,
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
];
/** Narrows an event payload to an activation target; returns null when absent. */
export function activationTargetOf(event) {
    if (event.kind !== "ui.activation_requested")
        return null;
    const target = event.payload.target;
    if (!target || typeof target !== "object")
        return null;
    if (!("kind" in target) || !("id" in target))
        return null;
    const kind = target.kind;
    const id = target.id;
    if (typeof id !== "string")
        return null;
    if (kind !== "recording" && kind !== "job" && kind !== "settings")
        return null;
    return { kind, id };
}
