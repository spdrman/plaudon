/**
 * Deterministic in-memory implementation of the §15 command/event surface.
 *
 * It exists so the UI can be developed and tested without the Rust process:
 * unit tests, component tests and the Playwright e2e suite all drive this
 * transport. It is NOT shipped in the production bundle — `main.tsx` only
 * installs it when `import.meta.env.DEV` and the `?harness=1` flag are present.
 */
import type {
  CreateBackupResponse,
  DeleteRecordingResponse,
  DeletionDisclosureDto,
  ExportMediaResponse,
  InspectArchiveResponse,
  RestoreArchiveResponse,
  ActivityEntry,
  ActivityPage,
  Capabilities,
  CloudBudget,
  CommandName,
  ConsentSummary,
  CostValue,
  ErrorEnvelope,
  EventKind,
  ImportOutcome,
  JobSummary,
  ListModelsResponse,
  ModelCard,
  MoneyAmount,
  NotificationSettings,
  PlaudConnectorSnapshot,
  PlaudImportOutcome,
  PlaudListResponse,
  Preferences,
  ProcessingStage,
  RecordingDetail,
  RecordingPage,
  RecordingSummary,
  TranscriptPage,
  TranscriptRevisionRef,
  TranscriptSegment,
  UiEvent,
  UsageAttempt,
  UsageTotals,
} from "../ipc/contract";
import type { Transport, Unlisten } from "../ipc/client";
import {
  CAPABILITY_CHECK_CLIP_MS,
  CONSENT_PURPOSE_BY_STAGE,
} from "../features/cloud/cloudPresentation";
import { estimateDispatchCost, parseMicros, usd } from "../features/cloud/money";
import { FakeDevices } from "./fakeDevices";

export interface FakeRecording extends RecordingDetail {
  segments: Map<string, TranscriptSegment[]>;
}

export interface FakeBackendOptions {
  recordings?: number;
  segmentsPerTranscript?: number;
  capabilities?: Partial<Capabilities>;
  /** Simulated OS notification permission. */
  osNotificationPermission?: "granted" | "denied" | "not_requested" | "unsupported";
  /** Replaces the seeded cloud catalogue for the listed stages. */
  models?: Partial<Record<ProcessingStage, ModelCard[]>>;
  /** Forces `list_models` to answer from the pinned allowlist (§10.1). */
  catalogDegradedReason?: ListModelsResponse["degraded_reason"];
}

const DEFAULT_NOTIFICATIONS: NotificationSettings = {
  enabled: true,
  categories: {
    import_finished: true,
    transcription_finished: true,
    requires_attention: true,
    cloud_budget_exceeded: true,
  },
};

/** §10.4: the fixed enforcement note the backend always returns with a budget. */
export const CLOUD_BUDGET_ENFORCEMENT_NOTE =
  "Local budgets cannot prevent charges made elsewhere with the same key. Set an OpenRouter-side key limit for hard enforcement.";

/** Version of the privacy statement the seeded consent flow records (§4.4 #5). */
export const DISCLOSURE_VERSION = "2026-02-01";

/** Used by the seeded catalogue to express sub-cent unit prices exactly. */
export { parseMicros, usd } from "../features/cloud/money";

function cloudModel(partial: Partial<ModelCard> & Pick<ModelCard, "model_id">): ModelCard {
  return {
    display_name: partial.model_id,
    engine: "openrouter",
    request_family: "unknown",
    verification: "not_verified",
    verification_reason: "catalog_only",
    fixture_id: null,
    validated_at: null,
    features: {
      segments: "unknown",
      word_timestamps: "unknown",
      diarization: "unknown",
      language_detection: "unknown",
    },
    locales: null,
    max_input_bytes: null,
    max_duration_ms: null,
    pricing: { basis: "other", unit_label: "pricing not published", amount_per_unit: null },
    privacy: {
      relay: "openrouter",
      downstream_provider: null,
      retention: "unknown",
      retention_days: null,
      zero_retention_available: false,
    },
    selectable: false,
    ...partial,
  };
}

/**
 * Seeded catalogue. It deliberately mixes a verified, dispatchable model with
 * models that are catalogue-only or whose privacy route never resolved, so the
 * picker's honest states are exercised by default.
 */
function seedModels(): Partial<Record<ProcessingStage, ModelCard[]>> {
  const verifiedStt = cloudModel({
    model_id: "acme/scribe-2",
    display_name: "Scribe 2",
    request_family: "multipart_stt",
    verification: "verified",
    verification_reason: null,
    fixture_id: "fixture-stt-001",
    validated_at: "2026-01-20T08:00:00Z",
    features: {
      segments: "yes",
      word_timestamps: "yes",
      diarization: "no",
      language_detection: "yes",
    },
    locales: ["en", "de"],
    max_input_bytes: 26_214_400,
    max_duration_ms: 3_600_000,
    pricing: {
      basis: "per_audio_second",
      unit_label: "USD per audio second",
      amount_per_unit: usd(100),
    },
    privacy: {
      relay: "openrouter",
      downstream_provider: "Acme Speech",
      retention: "limited_retention",
      retention_days: 30,
      zero_retention_available: true,
    },
    selectable: true,
  });
  const secondStt = cloudModel({
    model_id: "acme/scribe-mini",
    display_name: "Scribe Mini",
    request_family: "multipart_stt",
    verification: "verified",
    verification_reason: null,
    fixture_id: "fixture-stt-002",
    validated_at: "2026-01-21T08:00:00Z",
    features: {
      segments: "yes",
      word_timestamps: "no",
      diarization: "no",
      language_detection: "unknown",
    },
    locales: ["en"],
    max_input_bytes: 10_485_760,
    max_duration_ms: 1_800_000,
    pricing: {
      basis: "per_audio_second",
      unit_label: "USD per audio second",
      amount_per_unit: usd(40),
    },
    privacy: {
      relay: "openrouter",
      downstream_provider: "Acme Speech",
      retention: "zero_retention",
      retention_days: null,
      zero_retention_available: true,
    },
    selectable: true,
  });
  const unresolvedRoute = cloudModel({
    model_id: "nimbus/audio-chat-preview",
    display_name: "Nimbus Audio Chat (preview)",
    request_family: "audio_chat",
    verification: "not_verified",
    verification_reason: "privacy_route_unresolved",
    pricing: {
      basis: "per_token",
      unit_label: "USD per audio input token",
      amount_per_unit: usd(2),
    },
  });
  const catalogOnly = cloudModel({
    model_id: "vendor/catalog-audio-xl",
    display_name: "Catalog Audio XL",
  });
  const summarizer = cloudModel({
    model_id: "acme/summarize-1",
    display_name: "Summarize 1",
    request_family: "audio_chat",
    verification: "verified",
    verification_reason: null,
    fixture_id: "fixture-text-001",
    validated_at: "2026-01-22T08:00:00Z",
    pricing: { basis: "per_token", unit_label: "USD per output token", amount_per_unit: usd(5) },
    privacy: {
      relay: "openrouter",
      downstream_provider: "Acme Text",
      retention: "provider_default",
      retention_days: null,
      zero_retention_available: false,
    },
    selectable: true,
  });
  return {
    transcribe: [verifiedStt, secondStt, unresolvedRoute, catalogOnly],
    consolidated_summary: [summarizer, catalogOnly],
  };
}

