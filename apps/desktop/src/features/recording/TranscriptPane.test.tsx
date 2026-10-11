import { beforeEach, describe, expect, it, vi, type Mock } from "vitest";
import { screen, waitFor, within } from "@testing-library/react";
import { renderWithApp } from "../../testing/renderWithApp";
import { expectNoAxeViolations } from "../../testing/axe";
import { FakeBackend } from "../../testing/fakeBackend";
import { invokeCommand } from "../../ipc/client";
import type { SaveTranscriptRevisionResponse, TranscriptRevisionRef } from "../../ipc/contract";
import { TranscriptPane } from "./TranscriptPane";

/**
 * jsdom has no layout engine; `@tanstack/react-virtual` sizes the scroll
 * element from `offsetWidth`/`offsetHeight`. Only the scroller reports a size,
 * so rows keep the row estimate and a bounded window is rendered.
 */
function stubLayout(): void {
  vi.spyOn(HTMLElement.prototype, "offsetHeight", "get").mockImplementation(function (
    this: HTMLElement,
  ) {
    return this.classList.contains("transcript__scroller") ? 600 : 0;
  });
  vi.spyOn(HTMLElement.prototype, "offsetWidth", "get").mockReturnValue(900);
}

interface PaneHarness {
  backend: FakeBackend;
  recordingId: string;
  revision: TranscriptRevisionRef;
  onSeek: Mock;
  onSaved: Mock;
  onReloadRequested: Mock;
}

function renderPane(options: {
  segments?: number;
  alignment?: "native" | "derived" | "none";
  editable?: boolean;
}) {
  const backend = new FakeBackend();
  const recording = backend.addRecording({
    segments: options.segments ?? 5,
    alignment: options.alignment ?? "native",
  });
  const revision = recording.revisions[0]!;
  const harness: PaneHarness = {
    backend,
    recordingId: recording.recording_id,
    revision,
    onSeek: vi.fn(),
    onSaved: vi.fn(),
    onReloadRequested: vi.fn(),
  };
  const rendered = renderWithApp(
    <TranscriptPane
      recordingId={harness.recordingId}
      revision={revision}
      editable={options.editable ?? true}
      onSeek={harness.onSeek}
      onSaved={harness.onSaved}
      onReloadRequested={harness.onReloadRequested}
    />,
    { backend },
  );
  return { ...rendered, ...harness };
}

beforeEach(() => {
  stubLayout();
});

describe("segment rendering", () => {
  it("shows the engine-scoped confidence with its non-comparability note", async () => {
    renderPane({ segments: 3 });
    const row = await screen.findByTestId("segment-0");
    expect(
      within(row).getByText("whisper.cpp confidence 0.42 — not comparable across engines"),
    ).toBeInTheDocument();
  });

  it("flags annotations as text without hiding the segment text", async () => {
    renderPane({ segments: 3 });
    const row = await screen.findByTestId("segment-0");
    expect(within(row).getByText("Low confidence")).toBeInTheDocument();
    expect(within(row).getByText(/we agreed to ship the import path first/)).toBeInTheDocument();
  });

  it("seeks the player from a timestamp chip", async () => {
    const { user, onSeek } = renderPane({ segments: 3 });
    const chip = await screen.findByRole("button", { name: "Play from 0:04" });
    await user.click(chip);
    expect(onSeek).toHaveBeenCalledWith(4_000);
  });

  it("labels derived timings as approximate", async () => {
    renderPane({ segments: 3, alignment: "derived" });
    expect(
      await screen.findByRole("button", {
        name: "Play from 0:04, approximate (derived) timing",
      }),
    ).toBeInTheDocument();
    expect(screen.getAllByText("approximate (derived)").length).toBeGreaterThan(0);
  });

  it("offers no chip and no seek affordance without a timeline", async () => {
    const { onSeek } = renderPane({ segments: 5, alignment: "none" });
    await screen.findByTestId("segment-0");
    expect(screen.queryByRole("button", { name: /play from/i })).toBeNull();
    expect(onSeek).not.toHaveBeenCalled();
  });

  it("has no accessibility violations", async () => {
    renderPane({ segments: 4 });
    await screen.findByTestId("segment-0");
    await expectNoAxeViolations();
  });
});

