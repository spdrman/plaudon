/**
 * Deterministic in-memory implementation of the §15 command/event surface.
 *
 * It exists so the UI can be developed and tested without the Rust process:
 * unit tests, component tests and the Playwright e2e suite all drive this
 * transport. It is NOT shipped in the production bundle — `main.tsx` only
 * installs it when `import.meta.env.DEV` and the `?harness=1` flag are present.
 */
import type {
  ActivityEntry,
  ActivityPage,
  Capabilities,
  CommandName,
  ErrorEnvelope,
  EventKind,
  ImportOutcome,
  JobSummary,
  NotificationSettings,
  Preferences,
  RecordingDetail,
  RecordingPage,
  RecordingSummary,
  TranscriptPage,
  TranscriptRevisionRef,
  TranscriptSegment,
  UiEvent,
} from "../ipc/contract";
import type { Transport, Unlisten } from "../ipc/client";
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
  /** Direct PLAUD device lab (`device_*` commands). */
  devices = new FakeDevices(
    (kind, partial) => this.emit(kind, partial),
    (code, message, extra) => envelope(code, message, extra),
  );

  #seq = 0;
  #handlers = new Map<EventKind, Set<(e: UiEvent) => void>>();
  #capabilities: Capabilities;
  #idCounter = 0;

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
          engine_id: "apple.speech",
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
      ...options.capabilities,
    };
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
        return { ...this.#capabilities, preferences: this.preferences, openrouter_key_present: this.openRouterKeyPresent };
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
