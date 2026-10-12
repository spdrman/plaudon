export const JOB_STATE_PRESENTATION = {
    queued: {
        label: "Queued",
        glyph: "…",
        tone: "neutral",
        description: "Waiting for a free slot.",
        canCancel: true,
        canRetry: false,
        terminal: false,
        requiresCostAck: false,
    },
    preflight: {
        label: "Preflight",
        glyph: "?",
        tone: "running",
        description: "Checking inputs, space and engine availability before starting.",
        canCancel: true,
        canRetry: false,
        terminal: false,
        requiresCostAck: false,
    },
    waiting_for_user: {
        label: "Waiting for you",
        glyph: "!",
        tone: "waiting",
        description: "This job needs a decision from you before it can continue.",
        canCancel: true,
        canRetry: false,
        terminal: false,
        requiresCostAck: false,
    },
    waiting_for_network: {
        label: "Waiting for network",
        glyph: "~",
        tone: "waiting",
        description: "Queued until a network connection is available again.",
        canCancel: true,
        canRetry: false,
        terminal: false,
        requiresCostAck: false,
    },
    waiting_for_model: {
        label: "Waiting for model",
        glyph: "M",
        tone: "waiting",
        description: "Paused because the model assets are not available right now.",
        canCancel: true,
        canRetry: false,
        terminal: false,
        requiresCostAck: false,
    },
    waiting_for_source: {
        label: "Waiting for source",
        glyph: "S",
        tone: "waiting",
        description: "Paused because the source file or device is unavailable.",
        canCancel: true,
        canRetry: false,
        terminal: false,
        requiresCostAck: false,
    },
    waiting_for_authorization: {
        label: "Waiting for authorization",
        glyph: "K",
        tone: "waiting",
        description: "Paused until the required consent or credential is granted.",
        canCancel: true,
        canRetry: false,
        terminal: false,
        requiresCostAck: false,
    },
    running_local: {
        label: "Running locally",
        glyph: ">",
        tone: "running",
        description: "Processing on this computer; nothing leaves the device.",
        canCancel: true,
        canRetry: false,
        terminal: false,
        requiresCostAck: false,
    },
    cloud_ready: {
        label: "Ready to send",
        glyph: "^",
        tone: "running",
        description: "Prepared for the cloud provider; nothing has been sent yet.",
        canCancel: true,
        canRetry: false,
        terminal: false,
        requiresCostAck: false,
    },
    cloud_sending: {
        label: "Sending to provider",
        glyph: "^^",
        tone: "running",
        description: "The request is in flight to the cloud provider.",
        canCancel: true,
        canRetry: false,
        terminal: false,
        requiresCostAck: false,
    },
    cloud_outcome_unknown: {
        label: "Cloud outcome unknown",
        glyph: "??",
        tone: "warning",
        description: "The provider response was lost. The request may already have been accepted and billed, so it is never retried automatically.",
        canCancel: false,
        canRetry: true,
        terminal: false,
        requiresCostAck: true,
    },
    verifying: {
        label: "Verifying",
        glyph: "=",
        tone: "running",
        description: "Checking the produced output before committing it.",
        canCancel: true,
        canRetry: false,
        terminal: false,
        requiresCostAck: false,
    },
    committing: {
        label: "Committing",
        glyph: "+",
        tone: "running",
        description: "Writing the output durably; this step is not interrupted.",
        canCancel: false,
        canRetry: false,
        terminal: false,
        requiresCostAck: false,
    },
    cancelling: {
        label: "Cancelling",
        glyph: "x-",
        tone: "warning",
        description: "Stopping our remaining work and discarding incomplete output.",
        canCancel: false,
        canRetry: false,
        terminal: false,
        requiresCostAck: false,
    },
    completed: {
        label: "Completed",
        glyph: "OK",
        tone: "success",
        description: "The output and its metadata are durably committed.",
        canCancel: false,
        canRetry: false,
        terminal: true,
        requiresCostAck: false,
    },
    failed_retryable: {
        label: "Failed — can retry",
        glyph: "!r",
        tone: "danger",
        description: "The job stopped on an error that may succeed on another attempt.",
        canCancel: false,
        canRetry: true,
        terminal: false,
        requiresCostAck: false,
    },
    failed_terminal: {
        label: "Failed — cannot retry",
        glyph: "!!",
        tone: "danger",
        description: "The job stopped for a reason retrying cannot fix.",
        canCancel: false,
        canRetry: false,
        terminal: true,
        requiresCostAck: false,
    },
    cancelled: {
        label: "Cancelled",
        glyph: "x",
        tone: "neutral",
        description: "You stopped this job. Incomplete output was discarded.",
        canCancel: false,
        canRetry: true,
        terminal: true,
        requiresCostAck: false,
    },
};
/** §14.7: cloud cancellation cannot recall an accepted request or a charge. */
export const CANCEL_DISCLOSURE = "Cancelling stops our remaining work only. A cloud request that was already accepted may not be reversed, and an upstream charge may still apply.";
export const RETRY_CONTROL = { label: "Retry", intent: "retry" };
export const KEEP_QUEUED_CONTROL = { label: "Keep queued", intent: "keep_queued" };
export const CHOOSE_LOCAL_ENGINE_CONTROL = {
    label: "Choose local engine",
    intent: "choose_local_engine",
};
/**
 * Secondary affordance asked for by a §15.4 envelope. `action` may be omitted
 * entirely by Rust: an omitted action still offers Retry when the envelope is
 * retryable, otherwise the message stands alone.
 */