describe("search", () => {
  it("counts matches, moves between them and announces the count", async () => {
    const { user } = renderPane({ segments: 8 });
    await screen.findByTestId("segment-0");
    await user.type(screen.getByLabelText("Search transcript"), "decision");
    const status = screen.getByRole("status");
    await waitFor(() => expect(status.textContent).toMatch(/Match 1 of 2 segments/));
    expect(status).toHaveAttribute("aria-live", "polite");
    expect(screen.getAllByText("decision", { selector: "mark" }).length).toBeGreaterThan(0);
    await user.click(screen.getByRole("button", { name: /next match/i }));
    expect(status.textContent).toMatch(/Match 2 of 2 segments/);
  });

  it("renders a small window of a 100k segment transcript and still searches", async () => {
    const { user } = renderPane({ segments: 100_000 });
    await screen.findByTestId("segment-0");
    const rows = screen.getAllByRole("listitem");
    expect(rows.length).toBeGreaterThan(0);
    expect(rows.length).toBeLessThan(40);

    await user.type(screen.getByLabelText("Search transcript"), "decision");
    const status = screen.getByRole("status");
    await waitFor(() => expect(status.textContent).toMatch(/segments loaded of 100000/));
    expect(status.textContent).toMatch(/Match 1 of 50 segments/);
  });
});

describe("copying", () => {
  it("copies the selected passage as [m:ss] text", async () => {
    const { user } = renderPane({ segments: 4 });
    await screen.findByTestId("segment-1");
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: { writeText },
    });
    await user.click(screen.getByRole("checkbox", { name: "Select segment 2 for copying" }));
    await user.click(screen.getByRole("button", { name: /copy passage/i }));
    await waitFor(() =>
      expect(writeText).toHaveBeenCalledWith(
        "[0:04] Segment 2: the decision is to keep everything local by default",
      ),
    );
    expect(screen.getByRole("status").textContent).toMatch(/Copied 1 segment/);
  });
});

