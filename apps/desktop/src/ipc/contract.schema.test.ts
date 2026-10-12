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

  it("validates emitted events against the event schema", () => {
    const backend = new FakeBackend({ recordings: 1 });
    const event = backend.emit("library.changed", { recording_id: "rec-00001" });
    const validator = ajv().compile(JSON.parse(readFileSync(EVENT_SCHEMA, "utf8")));
    const ok = validator(event);
    expect(ok, ajv().errorsText(validator.errors)).toBe(true);
  });
});
