import { readFileSync, readdirSync } from "node:fs";
import { join } from "node:path";
import Ajv2020 from "ajv/dist/2020";
import addFormats from "ajv-formats";
import { describe, expect, it } from "vitest";
import { ALL_COMMANDS } from "./contract";
import { FakeBackend } from "../testing/fakeBackend";

/**
 * E1.2: the versioned JSON schemas under `schemas/` are the wire contract.
 * Rust validates its own DTOs against them; this suite proves the TypeScript
 * side has not drifted, so a renamed or added field fails CI on both sides.
 */
const SCHEMA_DIR = join(process.cwd(), "..", "..", "schemas", "commands", "v1");
const EVENT_SCHEMA = join(process.cwd(), "..", "..", "schemas", "events", "ui-event.v1.schema.json");

function ajv() {
  const instance = new Ajv2020({ strict: false, allErrors: true });
  addFormats(instance);
  return instance;
}

function validate(file: string, value: unknown): void {
  const validator = ajv().compile(JSON.parse(readFileSync(join(SCHEMA_DIR, file), "utf8")));
  const ok = validator(value);
  expect(
    ok,
    `${file} rejected the payload: ${ajv().errorsText(validator.errors, { separator: "\n" })}`,
  ).toBe(true);
}

describe("command schemas", () => {
  const files = readdirSync(SCHEMA_DIR);

  it("has a request and a response schema for every command in the contract", () => {
    const missing: string[] = [];
    for (const command of ALL_COMMANDS) {
      for (const kind of ["request", "response"] as const) {
        if (!files.includes(`${command}.${kind}.schema.json`)) {
          missing.push(`${command}.${kind}`);
        }
      }
    }
    expect(missing).toEqual([]);
  });

  it("has no schema for a command the frontend cannot call", () => {
    const declared = new Set<string>(ALL_COMMANDS);
    const extra = files
      .map((file) => file.replace(/\.(request|response)\.schema\.json$/, ""))
      .filter((name) => !declared.has(name));
    expect([...new Set(extra)]).toEqual([]);
  });
});

