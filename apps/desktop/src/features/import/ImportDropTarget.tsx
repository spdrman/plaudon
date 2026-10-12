/**
 * Window-level drag-and-drop import (§5.2, §7.4) and the shared import state
 * for everything below it (the toolbar button, the empty states).
 *
 * Inside Tauri the real drop paths arrive on `onDragDropEvent`; in the browser
 * harness the DOM `drop` event is the fallback. Either way only strings are
 * forwarded to `import_files` — a `File` object never crosses the boundary.
 */
import { useCallback, useEffect, useRef, useState, type ReactNode } from "react";
import type { ImportOutcome } from "../../ipc/contract";
import { StatusBadge } from "../../components/StatusBadge";
import { Button } from "../../components/Button";
import { Progress } from "../../components/Progress";
import { useAppState } from "../../app/AppState";
import { envelopeActionControl } from "../library/ErrorNotice";
import { DuplicateDialog } from "./DuplicateDialog";
import {
  AUDIO_EXTENSIONS,
  ImportContext,
  OUTCOME_BADGE,
  toPathStrings,
  useImportController,
  type DuplicateChoice,
  type ImportController,
} from "./useImport";
import "./import.css";

/**
 * Paths for a browser `FileList`. A web `File` exposes no filesystem path, so
 * the harness falls back to the name; Rust still rejects anything that is not
 * an approved, resolvable path (§15.1).
 */
export function filesToPaths(files: FileList | null | undefined): string[] {
  if (!files) return [];
  const paths: string[] = [];
  for (const file of Array.from(files)) {
    const candidate = "path" in file ? file.path : undefined;
    paths.push(typeof candidate === "string" && candidate.length > 0 ? candidate : file.name);
  }
  return paths;
}

export function ImportDropTarget({ children }: { children: ReactNode }) {
  const controller = useImportController();
  const { importPaths, setDragging, dragging } = controller;
  const [batch, setBatch] = useState<string[]>([]);
  const [decideLater, setDecideLater] = useState(false);
  const tauriActive = useRef(false);
  const dragDepth = useRef(0);

  const startBatch = useCallback(
    (paths: string[]) => {
      if (paths.length === 0) return;
      setBatch(paths);
      setDecideLater(false);
      void importPaths(paths, "ask");
    },
    [importPaths],
  );

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let cancelled = false;
    void (async () => {
      try {
        // Exception to the static-import rule: `@tauri-apps/api/webview` only
        // resolves inside the Tauri webview; the browser harness uses the DOM
        // listeners below instead.
        const { getCurrentWebview } = await import("@tauri-apps/api/webview");
        const off = await getCurrentWebview().onDragDropEvent((event) => {
          const payload = event.payload;
          if (payload.type === "enter" || payload.type === "over") {
            setDragging(true);
            return;
          }
          if (payload.type === "leave") {
            setDragging(false);
            return;
          }
          setDragging(false);
          startBatch(toPathStrings(payload.paths));
        });
        if (cancelled) off();
        else {
          unlisten = off;
          tauriActive.current = true;
        }
      } catch {
        tauriActive.current = false;
      }
    })();
    return () => {
      cancelled = true;
      tauriActive.current = false;
      unlisten?.();
    };
  }, [setDragging, startBatch]);

  useEffect(() => {
    const onDragEnter = (event: DragEvent) => {
      if (tauriActive.current) return;
      event.preventDefault();
      dragDepth.current += 1;
      setDragging(true);
    };
    const onDragOver = (event: DragEvent) => {
      if (tauriActive.current) return;
      // Required for the window to accept a drop at all.
      event.preventDefault();
      setDragging(true);
    };
    const onDragLeave = (event: DragEvent) => {
      if (tauriActive.current) return;
      event.preventDefault();
      dragDepth.current = Math.max(0, dragDepth.current - 1);
      if (dragDepth.current === 0) setDragging(false);
    };
    const onDrop = (event: DragEvent) => {
      if (tauriActive.current) return;
      event.preventDefault();
      dragDepth.current = 0;
      setDragging(false);
      startBatch(filesToPaths(event.dataTransfer?.files));
    };
    window.addEventListener("dragenter", onDragEnter);
    window.addEventListener("dragover", onDragOver);
    window.addEventListener("dragleave", onDragLeave);
    window.addEventListener("drop", onDrop);
    return () => {
      window.removeEventListener("dragenter", onDragEnter);
      window.removeEventListener("dragover", onDragOver);
      window.removeEventListener("dragleave", onDragLeave);
      window.removeEventListener("drop", onDrop);
    };
  }, [setDragging, startBatch]);

  const onChoose = useCallback(
    (path: string, choice: DuplicateChoice) => {
      void controller.resolveDuplicate(path, choice);
    },
    [controller],
  );

  return (
    <ImportContext.Provider value={controller}>
      {children}
      {dragging ? (
        <div className="import-drop__overlay" data-testid="import-drop-overlay">
          <p className="import-drop__overlay-title">
            <span aria-hidden="true">⤓ </span>
            Drop audio files to import
          </p>
          <p className="import-drop__overlay-hint">
            {AUDIO_EXTENSIONS.map((extension) => extension.toUpperCase()).join(", ")}
          </p>
        </div>
      ) : null}
      <div className="sr-only" role="status">
        {dragging ? "Drop audio files to import." : controller.announcement}
      </div>
      <ImportResultsPanel controller={controller} batch={batch} onClearBatch={() => setBatch([])} />
      <DuplicateDialog
        open={!decideLater}
        duplicates={controller.duplicates}
        pending={controller.pending}
        onChoose={onChoose}
        onDecideLater={() => setDecideLater(true)}
      />
    </ImportContext.Provider>
  );
}

