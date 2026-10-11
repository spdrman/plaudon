import { describe, expect, it, vi } from "vitest";
import { screen, waitFor, within } from "@testing-library/react";
import { renderWithApp } from "../../testing/renderWithApp";
import { FakeBackend, envelope } from "../../testing/fakeBackend";
import { expectNoAxeViolations } from "../../testing/axe";
import { LibraryScreen } from "./LibraryScreen";

function dataRows(): HTMLElement[] {
  return screen.queryAllByRole("row").filter((row) => row.hasAttribute("data-row-index"));
}

function listCalls(backend: FakeBackend) {
  return backend.calls.filter((call) => call.command === "list_recordings");
}

describe("LibraryScreen", () => {
  it("renders the library and reports the full matching set", async () => {
    const backend = new FakeBackend();
    backend.addRecording({ title: "Quarterly review", segments: 2 });
    backend.addRecording({ title: "Budget call", durationMs: null });
    const { container } = renderWithApp(<LibraryScreen onOpenRecording={vi.fn()} />, { backend });

    expect(await screen.findByText("Quarterly review")).toBeInTheDocument();
    expect(screen.getByRole("grid", { name: "Recordings" })).toHaveAttribute("aria-rowcount", "3");
    expect(screen.getByText("Unknown")).toBeInTheDocument();
    await expectNoAxeViolations(container);
  });

  it("opens a recording through the callback instead of rendering a detail view", async () => {
    const onOpen = vi.fn();
    const backend = new FakeBackend({ recordings: 3 });
    const { user } = renderWithApp(<LibraryScreen onOpenRecording={onOpen} />, { backend });
    await screen.findByRole("grid");

    dataRows()[0]!.focus();
    await user.keyboard("{Enter}");

    expect(onOpen).toHaveBeenCalledTimes(1);
    expect(onOpen.mock.calls[0]![0]).toMatch(/^rec-/);
    // Detail stays a Library concern: the screen itself renders no detail route.
    expect(screen.queryByRole("region", { name: /transcript/i })).not.toBeInTheDocument();
  });

  it("offers Import and Connect PLAUD when the library is empty", async () => {
    const { container } = renderWithApp(<LibraryScreen onOpenRecording={vi.fn()} />, {
      recordings: 0,
    });
    expect(await screen.findByText("No recordings")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Import an audio file" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Connect PLAUD" })).toBeInTheDocument();
    expect(screen.getByText(/not part of this build/i)).toBeInTheDocument();
    await expectNoAxeViolations(container);
  });

  it("sends Connect PLAUD to Settings", async () => {
    const { user } = renderWithApp(<LibraryScreen onOpenRecording={vi.fn()} />, { recordings: 0 });
    await screen.findByText("No recordings");
    await user.click(screen.getByRole("button", { name: "Connect PLAUD" }));
    expect(window.location.hash).toContain("settings");
  });

  it("shows the envelope message and the control its action implies", async () => {
    const backend = new FakeBackend({ recordings: 2 });
    backend.failures.set(
      "list_recordings",
      envelope("STORAGE_UNAVAILABLE", "The library folder is not reachable.", {
        action: "FREE_DISK_SPACE",
      }),
    );
    const { user } = renderWithApp(<LibraryScreen onOpenRecording={vi.fn()} />, { backend });

    expect(await screen.findByText(/library folder is not reachable/i)).toBeInTheDocument();
    expect(screen.queryByText(/at Object\./)).not.toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Open storage settings" }));
    expect(window.location.hash).toContain("settings");
  });

  it("retries a retryable failure and recovers", async () => {
    const backend = new FakeBackend();
    backend.addRecording({ title: "Recovered recording" });
    backend.failures.set(
      "list_recordings",
      envelope("SOURCE_UNAVAILABLE", "The library is busy.", { retryable: true }),
    );
    const { user } = renderWithApp(<LibraryScreen onOpenRecording={vi.fn()} />, { backend });

    await screen.findByText(/The library is busy\./);
    backend.failures.delete("list_recordings");
    await user.click(screen.getByRole("button", { name: "Retry" }));
    expect(await screen.findByText("Recovered recording")).toBeInTheDocument();
  });

  it("searches, filters and sorts, rebuilding the request each time", async () => {
    const backend = new FakeBackend();
    backend.addRecording({ title: "Alpha standup" });
    backend.addRecording({ title: "Beta retro" });
    backend.addRecording({ title: "Gamma review", transcriptionStatus: "failed" });
    const { user } = renderWithApp(<LibraryScreen onOpenRecording={vi.fn()} />, { backend });
    await screen.findByText("Alpha standup");

    await user.type(screen.getByLabelText("Search recordings"), "beta");
    await waitFor(() => expect(dataRows()).toHaveLength(1));
    expect(screen.getByText("Beta retro")).toBeInTheDocument();
    const searched = listCalls(backend).at(-1)!.args as { filter: { query: string | null } };
    expect(searched.filter.query).toBe("beta");

    await user.clear(screen.getByLabelText("Search recordings"));
    await waitFor(() => expect(dataRows()).toHaveLength(3));

    await user.selectOptions(screen.getByLabelText("Transcription status"), "failed");
    await waitFor(() => expect(dataRows()).toHaveLength(1));
    expect(screen.getByText("Gamma review")).toBeInTheDocument();

    await user.selectOptions(screen.getByLabelText("Transcription status"), "");
    await waitFor(() => expect(dataRows()).toHaveLength(3));

    await user.click(screen.getByRole("button", { name: /^Title/ }));
    await waitFor(() =>
      expect(screen.getByRole("columnheader", { name: /^Title/ })).toHaveAttribute(
        "aria-sort",
        "ascending",
      ),
    );
    await waitFor(() =>
      expect(within(dataRows()[0]!).getByText("Alpha standup")).toBeInTheDocument(),
    );

    await user.click(screen.getByRole("button", { name: /^Title/ }));
    await waitFor(() =>
      expect(screen.getByRole("columnheader", { name: /^Title/ })).toHaveAttribute(
        "aria-sort",
        "descending",
      ),
    );
    await waitFor(() =>
      expect(within(dataRows()[0]!).getByText("Gamma review")).toBeInTheDocument(),
    );
  });

  it("restarts pagination when a filter changes", async () => {
    const backend = new FakeBackend({ recordings: 150 });
    const { user } = renderWithApp(<LibraryScreen onOpenRecording={vi.fn()} />, { backend });
    await screen.findByRole("grid");
    await user.click(await screen.findByRole("button", { name: "Load more recordings" }));
    await waitFor(() => expect(listCalls(backend).at(-1)!.args).toMatchObject({ cursor: expect.any(String) }));

    await user.selectOptions(screen.getByLabelText("Import status"), "imported");
    await waitFor(() => {
      const last = listCalls(backend).at(-1)!.args as { cursor: string | null };
      expect(last.cursor).toBeNull();
    });
  });

  it("appends the next page without dropping or repeating a recording", async () => {
    const backend = new FakeBackend({ recordings: 150 });
    const { user } = renderWithApp(<LibraryScreen onOpenRecording={vi.fn()} />, { backend });
    await screen.findByRole("grid");
    await waitFor(() => expect(screen.getByText(/Showing 100 of 150/)).toBeInTheDocument());

    await user.click(screen.getByRole("button", { name: "Load more recordings" }));
    await waitFor(() => expect(screen.getByText(/Showing 150 of 150/)).toBeInTheDocument());

    const indexes = dataRows().map((row) => row.getAttribute("aria-rowindex"));
    expect(new Set(indexes).size).toBe(indexes.length);
    expect(screen.queryByRole("button", { name: "Load more recordings" })).not.toBeInTheDocument();
  });

  it("refetches after a dropped event instead of rendering stale rows", async () => {
    const backend = new FakeBackend({ recordings: 2 });
    renderWithApp(<LibraryScreen onOpenRecording={vi.fn()} />, { backend });
    await screen.findByRole("grid");
    const before = listCalls(backend).length;

    backend.addRecording({ title: "Arrived while we were not listening" });
    backend.dropNextEvent = true;
    backend.emit("library.changed");

    expect(
      await screen.findByText("Arrived while we were not listening"),
    ).toBeInTheDocument();
    expect(listCalls(backend).length).toBeGreaterThan(before);
  });

  it("handles a 100k-row library: virtualized rows, working search and sort", async () => {
    const backend = new FakeBackend({ recordings: 100_000 });
    backend.recordings[42_000]!.title = "Needle in the haystack";
    const { user } = renderWithApp(<LibraryScreen onOpenRecording={vi.fn()} />, { backend });

    await screen.findByRole("grid");
    expect(screen.getByRole("grid")).toHaveAttribute("aria-rowcount", "100001");
    expect(dataRows().length).toBeLessThan(100);

    await user.type(screen.getByLabelText("Search recordings"), "Needle in the haystack");
    await waitFor(() => expect(dataRows()).toHaveLength(1), { timeout: 10_000 });
    expect(screen.getByText("Needle in the haystack")).toBeInTheDocument();

    await user.clear(screen.getByLabelText("Search recordings"));
    await waitFor(() => expect(dataRows().length).toBeGreaterThan(1), { timeout: 10_000 });

    // A filter narrows the same 100k set, still without rendering it all.
    await user.selectOptions(screen.getByLabelText("Transcription status"), "completed");
    expect(
      await screen.findByText("No recordings match these filters", undefined, { timeout: 10_000 }),
    ).toBeInTheDocument();

    await user.selectOptions(screen.getByLabelText("Transcription status"), "");
    await waitFor(() => expect(dataRows().length).toBeGreaterThan(1), { timeout: 10_000 });

    // Sorting re-runs the query and changes which rows are rendered.
    const firstBefore = dataRows()[0]!.textContent;
    await user.click(screen.getByRole("button", { name: /^Title/ }));
    await waitFor(
      () => expect(dataRows()[0]!.textContent).not.toBe(firstBefore),
      { timeout: 10_000 },
    );
    expect(screen.getByRole("columnheader", { name: /^Title/ })).toHaveAttribute(
      "aria-sort",
      "ascending",
    );
    expect(dataRows().length).toBeLessThan(100);
  }, 120_000);
});
