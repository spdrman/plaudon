import { describe, expect, it, vi } from "vitest";
import { screen, waitFor } from "@testing-library/react";
import { open } from "@tauri-apps/plugin-dialog";
import type { ImportFilesRequest } from "../../ipc/contract";
import { renderWithApp } from "../../testing/renderWithApp";
import { FakeBackend } from "../../testing/fakeBackend";
import { expectNoAxeViolations } from "../../testing/axe";
import { ImportButton } from "./ImportButton";
import { ImportDropTarget } from "./ImportDropTarget";

vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn() }));
const openPicker = vi.mocked(open);

const PATH = "/Users/t/kickoff.mp3";

function importRequests(backend: FakeBackend): ImportFilesRequest[] {
  return backend.calls
    .filter((call) => call.command === "import_files")
    .map((call) => call.args as ImportFilesRequest);
}

/** Imports the same file twice; the second attempt is reported as a duplicate. */
async function importTwice() {
  openPicker.mockResolvedValue([PATH]);
  const backend = new FakeBackend();
  const rendered = renderWithApp(
    <ImportDropTarget>
      <ImportButton />
    </ImportDropTarget>,
    { backend },
  );
  const button = screen.getByRole("button", { name: /Import audio/ });
  await rendered.user.click(button);
  await waitFor(() => expect(importRequests(backend)).toHaveLength(1));
  await rendered.user.click(button);
  await screen.findByRole("dialog");
  return { ...rendered, backend };
}

describe("DuplicateDialog", () => {
  it("offers the three explicit choices and says the match is by content hash", async () => {
    await importTwice();

    const dialog = screen.getByRole("dialog");
    expect(dialog).toHaveTextContent(/content hash \(SHA-256\)/i);
    expect(dialog).toHaveTextContent(/not by file name/i);
    expect(
      screen.getByRole("button", { name: "Import anyway as a separate recording" }),
    ).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Open existing recording" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Skip" })).toBeInTheDocument();
    await expectNoAxeViolations();
  });

  it("re-invokes import_files with import_anyway when the user keeps both", async () => {
    const { backend, user } = await importTwice();

    await user.click(screen.getByRole("button", { name: "Import anyway as a separate recording" }));

    await waitFor(() => expect(importRequests(backend)).toHaveLength(3));
    expect(importRequests(backend)[2]).toEqual({
      paths: [PATH],
      duplicate_policy: "import_anyway",
    });
    await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
    expect(backend.recordings).toHaveLength(2);
  });

  it("records an explicit skip rather than silently dropping the file", async () => {
    const { backend, user } = await importTwice();

    await user.click(screen.getByRole("button", { name: "Skip" }));

    await waitFor(() => expect(importRequests(backend)).toHaveLength(3));
    expect(importRequests(backend)[2]!.duplicate_policy).toBe("skip");
    expect(await screen.findByText("Skipped")).toBeInTheDocument();
    expect(backend.recordings).toHaveLength(1);
  });

  it("navigates to the existing recording instead of importing again", async () => {
    const { backend, user } = await importTwice();

    await user.click(screen.getByRole("button", { name: "Open existing recording" }));

    await waitFor(() => expect(importRequests(backend)).toHaveLength(3));
    expect(importRequests(backend)[2]!.duplicate_policy).toBe("open_existing");
    await waitFor(() => expect(window.location.hash).toContain(backend.recordings[0]!.recording_id));
    expect(backend.recordings).toHaveLength(1);
  });

  it("keeps undecided duplicates visible when the user defers", async () => {
    const { user } = await importTwice();

    await user.click(screen.getByRole("button", { name: "Decide later" }));

    await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
    const panel = screen.getByRole("region", { name: "Import results" });
    expect(panel).toHaveTextContent("Needs a decision");
    expect(panel).toHaveTextContent(PATH);
  });
});
