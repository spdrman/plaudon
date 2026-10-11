import { beforeEach, describe, expect, it, vi } from "vitest";
import { screen, waitFor, within } from "@testing-library/react";
import { renderWithApp } from "../../testing/renderWithApp";
import { expectNoAxeViolations } from "../../testing/axe";
import { FakeBackend, envelope } from "../../testing/fakeBackend";
import { RecordingDetailScreen } from "./RecordingDetailScreen";

const { saveDialog } = vi.hoisted(() => ({ saveDialog: vi.fn() }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ save: saveDialog }));

/** The virtualizer measures the scroll element with `offset*` (see TranscriptPane tests). */
function stubLayout(): void {
  vi.spyOn(HTMLElement.prototype, "offsetHeight", "get").mockImplementation(function (
    this: HTMLElement,
  ) {
    return this.classList.contains("transcript__scroller") ? 600 : 0;
  });
  vi.spyOn(HTMLElement.prototype, "offsetWidth", "get").mockReturnValue(900);
}

beforeEach(() => {
  stubLayout();
  saveDialog.mockReset();
  vi.spyOn(HTMLMediaElement.prototype, "play").mockImplementation(() => Promise.resolve());
  vi.spyOn(HTMLMediaElement.prototype, "pause").mockImplementation(() => undefined);
});

function renderDetail(backend: FakeBackend, recordingId: string, onBack = vi.fn()) {
  return {
    onBack,
    ...renderWithApp(<RecordingDetailScreen recordingId={recordingId} onBack={onBack} />, {
      backend,
      route: { name: "library.recording", recordingId },
    }),
  };
}

describe("header and provenance", () => {
  it("shows provenance, the privacy badge and an honest read-only title", async () => {
    const backend = new FakeBackend();
    const recording = backend.addRecording({ segments: 3, title: "Design review" });
    renderDetail(backend, recording.recording_id);

    expect(await screen.findByRole("heading", { name: "Design review" })).toBeInTheDocument();
    expect(screen.getByText(/Renaming is not part of this build/)).toBeInTheDocument();
    expect(screen.getByText("Manual import")).toBeInTheDocument();
    expect(screen.getByText(recording.media.original_file_name)).toBeInTheDocument();
    expect(screen.getByText(`${recording.sha256.slice(0, 12)}…`)).toBeInTheDocument();
    // Local-only is the fake backend's default preference.
    expect(screen.getByText("Local only")).toBeInTheDocument();
    expect(screen.getByText("Transcribed")).toBeInTheDocument();
  });

  it("returns to the library from the Back control", async () => {
    const backend = new FakeBackend();
    const recording = backend.addRecording({ segments: 1 });
    const { user, onBack } = renderDetail(backend, recording.recording_id);
    // Wait for the loaded view: the loading state renders its own Back button.
    await screen.findByRole("heading", { name: recording.title });
    await user.click(screen.getByRole("button", { name: /back to library/i }));
    expect(onBack).toHaveBeenCalled();
  });

  it("labels a cloud-eligible recording when local-only mode is off", async () => {
    const backend = new FakeBackend();
    backend.preferences = { ...backend.preferences, local_only_mode: false };
    const recording = backend.addRecording({ segments: 1 });
    renderDetail(backend, recording.recording_id);
    expect(await screen.findByText("Cloud eligible")).toBeInTheDocument();
  });

  it("renders the error envelope with a retry control", async () => {
    const backend = new FakeBackend();
    backend.addRecording({ segments: 1 });
    backend.failures.set(
      "get_recording",
      envelope("RECORD_NOT_FOUND", "That recording no longer exists.", { retryable: true }),
    );
    const { user } = renderDetail(backend, "rec-00001");
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toMatch(/That recording no longer exists/);
    backend.failures.delete("get_recording");
    await user.click(within(alert).getByRole("button", { name: /retry/i }));
    expect(await screen.findByRole("heading", { name: /weekly sync/i })).toBeInTheDocument();
  });
});

describe("playback integration", () => {
  it("drives the player with the detail-scoped shortcuts", async () => {
    const backend = new FakeBackend();
    const recording = backend.addRecording({ segments: 2 });
    const { user } = renderDetail(backend, recording.recording_id);
    await screen.findByRole("slider", { name: "Seek" });
    await user.keyboard("l");
    expect(screen.getByRole("slider", { name: "Seek" })).toHaveAttribute("aria-valuenow", "10");
  });

  it("seeks from a transcript timestamp chip", async () => {
    const backend = new FakeBackend();
    const recording = backend.addRecording({ segments: 4 });
    const { user } = renderDetail(backend, recording.recording_id);
    await user.click(await screen.findByRole("button", { name: "Play from 0:08" }));
    expect(screen.getByRole("slider", { name: "Seek" })).toHaveAttribute("aria-valuenow", "8");
  });

  it("renders the indeterminate transport and no chips without a duration", async () => {
    const backend = new FakeBackend();
    const recording = backend.addRecording({ durationMs: null, alignment: "none", segments: 5 });
    renderDetail(backend, recording.recording_id);
    await screen.findByTestId("segment-0");
    expect(screen.queryByRole("slider")).toBeNull();
    expect(screen.getByText(/Duration unknown/)).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /play from/i })).toBeNull();
    expect(screen.getByRole("button", { name: /forward 10 s/i })).toBeDisabled();
    expect(screen.queryByText("0:00 of 0:00")).toBeNull();
    await expectNoAxeViolations();
  });
});