export function envelopeActionControl(action, retryable = false) {
    switch (action) {
        case "WAIT_AND_RETRY":
            return { label: "Wait and retry", intent: "retry" };
        case "RETRY_IMPORT":
            return { label: "Retry import", intent: "retry_import" };
        case "CHOOSE_ANOTHER_MODEL":
            return { label: "Choose another model", intent: "choose_local_engine" };
        case "FREE_DISK_SPACE":
            return { label: "Free up space", intent: "free_space" };
        case "OPEN_SETTINGS":
            return { label: "Open settings", intent: "open_settings" };
        case "OPEN_PERMISSION_SETTINGS":
            return { label: "Open permission settings", intent: "open_permission_settings" };
        case "OPEN_PLAUD_CONNECT":
            return { label: "Open PLAUD Cloud settings", intent: "open_plaud_connect" };
        case "OPEN_CLI_SETUP":
            return { label: "Set up the official CLI", intent: "open_cli_setup" };
        case "REFRESH_SOURCE_LINK":
            return { label: "Refresh source link", intent: "refresh_source_link" };
        case "REVIEW_CONSENT":
            return { label: "Review consent", intent: "review_consent" };
        case "REVIEW_LOCAL_ONLY_MODE":
            return { label: "Review Local-only Mode", intent: "review_local_only_mode" };
        case "OPEN_DEVICE_DIAGNOSTICS":
            return { label: "Open device diagnostics", intent: "open_device_diagnostics" };
        case "CHARGE_DEVICE":
            return { label: "Charge the device", intent: "charge_device" };
        case "OPEN_DIAGNOSTICS":
            return { label: "Open diagnostics", intent: "open_diagnostics" };
        case undefined:
        case null:
            return retryable ? RETRY_CONTROL : null;
    }
}
/**
 * §5.4 conditions keyed by the domain error code the job carries. The required
 * corrective controls come from the code, not from `action`; `action` only adds
 * the secondary affordance.
 */
export const ERROR_GUIDANCE = {
    SOURCE_UNAVAILABLE: {
        title: "OpenRouter is unavailable",
        controls: [RETRY_CONTROL, CHOOSE_LOCAL_ENGINE_CONTROL, KEEP_QUEUED_CONTROL],
        suppressRetry: false,
        showCapacity: false,
    },
    LOCAL_ONLY_MODE: {
        title: "You are offline",
        controls: [
            KEEP_QUEUED_CONTROL,
            RETRY_CONTROL,
            { label: "Review Local-only Mode", intent: "review_local_only_mode" },
        ],
        suppressRetry: false,
        showCapacity: false,
    },
    STORAGE_FULL: {
        title: "Not enough space",
        controls: [
            { label: "Free up space", intent: "free_space" },
            { label: "Open storage settings", intent: "open_storage_settings" },
        ],
        suppressRetry: false,
        showCapacity: true,
    },
    BUDGET_EXCEEDED: {
        title: "Provider credits exhausted",
        controls: [{ label: "Open key settings", intent: "open_ai_settings" }],
        suppressRetry: true,
        showCapacity: false,
    },
    OUTCOME_UNKNOWN: {
        title: "Cloud request outcome unknown",
        controls: [],
        suppressRetry: false,
        showCapacity: false,
    },
};
export function errorGuidance(code) {
    if (!code)
        return null;
    return ERROR_GUIDANCE[code] ?? null;
}
/**
 * Elapsed time between `started_at` and `finished_at` (or `now` while running).
 * Returns `null` when the backend has not reported a start: unknown stays
 * unknown, we never render `0` (§8.6A).
 */
export function elapsedMs(startedAt, finishedAt, now = Date.now()) {
    if (!startedAt)
        return null;
    const start = Date.parse(startedAt);
    if (Number.isNaN(start))
        return null;
    const end = finishedAt ? Date.parse(finishedAt) : now;
    if (Number.isNaN(end))
        return null;
    const delta = end - start;
    return delta < 0 ? null : delta;
}
export function formatElapsed(ms) {
    if (ms === null)
        return null;
    const total = Math.floor(ms / 1000);
    const hours = Math.floor(total / 3600);
    const minutes = Math.floor((total % 3600) / 60);
    const seconds = total % 60;
    if (hours > 0)
        return `${hours}h ${String(minutes).padStart(2, "0")}m`;
    if (minutes > 0)
        return `${minutes}m ${String(seconds).padStart(2, "0")}s`;
    return `${seconds}s`;
}
export function jobElapsedLabel(startedAt, finishedAt, now) {
    const text = formatElapsed(elapsedMs(startedAt, finishedAt, now));
    return text === null ? "Elapsed time unknown" : `Elapsed ${text}`;
}
export const JOB_KIND_LABELS = {
    import: "Import",
    transcribe: "Transcription",
    summarize: "Summary",
    export: "Export",
    model_install: "Model install",
    source_sync: "Source sync",
};