export function envelope(
  code: string,
  message: string,
  extra: Partial<ErrorEnvelope> = {},
): ErrorEnvelope {
  return {
    code,
    message,
    retryable: false,
    action: null,
    diagnostic_id: `diag-${code.toLowerCase()}`,
    ...extra,
  };
}

/** A connector that is signed in and ready; tests override the parts they script. */
export function fakePlaudSnapshot(
  overrides: Partial<PlaudConnectorSnapshot["diagnostics"]> = {},
  disconnectVariant: PlaudConnectorSnapshot["disconnect"]["variant"] = "association_only",
): PlaudConnectorSnapshot {
  const diagnostics = {
    state: "ready" as const,
    headline: "Connected to PLAUD",
    detail: "Your PLAUD recordings are listed.",
    next_action: { kind: "none" as const },
    detected_version: "1.4.2",
    compatibility: "tested" as const,
    program_origin: "standard installation directory",
    last_error: null,
    warnings: [],
    ...overrides,
  };
  return {
    status: {
      state: diagnostics.state,
      detected_version: diagnostics.detected_version,
      version_validated: diagnostics.compatibility === "tested",
      program_origin: diagnostics.program_origin,
      last_error: diagnostics.last_error,
      listing_possibly_incomplete: false,
      scraping_warning: false,
      updated_at_ms: 1_760_000_000_000,
    },
    diagnostics,
    disconnect:
      disconnectVariant === "association_only"
        ? {
            variant: "association_only",
            label: "Disconnect from this app",
            body: "This removes the link between this app and the PLAUD command line tool. It does not sign you out of PLAUD.",
          }
        : {
            variant: "with_vendor_logout",
            label: "Disconnect and sign out",
            body: "This removes the link and runs the tool's sign-out command.",
          },
  };
}

export function fakePlaudListing(count = 2): PlaudListResponse {
  return {
    source_id: "src-plaud",
    items: Array.from({ length: count }, (_, index) => ({
      external_id: `rec_${index + 1}`,
      display_name: `Recording ${index + 1}`,
      captured_at: "2026-10-11T09:00:00Z",
      duration_ms: 90_000 * (index + 1),
      declared_byte_length: null,
      imported_recording_id: null,
    })),
    next_cursor: null,
    completeness: "complete",
    empty_account_advisory: null,
  };
}

export class FakeBackend implements Transport {
  recordings: FakeRecording[] = [];
  jobs: JobSummary[] = [];
  activity: ActivityEntry[] = [];
  preferences: Preferences;
  openRouterKeyPresent = false;
  /** Secrets the UI handed over; tests assert they are never echoed back. */
  receivedSecrets: string[] = [];
  osNotificationPermission: FakeBackendOptions["osNotificationPermission"];
  /** Every invoke the UI performed, for assertions. */
  calls: Array<{ command: CommandName; args: unknown }> = [];
  /** Commands forced to fail, keyed by command name. */
  failures = new Map<CommandName, ErrorEnvelope>();
  /** When true, emitted events skip a sequence number to simulate a dropped event. */
  dropNextEvent = false;
  /** What `plaud_probe` / `plaud_connect` return; tests script the connector state. */
  plaud: PlaudConnectorSnapshot = fakePlaudSnapshot();
  /** What `plaud_list` returns. */
  plaudListing: PlaudListResponse = fakePlaudListing();
  /** Overrides the per-ID outcome of `plaud_import`. */
  plaudImportOutcome: ((externalId: string) => PlaudImportOutcome) | null = null;

  /* ------------------------------------------------------- cloud state */

  /** Cloud catalogue per stage; replaceable wholesale by tests. */
  models: Partial<Record<ProcessingStage, ModelCard[]>> = seedModels();
  catalogFetchedAt: string | null = "2026-02-01T08:55:00Z";
  catalogDegradedReason: ListModelsResponse["degraded_reason"] = null;
  /** Per-task defaults (§10.2); a recording override never writes here. */
  defaultModelByStage: Partial<Record<ProcessingStage, string>> = {
    transcribe: "acme/scribe-2",
    consolidated_summary: "acme/summarize-1",
  };
  /** Per-recording overrides keyed `${recording_id}:${stage}`. */
  recordingModelOverrides = new Map<string, { engine: string; model_id: string }>();
  consents: ConsentSummary[] = [];
  attempts: UsageAttempt[] = [];
  budget: CloudBudget = {
    per_job_limit: null,
    monthly_limit: null,
    month_to_date: { confidence: "unknown", amount: null },
    blocks_dispatch: false,
    enforcement_note: CLOUD_BUDGET_ENFORCEMENT_NOTE,
  };
  /** When true, `start_processing` demands an explicit cost acknowledgement. */
  requireCostAcknowledgement = false;
  /** Direct PLAUD device lab (`device_*` commands). */
  devices = new FakeDevices(
    (kind, partial) => this.emit(kind, partial),
    (code, message, extra) => envelope(code, message, extra),
  );

  #seq = 0;
  #handlers = new Map<EventKind, Set<(e: UiEvent) => void>>();
  #capabilities: Capabilities;
  #idCounter = 0;

