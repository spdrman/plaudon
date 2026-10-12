import { describe, expect, it, vi } from "vitest";
import { fireEvent, screen, waitFor } from "@testing-library/react";
import { open } from "@tauri-apps/plugin-dialog";
import type { ImportFilesRequest } from "../../ipc/contract";
import { renderWithApp } from "../../testing/renderWithApp";
import { FakeBackend } from "../../testing/fakeBackend";
import { expectNoAxeViolations } from "../../testing/axe";
import { ImportButton } from "./ImportButton";
import { ImportDropTarget, filesToPaths } from "./ImportDropTarget";

vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn() }));
const openPicker = vi.mocked(open);

function importRequests(backend: FakeBackend): ImportFilesRequest[] {
  return backend.calls
    .filter((call) => call.command === "import_files")
    .map((call) => call.args as ImportFilesRequest);
}

function audioFile(name: string, path: string): File {
  const file = new File(["audio-bytes"], name, { type: "audio/mpeg" });
  Object.defineProperty(file, "path", { value: path });
  return file;
}

function renderDropTarget(backend = new FakeBackend()) {
  return renderWithApp(
    <ImportDropTarget>
      <ImportButton />
    </ImportDropTarget>,
    { backend },
  );
}

describe("ImportDropTarget", () => {
  it("reads paths from dropped files without ever sending a File object", () => {
    const file = audioFile("one.mp3", "/Users/t/one.mp3");
    const nameOnly = new File(["x"], "two.mp3", { type: "audio/mpeg" });
    expect(filesToPaths([file, nameOnly] as unknown as FileList)).toEqual([
      "/Users/t/one.mp3",
      "two.mp3",
    ]);
    expect(filesToPaths(null)).toEqual([]);
  });

  it("shows a worded drop affordance while dragging and announces it", async () => {
    renderDropTarget();
    fireEvent.dragEnter(window);

    const overlay = await screen.findByTestId("import-drop-overlay");
    expect(overlay).toHaveTextContent("Drop audio files to import");
    expect(overlay).toHaveTextContent("MP3");
    expect(screen.getByText("Drop audio files to import.")).toBeInTheDocument();

    fireEvent.dragLeave(window);
    await waitFor(() => expect(screen.queryByTestId("import-drop-overlay")).not.toBeInTheDocument());
  });

  it("imports dropped paths with duplicate_policy ask and reports per-file progress", async () => {
    const { backend } = renderDropTarget();
    fireEvent.dragEnter(window);
    fireEvent.drop(window, {
      dataTransfer: {
        files: [audioFile("one.mp3", "/Users/t/one.mp3"), audioFile("two.m4a", "/Users/t/two.m4a")],
      },
    });

    await waitFor(() => expect(importRequests(backend)).toHaveLength(1));
    expect(importRequests(backend)[0]).toEqual({
      paths: ["/Users/t/one.mp3", "/Users/t/two.m4a"],
      duplicate_policy: "ask",
    });
    const panel = await screen.findByRole("region", { name: "Import results" });
    expect(panel).toHaveTextContent("/Users/t/one.mp3");
    expect(panel).toHaveTextContent("/Users/t/two.m4a");
    expect(screen.queryByTestId("import-drop-overlay")).not.toBeInTheDocument();
  });

  it("sends the same paths from the picker and from a drop", async () => {
    const paths = ["/Users/t/one.mp3", "/Users/t/two.m4a"];
    openPicker.mockResolvedValue(paths);
    const { backend, user } = renderDropTarget();

    await user.click(screen.getByRole("button", { name: /Import audio/ }));
    await waitFor(() => expect(importRequests(backend)).toHaveLength(1));

    fireEvent.drop(window, {
      dataTransfer: { files: [audioFile("one.mp3", paths[0]!), audioFile("two.m4a", paths[1]!)] },
    });
    await waitFor(() => expect(importRequests(backend)).toHaveLength(2));

    expect(importRequests(backend)[0]).toEqual(importRequests(backend)[1]);
    expect(importRequests(backend)[1]!.duplicate_policy).toBe("ask");
  });

  it("has no axe violations with results on screen", async () => {
    openPicker.mockResolvedValue(["/Users/t/one.mp3"]);
    const { user } = renderDropTarget();
    await user.click(screen.getByRole("button", { name: /Import audio/ }));
    await screen.findByRole("region", { name: "Import results" });
    await expectNoAxeViolations();
  });
});
