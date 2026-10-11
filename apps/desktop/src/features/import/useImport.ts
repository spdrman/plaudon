/**
 * Manual import (§7.4) over the single `import_files` command (§15.1).
 *
 * JavaScript never constructs, resolves or rewrites a path: it forwards the
 * exact strings produced by the OS file picker or by the window drop event and
 * lets Rust validate them. A `File` object is never sent.
 */
import { createContext, useCallback, useContext, useMemo, useState } from "react";
import type { DuplicatePolicy, ImportOutcome } from "../../ipc/contract";
import type { CommandError } from "../../ipc/client";
import type { StatusTone } from "../../components/StatusBadge";
import { useCommandMutation } from "../../ipc/useCommand";
import { useAppState, type Toast } from "../../app/AppState";

/** Extensions offered by the picker; Rust re-validates the real bytes (§8.1). */
export const AUDIO_EXTENSIONS = ["wav", "mp3", "m4a", "flac", "ogg", "opus"] as const;

export type DuplicateOutcome = Extract<ImportOutcome, { outcome: "duplicate" }>;
export type DuplicateChoice = "import_anyway" | "open_existing" | "skip";

export interface ImportController {
  /** Outcomes of the current batch, newest decision last, keyed by path. */
  results: ImportOutcome[];
  /** Duplicates still waiting for an explicit decision (§7.4). */
  duplicates: DuplicateOutcome[];
  pending: boolean;
  error: CommandError | null;
  /** Live-region text describing the current drag/import phase. */
  announcement: string;
  dragging: boolean;
  setDragging: (dragging: boolean) => void;
  /** Opens the OS picker and imports whatever it returns. */
  pickFiles: () => Promise<void>;
  importPaths: (paths: readonly string[], policy?: DuplicatePolicy) => Promise<void>;
  resolveDuplicate: (path: string, choice: DuplicateChoice) => Promise<void>;
  clearResults: () => void;
}

export const ImportContext = createContext<ImportController | null>(null);

/**
 * The shared controller when inside `ImportDropTarget`, otherwise a private
 * one so the button still works in isolation.
 */
export function useImport(): ImportController {
  const shared = useContext(ImportContext);
  const standalone = useImportController();
  return shared ?? standalone;
}

export interface OutcomeCounts {
  accepted: number;
  duplicate: number;
  skipped: number;
  rejected: number;
}

/** Per-file outcome presentation: text + glyph, never color alone (§5.3). */
export const OUTCOME_BADGE: Record<
  ImportOutcome["outcome"],
  { tone: StatusTone; glyph: string; label: string; description: string }
> = {
  accepted: { tone: "success", glyph: "✓", label: "Imported", description: "Added to your library" },
  duplicate: {
    tone: "warning",
    glyph: "!",
    label: "Needs a decision",
    description: "Already in your library by content hash",
  },
  skipped: { tone: "neutral", glyph: "–", label: "Skipped", description: "Left out at your request" },
  rejected: { tone: "danger", glyph: "✕", label: "Rejected", description: "Not imported" },
};

export function summarizeOutcomes(results: readonly ImportOutcome[]): OutcomeCounts {
  const counts: OutcomeCounts = { accepted: 0, duplicate: 0, skipped: 0, rejected: 0 };
  for (const result of results) counts[result.outcome] += 1;
  return counts;
}

/** Batch toast copy (§5.5): states every outcome class, never only the happy one. */
export function describeBatch(counts: OutcomeCounts): Omit<Toast, "id"> {
  const parts: string[] = [];
  if (counts.accepted > 0) parts.push(`${counts.accepted} imported`);
  if (counts.duplicate > 0) parts.push(`${counts.duplicate} duplicate`);
  if (counts.skipped > 0) parts.push(`${counts.skipped} skipped`);
  if (counts.rejected > 0) parts.push(`${counts.rejected} rejected`);
  const total = counts.accepted + counts.duplicate + counts.skipped + counts.rejected;
  const tone: Toast["tone"] =
    counts.rejected > 0 ? "warning" : counts.accepted > 0 ? "success" : "info";
  return {
    title: counts.accepted === total ? "Import finished" : "Import finished with decisions to make",
    body: parts.length > 0 ? `${parts.join(", ")}.` : "Nothing to import.",
    tone,
  };
}

