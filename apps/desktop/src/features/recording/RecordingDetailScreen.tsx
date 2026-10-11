/**
 * Recording detail (§5.2): header with provenance and privacy, playback,
 * transcript, and the Summary/Actions/Decisions/History pane.
 */
import { useEffect, useMemo, useRef, useState } from "react";
import "./recording.css";
import { Button } from "../../components/Button";
import { Dialog } from "../../components/Dialog";
import { EmptyState } from "../../components/EmptyState";
import { SelectField } from "../../components/Field";
import { StatusBadge, type StatusTone } from "../../components/StatusBadge";
import { useAppState } from "../../app/AppState";
import { useRefetchOnEvents } from "../../app/useRefetchOnEvents";
import { useCommandMutation, useCommandQuery } from "../../ipc/useCommand";
import type {
  ExportFormat,
  RecordingDetail,
  RecordingOutput,
  TranscriptionStatus,
} from "../../ipc/contract";
import { AudioPlayer, usePlayerController, usePlayerShortcuts } from "./AudioPlayer";
import { TranscriptPane } from "./TranscriptPane";
import { RevisionSwitcher, revisionLabel } from "./RevisionSwitcher";
import { CommandErrorNotice } from "./errorNotice";

const DETAIL_EVENTS = ["transcript.completed", "recording.source_updated", "library.changed"] as const;

const TABS = [
  { id: "summary", label: "Summary" },
  { id: "actions", label: "Actions" },
  { id: "decisions", label: "Decisions" },
  { id: "history", label: "History" },
] as const;
type TabId = (typeof TABS)[number]["id"];

const OUTPUT_KIND_BY_TAB: Record<"summary" | "actions" | "decisions", RecordingOutput["kind"]> = {
  summary: "summary",
  actions: "actions",
  decisions: "decisions",
};

const TRANSCRIPTION_PRESENTATION: Record<
  TranscriptionStatus,
  { label: string; glyph: string; tone: StatusTone }
> = {
  none: { label: "Not transcribed", glyph: "–", tone: "neutral" },
  queued: { label: "Queued", glyph: "…", tone: "waiting" },
  running: { label: "Transcribing", glyph: "▸", tone: "running" },
  completed: { label: "Transcribed", glyph: "✓", tone: "success" },
  failed: { label: "Transcription failed", glyph: "✕", tone: "danger" },
  stale: { label: "Transcript stale", glyph: "⟳", tone: "warning" },
};

const EXPORT_FORMATS: Array<{ value: ExportFormat; label: string }> = [
  { value: "txt", label: "Plain text (.txt)" },
  { value: "markdown", label: "Markdown (.md)" },
  { value: "json", label: "Structured JSON (.json)" },
];