  #nextId(prefix: string): string {
    this.#idCounter += 1;
    return `${prefix}-${this.#idCounter}`;
  }

  constructor(options: FakeBackendOptions = {}) {
    this.osNotificationPermission = options.osNotificationPermission ?? "granted";
    this.preferences = {
      onboarding_completed: false,
      local_only_mode: true,
      default_stt_engine_id: null,
      default_text_engine_id: null,
      appearance: "system",
      contrast: "standard",
      notifications: DEFAULT_NOTIFICATIONS,
    };
    this.#capabilities = {
      os: { platform: "macos", version: "15.3" },
      hardware: { arch: "aarch64", logical_cores: 10, total_memory_bytes: 17_179_869_184 },
      engines: [
        {
          engine_id: "whisper.cpp",
          display_name: "Whisper (local)",
          stage: "stt",
          status: "supported",
          unavailable_reason: null,
          installed_models: [
            {
              model_id: "ggml-base.en",
              display_name: "base.en",
              size_bytes: 147_951_465,
              installed: false,
            },
          ],
          download_bytes_required: 147_951_465,
          requires_network: false,
        },
        {
          engine_id: "apple-speech",
          display_name: "Apple Speech (on-device)",
          stage: "stt",
          status: "unsupported",
          unavailable_reason: "Requires macOS 26 or newer on Apple silicon.",
          installed_models: [],
          download_bytes_required: null,
          requires_network: false,
        },
        {
          engine_id: "openrouter",
          display_name: "OpenRouter (cloud)",
          stage: "text",
          status: "unknown",
          unavailable_reason: "No API key configured.",
          installed_models: [],
          download_bytes_required: null,
          requires_network: true,
        },
      ],
      storage: {
        library_path: "/Users/tester/Library/Application Support/Plaudon",
        free_bytes: 220_000_000_000,
        total_bytes: 500_000_000_000,
      },
      integrations: { plaud_cloud: "unknown", openrouter: "unknown", plaud_device: "experimental" },
      permissions: { notifications: "not_requested", bluetooth: "not_requested" },
      preferences: this.preferences,
      openrouter_key_present: false,
      cloud_consents: [],
      ...options.capabilities,
    };
    if (options.models) this.models = { ...this.models, ...options.models };
    if (options.catalogDegradedReason !== undefined) {
      this.degradeCatalog(options.catalogDegradedReason);
    }
    this.seed(options.recordings ?? 0, options.segmentsPerTranscript ?? 0);
  }

  /* ----------------------------------------------------------- seeding */

  seed(recordings: number, segments: number): void {
    for (let i = 0; i < recordings; i += 1) this.addRecording({ segments });
  }

  addRecording(options: {
    title?: string;
    segments?: number;
    alignment?: "native" | "derived" | "none";
    durationMs?: number | null;
    transcriptionStatus?: RecordingSummary["transcription_status"];
    lastError?: ErrorEnvelope | null;
  } = {}): FakeRecording {
    const n = this.recordings.length;
    const id = `rec-${String(n + 1).padStart(5, "0")}`;
    const alignment = options.alignment ?? "native";
    const segmentCount = options.segments ?? 0;
    const revisionId = `${id}-rev-1`;
    const revision: TranscriptRevisionRef = {
      revision_id: revisionId,
      kind: "machine",
      created_at: isoAt(n, 60),
      engine_id: "whisper.cpp",
      model_id: "ggml-base.en",
      alignment_origin: alignment,
      segment_count: segmentCount,
      superseded: false,
    };
    const durationMs =
      options.durationMs === undefined ? (alignment === "none" ? null : 60_000 + n * 1_000) : options.durationMs;
    const rec: FakeRecording = {
      recording_id: id,
      title: options.title ?? `Weekly sync ${n + 1}`,
      recorded_at: isoAt(n, 0),
      imported_at: isoAt(n, 30),
      duration_ms: durationMs,
      source: { source_kind: "manual_import", label: "Manual import" },
      import_status: "imported",
      transcription_status:
        options.transcriptionStatus ?? (segmentCount > 0 ? "completed" : "none"),
      processing_engine: segmentCount > 0 ? "whisper.cpp" : null,
      last_error: options.lastError ?? null,
      tags: n % 3 === 0 ? ["standup"] : [],
      byte_length: 1_024 * (n + 1),
      sha256: `${"0".repeat(56)}${String(n).padStart(8, "0")}`.slice(0, 64),
      media: {
        observed_codec: "audio/mpeg",
        sample_rate_hz: 44_100,
        channels: 1,
        original_file_name: `weekly-sync-${n + 1}.mp3`,
      },
      revisions: segmentCount > 0 ? [revision] : [],
      current_revision_id: segmentCount > 0 ? revisionId : null,
      outputs: [],
      segments: new Map(),
    };
    if (segmentCount > 0) {
      rec.segments.set(revisionId, buildSegments(segmentCount, alignment));
    }
    this.recordings.push(rec);
    return rec;
  }

  addJob(job: Partial<JobSummary> & Pick<JobSummary, "state" | "kind">): JobSummary {
    this.#idCounter += 1;
    const full: JobSummary = {
      job_id: `job-${this.#idCounter}`,
      stage: null,
      progress: null,
      created_at: "2026-02-01T09:00:00Z",
      updated_at: "2026-02-01T09:00:05Z",
      started_at: null,
      finished_at: null,
      recording_id: null,
      label: job.kind,
      error: null,
      cost_acknowledgement_required: false,
      provider_request_id: null,
      output_link: null,
      cost: { confidence: "unknown", amount: null },
      ...job,
    };
    this.jobs.push(full);
    return full;
  }

  addActivity(entry: Partial<ActivityEntry> & Pick<ActivityEntry, "category" | "title">): ActivityEntry {
    this.#idCounter += 1;
    const full: ActivityEntry = {
      activity_id: `act-${this.#idCounter}`,
      occurred_at: "2026-02-01T09:00:10Z",
      body: "",
      notification_delivered: this.osNotificationPermission === "granted",
      notification_suppressed_reason:
        this.osNotificationPermission === "granted" ? null : "OS notifications are not permitted.",
      target: null,
      read: false,
      ...entry,
    };
    this.activity.unshift(full);
    return full;
  }

  /* ---------------------------------------------------------- transport */

  async listen(kind: EventKind, handler: (event: UiEvent) => void): Promise<Unlisten> {
    const set = this.#handlers.get(kind) ?? new Set();
    set.add(handler);
    this.#handlers.set(kind, set);
    return () => set.delete(handler);
  }

  emit(kind: EventKind, partial: Partial<UiEvent> = {}): UiEvent {
    this.#seq += 1;
    if (this.dropNextEvent) {
      // Simulate a lost delivery: the sequence advances but nothing is sent.
      this.dropNextEvent = false;
      this.#seq += 1;
    }
    const event: UiEvent = {
      seq: this.#seq,
      kind,
      occurred_at: "2026-02-01T09:00:00Z",
      job_id: null,
      recording_id: null,
      payload: {},
      ...partial,
    };
    for (const handler of this.#handlers.get(kind) ?? []) handler(event);
    return event;
  }

  async invoke(command: CommandName, args: unknown): Promise<unknown> {
    this.calls.push({ command, args });
    const failure = this.failures.get(command);
    if (failure) throw failure;
    const request = (args ?? {}) as Record<string, any>;
    switch (command) {
      case "get_capabilities":
        return {
          ...this.#capabilities,
          preferences: this.preferences,
          openrouter_key_present: this.openRouterKeyPresent,
          cloud_consents: this.consents.map((grant) => ({ ...grant })),
        };
      case "list_recordings":
        return this.#listRecordings(request);
      case "get_recording": {
        const rec = this.#requireRecording(request.recording_id);
        const { segments: _segments, ...detail } = rec;
        return detail;
      }
      case "get_transcript":
        return this.#getTranscript(request);
      case "save_transcript_revision":
        return this.#saveRevision(request);
      case "import_files":
        return { results: this.#import(request) };
      case "list_jobs":
        return this.#listJobs(request);
      case "cancel_job":
        return this.#updateJob(request.job_id, { state: "cancelling" });
      case "retry_job": {
        const job = this.#requireJob(request.job_id);
        if (job.cost_acknowledgement_required && request.cost_acknowledged !== true) {
          throw envelope(
            "COST_ACKNOWLEDGEMENT_REQUIRED",
            "This retry may be billed again. Confirm the cost to continue.",
            { action: "REVIEW_CONSENT" },
          );
        }
        return this.#updateJob(request.job_id, { state: "queued", error: null });
      }
      case "export_recording": {
        this.#requireRecording(request.recording_id);
        const job = this.addJob({ kind: "export", state: "queued", label: "Export" });
        return { job_id: job.job_id };
      }
      case "export_media": {
        this.#requireRecording(request.recording_id);
        const original = {
          file_name: "recording-audio.mp3",
          sha256: fakeSha("original"),
          byte_length: 2_048_000,
          is_original: true,
          label: "Original recording",
          stripped_metadata: [],
        };
        const response: ExportMediaResponse = {
          original,
          sanitized: request.include_sanitized_derivative
            ? {
                file_name: "recording-audio-sanitized.mp3",
                sha256: fakeSha("sanitized"),
                byte_length: 2_047_500,
                is_original: false,
                label: "Sanitized copy",
                stripped_metadata: ["ID3v2.PRIV", "ID3v2.GEOB"],
              }
            : null,
          sanitized_refusal: null,
        };
        return response;
      }
      case "get_deletion_disclosure": {
        const rec = this.#requireRecording(request.recording_id);
        const response: DeletionDisclosureDto = {
          disclosure_version: "1.0",
          recording_id: rec.recording_id,
          title: rec.title,
          has_annotation: false,
          media: [
            {
              sha256: rec.sha256,
              byte_length: rec.byte_length,
              quality_label: "Original",
              shared_with_other_recordings: false,
              bytes_will_be_removed: true,
            },
          ],
          categories: [
            { label: "Transcript revisions", count: rec.revisions.length },
            { label: "Generated outputs", count: rec.outputs.length },
          ],
          bytes_to_free: rec.byte_length,
          limits: ["Cloud recordings cannot be deleted from this device."],
          confirmation_token: fakeSha(`delete-${rec.recording_id}`),
        };
        return response;
      }
      case "delete_recording": {
        const rec = this.#requireRecording(request.recording_id);
        const expectedToken = fakeSha(`delete-${rec.recording_id}`);
        if (request.confirmation_token !== expectedToken) {
          throw envelope("INVALID_ARGUMENT", "The confirmation token is stale or invalid.");
        }
        const index = this.recordings.findIndex((r) => r.recording_id === rec.recording_id);
        if (index >= 0) this.recordings.splice(index, 1);
        this.emit("library.changed");
        const response: DeleteRecordingResponse = {
          deletion_id: `del-${this.#idCounter++}`,
          blobs_removed: 1,
          blobs_retained_shared: 0,
          bytes_freed: rec.byte_length,
          tombstoned: true,
        };
        return response;
      }
      case "create_portable_backup": {
        const response: CreateBackupResponse = {
          file_name: `plaudon-backup-${new Date().toISOString().split("T")[0]}.plaudon`,
          encrypted: request.password !== null && request.password !== undefined,
          protection_disclosure:
            request.password !== null && request.password !== undefined
              ? "This backup is encrypted with your password."
              : "This backup is NOT encrypted. Anyone with the file can read your recordings.",
          entry_count: this.recordings.length * 3,
          total_byte_length: this.recordings.reduce((sum, r) => sum + r.byte_length, 0),
          file_byte_length: this.recordings.reduce((sum, r) => sum + r.byte_length, 0) + 1024,
          integrity_digest: fakeSha("backup-integrity"),
          integrity_note: "The digest proves the manifest matches the backup, not that the data is correct.",
          manifest_schema_version: "1.0",
          nonportable_settings: ["OpenRouter API key", "Notification permissions"],
        };
        if (request.password) this.receivedSecrets.push(request.password);
        return response;
      }
      case "inspect_restore_archive": {
        if (request.password) this.receivedSecrets.push(request.password);
        const response: InspectArchiveResponse = {
          encrypted: true,
          manifest_schema_version: "1.0",
          format_version: 1,
          created_at: "2026-01-15T12:30:00Z",
          source_os: "macos",
          entries_by_namespace: [
            { label: "Recordings", count: 5 },
            { label: "Transcripts", count: 5 },
            { label: "Media", count: 5 },
          ],
          total_byte_length: 10_240_000,
          payloads_verified: true,
          integrity_digest: fakeSha("archive-integrity"),
          integrity_note: "The digest proves the manifest matches the archive, not that the data is correct.",
          nonportable_settings: ["OpenRouter API key", "Notification permissions"],
          conflicts: this.recordings.length > 0
            ? [
                {
                  entry_path: "recordings/rec-00001",
                  reason: "A recording with this ID already exists.",
                },
              ]
            : [],
          target_is_empty: this.recordings.length === 0,
        };
        return response;
      }
      case "restore_portable_archive": {
        if (request.password) this.receivedSecrets.push(request.password);
        if (request.mode === "new_library" && this.recordings.length > 0) {
          throw envelope(
            "CONFLICT",
            "The library must be empty to restore as a new library.",
          );
        }
        const response: RestoreArchiveResponse = {
          recordings_restored: 5,
          transcripts_restored: 5,
          outputs_restored: 3,
          sources_restored: 2,
          media_committed: 5,
          media_reused: 0,
          conflicts: request.on_conflict === "skip" && this.recordings.length > 0
            ? [
                {
                  entry_path: "recordings/rec-00001",
                  reason: "A recording with this ID already exists.",
                },
              ]
            : [],
        };
        for (let i = 0; i < 5; i += 1) {
          this.addRecording({ title: `Restored recording ${i + 1}`, segments: 10 });
        }
        this.emit("library.changed");
        return response;
      }
      case "set_notification_settings":
        this.preferences = { ...this.preferences, notifications: request.settings };
        return {
          settings: this.preferences.notifications,
          os_permission: this.osNotificationPermission,
        };
      case "set_local_only_mode": {
        if (request.confirmed !== true) {
          throw envelope("CONFIRMATION_REQUIRED", "Confirm the change to network policy.");
        }
        this.preferences = { ...this.preferences, local_only_mode: Boolean(request.enabled) };
        return {
          enabled: this.preferences.local_only_mode,
          description: this.preferences.local_only_mode
            ? "All outbound network access is denied."
            : "Outbound access allowed only to providers you enable.",
          allowed_hosts: this.preferences.local_only_mode ? [] : ["openrouter.ai"],
        };
      }
      case "set_openrouter_key": {
        if (typeof request.key !== "string" || request.key.trim().length === 0) {
          throw envelope("INVALID_ARGUMENT", "The API key is empty.");
        }
        this.receivedSecrets.push(request.key);
        this.openRouterKeyPresent = true;
        return { ok: true };
      }
      case "remove_openrouter_key":
        this.openRouterKeyPresent = false;
        return { ok: true };
      case "list_activity":
        return this.#listActivity(request);
      case "complete_onboarding": {
        this.preferences = {
          ...this.preferences,
          onboarding_completed: true,
          default_stt_engine_id: request.default_stt_engine_id ?? null,
          default_text_engine_id: request.default_text_engine_id ?? null,
          local_only_mode: Boolean(request.local_only_mode),
          notifications: request.notifications ?? this.preferences.notifications,
        };
        return this.preferences;
      }
      case "plaud_probe":
      case "plaud_connect":
        return this.plaud;
      case "plaud_disconnect":
        return { disconnect: this.plaud.disconnect, snapshot: this.plaud };
      case "plaud_list":
        return this.plaudListing;
      case "plaud_import":
        return {
          results: (request.external_ids as string[]).map(
            (id): PlaudImportOutcome =>
              this.plaudImportOutcome?.(id) ?? {
                outcome: "imported",
                external_id: id,
                job_id: this.#nextId("job"),
                recording_id: this.#nextId("rec"),
              },
          ),
        };
      case "source_add_folder":
        return { source_id: this.#nextId("src"), rule: request.rule, recursive: request.recursive };
      case "source_sync":
        return {
          job_id: this.#nextId("job"),
          source_id: request.source_id,
          source_kind: "plaud_cloud",
          state: "completed",
          folder: null,
          listed: this.plaudListing.items.length,
          imported_recording_ids: [],
        };
      case "source_reconcile":
        return {
          job_id: this.#nextId("job"),
          source_id: request.source_id,
          source_kind: "plaud_cloud",
          decisions: [],
          listing_complete: false,
          folder: null,
        };
      case "list_source_artifacts":
        return { artifacts: [] };
      case "list_models":
        return this.#listModels(request);
      case "get_usage":
        return this.#getUsage(request);
      case "set_cloud_budget":
        return this.#setBudget(request);
      case "grant_cloud_consent":
        return this.#grantConsent(request);
      case "revoke_cloud_consent":
        return this.#revokeConsent(request);
      case "set_model_preference":
        return this.#setModelPreference(request);
      case "start_processing":
        return this.#startProcessing(request);
      case "verify_model":
        return this.#verifyModel(request);
      case "list_nearby_devices":
      case "device_probe":
      case "device_authorize":
      case "device_list_recordings":
      case "device_import":
      case "device_disconnect":
      case "device_sync_policy":
        return this.devices.invoke(command, request);
      default: {
        const never: never = command;
        throw envelope("INTERNAL_ERROR", `No such command: ${String(never)}`);
      }
    }
  }

  /* ----------------------------------------------------------- handlers */

  #requireRecording(id: string): FakeRecording {
    const rec = this.recordings.find((r) => r.recording_id === id);
    if (!rec) throw envelope("RECORD_NOT_FOUND", "That recording no longer exists.");
    return rec;
  }

  #requireJob(id: string): JobSummary {
    const job = this.jobs.find((j) => j.job_id === id);
    if (!job) throw envelope("RECORD_NOT_FOUND", "That job no longer exists.");
    return job;
  }

  #listRecordings(request: Record<string, any>): RecordingPage {
    const filter = request.filter ?? {};
    const query = (filter.query ?? "").trim().toLowerCase();
    let items = this.recordings.filter((rec) => {
      if (query && !rec.title.toLowerCase().includes(query)) return false;
      if (filter.import_status?.length && !filter.import_status.includes(rec.import_status)) return false;
      if (
        filter.transcription_status?.length &&
        !filter.transcription_status.includes(rec.transcription_status)
      ) {
        return false;
      }
      if (filter.sources?.length && !filter.sources.includes(rec.source.source_kind)) return false;
      if (filter.tags?.length && !filter.tags.some((t: string) => rec.tags.includes(t))) return false;
      const when = rec.recorded_at ?? rec.imported_at;
      if (filter.date_from && when < filter.date_from) return false;
      if (filter.date_to && when > filter.date_to) return false;
      return true;
    });
    const field = request.sort?.field ?? "recorded_at";
    const dir = request.sort?.direction === "asc" ? 1 : -1;
    items = [...items].sort((a, b) => {
      const av = sortValue(a, field);
      const bv = sortValue(b, field);
      if (av === bv) return a.recording_id < b.recording_id ? -1 : 1;
      return av < bv ? -dir : dir;
    });
    const limit = Number(request.limit ?? 50);
    // Cursor is the stable id of the last row of the previous page.
    const start = request.cursor ? items.findIndex((r) => r.recording_id === request.cursor) + 1 : 0;
    const slice = items.slice(start, start + limit).map(stripSegments);
    const next = start + limit < items.length ? slice[slice.length - 1]?.recording_id ?? null : null;
    return { items: slice, next_cursor: next, total: items.length };
  }

  #getTranscript(request: Record<string, any>): TranscriptPage {
    const rec = this.recordings.find((r) =>
      r.revisions.some((v) => v.revision_id === request.revision_id),
    );
    if (!rec) throw envelope("RECORD_NOT_FOUND", "That transcript revision does not exist.");
    const revision = rec.revisions.find((v) => v.revision_id === request.revision_id)!;
    const all = rec.segments.get(revision.revision_id) ?? [];
    const cursor = Number(request.cursor ?? 0);
    const limit = Number(request.limit ?? 200);
    const segments = all.slice(cursor, cursor + limit);
    const next = cursor + limit < all.length ? cursor + limit : null;
    return { revision, segments, next_cursor: next, total_segments: all.length };
  }

  #saveRevision(request: Record<string, any>): unknown {
    const rec = this.#requireRecording(request.recording_id);
    const head = rec.current_revision_id;
    if (head !== request.base_revision_id) {
      throw envelope(
        "CONFLICT",
        "This transcript changed since you opened it. Review the newer revision before saving.",
      );
    }
    const base = rec.revisions.find((v) => v.revision_id === head)!;
    const segments = [...(rec.segments.get(base.revision_id) ?? [])];
    for (const edit of request.edits ?? []) {
      const current = segments[edit.index];
      if (!current) throw envelope("INVALID_ARGUMENT", "Edited segment is out of range.");
      segments[edit.index] = {
        ...current,
        text: edit.text,
        annotations: [
          ...current.annotations.filter((a) => a.kind !== "edited"),
          {
            kind: "edited",
            reason: "Edited by you",
            segment_index: edit.index,
            start_ms: current.start_ms,
            end_ms: current.end_ms,
          },
        ],
      };
    }
    const revisionId = `${rec.recording_id}-rev-${rec.revisions.length + 1}`;
    const revision: TranscriptRevisionRef = {
      revision_id: revisionId,
      kind: "human",
      created_at: new Date().toISOString(),
      engine_id: base.engine_id,
      model_id: base.model_id,
      alignment_origin: base.alignment_origin,
      segment_count: segments.length,
      superseded: false,
    };
    rec.revisions = rec.revisions.map((v) =>
      v.revision_id === base.revision_id ? { ...v, superseded: true } : v,
    );
    rec.revisions.push(revision);
    rec.segments.set(revisionId, segments);
    rec.current_revision_id = revisionId;
    const stale = rec.outputs.filter((o) => o.source_revision_id !== revisionId);
    rec.outputs = rec.outputs.map((o) =>
      o.source_revision_id === revisionId ? o : { ...o, stale: true },
    );
    this.emit("transcript.completed", { recording_id: rec.recording_id });
    return { revision_id: revisionId, stale_output_ids: stale.map((o) => o.output_id) };
  }

  #import(request: Record<string, any>): ImportOutcome[] {
    const policy = request.duplicate_policy ?? "ask";
    const results: ImportOutcome[] = [];
    for (const path of request.paths ?? []) {
      const name = String(path).split(/[\\/]/).pop() ?? String(path);
      const ext = name.includes(".") ? name.slice(name.lastIndexOf(".") + 1).toLowerCase() : "";
      if (!["wav", "mp3", "m4a", "flac", "ogg", "opus"].includes(ext)) {
        results.push({
          outcome: "rejected",
          path,
          error: envelope("UNSUPPORTED_MEDIA", `${name} is not a supported audio file.`),
        });
        continue;
      }
      const sha = fakeSha(name);
      const existing = this.recordings.find((r) => r.sha256 === sha);
      if (existing && policy === "ask") {
        results.push({
          outcome: "duplicate",
          path,
          sha256: sha,
          existing_recording_id: existing.recording_id,
          existing_title: existing.title,
        });
        continue;
      }
      if (existing && policy === "skip") {
        results.push({ outcome: "skipped", path, sha256: sha, existing_recording_id: existing.recording_id });
        continue;
      }
      if (existing && policy === "open_existing") {
        results.push({ outcome: "skipped", path, sha256: sha, existing_recording_id: existing.recording_id });
        continue;
      }
      const rec = this.addRecording({ title: name.replace(/\.[^.]+$/, "") });
      rec.sha256 = sha;
      rec.media.original_file_name = name;
      const job = this.addJob({
        kind: "import",
        state: "completed",
        label: rec.title,
        recording_id: rec.recording_id,
        output_link: { kind: "recording", id: rec.recording_id },
      });
      this.addActivity({
        category: "import_finished",
        title: "Import finished",
        body: "1 recording was added to your library.",
        target: { kind: "recording", id: rec.recording_id },
      });
      this.emit("recording.imported", { recording_id: rec.recording_id, job_id: job.job_id });
      this.emit("library.changed");
      results.push({ outcome: "accepted", path, job_id: job.job_id });
    }
    return results;
  }

  #listJobs(request: Record<string, any>) {
    let items = this.jobs;
    if (request.states?.length) items = items.filter((j) => request.states.includes(j.state));
    if (request.kinds?.length) items = items.filter((j) => request.kinds.includes(j.kind));
    const counts: Record<string, number> = {};
    for (const job of this.jobs) counts[job.state] = (counts[job.state] ?? 0) + 1;
    const limit = Number(request.limit ?? 50);
    const start = request.cursor ? items.findIndex((j) => j.job_id === request.cursor) + 1 : 0;
    const slice = items.slice(start, start + limit);
    return {
      items: slice,
      next_cursor: start + limit < items.length ? slice[slice.length - 1]?.job_id ?? null : null,
      state_counts: counts,
    };
  }

  #listActivity(request: Record<string, any>): ActivityPage {
    const limit = Number(request.limit ?? 50);
    const start = request.cursor
      ? this.activity.findIndex((a) => a.activity_id === request.cursor) + 1
      : 0;
    const slice = this.activity.slice(start, start + limit);
    return {
      items: slice,
      next_cursor:
        start + limit < this.activity.length ? slice[slice.length - 1]?.activity_id ?? null : null,
      unread_count: this.activity.filter((a) => !a.read).length,
    };
  }

  #updateJob(id: string, patch: Partial<JobSummary>): JobSummary {
    const job = this.#requireJob(id);
    Object.assign(job, patch, { updated_at: new Date().toISOString() });
    this.emit("job.updated", { job_id: job.job_id });
    return job;
  }

  /* -------------------------------------------------------- cloud knobs */

  setModels(stage: ProcessingStage, models: ModelCard[]): void {
    this.models = { ...this.models, [stage]: models };
  }

  addModel(
    stage: ProcessingStage,
    partial: Partial<ModelCard> & Pick<ModelCard, "model_id">,
  ): ModelCard {
    const card = cloudModel(partial);
    this.models = { ...this.models, [stage]: [...(this.models[stage] ?? []), card] };
    return card;
  }

  /** §10.1: discovery failed, so answers come from the pinned allowlist. */
  degradeCatalog(reason: ListModelsResponse["degraded_reason"]): void {
    this.catalogDegradedReason = reason;
    this.catalogFetchedAt = reason === null ? "2026-02-01T08:55:00Z" : null;
  }

  /** Turns off local-only mode and records a key, so cloud dispatch is possible. */
  enableCloud(): void {
    this.preferences = { ...this.preferences, local_only_mode: false };
    this.openRouterKeyPresent = true;
  }

  addConsent(
    partial: Partial<ConsentSummary> & Pick<ConsentSummary, "model_id">,
  ): ConsentSummary {
    this.#idCounter += 1;
    const grant: ConsentSummary = {
      consent_id: `consent-${this.#idCounter}`,
      purpose: "transcription",
      content_type: "audio_bytes",
      scope_kind: "model_policy",
      recording_id: null,
      downstream_provider: null,
      required_retention: null,
      disclosure_version: DISCLOSURE_VERSION,
      granted_at: this.#stamp(),
      expires_at: null,
      revoked_at: null,
      ...partial,
    };
    this.consents.push(grant);
    return grant;
  }

  addAttempt(partial: Partial<UsageAttempt> & Pick<UsageAttempt, "model_id">): UsageAttempt {
    this.#idCounter += 1;
    const attempt: UsageAttempt = {
      attempt_id: `attempt-${this.#idCounter}`,
      job_id: "job-0",
      request_family: "multipart_stt",
      dispatch_state: "completed",
      cost: { confidence: "unknown", amount: null },
      provider_request_id: null,
      created_at: this.#stamp(),
      responded_at: null,
      possibly_billed: false,
      ...partial,
    };
    this.attempts.push(attempt);
    return attempt;
  }

  /* ----------------------------------------------------- cloud handlers */

  #stamp(): string {
    this.#idCounter += 1;
    return `2026-02-01T10:${String(Math.floor(this.#idCounter / 60) % 60).padStart(2, "0")}:${String(
      this.#idCounter % 60,
    ).padStart(2, "0")}Z`;
  }

  #modelsFor(stage: ProcessingStage, engine: string | null): ModelCard[] {
    const all = this.models[stage] ?? [];
    return engine === null ? all : all.filter((model) => model.engine === engine);
  }

  #findModel(stage: ProcessingStage, modelId: string): ModelCard | null {
    return this.#modelsFor(stage, null).find((model) => model.model_id === modelId) ?? null;
  }

  #anyModel(modelId: string): ModelCard | null {
    for (const cards of Object.values(this.models)) {
      const hit = cards?.find((model) => model.model_id === modelId);
      if (hit) return hit;
    }
    return null;
  }

  #listModels(request: Record<string, any>): ListModelsResponse {
    const stage = request.stage as ProcessingStage;
    const degraded = this.catalogDegradedReason;
    return {
      models: this.#modelsFor(stage, request.engine ?? null),
      catalog_source: degraded === null ? "live" : "pinned_allowlist",
      catalog_fetched_at: degraded === null ? this.catalogFetchedAt : null,
      degraded_reason: degraded,
      default_model_id: this.defaultModelByStage[stage] ?? null,
    };
  }

  #effectiveModel(stage: ProcessingStage, recordingId: string | null): ModelCard | null {
    if (recordingId) {
      const override = this.recordingModelOverrides.get(`${recordingId}:${stage}`);
      if (override) return this.#findModel(stage, override.model_id);
    }
    const fallback = this.defaultModelByStage[stage];
    return fallback ? this.#findModel(stage, fallback) : null;
  }

  #budgetSnapshot(): CloudBudget {
    return { ...this.budget, month_to_date: totalsToCost(usageTotals(this.attempts)) };
  }

  #setBudget(request: Record<string, any>): CloudBudget {
    if (request.currency !== "USD") {
      throw envelope("INVALID_ARGUMENT", "Budgets are only supported in USD in this version.");
    }
    const limits: Array<["per_job_limit" | "monthly_limit", MoneyAmount | null]> = [];
    for (const key of ["per_job_limit", "monthly_limit"] as const) {
      const raw = request[key];
      if (raw === null || raw === undefined) {
        limits.push([key, null]);
        continue;
      }
      const micros = parseMicros(String(raw));
      if (micros === null) {
        throw envelope("INVALID_ARGUMENT", "Enter an amount such as 2.50, or leave it empty.");
      }
      limits.push([key, usd(micros)]);
    }
    const perJob = limits[0]![1];
    const monthly = limits[1]![1];
    this.budget = {
      ...this.budget,
      per_job_limit: perJob,
      monthly_limit: monthly,
      blocks_dispatch: perJob !== null || monthly !== null,
      enforcement_note: CLOUD_BUDGET_ENFORCEMENT_NOTE,
    };
    return this.#budgetSnapshot();
  }

  #getUsage(request: Record<string, any>) {
    const from = String(request.from ?? "");
    const to = String(request.to ?? "");
    const inRange = this.attempts.filter((a) => a.created_at >= from && a.created_at < to);
    const ids = [...new Set(inRange.map((a) => a.model_id))].sort();
    return {
      from,
      to,
      totals: usageTotals(inRange),
      by_model: ids.map((model_id) => {
        const rows = inRange.filter((a) => a.model_id === model_id);
        return { model_id, attempts: rows.length, ...usageTotals(rows) };
      }),
      attempts: inRange.map((a) => ({ ...a })),
      budget: this.#budgetSnapshot(),
    };
  }

  #grantConsent(request: Record<string, any>) {
    const scope = request.scope ?? { kind: "ask_every_time", recording_id: null };
    if (scope.kind === "recording" && !scope.recording_id) {
      throw envelope("INVALID_ARGUMENT", "A recording-scoped grant needs a recording.");
    }
    const model = this.#anyModel(String(request.model_id));
    if (request.require_zero_retention === true && !(model?.privacy.zero_retention_available ?? false)) {
      throw envelope(
        "POLICY_UNRESOLVED",
        "Zero retention could not be confirmed for this exact route, so nothing was sent.",
        { action: "CHOOSE_ANOTHER_MODEL" },
      );
    }
    const grant = this.addConsent({
      purpose: request.purpose,
      content_type: request.content_type,
      scope_kind: scope.kind,
      recording_id: scope.recording_id ?? null,
      model_id: String(request.model_id),
      downstream_provider: request.downstream_provider ?? null,
      required_retention:
        request.require_zero_retention === true ? "zero_retention" : model?.privacy.retention ?? null,
      disclosure_version: String(request.disclosure_version),
    });
    return {
      consent_id: grant.consent_id,
      scope_kind: grant.scope_kind,
      granted_at: grant.granted_at,
      expires_at: grant.expires_at,
    };
  }

  #revokeConsent(request: Record<string, any>) {
    const grant = this.consents.find((c) => c.consent_id === request.consent_id);
    if (!grant) throw envelope("RECORD_NOT_FOUND", "That consent grant no longer exists.");
    grant.revoked_at = this.#stamp();
    return { ok: true };
  }

  #setModelPreference(request: Record<string, any>) {
    const stage = request.stage as ProcessingStage;
    const scope = request.scope ?? { kind: "default", recording_id: null };
    const modelId: string | null = request.model_id ?? null;
    if (modelId !== null) {
      const model = this.#findModel(stage, modelId);
      if (!model) {
        throw envelope("MODEL_UNAVAILABLE", "That model is not offered for this stage.", {
          action: "CHOOSE_ANOTHER_MODEL",
        });
      }
      if (!model.selectable) {
        throw envelope(
          "CAPABILITY_UNVERIFIED",
          "This model has not passed a capability smoke test on the request contract it would use, so it cannot be selected.",
          { action: "CHOOSE_ANOTHER_MODEL" },
        );
      }
    }
    if (scope.kind === "recording") {
      if (!scope.recording_id) {
        throw envelope("INVALID_ARGUMENT", "A recording override needs a recording.");
      }
      this.#requireRecording(scope.recording_id);
      const key = `${scope.recording_id}:${stage}`;
      if (modelId === null) this.recordingModelOverrides.delete(key);
      else this.recordingModelOverrides.set(key, { engine: String(request.engine), model_id: modelId });
    } else {
      if (modelId === null) {
        throw envelope("INVALID_ARGUMENT", "Choose a model; the task default cannot be empty.");
      }
      this.defaultModelByStage = { ...this.defaultModelByStage, [stage]: modelId };
    }
    return {
      stage,
      scope_kind: scope.kind,
      recording_id: scope.recording_id ?? null,
      engine: String(request.engine),
      model_id: modelId,
      effective_model: this.#effectiveModel(stage, scope.recording_id ?? null),
    };
  }

  #startProcessing(request: Record<string, any>) {
    const rec = this.#requireRecording(request.recording_id);
    const stage = request.stage as ProcessingStage;
    const override = request.model_override ?? null;
    const model = override
      ? this.#findModel(stage, String(override.model_id))
      : this.#effectiveModel(stage, rec.recording_id);
    if (!model) {
      throw envelope("MODEL_UNAVAILABLE", "No model is selected for this stage.", {
        action: "CHOOSE_ANOTHER_MODEL",
      });
    }
    if (!model.selectable) {
      throw envelope(
        "CAPABILITY_UNVERIFIED",
        "This model has not passed a capability smoke test, so nothing was dispatched.",
        { action: "CHOOSE_ANOTHER_MODEL" },
      );
    }
    const cloud = model.engine === "openrouter";
    if (cloud && this.preferences.local_only_mode) {
      throw envelope("LOCAL_ONLY_MODE", "Local-only mode is on, so nothing was sent.", {
        action: "REVIEW_LOCAL_ONLY_MODE",
      });
    }
    const estimate = estimateDispatchCost(model, rec.duration_ms);
    if (this.requireCostAcknowledgement && request.cost_acknowledged !== true) {
      throw envelope(
        "COST_ACKNOWLEDGEMENT_REQUIRED",
        "A previous attempt may already have been billed. Confirm the cost to run this stage again.",
        { action: "REVIEW_CONSENT" },
      );
    }
    const blocked = this.#budgetBlock(estimate);
    if (blocked) {
      throw envelope("BUDGET_EXCEEDED", blocked, { action: "OPEN_SETTINGS" });
    }
    if (
      cloud &&
      !this.#consentCovers(model.model_id, rec.recording_id, CONSENT_PURPOSE_BY_STAGE[stage])
    ) {
      throw envelope(
        "CONSENT_REQUIRED",
        "This run would send content to a cloud provider. Review and approve it first.",
        { action: "REVIEW_CONSENT" },
      );
    }
    const job = this.addJob({
      kind: stage === "transcribe" || stage === "diarize" ? "transcribe" : "summarize",
      state: "queued",
      stage,
      label: rec.title,
      recording_id: rec.recording_id,
      cost: estimate,
    });
    this.addAttempt({
      job_id: job.job_id,
      model_id: model.model_id,
      request_family: model.request_family,
      dispatch_state: cloud ? "not_sent" : "completed",
      cost: estimate,
    });
    this.emit("job.updated", { job_id: job.job_id, recording_id: rec.recording_id });
    return {
      job_id: job.job_id,
      estimated_cost: estimate,
      dispatch: "queued" as const,
      blocked_reason: null,
    };
  }

  /**
   * An active grant for this model and purpose. `recordingId` is `null` for a
   * dispatch that belongs to no recording, which only a model-policy grant can
   * cover. The purpose follows the content type — anything carrying audio
   * bytes is `transcription`, never `credential_validation`, which §4.4(3)
   * reserves for a key check that sends no recording-derived content.
   */
  #consentCovers(
    modelId: string,
    recordingId: string | null,
    purpose: ConsentSummary["purpose"],
  ): boolean {
    return this.consents.some(
      (grant) =>
        grant.revoked_at === null &&
        grant.model_id === modelId &&
        grant.purpose === purpose &&
        (grant.scope_kind === "model_policy" ||
          (grant.scope_kind === "recording" &&
            recordingId !== null &&
            grant.recording_id === recordingId)),
    );
  }

  /**
   * ADR-0022 capability check: one real bundled-fixture request. It is the only
   * path that can turn `not_verified` into `verified`, and it is billable, so
   * it needs consent, budget headroom and an explicit cost acknowledgement.
   */
  #verifyModel(request: Record<string, any>) {
    const modelId = String(request.model_id ?? "");
    const model = this.#anyModel(modelId);
    if (!model) {
      throw envelope("MODEL_UNAVAILABLE", "That model is not in the catalogue.", {
        action: "CHOOSE_ANOTHER_MODEL",
      });
    }
    if (this.preferences.local_only_mode) {
      throw envelope(
        "LOCAL_ONLY_MODE",
        "Local-only mode is on, so the capability check was not sent.",
        { action: "REVIEW_LOCAL_ONLY_MODE" },
      );
    }
    if (!this.#consentCovers(modelId, null, "transcription")) {
      throw envelope(
        "CONSENT_REQUIRED",
        "The capability check sends test audio to this provider. Review and approve it first.",
        { action: "REVIEW_CONSENT" },
      );
    }
    const cost = estimateDispatchCost(model, CAPABILITY_CHECK_CLIP_MS);
    const blocked = this.#budgetBlock(cost);
    if (blocked) throw envelope("BUDGET_EXCEEDED", blocked, { action: "OPEN_SETTINGS" });
    if (request.cost_acknowledged !== true) {
      throw envelope(
        "COST_ACKNOWLEDGEMENT_REQUIRED",
        "The capability check is a billable request. Confirm the cost to run it.",
        { action: "REVIEW_CONSENT" },
      );
    }

    // The exchange happened, so what was observed is now known.
    const observed: CostValue =
      cost.amount === null ? { confidence: "unknown", amount: null }
        : { confidence: "finalized", amount: cost.amount };
    const providerRequestId = `provider-req-${this.#idCounter + 1}`;
    const verified: ModelCard = {
      ...model,
      verification: "verified",
      verification_reason: null,
      fixture_id: `fixture-capability-${modelId.replace(/[^a-z0-9]+/gi, "-")}`,
      validated_at: this.#stamp(),
      request_family:
        model.request_family !== "unknown"
          ? model.request_family
          : request.task === "transcription"
            ? "multipart_stt"
            : "audio_chat",
      selectable: true,
    };
    this.#replaceModel(modelId, verified);
    this.addAttempt({
      model_id: modelId,
      request_family: verified.request_family,
      dispatch_state: "completed",
      cost: observed,
      provider_request_id: providerRequestId,
      responded_at: this.#stamp(),
      possibly_billed: true,
    });
    return { model: verified, cost: observed, provider_request_id: providerRequestId };
  }

  #replaceModel(modelId: string, next: ModelCard): void {
    const updated: Partial<Record<ProcessingStage, ModelCard[]>> = {};
    for (const [stage, cards] of Object.entries(this.models) as Array<
      [ProcessingStage, ModelCard[]]
    >) {
      updated[stage] = cards.map((card) => (card.model_id === modelId ? next : card));
    }
    this.models = updated;
  }

  /** Message describing the locally calculable limit this run would breach. */
  #budgetBlock(estimate: CostValue): string | null {
    if (estimate.amount === null) return null;
    const cost = parseMicros(estimate.amount.amount) ?? 0;
    const perJob = this.budget.per_job_limit;
    if (perJob !== null && cost > (parseMicros(perJob.amount) ?? 0)) {
      return `This run is estimated at ${estimate.amount.currency} ${estimate.amount.amount}, above your per-job limit of ${perJob.currency} ${perJob.amount}. Nothing was sent.`;
    }
    const monthly = this.budget.monthly_limit;
    if (monthly !== null) {
      const mtd = this.#budgetSnapshot().month_to_date.amount;
      const spent = mtd === null ? 0 : parseMicros(mtd.amount) ?? 0;
      if (spent + cost > (parseMicros(monthly.amount) ?? 0)) {
        return `This run would take the month past your limit of ${monthly.currency} ${monthly.amount}. Nothing was sent.`;
      }
    }
    return null;
  }
}