interface ImportResultsPanelProps {
  controller: ImportController;
  batch: string[];
  onClearBatch: () => void;
}

/** Per-file outcomes for the batch: a rejected file never hides the rest. */
function ImportResultsPanel({ controller, batch, onClearBatch }: ImportResultsPanelProps) {
  const { navigate } = useAppState();
  const { results, pending, importPaths } = controller;
  const paths = batch.length > 0 ? batch : results.map((result) => result.path);
  if (paths.length === 0) return null;

  return (
    <section className="import-results" aria-label="Import results">
      <header className="import-results__header">
        <h2 className="import-results__title">Import</h2>
        <Button
          variant="quiet"
          onClick={() => {
            controller.clearResults();
            onClearBatch();
          }}
        >
          Dismiss
        </Button>
      </header>
      {pending ? <Progress label="Importing files" value={null} /> : null}
      <ul className="import-results__list">
        {paths.map((path) => {
          const outcome = results.find((result) => result.path === path);
          return (
            <li className="import-results__item" key={path}>
              <span className="import-results__path">{path}</span>
              {outcome ? (
                <StatusBadge {...OUTCOME_BADGE[outcome.outcome]} />
              ) : (
                <StatusBadge
                  tone="running"
                  glyph="⟳"
                  label={pending ? "Importing" : "Waiting"}
                  description="No outcome reported yet"
                />
              )}
              <OutcomeDetail
                outcome={outcome}
                onRetry={() => void importPaths([path], "ask")}
                onOpenSettings={() => navigate({ name: "settings" })}
              />
            </li>
          );
        })}
      </ul>
    </section>
  );
}

interface OutcomeDetailProps {
  outcome: ImportOutcome | undefined;
  onRetry: () => void;
  onOpenSettings: () => void;
}

function OutcomeDetail({ outcome, onRetry, onOpenSettings }: OutcomeDetailProps) {
  if (!outcome) return null;
  if (outcome.outcome === "rejected") {
    const control = envelopeActionControl(outcome.error);
    return (
      <div className="import-results__detail">
        <p className="import-results__error">{outcome.error.message}</p>
        {control.kind === "retry" ? (
          <Button variant="quiet" onClick={onRetry}>
            {control.label}
          </Button>
        ) : null}
        {control.kind === "settings" ? (
          <Button variant="quiet" onClick={onOpenSettings}>
            {control.label}
          </Button>
        ) : null}
      </div>
    );
  }
  if (outcome.outcome === "skipped") {
    return (
      <p className="import-results__detail">
        Kept the recording you already have ({outcome.existing_recording_id}).
      </p>
    );
  }
  if (outcome.outcome === "duplicate") {
    return (
      <p className="import-results__detail">
        Same audio as <strong>{outcome.existing_title}</strong> by content hash. Choose what to do.
      </p>
    );
  }
  return null;
}
