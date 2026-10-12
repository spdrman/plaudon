import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import { ExportMenu } from "./ExportMenu";
import type { RecordingDetail } from "../../ipc/contract";
import { AppStateProvider } from "../../app/AppState";
import { FakeBackend } from "../../testing/fakeBackend";
import { IpcProvider } from "../../ipc/client";

const mockDetail: RecordingDetail = {
  recording_id: "rec-00001",
  title: "Test Recording",
  recorded_at: "2026-01-01T00:00:00Z",
  imported_at: "2026-01-01T00:10:00Z",
  duration_ms: 60000,
  source: { source_kind: "manual_import", label: "Manual import" },
  import_status: "imported",
  transcription_status: "completed",
  processing_engine: "whisper.cpp",
  last_error: null,
  tags: [],
  byte_length: 1024000,
  sha256: "0".repeat(64),
  media: {
    observed_codec: "audio/mpeg",
    sample_rate_hz: 44100,
    channels: 1,
    original_file_name: "test.mp3",
  },
  revisions: [
    {
      revision_id: "rev-001",
      kind: "machine",
      created_at: "2026-01-01T00:15:00Z",
      engine_id: "whisper.cpp",
      model_id: "base.en",
      alignment_origin: "native",
      segment_count: 10,
      superseded: false,
    },
  ],
  current_revision_id: "rev-001",
  outputs: [],
};

describe("ExportMenu", () => {
  let backend: FakeBackend;

  beforeEach(() => {
    backend = new FakeBackend();
    vi.clearAllMocks();
  });

  it("renders export button", () => {
    render(
      <IpcProvider transport={backend}>
        <AppStateProvider>
          <ExportMenu
            detail={mockDetail}
            currentRevisionId="rev-001"
            alignmentOrigin="native"
          />
        </AppStateProvider>
      </IpcProvider>
    );

    expect(screen.getByText("Export")).toBeInTheDocument();
  });

  it("shows transcript format options when opened", async () => {
    render(
      <IpcProvider transport={backend}>
        <AppStateProvider>
          <ExportMenu
            detail={mockDetail}
            currentRevisionId="rev-001"
            alignmentOrigin="native"
          />
        </AppStateProvider>
      </IpcProvider>
    );

    fireEvent.click(screen.getByText("Export"));

    await waitFor(() => {
      expect(screen.getByText("Plain text")).toBeInTheDocument();
      expect(screen.getByText("Markdown")).toBeInTheDocument();
      expect(screen.getByText("Structured JSON")).toBeInTheDocument();
      expect(screen.getByText("SubRip captions")).toBeInTheDocument();
      expect(screen.getByText("WebVTT captions")).toBeInTheDocument();
    });
  });

  it("disables SRT and VTT when alignment is none", async () => {
    render(
      <IpcProvider transport={backend}>
        <AppStateProvider>
          <ExportMenu
            detail={mockDetail}
            currentRevisionId="rev-001"
            alignmentOrigin="none"
          />
        </AppStateProvider>
      </IpcProvider>
    );

    fireEvent.click(screen.getByText("Export"));

    await waitFor(() => {
      const srtButton = screen.getByText(/SubRip captions/);
      const vttButton = screen.getByText(/WebVTT captions/);
      expect(srtButton.closest("button")).toBeDisabled();
      expect(vttButton.closest("button")).toBeDisabled();
      expect(srtButton.textContent).toContain("unavailable");
      expect(vttButton.textContent).toContain("unavailable");
    });
  });

  it("enables SRT and VTT when alignment is native", async () => {
    render(
      <IpcProvider transport={backend}>
        <AppStateProvider>
          <ExportMenu
            detail={mockDetail}
            currentRevisionId="rev-001"
            alignmentOrigin="native"
          />
        </AppStateProvider>
      </IpcProvider>
    );

    fireEvent.click(screen.getByText("Export"));

    await waitFor(() => {
      const srtButton = screen.getByText("SubRip captions");
      const vttButton = screen.getByText("WebVTT captions");
      expect(srtButton.closest("button")).not.toBeDisabled();
      expect(vttButton.closest("button")).not.toBeDisabled();
    });
  });

  it("enables SRT and VTT when alignment is derived", async () => {
    render(
      <IpcProvider transport={backend}>
        <AppStateProvider>
          <ExportMenu
            detail={mockDetail}
            currentRevisionId="rev-001"
            alignmentOrigin="derived"
          />
        </AppStateProvider>
      </IpcProvider>
    );

    fireEvent.click(screen.getByText("Export"));

    await waitFor(() => {
      const srtButton = screen.getByText("SubRip captions");
      const vttButton = screen.getByText("WebVTT captions");
      expect(srtButton.closest("button")).not.toBeDisabled();
      expect(vttButton.closest("button")).not.toBeDisabled();
    });
  });
});