export function RecordingDetailScreen({
  recordingId,
  onBack,
}: {
  recordingId: string;
  onBack: () => void;
}) {
  const { preferences, pushToast } = useAppState();
  const { data: detail, error, loading, refetch } = useCommandQuery(
    "get_recording",
    { recording_id: recordingId },
    { deps: [recordingId] },
  );
  useRefetchOnEvents(DETAIL_EVENTS, refetch);

  const controller = usePlayerController(detail?.duration_ms ?? null);
  const shortcuts = usePlayerShortcuts(controller);
  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => shortcuts(event);
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [shortcuts]);

  const [selectedRevisionId, setSelectedRevisionId] = useState<string | null>(null);
  const [staleOutputIds, setStaleOutputIds] = useState<readonly string[]>([]);
  const [tab, setTab] = useState<TabId>("summary");

  useEffect(() => {
    setSelectedRevisionId(null);
    setStaleOutputIds([]);
  }, [recordingId]);

  const revisions = useMemo(() => detail?.revisions ?? [], [detail]);
  const activeRevision =
    revisions.find((revision) => revision.revision_id === selectedRevisionId) ??
    revisions.find((revision) => revision.revision_id === detail?.current_revision_id) ??
    null;

  if (error) {
    return (
      <div className="recording">
        <header className="recording__header">
          <Button glyph="←" onClick={onBack}>
            Back to library
          </Button>
        </header>
        <CommandErrorNotice error={error} onRetry={() => void refetch()} />
      </div>
    );
  }

  if (!detail) {
    return (
      <div className="recording">
        <header className="recording__header">
          <Button glyph="←" onClick={onBack}>
            Back to library
          </Button>
        </header>
        <p role="status" aria-live="polite">
          {loading ? "Loading recording…" : "This recording is not available."}
        </p>
      </div>
    );
  }

  const status = TRANSCRIPTION_PRESENTATION[detail.transcription_status];
  const localOnly = preferences?.local_only_mode ?? true;

  return (
    <div className="recording">
      <header className="recording__header">
        <div className="recording__header-row">
          <Button glyph="←" onClick={onBack}>
            Back to library
          </Button>
          <ExportControl
            detail={detail}
            revisionId={activeRevision?.revision_id ?? null}
            onExported={(jobId) =>
              pushToast({ tone: "info", title: "Export queued", body: `Job ${jobId}` })
            }
          />
        </div>

        <h1 className="recording__title">{detail.title}</h1>
        <p className="recording__title-note">
          Renaming is not part of this build: no command persists a new title, so the title is shown
          exactly as it was imported.
        </p>

        <div className="recording__badges">
          <StatusBadge tone={status.tone} glyph={status.glyph} label={status.label} />
          {localOnly ? (
            <StatusBadge
              tone="success"
              glyph="◆"
              label="Local only"
              description="Local-only mode is on: this recording is never sent to a provider."
            />
          ) : (
            <StatusBadge
              tone="warning"
              glyph="◇"
              label="Cloud eligible"
              description="Cloud processing is allowed; each cloud run still asks for consent."
            />
          )}
          {detail.processing_engine ? (
            <StatusBadge tone="neutral" glyph="⚙" label={`Engine ${detail.processing_engine}`} />
          ) : null}
        </div>

        <dl className="recording__provenance">
          <div>
            <dt>Source</dt>
            <dd>{detail.source.label}</dd>
          </div>
          <div>
            <dt>Original file</dt>
            <dd>{detail.media.original_file_name}</dd>
          </div>
          <div>
            <dt>Content hash (SHA-256)</dt>
            <dd>
              <code>{detail.sha256.slice(0, 12)}…</code>
            </dd>
          </div>
          <div>
            <dt>Imported</dt>
            <dd>{detail.imported_at}</dd>
          </div>
        </dl>

        {detail.last_error ? (
          <p className="recording__last-error" role="status">
            Last error: {detail.last_error.message}
          </p>
        ) : null}
      </header>

      <div className="recording__layout">
        <div className="recording__main">
          <AudioPlayer recordingId={recordingId} controller={controller} title={detail.title} />

          {activeRevision ? (
            <>
              <RevisionSwitcher
                revisions={revisions}
                selectedRevisionId={activeRevision.revision_id}
                headRevisionId={detail.current_revision_id}
                onSelect={setSelectedRevisionId}
              />
              <TranscriptPane
                recordingId={recordingId}
                revision={activeRevision}
                editable={activeRevision.revision_id === detail.current_revision_id}
                onSeek={(positionMs) => controller.seekTo(positionMs)}
                onSaved={(response) => {
                  setSelectedRevisionId(response.revision_id);
                  setStaleOutputIds(response.stale_output_ids);
                  void refetch();
                  pushToast({
                    tone: "success",
                    title: "Transcript revision saved",
                    body: "The original machine transcript is unchanged and still available.",
                  });
                }}
                onReloadRequested={() => {
                  setSelectedRevisionId(null);
                  void refetch();
                }}
              />
            </>
          ) : (
            <EmptyState
              title="No transcript yet"
              body="This recording has not been transcribed, so there is nothing to read or edit here. Start a transcription from the Jobs screen once an engine and model are installed."
            />
          )}
        </div>

        <DetailPane
          detail={detail}
          tab={tab}
          onTabChange={setTab}
          staleOutputIds={staleOutputIds}
        />
      </div>
    </div>
  );
}

interface DetailPaneProps {
  detail: RecordingDetail;
  tab: TabId;
  onTabChange: (tab: TabId) => void;
  staleOutputIds: readonly string[];
}

function DetailPane({ detail, tab, onTabChange, staleOutputIds }: DetailPaneProps) {
  const tabRefs = useRef<Record<string, HTMLButtonElement | null>>({});

  const moveTab = (direction: 1 | -1) => {
    const position = TABS.findIndex((entry) => entry.id === tab);
    const next = TABS[(position + direction + TABS.length) % TABS.length];
    if (!next) return;
    onTabChange(next.id);
    tabRefs.current[next.id]?.focus();
  };

  return (
    <aside className="recording__pane" aria-label="Recording details">
      <div className="recording__tabs" role="tablist" aria-label="Recording outputs">
        {TABS.map((entry) => (
          <button
            key={entry.id}
            type="button"
            role="tab"
            id={`tab-${entry.id}`}
            aria-selected={tab === entry.id}
            aria-controls={`panel-${entry.id}`}
            tabIndex={tab === entry.id ? 0 : -1}
            className={`recording__tab${tab === entry.id ? " recording__tab--active" : ""}`}
            ref={(node) => {
              tabRefs.current[entry.id] = node;
            }}
            onClick={() => onTabChange(entry.id)}
            onKeyDown={(event) => {
              if (event.key === "ArrowRight") {
                event.preventDefault();
                moveTab(1);
              } else if (event.key === "ArrowLeft") {
                event.preventDefault();
                moveTab(-1);
              }
            }}
          >
            {entry.label}
          </button>
        ))}
      </div>

      <div
        className="recording__panel"
        role="tabpanel"
        id={`panel-${tab}`}
        aria-labelledby={`tab-${tab}`}
        tabIndex={0}
      >
        {tab === "history" ? (
          <RevisionHistory detail={detail} />
        ) : (
          <OutputList
            detail={detail}
            kind={OUTPUT_KIND_BY_TAB[tab]}
            staleOutputIds={staleOutputIds}
          />
        )}
      </div>
    </aside>
  );
}

