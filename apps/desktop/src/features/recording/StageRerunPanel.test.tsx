import { describe, expect, it, vi } from "vitest";
import { screen } from "@testing-library/react";
import { renderWithApp } from "../../testing/renderWithApp";
import { expectNoAxeViolations } from "../../testing/axe";
import { FakeBackend, envelope } from "../../testing/fakeBackend";
import type { RecordingDetail } from "../../ipc/contract";
import { StageRerunPanel } from "./StageRerunPanel";

function setup(configure: (backend: FakeBackend) => void = () => {}) {
  const backend = new FakeBackend();
  backend.enableCloud();
  const recording = backend.addRecording({ title: "Weekly sync", segments: 2 });
  configure(backend);
  const { segments: _segments, ...detail } = recording;
  const onQueued = vi.fn();
  const rendered = renderWithApp(
    <StageRerunPanel detail={detail as RecordingDetail} onQueued={onQueued} />,
    { backend },
  );
  return { ...rendered, backend, onQueued, recordingId: recording.recording_id };
}

describe("StageRerunPanel", () => {
  it("shows the estimate before anything is dispatched", async () => {
    const { user } = setup();
    await screen.findByRole("radio", { name: /Scribe 2/ });

    expect(screen.getByText(/choose a model to see an estimate/)).toBeInTheDocument();
    await user.click(screen.getByRole("radio", { name: /Scribe 2/ }));
    // 60 s of audio at USD 0.000100 per audio second.
    expect(screen.getByText("Estimated USD 0.006000")).toBeInTheDocument();
  });

  it("stores a per-recording override without changing the task default", async () => {
    const { user, backend, recordingId } = setup();
    await screen.findByRole("radio", { name: /Scribe Mini/ });

    await user.click(screen.getByRole("radio", { name: /Scribe Mini/ }));
    await user.click(
      screen.getByRole("button", { name: "Use this model for this recording only" }),
    );

    expect(await screen.findByText(/The task default is unchanged\./)).toBeInTheDocument();
    expect(backend.defaultModelByStage.transcribe).toBe("acme/scribe-2");
    expect(backend.recordingModelOverrides.get(`${recordingId}:transcribe`)).toEqual({
      engine: "openrouter",
      model_id: "acme/scribe-mini",
    });
  });

  it("asks for consent before any content leaves the device, then runs the stage", async () => {
    const { user, backend, onQueued } = setup();
    await screen.findByRole("radio", { name: /Scribe 2/ });

    await user.click(screen.getByRole("radio", { name: /Scribe 2/ }));
    await user.click(screen.getByRole("button", { name: "Re-run this stage" }));

    // Nothing was dispatched: the disclosure comes first (§4.4).
    await screen.findByRole("dialog", { name: /Send this content to a cloud provider/ });
    expect(backend.calls.some((call) => call.command === "start_processing")).toBe(false);
    expect(backend.jobs).toHaveLength(0);

    await user.click(screen.getByRole("radio", { name: /Allow for this recording/ }));
    await user.click(screen.getByRole("button", { name: "Approve and continue" }));

    await screen.findByText(/Queued job/);
    expect(onQueued).toHaveBeenCalledTimes(1);
    expect(backend.jobs).toHaveLength(1);
    expect(backend.jobs[0]?.cost.confidence).toBe("estimated");
  });

  it("renders a consent refusal from the backend's envelope", async () => {
    const { user, backend } = setup((b) => {
      b.addConsent({ model_id: "acme/scribe-2", scope_kind: "model_policy" });
      b.failures.set(
        "start_processing",
        envelope("CONSENT_REQUIRED", "A new consent check is needed for this route.", {
          action: "REVIEW_CONSENT",
        }),
      );
    });
    await screen.findByRole("radio", { name: /Scribe 2/ });

    await user.click(screen.getByRole("radio", { name: /Scribe 2/ }));
    await user.click(screen.getByRole("button", { name: "Re-run this stage" }));

    expect(
      await screen.findByText("A new consent check is needed for this route."),
    ).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Review and approve" })).toBeInTheDocument();
    expect(backend.jobs).toHaveLength(0);
  });

  it("stops a run that would exceed a locally calculable budget before sending", async () => {
    const { user, backend } = setup((b) => {
      b.addConsent({ model_id: "acme/scribe-2", scope_kind: "model_policy" });
      b.budget = {
        ...b.budget,
        per_job_limit: { amount: "0.001000", currency: "USD" },
        blocks_dispatch: true,
      };
    });
    await screen.findByRole("radio", { name: /Scribe 2/ });

    await user.click(screen.getByRole("radio", { name: /Scribe 2/ }));
    await user.click(screen.getByRole("button", { name: "Re-run this stage" }));

    expect(
      await screen.findByText(
        /above your per-job limit of USD 0\.001000\. Nothing was sent\./,
      ),
    ).toBeInTheDocument();
    expect(backend.jobs).toHaveLength(0);
    expect(backend.attempts).toHaveLength(0);
  });

  it("re-runs only the stage that was chosen", async () => {
    const { user, backend, recordingId } = setup((b) => {
      b.addConsent({
        model_id: "acme/summarize-1",
        scope_kind: "model_policy",
        purpose: "summarization",
        content_type: "transcript_text",
      });
    });
    await screen.findByRole("radio", { name: /Scribe 2/ });

    await user.selectOptions(screen.getByLabelText("Stage"), "consolidated_summary");
    await screen.findByRole("radio", { name: /Summarize 1/ });
    await user.click(screen.getByRole("radio", { name: /Summarize 1/ }));
    await user.click(screen.getByRole("button", { name: "Re-run this stage" }));

    await screen.findByText(/Queued job/);
    const starts = backend.calls.filter((call) => call.command === "start_processing");
    expect(starts).toHaveLength(1);
    expect(starts[0]?.args).toEqual({
      recording_id: recordingId,
      stage: "consolidated_summary",
      model_override: { engine: "openrouter", model_id: "acme/summarize-1" },
      cost_acknowledged: false,
      rerun: true,
    });
    expect(backend.jobs.map((job) => job.stage)).toEqual(["consolidated_summary"]);
  });

  it("refuses a model that has not passed a capability check", async () => {
    const { user, backend } = setup();
    await screen.findByRole("radio", { name: /Scribe 2/ });
    expect(screen.getByRole("radio", { name: /Catalog Audio XL/ })).toBeDisabled();
    await user.click(screen.getByRole("button", { name: "Re-run this stage" }));
    expect(backend.jobs).toHaveLength(0);
  });

  it("has no accessibility violations", async () => {
    setup();
    await screen.findByRole("radio", { name: /Scribe 2/ });
    await expectNoAxeViolations();
  });
});