describe("editing", () => {
  it("edits with the keyboard, undoes the whole session and saves a new revision", async () => {
    const { user, backend, revision, recordingId, onSaved } = renderPane({ segments: 3 });
    const row = await screen.findByTestId("segment-0");
    const original = backend.recordings[0]!.segments.get(revision.revision_id)![0]!.text;

    const text = within(row).getByRole("button", { name: /^Segment 1:/ });
    text.focus();
    await user.keyboard("{Enter}");
    const editor = await screen.findByRole("textbox", { name: "Edit segment 1" });
    await user.clear(editor);
    await user.type(editor, "corrected opening line");
    expect(editor).toHaveValue("corrected opening line");

    await user.keyboard("{Control>}z{/Control}");
    await waitFor(() => expect(screen.getByRole("textbox", { name: "Edit segment 1" })).toHaveValue(original));

    await user.type(screen.getByRole("textbox", { name: "Edit segment 1" }), " — revised");
    const save = screen.getByRole("button", { name: /save 1 edited segment/i });
    await user.click(save);

    await waitFor(() => expect(onSaved).toHaveBeenCalled());
    const response = onSaved.mock.calls[0]![0] as SaveTranscriptRevisionResponse;
    expect(response.revision_id).not.toBe(revision.revision_id);

    const saved = backend.calls.find((call) => call.command === "save_transcript_revision");
    expect(saved?.args).toMatchObject({
      recording_id: recordingId,
      base_revision_id: revision.revision_id,
      edits: [{ index: 0, text: `${original} — revised` }],
    });

    // The machine revision is immutable: it still reads back unchanged.
    const before = await invokeCommand("get_transcript", {
      revision_id: revision.revision_id,
      cursor: null,
      limit: 200,
    });
    expect(before.segments[0]!.text).toBe(original);
    const after = await invokeCommand("get_transcript", {
      revision_id: response.revision_id,
      cursor: null,
      limit: 200,
    });
    expect(after.segments[0]!.text).toBe(`${original} — revised`);
  });

  it("cancels an editing session with Escape", async () => {
    const { user } = renderPane({ segments: 2 });
    const row = await screen.findByTestId("segment-0");
    await user.dblClick(within(row).getByRole("button", { name: /^Segment 1:/ }));
    const editor = await screen.findByRole("textbox", { name: "Edit segment 1" });
    await user.type(editor, "typo");
    await user.keyboard("{Escape}");
    expect(screen.queryByRole("textbox", { name: "Edit segment 1" })).toBeNull();
    expect(screen.queryByText("Unsaved edit")).toBeNull();
  });

  it("offers no editing affordance on a read-only revision", async () => {
    renderPane({ segments: 2, editable: false });
    await screen.findByTestId("segment-0");
    expect(screen.queryByRole("button", { name: /^Segment 1:/ })).toBeNull();
    expect(screen.queryByRole("button", { name: /save/i })).toBeNull();
    expect(screen.getByText(/This revision is read-only/)).toBeInTheDocument();
  });

  it("keeps the user's text and offers both resolutions on a revision conflict", async () => {
    const { user, backend, revision, recordingId, onReloadRequested } = renderPane({ segments: 3 });
    await screen.findByTestId("segment-0");
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, "clipboard", { configurable: true, value: { writeText } });

    const row = screen.getByTestId("segment-0");
    await user.dblClick(within(row).getByRole("button", { name: /^Segment 1:/ }));
    const editor = await screen.findByRole("textbox", { name: "Edit segment 1" });
    await user.clear(editor);
    await user.type(editor, "my local wording");

    // Somebody else advanced the head revision in the meantime.
    await backend.invoke("save_transcript_revision", {
      recording_id: recordingId,
      base_revision_id: revision.revision_id,
      edits: [{ index: 1, text: "a newer revision" }],
    });

    await user.click(screen.getByRole("button", { name: /save 1 edited segment/i }));

    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toMatch(/changed since you opened it/);
    expect(within(alert).getByRole("button", { name: /reload newest revision/i })).toBeInTheDocument();
    expect(within(alert).getByRole("button", { name: /copy my changes/i })).toBeInTheDocument();
    // Nothing was overwritten: the edit is still pending with the user's text.
    const pendingRow = screen.getByTestId("segment-0");
    expect(within(pendingRow).getByText("Unsaved edit")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /save 1 edited segment/i })).toBeInTheDocument();
    await user.dblClick(within(pendingRow).getByRole("button", { name: /^Segment 1:/ }));
    expect(await screen.findByRole("textbox", { name: "Edit segment 1" })).toHaveValue(
      "my local wording",
    );

    await user.click(within(alert).getByRole("button", { name: /copy my changes/i }));
    await waitFor(() => expect(writeText).toHaveBeenCalledWith("[0:00] my local wording"));

    await user.click(within(alert).getByRole("button", { name: /reload newest revision/i }));
    expect(onReloadRequested).toHaveBeenCalled();
  });
});

describe("paging", () => {
  it("requests 200 segments per page and can load the next page", async () => {
    const { user, backend } = renderPane({ segments: 450 });
    await screen.findByTestId("segment-0");
    expect(backend.calls.filter((c) => c.command === "get_transcript")[0]?.args).toMatchObject({
      cursor: null,
      limit: 200,
    });
    await user.click(screen.getByRole("button", { name: /load more segments \(200 of 450\)/i }));
    await waitFor(() =>
      expect(screen.getByRole("status").textContent).toMatch(/400 of 450 segments loaded/),
    );
  });
});