describe("responses produced by the reference backend satisfy the schemas", () => {
  it("validates the read commands", async () => {
    const backend = new FakeBackend({ recordings: 3, segmentsPerTranscript: 5 });
    backend.addJob({ kind: "import", state: "completed", label: "Import" });
    backend.addActivity({ category: "import_finished", title: "Import finished", body: "1 added." });

    validate("get_capabilities.response.schema.json", await backend.invoke("get_capabilities", {}));

    const listRequest = {
      cursor: null,
      limit: 50,
      filter: {
        query: null,
        import_status: [],
        transcription_status: [],
        sources: [],
        tags: [],
        date_from: null,
        date_to: null,
      },
      sort: { field: "recorded_at", direction: "desc" },
    };
    validate("list_recordings.request.schema.json", listRequest);
    validate(
      "list_recordings.response.schema.json",
      await backend.invoke("list_recordings", listRequest),
    );

    validate(
      "get_recording.response.schema.json",
      await backend.invoke("get_recording", { recording_id: "rec-00001" }),
    );

    const transcriptRequest = { revision_id: "rec-00001-rev-1", cursor: 0, limit: 200 };
    validate("get_transcript.request.schema.json", transcriptRequest);
    validate(
      "get_transcript.response.schema.json",
      await backend.invoke("get_transcript", transcriptRequest),
    );

    const jobsRequest = { cursor: null, limit: 50, states: [], kinds: [] };
    validate("list_jobs.request.schema.json", jobsRequest);
    validate("list_jobs.response.schema.json", await backend.invoke("list_jobs", jobsRequest));

    const activityRequest = { cursor: null, limit: 50 };
    validate("list_activity.request.schema.json", activityRequest);
    validate(
      "list_activity.response.schema.json",
      await backend.invoke("list_activity", activityRequest),
    );
  });

  it("validates the write commands", async () => {
    const backend = new FakeBackend({ recordings: 2, segmentsPerTranscript: 4 });

    const importRequest = { paths: ["/tmp/a.mp3"], duplicate_policy: "ask" };
    validate("import_files.request.schema.json", importRequest);
    validate(
      "import_files.response.schema.json",
      await backend.invoke("import_files", importRequest),
    );

    const saveRequest = {
      recording_id: "rec-00001",
      base_revision_id: "rec-00001-rev-1",
      edits: [{ index: 0, text: "corrected" }],
    };
    validate("save_transcript_revision.request.schema.json", saveRequest);
    validate(
      "save_transcript_revision.response.schema.json",
      await backend.invoke("save_transcript_revision", saveRequest),
    );

    const localOnly = { enabled: true, confirmed: true };
    validate("set_local_only_mode.request.schema.json", localOnly);
    validate(
      "set_local_only_mode.response.schema.json",
      await backend.invoke("set_local_only_mode", localOnly),
    );

    const notifications = {
      settings: {
        enabled: true,
        categories: {
          import_finished: true,
          transcription_finished: false,
          requires_attention: true,
          cloud_budget_exceeded: false,
        },
      },
    };
    validate("set_notification_settings.request.schema.json", notifications);
    validate(
      "set_notification_settings.response.schema.json",
      await backend.invoke("set_notification_settings", notifications),
    );

    const onboarding = {
      default_stt_engine_id: "whisper.cpp",
      default_text_engine_id: null,
      local_only_mode: true,
      notifications: notifications.settings,
    };
    validate("complete_onboarding.request.schema.json", onboarding);
    validate(
      "complete_onboarding.response.schema.json",
      await backend.invoke("complete_onboarding", onboarding),
    );

    validate("set_openrouter_key.request.schema.json", { key: "sk-or-v1-example" });
    validate(
      "set_openrouter_key.response.schema.json",
      await backend.invoke("set_openrouter_key", { key: "sk-or-v1-example" }),
    );
    validate(
      "remove_openrouter_key.response.schema.json",
      await backend.invoke("remove_openrouter_key", {}),
    );
  });

  it("validates job control and export", async () => {
    const backend = new FakeBackend({ recordings: 1, segmentsPerTranscript: 3 });
    const job = backend.addJob({ kind: "transcribe", state: "running_local", label: "Transcribe" });
    validate("cancel_job.request.schema.json", { job_id: job.job_id });
    validate(
      "cancel_job.response.schema.json",
      await backend.invoke("cancel_job", { job_id: job.job_id }),
    );
    const retry = { job_id: job.job_id, cost_acknowledged: false };
    validate("retry_job.request.schema.json", retry);
    validate("retry_job.response.schema.json", await backend.invoke("retry_job", retry));

    const exportRequest = {
      recording_id: "rec-00001",
      revision_id: "rec-00001-rev-1",
      format: "markdown",
      destination: "/tmp/out.md",
    };
    validate("export_recording.request.schema.json", exportRequest);
    validate(
      "export_recording.response.schema.json",
      await backend.invoke("export_recording", exportRequest),
    );
  });

  it("validates the cloud commands", async () => {
    const backend = new FakeBackend({ recordings: 1, segmentsPerTranscript: 2 });
    backend.enableCloud();

    const modelsRequest = { stage: "transcribe", engine: null };
    validate("list_models.request.schema.json", modelsRequest);
    validate("list_models.response.schema.json", await backend.invoke("list_models", modelsRequest));

    const budgetRequest = { per_job_limit: "1.50", monthly_limit: null, currency: "USD" };
    validate("set_cloud_budget.request.schema.json", budgetRequest);
    validate(
      "set_cloud_budget.response.schema.json",
      await backend.invoke("set_cloud_budget", budgetRequest),
    );

    const consentRequest = {
      purpose: "transcription",
      content_type: "audio_bytes",
      scope: { kind: "model_policy", recording_id: null },
      model_id: "acme/scribe-2",
      downstream_provider: "Acme Speech",
      require_zero_retention: false,
      disclosure_version: "2026-02-01",
    };
    validate("grant_cloud_consent.request.schema.json", consentRequest);
    const granted = (await backend.invoke("grant_cloud_consent", consentRequest)) as {
      consent_id: string;
    };
    validate("grant_cloud_consent.response.schema.json", granted);
    // The grant must now be visible on the capabilities payload.
    validate("get_capabilities.response.schema.json", await backend.invoke("get_capabilities", {}));

    const preferenceRequest = {
      stage: "transcribe",
      scope: { kind: "recording", recording_id: "rec-00001" },
      engine: "openrouter",
      model_id: "acme/scribe-mini",
    };
    validate("set_model_preference.request.schema.json", preferenceRequest);
    validate(
      "set_model_preference.response.schema.json",
      await backend.invoke("set_model_preference", preferenceRequest),
    );

    const startRequest = {
      recording_id: "rec-00001",
      stage: "transcribe",
      model_override: { engine: "openrouter", model_id: "acme/scribe-2" },
      cost_acknowledged: false,
      rerun: true,
    };
    validate("start_processing.request.schema.json", startRequest);
    validate(
      "start_processing.response.schema.json",
      await backend.invoke("start_processing", startRequest),
    );

    const usageRequest = { from: "2026-02-01T00:00:00Z", to: "2026-03-01T00:00:00Z" };
    validate("get_usage.request.schema.json", usageRequest);
    validate("get_usage.response.schema.json", await backend.invoke("get_usage", usageRequest));

    const verifyRequest = {
      model_id: "vendor/catalog-audio-xl",
      task: "transcription",
      cost_acknowledged: true,
    };
    validate("verify_model.request.schema.json", verifyRequest);
    backend.addConsent({ model_id: "vendor/catalog-audio-xl", scope_kind: "model_policy" });
    validate(
      "verify_model.response.schema.json",
      await backend.invoke("verify_model", verifyRequest),
    );

    const revokeRequest = { consent_id: granted.consent_id };
    validate("revoke_cloud_consent.request.schema.json", revokeRequest);
    validate(
      "revoke_cloud_consent.response.schema.json",
      await backend.invoke("revoke_cloud_consent", revokeRequest),
    );

    // `list_jobs` carries the new cost field on every job.
    validate(
      "list_jobs.response.schema.json",
      await backend.invoke("list_jobs", { cursor: null, limit: 50, states: [], kinds: [] }),
    );
  });

  it("validates emitted events against the event schema", () => {
    const backend = new FakeBackend({ recordings: 1 });
    const event = backend.emit("library.changed", { recording_id: "rec-00001" });
    const validator = ajv().compile(JSON.parse(readFileSync(EVENT_SCHEMA, "utf8")));
    const ok = validator(event);
    expect(ok, ajv().errorsText(validator.errors)).toBe(true);
  });
});

