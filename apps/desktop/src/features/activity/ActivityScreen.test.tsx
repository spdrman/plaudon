import { describe, expect, it, vi } from "vitest";
import { screen, waitFor, within } from "@testing-library/react";
import { FakeBackend, envelope } from "../../testing/fakeBackend";
import { renderWithApp } from "../../testing/renderWithApp";
import { expectNoAxeViolations } from "../../testing/axe";
import { ActivityScreen } from "./ActivityScreen";

function noop(): void {
  /* deep links asserted explicitly where they matter */
}

const PATHLIKE = /(^|\s)([A-Za-z]:\\|\/[A-Za-z0-9._-]+\/)|\\\\/;

describe("ActivityScreen", () => {
  it("lists every category with a badge, timestamp, delivery state and deep link", async () => {
    const backend = new FakeBackend();
    backend.addActivity({
      category: "import_finished",
      title: "Import finished",
      body: "1 recording was added to your library.",
      target: { kind: "recording", id: "rec-00001" },
    });
    backend.addActivity({
      category: "transcription_finished",
      title: "Transcription finished",
      body: "A transcript is ready to review.",
      target: { kind: "job", id: "job-7" },
    });
    backend.addActivity({
      category: "requires_attention",
      title: "Requires attention",
      body: "A job needs a decision from you.",
    });
    backend.addActivity({
      category: "cloud_budget_exceeded",
      title: "Cloud budget exceeded",
      body: "Cloud processing stopped at your spending limit.",
      target: { kind: "settings", id: "ai" },
    });
    const onOpenTarget = vi.fn();
    const { user } = renderWithApp(<ActivityScreen onOpenTarget={onOpenTarget} />, { backend });

    expect(await screen.findByText("Cloud budget exceeded", { selector: "p" })).toBeInTheDocument();
    for (const label of [
      "Import finished",
      "Transcription finished",
      "Requires attention",
      "Cloud budget exceeded",
    ]) {
      expect(screen.getAllByText(label).length).toBeGreaterThan(0);
    }
    expect(screen.getAllByText(/Desktop notification delivered/).length).toBe(4);
    expect(screen.getAllByText("Unread").length).toBe(4);

    await user.click(screen.getByRole("button", { name: "Open recording" }));
    expect(onOpenTarget).toHaveBeenCalledWith({ kind: "recording", id: "rec-00001" });
    await user.click(screen.getByRole("button", { name: "Open job" }));
    expect(onOpenTarget).toHaveBeenCalledWith({ kind: "job", id: "job-7" });
    await user.click(screen.getByRole("button", { name: "Open settings" }));
    expect(onOpenTarget).toHaveBeenCalledWith({ kind: "settings", id: "ai" });

    await expectNoAxeViolations();
  });

  it("stays complete and explains suppression when the OS denied notifications", async () => {
    const backend = new FakeBackend({ osNotificationPermission: "denied" });
    backend.addActivity({
      category: "transcription_finished",
      title: "Transcription finished",
      body: "A transcript is ready to review.",
      target: { kind: "recording", id: "rec-00001" },
    });
    renderWithApp(<ActivityScreen onOpenTarget={noop} />, { backend });

    expect(await screen.findByText(/Desktop notification not delivered/)).toBeInTheDocument();
    expect(
      screen.getByText(/OS notifications are not permitted\./),
    ).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Open recording" })).toBeInTheDocument();
    await expectNoAxeViolations();
  });

  it("marks unread entries with words, not color", async () => {
    const backend = new FakeBackend();
    backend.addActivity({ category: "import_finished", title: "Import finished", read: true });
    backend.addActivity({ category: "requires_attention", title: "Requires attention" });
    renderWithApp(<ActivityScreen onOpenTarget={noop} />, { backend });

    expect(await screen.findByText("Unread")).toBeInTheDocument();
    expect(screen.getByText("Read")).toBeInTheDocument();
  });

  it("never shows transcript text or file paths", async () => {
    const backend = new FakeBackend({ recordings: 1 });
    await backend.invoke("import_files", {
      paths: ["/Users/tester/Audio/board-call.m4a"],
      duplicate_policy: "import_anyway",
    });
    renderWithApp(<ActivityScreen onOpenTarget={noop} />, { backend });

    await screen.findByText("Import finished", { selector: "p" });
    for (const entry of backend.activity) {
      expect(entry.body).not.toMatch(PATHLIKE);
      expect(entry.title).not.toMatch(PATHLIKE);
      expect(entry.body).toBe("1 recording was added to your library.");
    }
    const list = screen.getByRole("list");
    expect(within(list).queryByText(PATHLIKE)).toBeNull();
  });

  it("refetches after a dropped activity event instead of inventing entries", async () => {
    const backend = new FakeBackend();
    backend.addActivity({ category: "import_finished", title: "Import finished" });
    renderWithApp(<ActivityScreen onOpenTarget={noop} />, { backend });
    await screen.findByText("Import finished", { selector: "p" });

    const before = backend.calls.filter((c) => c.command === "list_activity").length;
    backend.addActivity({ category: "requires_attention", title: "Requires attention" });
    backend.dropNextEvent = true;
    backend.emit("activity.added");

    await waitFor(() =>
      expect(backend.calls.filter((c) => c.command === "list_activity").length).toBe(before + 1),
    );
    expect(await screen.findByText("Requires attention", { selector: "p" })).toBeInTheDocument();
  });

  it("pages older activity on request", async () => {
    const backend = new FakeBackend();
    for (let i = 0; i < 30; i += 1) {
      backend.addActivity({ category: "import_finished", title: `Import ${i}` });
    }
    const { user } = renderWithApp(<ActivityScreen onOpenTarget={noop} />, { backend });

    await screen.findByText("Import 29", { selector: "p" });
    expect(screen.queryByText("Import 0", { selector: "p" })).toBeNull();
    await user.click(screen.getByRole("button", { name: "Load older activity" }));
    expect(await screen.findByText("Import 0", { selector: "p" })).toBeInTheDocument();
  });

  it("shows an empty state and a failing query honestly", async () => {
    const { unmount } = renderWithApp(<ActivityScreen onOpenTarget={noop} />);
    expect(await screen.findByText("No activity yet")).toBeInTheDocument();
    await expectNoAxeViolations();
    unmount();

    const backend = new FakeBackend();
    backend.failures.set(
      "list_activity",
      envelope("INTERNAL_ERROR", "The activity log could not be read."),
    );
    renderWithApp(<ActivityScreen onOpenTarget={noop} />, { backend });
    expect(await screen.findByText("The activity log could not be read.")).toBeInTheDocument();
    expect(screen.getByRole("alert")).toBeInTheDocument();
  });
});