function stripSegments(rec: FakeRecording): RecordingSummary {
  const { segments: _s, media: _m, revisions: _r, outputs: _o, current_revision_id: _c, ...summary } = rec;
  return summary;
}

function sortValue(rec: FakeRecording, field: string): string | number {
  switch (field) {
    case "title":
      return rec.title.toLowerCase();
    case "duration_ms":
      return rec.duration_ms ?? -1;
    case "imported_at":
      return rec.imported_at;
    default:
      return rec.recorded_at ?? rec.imported_at;
  }
}

function buildSegments(count: number, alignment: "native" | "derived" | "none"): TranscriptSegment[] {
  const segments: TranscriptSegment[] = [];
  for (let i = 0; i < count; i += 1) {
    const timed = alignment !== "none";
    segments.push({
      index: i,
      text: `Segment ${i + 1}: ${SAMPLE_TEXT[i % SAMPLE_TEXT.length]}`,
      start_ms: timed ? i * 4_000 : null,
      end_ms: timed ? i * 4_000 + 3_800 : null,
      alignment_origin: alignment,
      confidence: { engine_id: "whisper.cpp", value: i % 7 === 0 ? 0.42 : 0.93 },
      speaker_label: null,
      annotations:
        i % 7 === 0
          ? [
              {
                kind: "low_confidence",
                reason: "Engine reported low confidence for this segment.",
                segment_index: i,
                start_ms: timed ? i * 4_000 : null,
                end_ms: timed ? i * 4_000 + 3_800 : null,
              },
            ]
          : [],
    });
  }
  return segments;
}