describe("device commands produced by the reference backend satisfy the schemas", () => {
  it("validates every device request and response", async () => {
    const backend = new FakeBackend();
    backend.devices.addDevice({ peripheral_id: "d1", support_status: "supported", permits_transfer: true, may_advertise_sync: true });
    backend.devices.addDevice({ peripheral_id: "d2", nearby: false, authorized: true });

    const scanRequest = { timeout_ms: 5000 };
    validate("list_nearby_devices.request.schema.json", scanRequest);
    validate("list_nearby_devices.response.schema.json", await backend.invoke("list_nearby_devices", scanRequest));

    const probeRequest = { peripheral_id: "d1", experimental_probe_approved: false };
    validate("device_probe.request.schema.json", probeRequest);
    validate("device_probe.response.schema.json", await backend.invoke("device_probe", probeRequest));

    const authorizeRequest = { peripheral_id: "d1", token_hex: "00112233445566778899aabbccddeeff" };
    validate("device_authorize.request.schema.json", authorizeRequest);
    const authorized = await backend.invoke("device_authorize", authorizeRequest);
    validate("device_authorize.response.schema.json", authorized);
    expect(JSON.stringify(authorized)).not.toContain(authorizeRequest.token_hex);

    const idRequest = { peripheral_id: "d1" };
    validate("device_list_recordings.request.schema.json", idRequest);
    validate("device_list_recordings.response.schema.json", await backend.invoke("device_list_recordings", idRequest));

    const importRequest = { peripheral_id: "d1", recording_ids: ["rec-1"], local_only: true };
    validate("device_import.request.schema.json", importRequest);
    validate("device_import.response.schema.json", await backend.invoke("device_import", importRequest));
    backend.devices.setTransfer("d1", "rec-1", "receiving", { bytes_received: 5, observed_bytes_per_sec: 5, eta_seconds_estimate: 3 });
    validate("device_list_recordings.response.schema.json", await backend.invoke("device_list_recordings", idRequest));

    const syncRequest = { peripheral_id: "d1", enable_when_nearby: true };
    validate("device_sync_policy.request.schema.json", syncRequest);
    validate("device_sync_policy.response.schema.json", await backend.invoke("device_sync_policy", syncRequest));

    const disconnectRequest = { peripheral_id: "d1", forget_authorization: true };
    validate("device_disconnect.request.schema.json", disconnectRequest);
    validate("device_disconnect.response.schema.json", await backend.invoke("device_disconnect", disconnectRequest));
  });

  it("accepts the new device event kinds", () => {
    const schema = JSON.parse(readFileSync(EVENT_SCHEMA, "utf8"));
    for (const kind of ["device.discovered", "device.connected", "device.compatibility_changed", "device.transfer_progress", "device.transfer_verified", "device.disconnected"]) {
      expect(schema.properties.kind.enum).toContain(kind);
    }
  });
});
