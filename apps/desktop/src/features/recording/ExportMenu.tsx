/**
 * Export menu offering transcript formats (TXT, Markdown, JSON, SRT, VTT) and
 * original audio with optional sanitized derivative (J1.3).
 */
import { useState } from "react";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { Button } from "../../components/Button";
import { Dialog } from "../../components/Dialog";
import { useCommandMutation } from "../../ipc/useCommand";
import type { ExportFormat, RecordingDetail, ExportMediaResponse } from "../../ipc/contract";
import { CommandErrorNotice } from "./errorNotice";
import { useAppState } from "../../app/AppState";

const TRANSCRIPT_FORMATS: Array<{ value: ExportFormat; label: string; extension: string }> = [
  { value: "txt", label: "Plain text", extension: ".txt" },
  { value: "markdown", label: "Markdown", extension: ".md" },
  { value: "json", label: "Structured JSON", extension: ".json" },
  { value: "srt", label: "SubRip captions", extension: ".srt" },
  { value: "vtt", label: "WebVTT captions", extension: ".vtt" },
];

export function ExportMenu({
  detail,
  currentRevisionId,
  alignmentOrigin,
}: {
  detail: RecordingDetail;
  currentRevisionId: string | null;
  alignmentOrigin: "native" | "derived" | "none";
}) {
  const { pushToast } = useAppState();
  const [exportDialogOpen, setExportDialogOpen] = useState(false);
  const [audioExportOpen, setAudioExportOpen] = useState(false);
  const [includeSanitized, setIncludeSanitized] = useState(false);
  const [exportResult, setExportResult] = useState<ExportMediaResponse | null>(null);

  const exportTranscript = useCommandMutation("export_recording");
  const exportMedia = useCommandMutation("export_media");

  const handleTranscriptExport = async (format: ExportFormat) => {
    if (!currentRevisionId) return;
    
    const captionFormats = ["srt", "vtt"];
    if (captionFormats.includes(format) && alignmentOrigin === "none") {
      pushToast({
        id: `export-${Date.now()}`,
        tone: "warning",
        title: "Captions unavailable",
        message: "This transcript has no real timestamps; we do not invent seek times.",
      });
      return;
    }

    const ext = TRANSCRIPT_FORMATS.find(f => f.value === format)?.extension ?? ".txt";
    const destination = await openDialog({
      title: "Export transcript",
      defaultPath: `${detail.title}${ext}`,
      filters: [{ name: "Export", extensions: [ext.slice(1)] }],
    });

    if (!destination) return;

    try {
      const response = await exportTranscript.mutateAsync({
        recording_id: detail.recording_id,
        revision_id: currentRevisionId,
        format,
        destination,
      });
      pushToast({
        id: `export-${Date.now()}`,
        tone: "success",
        title: "Export started",
        message: `Job ${response.job_id} queued.`,
      });
      setExportDialogOpen(false);
    } catch (err) {
      // Error is shown by CommandErrorNotice
    }
  };

  const handleAudioExport = async () => {
    const directory = await openDialog({
      title: "Choose export destination",
      directory: true,
    });

    if (!directory) return;

    try {
      const response = await exportMedia.mutateAsync({
        recording_id: detail.recording_id,
        media_asset_id: null,
        destination_directory: directory,
        include_sanitized_derivative: includeSanitized,
      });
      setExportResult(response);
    } catch (err) {
      // Error is shown by CommandErrorNotice
    }
  };

  return (
    <>
      <Button variant="default" onClick={() => setExportDialogOpen(true)}>
        Export
      </Button>

      <Dialog
        open={exportDialogOpen}
        title="Export recording"
        onClose={() => setExportDialogOpen(false)}
        footer={
          <Button variant="quiet" onClick={() => setExportDialogOpen(false)}>
            Cancel
          </Button>
        }
      >
        <div style={{ display: "flex", flexDirection: "column", gap: "1rem" }}>
          <section>
            <h3>Transcript formats</h3>
            <div style={{ display: "flex", flexDirection: "column", gap: "0.5rem" }}>
              {TRANSCRIPT_FORMATS.map(({ value, label }) => {
                const captionFormats = ["srt", "vtt"];
                const disabled = captionFormats.includes(value) && alignmentOrigin === "none";
                return (
                  <Button
                    key={value}
                    variant="default"
                    onClick={() => handleTranscriptExport(value)}
                    disabled={disabled || !currentRevisionId}
                    title={
                      disabled
                        ? "This transcript has no real timestamps; we do not invent seek times."
                        : undefined
                    }
                  >
                    {label}
                    {disabled ? " (unavailable)" : ""}
                  </Button>
                );
              })}
            </div>
          </section>

          <section>
            <h3>Original audio</h3>
            <Button variant="default" onClick={() => {
              setExportDialogOpen(false);
              setAudioExportOpen(true);
            }}>
              Export audio file
            </Button>
          </section>

          <CommandErrorNotice mutation={exportTranscript} />
        </div>
      </Dialog>

      <Dialog
        open={audioExportOpen}
        title="Export audio"
        onClose={() => {
          setAudioExportOpen(false);
          setExportResult(null);
        }}
        footer={
          exportResult ? (
            <Button variant="primary" onClick={() => {
              setAudioExportOpen(false);
              setExportResult(null);
              setIncludeSanitized(false);
            }}>
              Done
            </Button>
          ) : (
            <>
              <Button variant="quiet" onClick={() => setAudioExportOpen(false)}>
                Cancel
              </Button>
              <Button
                variant="primary"
                onClick={handleAudioExport}
                disabled={exportMedia.isPending}
              >
                {exportMedia.isPending ? "Exporting..." : "Export"}
              </Button>
            </>
          )
        }
      >
        {exportResult ? (
          <div style={{ display: "flex", flexDirection: "column", gap: "1rem" }}>
            <section>
              <h3>{exportResult.original.label}</h3>
              <dl>
                <dt>File</dt>
                <dd>{exportResult.original.file_name}</dd>
                <dt>Size</dt>
                <dd>{(exportResult.original.byte_length / 1024 / 1024).toFixed(2)} MB</dd>
                <dt>SHA-256</dt>
                <dd style={{ fontFamily: "monospace", fontSize: "0.9em", wordBreak: "break-all" }}>
                  {exportResult.original.sha256}
                </dd>
                {exportResult.original.stripped_metadata.length > 0 && (
                  <>
                    <dt>Removed metadata</dt>
                    <dd>{exportResult.original.stripped_metadata.join(", ")}</dd>
                  </>
                )}
              </dl>
            </section>

            {exportResult.sanitized && (
              <section>
                <h3>{exportResult.sanitized.label}</h3>
                <dl>
                  <dt>File</dt>
                  <dd>{exportResult.sanitized.file_name}</dd>
                  <dt>Size</dt>
                  <dd>{(exportResult.sanitized.byte_length / 1024 / 1024).toFixed(2)} MB</dd>
                  <dt>SHA-256</dt>
                  <dd style={{ fontFamily: "monospace", fontSize: "0.9em", wordBreak: "break-all" }}>
                    {exportResult.sanitized.sha256}
                  </dd>
                  <dt>Removed metadata</dt>
                  <dd>{exportResult.sanitized.stripped_metadata.join(", ")}</dd>
                </dl>
              </section>
            )}

            {exportResult.sanitized_refusal && (
              <p style={{ color: "var(--color-warning)" }}>
                {exportResult.sanitized_refusal}
              </p>
            )}
          </div>
        ) : (
          <div style={{ display: "flex", flexDirection: "column", gap: "1rem" }}>
            <p>Export the original audio file for this recording.</p>
            
            <label style={{ display: "flex", gap: "0.5rem", alignItems: "flex-start" }}>
              <input
                type="checkbox"
                checked={includeSanitized}
                onChange={(e) => setIncludeSanitized(e.target.checked)}
              />
              <span>
                Also produce a sanitized derivative (removes metadata that could identify the
                source device or editing software)
              </span>
            </label>

            <CommandErrorNotice mutation={exportMedia} />
          </div>
        )}
      </Dialog>
    </>
  );
}