describe("detail pane", () => {
  it("states honestly that no outputs exist and lists revisions in History", async () => {
    const backend = new FakeBackend();
    const recording = backend.addRecording({ segments: 2 });
    const { user } = renderDetail(backend, recording.recording_id);

    expect(await screen.findByText(/No summary output exists for this recording yet/)).toBeInTheDocument();
    await user.click(screen.getByRole("tab", { name: "Actions" }));
    expect(screen.getByText(/No actions output exists for this recording yet/)).toBeInTheDocument();

    // Arrow keys move between tabs.
    await user.keyboard("{ArrowRight}");
    expect(screen.getByRole("tab", { name: "Decisions" })).toHaveAttribute("aria-selected", "true");
    await user.keyboard("{ArrowRight}");
    const history = screen.getByRole("tabpanel");
    expect(within(history).getByText("Original (machine)")).toBeInTheDocument();
    expect(within(history).getByText("Current revision")).toBeInTheDocument();
    expect(within(history).getByText(/native timestamps/)).toBeInTheDocument();
  });

  it("marks an output that is already stale on load", async () => {
    const backend = new FakeBackend();
    const recording = backend.addRecording({ segments: 2 });
    recording.outputs.push({
      output_id: "out-1",
      kind: "summary",
      source_revision_id: "rec-00001-rev-0",
      created_at: "2026-02-01T09:00:00Z",
      stale: true,
    });
    renderDetail(backend, recording.recording_id);
    expect(await screen.findByText("Needs regeneration")).toBeInTheDocument();
  });

  it("marks outputs as needing regeneration after a transcript edit is saved", async () => {
    const backend = new FakeBackend();
    const recording = backend.addRecording({ segments: 3 });
    recording.outputs.push({
      output_id: "out-1",
      kind: "summary",
      source_revision_id: recording.revisions[0]!.revision_id,
      created_at: "2026-02-01T09:00:00Z",
      stale: false,
    });
    const { user } = renderDetail(backend, recording.recording_id);
    expect(await screen.findByText("Current")).toBeInTheDocument();

    const row = await screen.findByTestId("segment-0");
    await user.dblClick(within(row).getByRole("button", { name: /^Segment 1:/ }));
    const editor = await screen.findByRole("textbox", { name: "Edit segment 1" });
    await user.type(editor, " and then some");
    await user.click(screen.getByRole("button", { name: /save 1 edited segment/i }));

    expect(await screen.findByText("Needs regeneration")).toBeInTheDocument();
    // A human revision was added and the machine original is kept.
    await waitFor(() => expect(backend.recordings[0]!.revisions).toHaveLength(2));
    expect(backend.recordings[0]!.revisions[0]!.kind).toBe("machine");
    expect(backend.recordings[0]!.revisions[0]!.superseded).toBe(true);
  });
});

describe("export", () => {
  it("exports through the OS save dialog with the chosen format", async () => {
    const backend = new FakeBackend();
    const recording = backend.addRecording({ segments: 2 });
    saveDialog.mockResolvedValue("/tmp/export.md");
    const { user } = renderDetail(backend, recording.recording_id);

    await user.click(await screen.findByRole("button", { name: /export/i }));
    const dialog = await screen.findByRole("dialog");
    await user.selectOptions(within(dialog).getByLabelText("Format"), "markdown");
    await user.click(within(dialog).getByRole("button", { name: /choose destination/i }));

    await waitFor(() =>
      expect(backend.calls.some((call) => call.command === "export_recording")).toBe(true),
    );
    const call = backend.calls.find((c) => c.command === "export_recording");
    expect(call?.args).toMatchObject({
      recording_id: recording.recording_id,
      revision_id: recording.revisions[0]!.revision_id,
      format: "markdown",
      destination: "/tmp/export.md",
    });
  });

  it("starts no export when the save dialog is cancelled", async () => {
    const backend = new FakeBackend();
    const recording = backend.addRecording({ segments: 2 });
    saveDialog.mockResolvedValue(null);
    const { user } = renderDetail(backend, recording.recording_id);
    await user.click(await screen.findByRole("button", { name: /export/i }));
    const dialog = await screen.findByRole("dialog");
    await user.click(within(dialog).getByRole("button", { name: /choose destination/i }));
    expect(backend.calls.some((call) => call.command === "export_recording")).toBe(false);
  });
});

describe("accessibility", () => {
  it("has no violations with a transcript", async () => {
    const backend = new FakeBackend();
    const recording = backend.addRecording({ segments: 3 });
    renderDetail(backend, recording.recording_id);
    await screen.findByTestId("segment-0");
    await expectNoAxeViolations();
  });

  it("has no violations without a transcript", async () => {
    const backend = new FakeBackend();
    const recording = backend.addRecording({ segments: 0 });
    renderDetail(backend, recording.recording_id);
    expect(await screen.findByText("No transcript yet")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /export/i })).toBeDisabled();
    await expectNoAxeViolations();
  });
});
