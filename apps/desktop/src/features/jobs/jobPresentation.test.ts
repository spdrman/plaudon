import { describe, expect, it } from "vitest";
import { ERROR_ACTIONS, JOB_STATES } from "../../ipc/contract";
import {
  ERROR_GUIDANCE,
  JOB_STATE_PRESENTATION,
  elapsedMs,
  envelopeActionControl,
  errorGuidance,
  formatElapsed,
  jobElapsedLabel,
} from "./jobPresentation";

describe("job state presentation", () => {
  it("covers every contract state with a unique, non-empty label and glyph", () => {
    const labels = new Set<string>();
    const glyphs = new Set<string>();
    for (const state of JOB_STATES) {
      const presentation = JOB_STATE_PRESENTATION[state];
      expect(presentation, state).toBeDefined();
      expect(presentation.label.trim().length, state).toBeGreaterThan(0);
      expect(presentation.glyph.trim().length, state).toBeGreaterThan(0);
      expect(presentation.description.trim().length, state).toBeGreaterThan(0);
      labels.add(presentation.label);
      glyphs.add(presentation.glyph);
    }
    expect(labels.size).toBe(JOB_STATES.length);
    expect(glyphs.size).toBe(JOB_STATES.length);
  });

  it("has no extra states beyond the contract", () => {
    expect(Object.keys(JOB_STATE_PRESENTATION).sort()).toEqual([...JOB_STATES].sort());
  });

  it("only requires a cost acknowledgement for an uncertain cloud outcome", () => {
    const requiring = JOB_STATES.filter((s) => JOB_STATE_PRESENTATION[s].requiresCostAck);
    expect(requiring).toEqual(["cloud_outcome_unknown"]);
  });

  it("marks exactly the finished states terminal and never cancels a committing job", () => {
    const terminal = JOB_STATES.filter((s) => JOB_STATE_PRESENTATION[s].terminal);
    expect(terminal).toEqual(["completed", "failed_terminal", "cancelled"]);
    expect(JOB_STATE_PRESENTATION.committing.canCancel).toBe(false);
    expect(JOB_STATE_PRESENTATION.completed.canRetry).toBe(false);
  });
});

describe("envelopeActionControl", () => {
  it("maps every contract action to a labelled control", () => {
    for (const action of ERROR_ACTIONS) {
      const control = envelopeActionControl(action);
      expect(control, action).not.toBeNull();
      expect(control?.label.trim().length, action).toBeGreaterThan(0);
    }
  });

  it("falls back to Retry for an omitted action only when the envelope is retryable", () => {
    expect(envelopeActionControl(undefined, true)).toEqual({ label: "Retry", intent: "retry" });
    expect(envelopeActionControl(undefined, false)).toBeNull();
    expect(envelopeActionControl(null, false)).toBeNull();
    expect(envelopeActionControl(undefined)).toBeNull();
  });
});

describe("error guidance", () => {
  it("offers the §5.4 controls for a provider outage", () => {
    const guidance = errorGuidance("SOURCE_UNAVAILABLE");
    expect(guidance?.controls.map((c) => c.label)).toEqual([
      "Retry",
      "Choose local engine",
      "Keep queued",
    ]);
  });

  it("stops billable retries when credits are exhausted", () => {
    expect(ERROR_GUIDANCE.BUDGET_EXCEEDED?.suppressRetry).toBe(true);
    expect(ERROR_GUIDANCE.BUDGET_EXCEEDED?.controls.map((c) => c.intent)).toEqual([
      "open_ai_settings",
    ]);
  });

  it("asks for a capacity comparison when storage is full", () => {
    expect(ERROR_GUIDANCE.STORAGE_FULL?.showCapacity).toBe(true);
  });

  it("returns null for an unknown or missing code", () => {
    expect(errorGuidance(undefined)).toBeNull();
    expect(errorGuidance("INTERNAL_ERROR")).toBeNull();
  });
});

describe("elapsed time", () => {
  it("is unknown when the backend never reported a start", () => {
    expect(elapsedMs(null, null, 1_000)).toBeNull();
    expect(formatElapsed(null)).toBeNull();
    expect(jobElapsedLabel(null, null)).toBe("Elapsed time unknown");
  });

  it("measures to the finish when the job ended", () => {
    const ms = elapsedMs("2026-02-01T09:00:00Z", "2026-02-01T09:02:05Z");
    expect(ms).toBe(125_000);
    expect(formatElapsed(ms)).toBe("2m 05s");
  });

  it("measures to now while the job runs", () => {
    const now = Date.parse("2026-02-01T10:30:00Z");
    expect(formatElapsed(elapsedMs("2026-02-01T09:00:00Z", null, now))).toBe("1h 30m");
    expect(jobElapsedLabel("2026-02-01T09:59:12Z", null, now)).toBe("Elapsed 30m 48s");
  });

  it("formats sub-minute runs in seconds", () => {
    expect(formatElapsed(9_000)).toBe("9s");
  });
});