function OutputList({
  detail,
  kind,
  staleOutputIds,
}: {
  detail: RecordingDetail;
  kind: RecordingOutput["kind"];
  staleOutputIds: readonly string[];
}) {
  const outputs = detail.outputs.filter((output) => output.kind === kind);
  if (outputs.length === 0) {
    return (
      <p className="recording__empty-output">
        No {kind} output exists for this recording yet. Generating one needs a text model, which
        this build does not run.
      </p>
    );
  }
  return (
    <ul className="recording__outputs">
      {outputs.map((output) => {
        // §4.5: an output generated from a superseded revision needs regeneration.
        const stale = output.stale || staleOutputIds.includes(output.output_id);
        return (
          <li key={output.output_id}>
            <p>Generated {output.created_at}</p>
            <p className="recording__output-source">From revision {output.source_revision_id}</p>
            {stale ? (
              <StatusBadge
                tone="warning"
                glyph="⟳"
                label="Needs regeneration"
                description="The transcript changed after this output was produced."
              />
            ) : (
              <StatusBadge tone="success" glyph="✓" label="Current" />
            )}
          </li>
        );
      })}
    </ul>
  );
}

function RevisionHistory({ detail }: { detail: RecordingDetail }) {
  if (detail.revisions.length === 0) {
    return <p className="recording__empty-output">No transcript revisions exist yet.</p>;
  }
  return (
    <ol className="recording__revisions">
      {detail.revisions.map((revision) => (
        <li key={revision.revision_id}>
          <p>{revisionLabel(revision)}</p>
          <p className="recording__output-source">
            {revision.engine_id ?? "engine unknown"} · {revision.segment_count} segments ·{" "}
            {revision.alignment_origin === "none"
              ? "no timestamps"
              : `${revision.alignment_origin} timestamps`}
          </p>
          {revision.revision_id === detail.current_revision_id ? (
            <StatusBadge tone="success" glyph="✓" label="Current revision" />
          ) : (
            <StatusBadge tone="neutral" glyph="▣" label="Superseded revision" />
          )}
        </li>
      ))}
    </ol>
  );
}

function ExportControl({
  detail,
  revisionId,
  onExported,
}: {
  detail: RecordingDetail;
  revisionId: string | null;
  onExported: (jobId: string) => void;
}) {
  const [open, setOpen] = useState(false);
  const [format, setFormat] = useState<ExportFormat>("markdown");
  const [problem, setProblem] = useState<string | null>(null);
  const exportRecording = useCommandMutation("export_recording");

  const start = async () => {
    setProblem(null);
    if (!revisionId) return;
    try {
      // Exception to the static-import rule, as in `features/import/useImport.ts`:
      // `@tauri-apps/plugin-dialog` only resolves inside the Tauri webview, and
      // the browser harness and tests substitute it at this boundary.
      const { save } = await import("@tauri-apps/plugin-dialog");
      const destination = await save({
        title: "Export transcript",
        defaultPath: `${detail.title}.${format === "markdown" ? "md" : format}`,
      });
      if (!destination) return;
      const response = await exportRecording.run({
        recording_id: detail.recording_id,
        revision_id: revisionId,
        format,
        destination,
      });
      setOpen(false);
      onExported(response.job_id);
    } catch (caught) {
      setProblem(
        caught instanceof Error ? caught.message : "The export could not be started.",
      );
    }
  };

  return (
    <>
      <Button glyph="↗" disabled={revisionId === null} onClick={() => setOpen(true)}>
        Export…
      </Button>
      {revisionId === null ? (
        <span className="field__hint">Export needs a transcript revision.</span>
      ) : null}
      <Dialog
        open={open}
        title="Export transcript"
        description="Choose a format, then pick where to save the file."
        onClose={() => setOpen(false)}
        footer={
          <>
            <Button onClick={() => setOpen(false)}>Cancel</Button>
            <Button variant="primary" disabled={exportRecording.pending} onClick={() => void start()}>
              {exportRecording.pending ? "Starting…" : "Choose destination…"}
            </Button>
          </>
        }
      >
        <SelectField
          label="Format"
          value={format}
          onChange={(event) => setFormat(event.target.value as ExportFormat)}
        >
          {EXPORT_FORMATS.map((entry) => (
            <option key={entry.value} value={entry.value}>
              {entry.label}
            </option>
          ))}
        </SelectField>
        {exportRecording.error ? (
          <CommandErrorNotice error={exportRecording.error} onRetry={() => void start()} />
        ) : null}
        {problem ? (
          <p className="error-notice__message" role="alert">
            {problem}
          </p>
        ) : null}
      </Dialog>
    </>
  );
}
