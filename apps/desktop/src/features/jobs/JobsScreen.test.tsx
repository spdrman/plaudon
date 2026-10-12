import { describe, expect, it, vi } from "vitest";
import { screen, waitFor, within } from "@testing-library/react";
import { JOB_STATES } from "../../ipc/contract";
import { FakeBackend, envelope } from "../../testing/fakeBackend";
import { renderWithApp } from "../../testing/renderWithApp";
import { expectNoAxeViolations } from "../../testing/axe";
import { JobsScreen } from "./JobsScreen";
import { JOB_STATE_PRESENTATION } from "./jobPresentation";

function noop(): void {
  /* deep links are exercised explicitly where they matter */
}

async function settled(): Promise<void> {
  await waitFor(() => expect(screen.getByRole("heading", { name: "Jobs" })).toBeInTheDocument());
}

function countCalls(backend: FakeBackend, command: string): number {
  return backend.calls.filter((c) => c.command === command).length;
}

describe("JobsScreen", () => {
  it("renders every contract job state with a distinct label and a text glyph", async () => {
    const backend = new FakeBackend();
    for (const state of JOB_STATES) {
      backend.addJob({ kind: "transcribe", state, label: `Job ${state}` });
    }
    renderWithApp(<JobsScreen focusJobId={null} onOpenTarget={noop} />, { backend });

    for (const state of JOB_STATES) {
      const row = await screen.findByText(`Job ${state}`);
      const item = row.closest("li");
      expect(item, state).not.toBeNull();
      const presentation = JOB_STATE_PRESENTATION[state];
      const badge = within(item as HTMLElement).getByText(presentation.label);
      // Status is text + glyph, never color alone (§5.3).
      const glyph = (badge.parentElement as HTMLElement).querySelector(".status-badge__glyph");
      expect(glyph?.textContent, state).toBe(presentation.glyph);
    }
    await expectNoAxeViolations();
  });

  it("shows an honest indeterminate bar when progress is unknown and never invents completion", async () => {
    const backend = new FakeBackend();
    backend.addJob({
      kind: "transcribe",
      state: "running_local",
      label: "Unknown progress",
      stage: null,
      progress: null,
      started_at: null,
    });
    renderWithApp(<JobsScreen focusJobId={null} onOpenTarget={noop} />, { backend });

    const bar = await screen.findByRole("progressbar", {
      name: /Transcription progress for Unknown progress/,
    });
    expect(bar).toHaveAttribute("aria-valuetext", "in progress, amount unknown");
    expect(bar).not.toHaveAttribute("aria-valuenow");
    expect(screen.getByText("Not reported")).toBeInTheDocument();
    expect(screen.getByText("Elapsed time unknown")).toBeInTheDocument();
    expect(screen.queryByText("Completed")).not.toBeInTheDocument();
  });

  it("marks a job completed only when the backend says so", async () => {
    const backend = new FakeBackend();
    backend.addJob({ kind: "transcribe", state: "verifying", label: "Nearly", progress: 1 });
    renderWithApp(<JobsScreen focusJobId={null} onOpenTarget={noop} />, { backend });

    expect(await screen.findByText("Verifying")).toBeInTheDocument();
    expect(screen.queryByText("Completed")).not.toBeInTheDocument();
  });

  it("opens the produced output through onOpenTarget", async () => {
    const backend = new FakeBackend();
    backend.addJob({
      kind: "import",
      state: "completed",
      label: "Weekly sync",
      output_link: { kind: "recording", id: "rec-00001" },
    });
    const onOpenTarget = vi.fn();
    const { user } = renderWithApp(
      <JobsScreen focusJobId={null} onOpenTarget={onOpenTarget} />,
      { backend },
    );

    await user.click(await screen.findByRole("button", { name: "Open result" }));
    expect(onOpenTarget).toHaveBeenCalledWith({ kind: "recording", id: "rec-00001" });
  });

  it("focuses and scrolls the deep-linked row", async () => {
    const backend = new FakeBackend();
    backend.addJob({ kind: "import", state: "queued", label: "First" });
    const target = backend.addJob({ kind: "transcribe", state: "queued", label: "Deep linked" });
    renderWithApp(<JobsScreen focusJobId={target.job_id} onOpenTarget={noop} />, { backend });

    await waitFor(() => {
      const row = document.querySelector(`[data-job-id="${target.job_id}"]`);
      expect(row).toBe(document.activeElement);
    });
  });

  it("cancels with the §14.7 irreversibility disclosure and shows cancelling then cancelled", async () => {
    const backend = new FakeBackend();
    const job = backend.addJob({ kind: "summarize", state: "cloud_sending", label: "Summary" });
    const { user } = renderWithApp(<JobsScreen focusJobId={null} onOpenTarget={noop} />, {
      backend,
    });

    await user.click(await screen.findByRole("button", { name: "Cancel" }));
    const dialog = screen.getByRole("alertdialog");
    expect(
      within(dialog).getByText(/may not be reversed, and an upstream charge may still apply/),
    ).toBeInTheDocument();
    await user.click(within(dialog).getByRole("button", { name: "Cancel this job" }));

    expect(await screen.findByText("Cancelling")).toBeInTheDocument();
    expect(
      screen.getByText(/may not be reversed, and an upstream charge may still apply/),
    ).toBeInTheDocument();

    job.state = "cancelled";
    backend.emit("job.updated", { job_id: job.job_id });
    expect(await screen.findByText("Cancelled")).toBeInTheDocument();
  });

  it("never retries an uncertain cloud outcome without an explicit cost acknowledgement", async () => {
    const backend = new FakeBackend();
    const job = backend.addJob({
      kind: "summarize",
      state: "cloud_outcome_unknown",
      label: "Board summary",
      cost_acknowledgement_required: true,
      provider_request_id: "req_8f21",
    });
    const { user } = renderWithApp(<JobsScreen focusJobId={null} onOpenTarget={noop} />, {
      backend,
    });

    const panel = await screen.findByRole("region", {
      name: "Cloud outcome unknown for Board summary",
    });
    expect(within(panel).getByText(/may already have been accepted and billed/)).toBeInTheDocument();
    expect(within(panel).getByText("req_8f21")).toBeInTheDocument();

    // No automatic retry, and no retry driven by refresh events either.
    backend.emit("job.updated", { job_id: job.job_id });
    backend.emit("job.updated", { job_id: job.job_id });
    await waitFor(() => expect(countCalls(backend, "list_jobs")).toBeGreaterThan(1));
    expect(countCalls(backend, "retry_job")).toBe(0);

    await user.click(screen.getByRole("button", { name: "Retry" }));
    const dialog = screen.getByRole("dialog");
    const confirm = within(dialog).getByRole("button", { name: "Retry and accept the cost" });
    expect(confirm).toBeDisabled();
    expect(countCalls(backend, "retry_job")).toBe(0);

    await user.click(
      within(dialog).getByRole("checkbox", { name: /may incur a new charge/ }),
    );
    await user.click(confirm);

    await waitFor(() => expect(countCalls(backend, "retry_job")).toBe(1));
    const call = backend.calls.find((c) => c.command === "retry_job");
    expect(call?.args).toEqual({ job_id: job.job_id, cost_acknowledged: true });
    expect(
      backend.calls.some(
        (c) =>
          c.command === "retry_job" &&
          (c.args as { cost_acknowledged: boolean }).cost_acknowledged === false,
      ),
    ).toBe(false);
  });

  it("offers Retry, Choose local engine and Keep queued when the provider is unavailable", async () => {
    const backend = new FakeBackend();
    backend.addJob({
      kind: "summarize",
      state: "failed_retryable",
      label: "Summary",
      error: envelope("SOURCE_UNAVAILABLE", "OpenRouter did not respond.", {
        retryable: true,
        action: "WAIT_AND_RETRY",
      }),
    });
    renderWithApp(<JobsScreen focusJobId={null} onOpenTarget={noop} />, { backend });

    expect(await screen.findByText("OpenRouter is unavailable")).toBeInTheDocument();
    expect(screen.getByText("OpenRouter did not respond.")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Retry" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Choose local engine" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Keep queued" })).toBeInTheDocument();
    await expectNoAxeViolations();
  });

  it("keeps a job queued without touching the provider", async () => {
    const backend = new FakeBackend();
    backend.addJob({
      kind: "summarize",
      state: "failed_retryable",
      label: "Summary",
      error: envelope("SOURCE_UNAVAILABLE", "OpenRouter did not respond.", { retryable: true }),
    });
    const { user } = renderWithApp(<JobsScreen focusJobId={null} onOpenTarget={noop} />, {
      backend,
    });

    await user.click(await screen.findByRole("button", { name: "Keep queued" }));
    expect(countCalls(backend, "retry_job")).toBe(0);
  });

  it("shows needed versus available capacity and a storage settings control", async () => {
    const backend = new FakeBackend();
    backend.addJob({
      kind: "transcribe",
      state: "failed_retryable",
      label: "Long meeting",
      error: envelope("STORAGE_FULL", "This job needs 2.1 GB of free space.", {
        action: "FREE_DISK_SPACE",
      }),
    });
    renderWithApp(<JobsScreen focusJobId={null} onOpenTarget={noop} />, { backend });

    expect(await screen.findByText("Not enough space")).toBeInTheDocument();
    expect(screen.getByText("Needed")).toBeInTheDocument();
    expect(screen.getByText("Available")).toBeInTheDocument();
    expect(screen.getByText("220.0 GB")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Open storage settings" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Free up space" })).toBeInTheDocument();
    await expectNoAxeViolations();
  });

  it("stops billable retries and links to key settings when credits are exhausted", async () => {
    const backend = new FakeBackend();
    backend.addJob({
      kind: "summarize",
      state: "failed_retryable",
      label: "Summary",
      error: envelope("BUDGET_EXCEEDED", "Your provider credits are used up.", {
        retryable: false,
        action: "OPEN_SETTINGS",
      }),
    });
    renderWithApp(<JobsScreen focusJobId={null} onOpenTarget={noop} />, { backend });

    expect(await screen.findByText("Provider credits exhausted")).toBeInTheDocument();
    expect(screen.getByText(/Billable retries are stopped/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Open key settings" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Retry" })).not.toBeInTheDocument();
    await expectNoAxeViolations();
  });

  it("explains an offline job and keeps it queued", async () => {
    const backend = new FakeBackend();
    backend.addJob({
      kind: "summarize",
      state: "waiting_for_network",
      label: "Summary",
      error: envelope("LOCAL_ONLY_MODE", "No network connection is available.", {
        retryable: true,
      }),
    });
    renderWithApp(<JobsScreen focusJobId={null} onOpenTarget={noop} />, { backend });

    expect(await screen.findByText("You are offline")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Keep queued" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Review Local-only Mode" })).toBeInTheDocument();
    await expectNoAxeViolations();
  });

  it("offers an empty state with a next step when there are no jobs", async () => {
    renderWithApp(<JobsScreen focusJobId={null} onOpenTarget={noop} />);
    expect(await screen.findByText("No jobs")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Refresh" })).toBeInTheDocument();
    await expectNoAxeViolations();
  });

  it("refetches authoritative state after a dropped job.updated event", async () => {
    const backend = new FakeBackend();
    backend.addJob({ kind: "transcribe", state: "running_local", label: "Live", progress: 0.25 });
    renderWithApp(<JobsScreen focusJobId={null} onOpenTarget={noop} />, { backend });
    await settled();
    await screen.findByText("Live");
    const before = countCalls(backend, "list_jobs");

    backend.dropNextEvent = true;
    backend.emit("job.updated");

    await waitFor(() => expect(countCalls(backend, "list_jobs")).toBe(before + 1));
    // Progress still mirrors committed state; nothing is advanced client-side.
    const bar = screen.getByRole("progressbar", { name: /Transcription progress for Live/ });
    expect(bar).toHaveAttribute("aria-valuenow", "25");
  });

  it("filters by state using the counts the backend reported", async () => {
    const backend = new FakeBackend();
    backend.addJob({ kind: "import", state: "completed", label: "Done import" });
    backend.addJob({ kind: "transcribe", state: "queued", label: "Pending work" });
    const { user } = renderWithApp(<JobsScreen focusJobId={null} onOpenTarget={noop} />, {
      backend,
    });

    const chip = await screen.findByRole("button", { name: "Queued (1)" });
    await user.click(chip);
    await waitFor(() => expect(screen.queryByText("Done import")).not.toBeInTheDocument());
    expect(screen.getByText("Pending work")).toBeInTheDocument();
    expect(chip).toHaveAttribute("aria-pressed", "true");
  });

  it("renders a failed list query with its message and a Retry control", async () => {
    const backend = new FakeBackend();
    backend.failures.set("list_jobs", envelope("INTERNAL_ERROR", "The job store is unavailable."));
    renderWithApp(<JobsScreen focusJobId={null} onOpenTarget={noop} />, { backend });

    expect(await screen.findByText("The job store is unavailable.")).toBeInTheDocument();
    expect(screen.getByRole("alert")).toBeInTheDocument();
    await expectNoAxeViolations();
  });
});
