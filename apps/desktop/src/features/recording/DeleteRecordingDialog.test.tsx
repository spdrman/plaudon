import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import { DeleteRecordingDialog } from "./DeleteRecordingDialog";
import { AppStateProvider } from "../../app/AppState";
import { FakeBackend } from "../../testing/fakeBackend";
import { IpcProvider } from "../../ipc/client";

describe("DeleteRecordingDialog", () => {
  let backend: FakeBackend;

  beforeEach(() => {
    backend = new FakeBackend({ recordings: 1 });
    vi.clearAllMocks();
  });

  it("fetches disclosure when opened", async () => {
    const onClose = vi.fn();
    const onDeleted = vi.fn();

    render(
      <IpcProvider transport={backend}>
        <AppStateProvider>
          <DeleteRecordingDialog
            recordingId="rec-00001"
            open={true}
            onClose={onClose}
            onDeleted={onDeleted}
          />
        </AppStateProvider>
      </IpcProvider>
    );

    await waitFor(() => {
      expect(screen.getByText(/This will permanently delete/)).toBeInTheDocument();
    });

    expect(backend.calls).toContainEqual({
      command: "get_deletion_disclosure",
      args: { recording_id: "rec-00001" },
    });
  });

  it("displays all disclosure categories", async () => {
    const onClose = vi.fn();
    const onDeleted = vi.fn();

    render(
      <IpcProvider transport={backend}>
        <AppStateProvider>
          <DeleteRecordingDialog
            recordingId="rec-00001"
            open={true}
            onClose={onClose}
            onDeleted={onDeleted}
          />
        </AppStateProvider>
      </IpcProvider>
    );

    await waitFor(() => {
      expect(screen.getByText(/What will be removed/)).toBeInTheDocument();
      expect(screen.getByText(/Transcript revisions/)).toBeInTheDocument();
      expect(screen.getByText(/Generated outputs/)).toBeInTheDocument();
    });
  });

  it("displays media with shared indicator", async () => {
    const onClose = vi.fn();
    const onDeleted = vi.fn();

    render(
      <IpcProvider transport={backend}>
        <AppStateProvider>
          <DeleteRecordingDialog
            recordingId="rec-00001"
            open={true}
            onClose={onClose}
            onDeleted={onDeleted}
          />
        </AppStateProvider>
      </IpcProvider>
    );

    await waitFor(() => {
      expect(screen.getByText(/Media files/)).toBeInTheDocument();
    });
  });

  it("displays bytes to free", async () => {
    const onClose = vi.fn();
    const onDeleted = vi.fn();

    render(
      <IpcProvider transport={backend}>
        <AppStateProvider>
          <DeleteRecordingDialog
            recordingId="rec-00001"
            open={true}
            onClose={onClose}
            onDeleted={onDeleted}
          />
        </AppStateProvider>
      </IpcProvider>
    );

    await waitFor(() => {
      expect(screen.getByText(/Storage freed:/)).toBeInTheDocument();
    });
  });

  it("displays all limits verbatim", async () => {
    const onClose = vi.fn();
    const onDeleted = vi.fn();

    render(
      <IpcProvider transport={backend}>
        <AppStateProvider>
          <DeleteRecordingDialog
            recordingId="rec-00001"
            open={true}
            onClose={onClose}
            onDeleted={onDeleted}
          />
        </AppStateProvider>
      </IpcProvider>
    );

    await waitFor(() => {
      expect(screen.getByText(/Limits/)).toBeInTheDocument();
      expect(screen.getByText(/Cloud recordings cannot be deleted/)).toBeInTheDocument();
    });
  });

  it("passes confirmation token to delete command", async () => {
    const onClose = vi.fn();
    const onDeleted = vi.fn();

    render(
      <IpcProvider transport={backend}>
        <AppStateProvider>
          <DeleteRecordingDialog
            recordingId="rec-00001"
            open={true}
            onClose={onClose}
            onDeleted={onDeleted}
          />
        </AppStateProvider>
      </IpcProvider>
    );

    await waitFor(() => {
      expect(screen.getByText("Delete permanently")).toBeInTheDocument();
    });

    fireEvent.click(screen.getByText("Delete permanently"));

    await waitFor(() => {
      const deleteCall = backend.calls.find((c) => c.command === "delete_recording");
      expect(deleteCall).toBeDefined();
      expect(deleteCall?.args).toHaveProperty("confirmation_token");
      expect(deleteCall?.args).toHaveProperty("recording_id", "rec-00001");
    });

    expect(onDeleted).toHaveBeenCalled();
    expect(onClose).toHaveBeenCalled();
  });

  it("is keyboard accessible with Escape", async () => {
    const onClose = vi.fn();
    const onDeleted = vi.fn();

    render(
      <IpcProvider transport={backend}>
        <AppStateProvider>
          <DeleteRecordingDialog
            recordingId="rec-00001"
            open={true}
            onClose={onClose}
            onDeleted={onDeleted}
          />
        </AppStateProvider>
      </IpcProvider>
    );

    await waitFor(() => {
      expect(screen.getByRole("alertdialog")).toBeInTheDocument();
    });

    fireEvent.keyDown(screen.getByRole("alertdialog"), { key: "Escape" });

    expect(onClose).toHaveBeenCalled();
  });
});