const SAMPLE_TEXT = [
  "we agreed to ship the import path first",
  "the decision is to keep everything local by default",
  "action item, send the compatibility matrix on Friday",
  "nobody objected to the revised timeline",
];

function isoAt(index: number, offsetMinutes: number): string {
  const base = Date.UTC(2026, 0, 5, 9, 0, 0) + index * 86_400_000 + offsetMinutes * 60_000;
  return new Date(base).toISOString();
}

function fakeSha(seed: string): string {
  let h1 = 0x811c9dc5;
  let out = "";
  for (let i = 0; i < 8; i += 1) {
    for (let c = 0; c < seed.length; c += 1) {
      h1 ^= seed.charCodeAt(c) + i;
      h1 = Math.imul(h1, 0x01000193) >>> 0;
    }
    out += h1.toString(16).padStart(8, "0");
  }
  return out.slice(0, 64);
}

/**
 * §10.4: finalized and estimated money are summed separately and attempts with
 * no derivable cost are counted, never folded into a zero amount.
 */
export function usageTotals(attempts: readonly UsageAttempt[]): UsageTotals {
  let finalized = 0;
  let estimated = 0;
  let finalizedSeen = false;
  let estimatedSeen = false;
  let unknown = 0;
  for (const attempt of attempts) {
    const micros = attempt.cost.amount === null ? null : parseMicros(attempt.cost.amount.amount);
    if (attempt.cost.confidence === "finalized" && micros !== null) {
      finalized += micros;
      finalizedSeen = true;
    } else if (attempt.cost.confidence === "estimated" && micros !== null) {
      estimated += micros;
      estimatedSeen = true;
    } else {
      unknown += 1;
    }
  }
  return {
    finalized: finalizedSeen ? usd(finalized) : null,
    estimated: estimatedSeen ? usd(estimated) : null,
    unknown_count: unknown,
  };
}

/**
 * Collapses split totals into the single month-to-date figure the budget
 * carries. Any estimated component keeps the whole figure an estimate.
 */
export function totalsToCost(totals: UsageTotals): CostValue {
  const finalized = totals.finalized === null ? null : parseMicros(totals.finalized.amount) ?? 0;
  const estimated = totals.estimated === null ? null : parseMicros(totals.estimated.amount) ?? 0;
  if (finalized === null && estimated === null) return { confidence: "unknown", amount: null };
  return {
    confidence: estimated === null ? "finalized" : "estimated",
    amount: usd((finalized ?? 0) + (estimated ?? 0)),
  };
}