/**
 * Normalizes whatever the picker or a drop event hands us into path strings.
 * Entries that are not usable strings are dropped; the strings themselves are
 * passed through byte-for-byte.
 */
export function toPathStrings(value: unknown): string[] {
  const candidates = Array.isArray(value) ? value : [value];
  const paths: string[] = [];
  for (const candidate of candidates) {
    if (typeof candidate === "string") {
      if (candidate.length > 0) paths.push(candidate);
      continue;
    }
    if (candidate && typeof candidate === "object" && "path" in candidate) {
      const path = candidate.path;
      if (typeof path === "string" && path.length > 0) paths.push(path);
    }
  }
  return paths;
}

/** Replaces outcomes for paths decided again, keeping first-seen order. */
export function mergeOutcomes(
  previous: readonly ImportOutcome[],
  incoming: readonly ImportOutcome[],
): ImportOutcome[] {
  const merged = previous.map((outcome) => {
    const replacement = incoming.find((candidate) => candidate.path === outcome.path);
    return replacement ?? outcome;
  });
  for (const outcome of incoming) {
    if (!merged.some((existing) => existing.path === outcome.path)) merged.push(outcome);
  }
  return merged;
}

export function useImportController(): ImportController {
  const { pushToast, navigate } = useAppState();
  const importFiles = useCommandMutation("import_files");
  const [results, setResults] = useState<ImportOutcome[]>([]);
  const [announcement, setAnnouncement] = useState("");
  const [dragging, setDragging] = useState(false);

  const importPaths = useCallback(
    async (paths: readonly string[], policy: DuplicatePolicy = "ask") => {
      if (paths.length === 0) return;
      setAnnouncement(`Importing ${paths.length} ${paths.length === 1 ? "file" : "files"}.`);
      try {
        const response = await importFiles.run({
          paths: [...paths],
          duplicate_policy: policy,
        });
        setResults((previous) => mergeOutcomes(previous, response.results));
        const counts = summarizeOutcomes(response.results);
        const toast = describeBatch(counts);
        pushToast(toast);
        setAnnouncement(`${toast.title}. ${toast.body ?? ""}`.trim());
      } catch (caught) {
        const message = caught instanceof Error ? caught.message : "Import failed.";
        setAnnouncement(`Import failed. ${message}`);
        pushToast({ title: "Import failed", body: message, tone: "danger" });
      }
    },
    [importFiles, pushToast],
  );

  const pickFiles = useCallback(async () => {
    let picked: unknown;
    try {
      // Exception to the static-import rule: `@tauri-apps/plugin-dialog` only
      // resolves inside the Tauri webview, and the browser harness and tests
      // substitute it at this boundary.
      const dialog = await import("@tauri-apps/plugin-dialog");
      picked = await dialog.open({
        multiple: true,
        directory: false,
        title: "Import audio files",
        filters: [{ name: "Audio", extensions: [...AUDIO_EXTENSIONS] }],
      });
    } catch {
      pushToast({
        title: "File picker unavailable",
        body: "The system file picker is only available in the desktop app. Drag files onto the window instead.",
        tone: "warning",
      });
      return;
    }
    const paths = toPathStrings(picked);
    if (paths.length === 0) {
      setAnnouncement("No files selected.");
      return;
    }
    await importPaths(paths, "ask");
  }, [importPaths, pushToast]);

  const resolveDuplicate = useCallback(
    async (path: string, choice: DuplicateChoice) => {
      const duplicate = results.find(
        (result): result is DuplicateOutcome => result.outcome === "duplicate" && result.path === path,
      );
      await importPaths([path], choice);
      if (choice === "open_existing" && duplicate) {
        navigate({ name: "library.recording", recordingId: duplicate.existing_recording_id });
      }
    },
    [importPaths, navigate, results],
  );

  const duplicates = useMemo(
    () => results.filter((result): result is DuplicateOutcome => result.outcome === "duplicate"),
    [results],
  );

  return {
    results,
    duplicates,
    pending: importFiles.pending,
    error: importFiles.error,
    announcement,
    dragging,
    setDragging,
    pickFiles,
    importPaths,
    resolveDuplicate,
    clearResults: useCallback(() => {
      setResults([]);
      setAnnouncement("");
      importFiles.reset();
    }, [importFiles]),
  };
}
