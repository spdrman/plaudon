import { describe, it, expect, vi, beforeEach } from "vitest";
import { screen, waitFor } from "@testing-library/react";
import { BackupRestoreSection } from "./BackupRestoreSection";
import { FakeBackend } from "../../testing/fakeBackend";
import { renderWithApp } from "../../testing/renderWithApp";

// Mock Tauri dialog
const mockOpen = vi.hoisted(() => vi.fn());
vi.mock("@tauri-apps/plugin-dialog", () => ({
  open: mockOpen,
}));

describe("BackupRestoreSection", () => {
  let backend: FakeBackend;

  beforeEach(() => {
    backend = new FakeBackend({ recordings: 5 });
    vi.clearAllMocks();
  });

  it("renders backup and restore buttons", () => {
    renderWithApp(<BackupRestoreSection />, { backend });

    expect(screen.getByText("Create backup")).toBeInTheDocument();
    expect(screen.getByText("Restore from archive")).toBeInTheDocument();
  });

  it("shows plaintext disclosure when no password provided", async () => {
    const { user } = renderWithApp(<BackupRestoreSection />, { backend });

    await user.click(screen.getByText("Create backup"));

    await waitFor(() => {
      expect(screen.getByText(/This backup will NOT be encrypted/)).toBeInTheDocument();
    });
  });

  it("shows encrypted disclosure when password provided", async () => {
    const { user } = renderWithApp(<BackupRestoreSection />, { backend });

    await user.click(screen.getByText("Create backup"));

    await waitFor(async () => {
      const passwordField = screen.getByLabelText(/Password \(optional\)/);
      await user.type(passwordField, "test123");
    });

    await waitFor(() => {
      expect(screen.getByText(/This backup will be encrypted/)).toBeInTheDocument();
      expect(screen.getByText(/We cannot recover a lost password/)).toBeInTheDocument();
    });
  });

  it("displays backup results with integrity note", async () => {
    mockOpen.mockResolvedValue("/fake/destination");
    const { user } = renderWithApp(<BackupRestoreSection />, { backend });

    await user.click(screen.getByText("Create backup"));

    await waitFor(async () => {
      const createButton = screen.getByText("Create backup now", { selector: "button" });
      await user.click(createButton);
    });

    await waitFor(() => {
      expect(screen.getByText(/Backup created successfully/)).toBeInTheDocument();
      expect(screen.getByText(/Integrity digest/)).toBeInTheDocument();
      expect(
        screen.getByText(/The digest proves the manifest matches the backup/)
      ).toBeInTheDocument();
    });
  });

  it("displays nonportable settings in backup result", async () => {
    mockOpen.mockResolvedValue("/fake/destination");
    const { user } = renderWithApp(<BackupRestoreSection />, { backend });

    await user.click(screen.getByText("Create backup"));

    await waitFor(async () => {
      const createButton = screen.getByText("Create backup now", { selector: "button" });
      await user.click(createButton);
    });

    await waitFor(() => {
      expect(screen.getByText(/Settings not included/)).toBeInTheDocument();
      expect(screen.getByText(/OpenRouter API key/)).toBeInTheDocument();
    });
  });

  it("calls inspect before allowing restore", async () => {
    mockOpen.mockResolvedValue("/fake/archive.plaudon");
    const { user } = renderWithApp(<BackupRestoreSection />, { backend });

    await user.click(screen.getByText("Restore from archive"));

    await waitFor(async () => {
      const chooseButton = screen.getByText("Choose archive");
      await user.click(chooseButton);
    });

    await waitFor(async () => {
      const inspectButton = screen.getByText("Inspect archive");
      expect(inspectButton).not.toBeDisabled();
      await user.click(inspectButton);
    });

    await waitFor(() => {
      expect(backend.calls).toContainEqual(
        expect.objectContaining({
          command: "inspect_restore_archive",
        })
      );
    });
  });

  it("displays inspection results with namespace counts", async () => {
    mockOpen.mockResolvedValue("/fake/archive.plaudon");
    const { user } = renderWithApp(<BackupRestoreSection />, { backend });

    await user.click(screen.getByText("Restore from archive"));

    await waitFor(async () => {
      const chooseButton = screen.getByText("Choose archive");
      await user.click(chooseButton);
    });

    await waitFor(async () => {
      const inspectButton = screen.getByText("Inspect archive");
      await user.click(inspectButton);
    });

    await waitFor(() => {
      expect(screen.getByText(/Archive information/)).toBeInTheDocument();
      expect(screen.getByText(/Contents/)).toBeInTheDocument();
      expect(screen.getByText(/Recordings: 5/)).toBeInTheDocument();
    });
  });

  it("shows conflicts in inspection", async () => {
    mockOpen.mockResolvedValue("/fake/archive.plaudon");
    const { user } = renderWithApp(<BackupRestoreSection />, { backend });

    await user.click(screen.getByText("Restore from archive"));

    await waitFor(async () => {
      const chooseButton = screen.getByText("Choose archive");
      await user.click(chooseButton);
    });

    await waitFor(async () => {
      const inspectButton = screen.getByText("Inspect archive");
      await user.click(inspectButton);
    });

    await waitFor(() => {
      expect(screen.getByText(/Conflicts/)).toBeInTheDocument();
      expect(screen.getByText(/already exist in this library/)).toBeInTheDocument();
    });
  });

  it("provides restore mode and conflict policy options", async () => {
    mockOpen.mockResolvedValue("/fake/archive.plaudon");
    const { user } = renderWithApp(<BackupRestoreSection />, { backend });

    await user.click(screen.getByText("Restore from archive"));

    await waitFor(async () => {
      const chooseButton = screen.getByText("Choose archive");
      await user.click(chooseButton);
    });

    await waitFor(async () => {
      const inspectButton = screen.getByText("Inspect archive");
      await user.click(inspectButton);
    });

    await waitFor(() => {
      expect(screen.getByText(/Restore options/)).toBeInTheDocument();
      expect(screen.getByText(/Mode/)).toBeInTheDocument();
      expect(screen.getByText(/On conflict/)).toBeInTheDocument();
      expect(
        screen.getByText(/Restore previously deleted recordings/)
      ).toBeInTheDocument();
    });
  });

  it("target_is_empty controls new_library mode availability", async () => {
    mockOpen.mockResolvedValue("/fake/archive.plaudon");
    const { user } = renderWithApp(<BackupRestoreSection />, { backend });

    await user.click(screen.getByText("Restore from archive"));

    await waitFor(async () => {
      const chooseButton = screen.getByText("Choose archive");
      await user.click(chooseButton);
    });

    await waitFor(async () => {
      const inspectButton = screen.getByText("Inspect archive");
      await user.click(inspectButton);
    });

    await waitFor(() => {
      const modeSelect = screen.getByLabelText(/Mode/) as HTMLSelectElement;
      const newLibraryOption = Array.from(modeSelect.options).find((opt) =>
        opt.textContent?.includes("New library")
      );
      // Should be disabled because backend has recordings (target_is_empty = false)
      expect(newLibraryOption?.disabled).toBe(true);
    });
  });
});
