/**
 * Transcript viewer and editor (§4.5, §5.2, §5.3, §8.5, §8.6A).
 *
 * The list is virtualized and paged through `get_transcript`; edits are a
 * draft overlay that never touches the loaded revision, and a save produces a
 * new revision through `save_transcript_revision`.
 */
import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type KeyboardEvent as ReactKeyboardEvent,
} from "react";
import { useVirtualizer } from "@tanstack/react-virtual";
import { Button } from "../../components/Button";
import { StatusBadge, type StatusTone } from "../../components/StatusBadge";
import { TextField } from "../../components/Field";
import { CommandError, invokeCommand, toEnvelope } from "../../ipc/client";
import { useCommandMutation } from "../../ipc/useCommand";
import type {
  SaveTranscriptRevisionResponse,
  TranscriptAnnotation,
  TranscriptRevisionRef,
  TranscriptSegment,
} from "../../ipc/contract";
import { formatClock } from "./playerState";
import {
  createEditState,
  findMatches,
  formatPassage,
  highlightRuns,
  redo,
  setSegmentText,
  textOf,
  toEdits,
  undo,
  undoRedoIntent,
  type EditState,
} from "./transcriptEditing";
import { CommandErrorNotice } from "./errorNotice";

/** §15.2 paging window for `get_transcript`. */
export const TRANSCRIPT_PAGE_SIZE = 200;
const ROW_ESTIMATE_PX = 96;

const ANNOTATION_PRESENTATION: Record<
  TranscriptAnnotation["kind"],
  { label: string; glyph: string; tone: StatusTone }
> = {
  low_confidence: { label: "Low confidence", glyph: "!", tone: "warning" },
  possible_hallucination: { label: "Possible hallucination", glyph: "?", tone: "warning" },
  inaudible: { label: "Inaudible", glyph: "~", tone: "neutral" },
  edited: { label: "Edited", glyph: "✎", tone: "running" },
};

export interface TranscriptPaneProps {
  recordingId: string;
  revision: TranscriptRevisionRef;
  /** Only the head revision may be edited; older ones are shown verbatim. */
  editable: boolean;
  /** Seek the player; only ever called for segments that carry a timeline. */
  onSeek: (positionMs: number) => void;
  onSaved: (response: SaveTranscriptRevisionResponse) => void;
  /** Reload the recording so the newest revision becomes current. */
  onReloadRequested: () => void;
}

export function TranscriptPane({
  recordingId,
  revision,
  editable,
  onSeek,
  onSaved,
  onReloadRequested,
}: TranscriptPaneProps) {
  const { segments, total, nextCursor, loading, error, loadMore, reload } = useTranscriptSegments(
    revision.revision_id,
  );
  const [edits, setEdits] = useState<EditState>(createEditState);
  const [editingIndex, setEditingIndex] = useState<number | null>(null);
  const [selected, setSelected] = useState<ReadonlySet<number>>(new Set());
  const [query, setQuery] = useState("");
  const [matchCursor, setMatchCursor] = useState(0);
  const [copyStatus, setCopyStatus] = useState("");
  const save = useCommandMutation("save_transcript_revision");
  const [saveError, setSaveError] = useState<CommandError | null>(null);
  const scrollRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    setEdits(createEditState());
    setEditingIndex(null);
    sessionRef.current = null;
    setSelected(new Set());
    setSaveError(null);
  }, [revision.revision_id]);

  const virtualizer = useVirtualizer<HTMLDivElement, HTMLDivElement>({
    count: segments.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => ROW_ESTIMATE_PX,
    overscan: 6,
    measureElement: (element: HTMLDivElement) => {
      // Without a layout engine (tests) the estimate stays authoritative.
      const measured = element.getBoundingClientRect().height;
      return measured > 0 ? measured : ROW_ESTIMATE_PX;
    },
  });

  const virtualRows = virtualizer.getVirtualItems();
  const lastRenderedIndex = virtualRows[virtualRows.length - 1]?.index ?? -1;
  useEffect(() => {
    if (nextCursor === null || loading) return;
    if (segments.length > 0 && lastRenderedIndex >= segments.length - 1) void loadMore();
  }, [lastRenderedIndex, nextCursor, loading, segments.length, loadMore]);

  const matches = useMemo(() => findMatches(segments, edits, query), [segments, edits, query]);
  const matchTotal = matches.reduce((sum, match) => sum + match.count, 0);
  const activeMatch = matches[Math.min(matchCursor, Math.max(0, matches.length - 1))];

  const goToMatch = (next: number) => {
    if (matches.length === 0) return;
    const wrapped = ((next % matches.length) + matches.length) % matches.length;
    setMatchCursor(wrapped);
    const target = matches[wrapped];
    if (!target) return;
    const position = segments.findIndex((segment) => segment.index === target.index);
    if (position >= 0) virtualizer.scrollToIndex(position, { align: "center" });
  };

  // Index whose editor already pushed a history entry in this editing session.
  const sessionRef = useRef<number | null>(null);

  const openEditor = (index: number) => {
    sessionRef.current = null;
    setEditingIndex(index);
  };

  const closeEditor = () => {
    sessionRef.current = null;
    setEditingIndex(null);
  };

  const commitText = (segment: TranscriptSegment, text: string) => {
    const coalesce = sessionRef.current === segment.index;
    setEdits((current) =>
      setSegmentText(current, segment.index, segment.text, text, { coalesce }),
    );
    sessionRef.current = segment.index;
  };

  /** Escape discards the current editing session without touching older edits. */
  const cancelEditor = (segmentIndex: number) => {
    if (sessionRef.current === segmentIndex) setEdits((current) => undo(current));
    closeEditor();
  };

  const onPaneKeyDown = (event: ReactKeyboardEvent<HTMLElement>) => {
    const intent = undoRedoIntent(event);
    if (!intent) return;
    event.preventDefault();
    setEdits((current) => (intent === "undo" ? undo(current) : redo(current)));
  };

  const copySelection = async () => {
    const chosen = segments.filter((segment) => selected.has(segment.index));
    if (chosen.length === 0) return;
    const text = formatPassage(chosen, edits);
    const written = await writeToClipboard(text);
    setCopyStatus(
      written
        ? `Copied ${chosen.length} segment${chosen.length === 1 ? "" : "s"} to the clipboard.`
        : "This system did not allow copying to the clipboard.",
    );
  };

  const copyMyChanges = async () => {
    const changed = segments.filter((segment) => edits.drafts.has(segment.index));
    const written = await writeToClipboard(formatPassage(changed, edits));
    setCopyStatus(
      written
        ? "Copied your unsaved changes to the clipboard."
        : "This system did not allow copying to the clipboard.",
    );
  };

  const runSave = async () => {
    setSaveError(null);
    try {
      const response = await save.run({
        recording_id: recordingId,
        base_revision_id: revision.revision_id,
        edits: toEdits(edits),
      });
      setEdits(createEditState());
      closeEditor();
      onSaved(response);
    } catch (caught) {
      // Drafts are deliberately kept: a failed save never discards user text.
      setSaveError(caught instanceof CommandError ? caught : new CommandError(toEnvelope(caught)));
    }
  };

  const dirtyCount = edits.drafts.size;
  const conflict = saveError?.envelope.code === "CONFLICT";

  if (error) {
    return (
      <div className="transcript">
        <CommandErrorNotice error={error} onRetry={() => void reload()} />
      </div>
    );
  }

  return (
    <section className="transcript" aria-label="Transcript" onKeyDown={onPaneKeyDown}>
      <div className="transcript__toolbar">
        <div className="transcript__search">
          <TextField
            label="Search transcript"
            type="search"
            value={query}
            onChange={(event) => {
              setQuery(event.target.value);
              setMatchCursor(0);
            }}
          />
          <Button
            glyph="↑"
            disabled={matches.length === 0}
            onClick={() => goToMatch(matchCursor - 1)}
          >
            Previous match
          </Button>
          <Button
            glyph="↓"
            disabled={matches.length === 0}
            onClick={() => goToMatch(matchCursor + 1)}
          >
            Next match
          </Button>
        </div>
        <div className="transcript__tools">
          <Button glyph="⧉" disabled={selected.size === 0} onClick={() => void copySelection()}>
            Copy passage
          </Button>
          {editable ? (
            <Button
              variant="primary"
              glyph="✓"
              disabled={dirtyCount === 0 || save.pending}
              onClick={() => void runSave()}
            >
              {save.pending ? "Saving…" : `Save ${dirtyCount} edited segment${dirtyCount === 1 ? "" : "s"}`}
            </Button>
          ) : null}
        </div>
      </div>

      <p className="transcript__status" role="status" aria-live="polite">
        {query.trim().length === 0
          ? `${segments.length} of ${total} segments loaded.`
          : matchTotal === 0
            ? `No matches in the ${segments.length} segments loaded of ${total}.`
            : `Match ${matchCursor + 1} of ${matches.length} segments, ${matchTotal} occurrence${matchTotal === 1 ? "" : "s"} in the ${segments.length} segments loaded of ${total}.`}
        {copyStatus ? ` ${copyStatus}` : ""}
      </p>

      {saveError ? (
        <CommandErrorNotice error={saveError} onRetry={conflict ? undefined : () => void runSave()}>
          {conflict ? (
            <>
              <Button glyph="↻" onClick={onReloadRequested}>
                Reload newest revision
              </Button>
              <Button glyph="⧉" onClick={() => void copyMyChanges()}>
                Copy my changes
              </Button>
            </>
          ) : null}
        </CommandErrorNotice>
      ) : null}

      {!editable ? (
        <p className="transcript__readonly">
          This revision is read-only. Switch to the current revision to make edits.
        </p>
      ) : null}

      <div className="transcript__scroller virtual-list" ref={scrollRef} data-testid="transcript-scroller">
        <div
          className="virtual-list__sizer"
          role="list"
          style={{ height: `${virtualizer.getTotalSize()}px` }}
        >
          {virtualRows.map((row) => {
            const segment = segments[row.index];
            if (!segment) return null;
            const text = textOf(edits, segment);
            const timed = segment.alignment_origin !== "none" && segment.start_ms !== null;
            const isMatch = activeMatch?.index === segment.index;
            return (
              <div
                key={segment.index}
                role="listitem"
                data-index={row.index}
                ref={virtualizer.measureElement}
                className={`virtual-list__item transcript__row${isMatch ? " transcript__row--active" : ""}`}
                style={{ transform: `translateY(${row.start}px)` }}
                data-testid={`segment-${segment.index}`}
              >
                <div className="transcript__gutter">
                  <label className="transcript__select">
                    <input
                      type="checkbox"
                      checked={selected.has(segment.index)}
                      onChange={(event) => {
                        setSelected((current) => {
                          const next = new Set(current);
                          if (event.target.checked) next.add(segment.index);
                          else next.delete(segment.index);
                          return next;
                        });
                      }}
                    />
                    <span className="sr-only">Select segment {segment.index + 1} for copying</span>
                  </label>
                  {timed && segment.start_ms !== null ? (
                    <button
                      type="button"
                      className="transcript__chip"
                      onClick={() => onSeek(segment.start_ms ?? 0)}
                      aria-label={
                        segment.alignment_origin === "derived"
                          ? `Play from ${formatClock(segment.start_ms)}, approximate (derived) timing`
                          : `Play from ${formatClock(segment.start_ms)}`
                      }
                    >
                      <span aria-hidden="true">{formatClock(segment.start_ms)}</span>
                      {segment.alignment_origin === "derived" ? (
                        <span className="transcript__chip-note" aria-hidden="true">
                          approximate (derived)
                        </span>
                      ) : null}
                    </button>
                  ) : null}
                </div>

                <div className="transcript__body">
                  {editingIndex === segment.index ? (
                    <textarea
                      className="transcript__editor"
                      aria-label={`Edit segment ${segment.index + 1}`}
                      value={text}
                      autoFocus
                      onChange={(event) => commitText(segment, event.target.value)}
                      onKeyDown={(event) => {
                        if (event.key === "Escape") {
                          event.stopPropagation();
                          cancelEditor(segment.index);
                        }
                      }}
                      onBlur={closeEditor}
                    />
                  ) : editable ? (
                    <div
                      className="transcript__text"
                      role="button"
                      tabIndex={0}
                      aria-label={`Segment ${segment.index + 1}: ${text}. Press Enter to edit.`}
                      onDoubleClick={() => openEditor(segment.index)}
                      onKeyDown={(event) => {
                        if (event.key === "Enter") {
                          event.preventDefault();
                          openEditor(segment.index);
                        }
                      }}
                    >
                      <HighlightedText text={text} query={query} />
                    </div>
                  ) : (
                    <p className="transcript__text transcript__text--readonly">
                      <HighlightedText text={text} query={query} />
                    </p>
                  )}

                  <div className="transcript__signals">
                    {segment.confidence ? (
                      <span className="transcript__confidence">
                        {segment.confidence.engine_id} confidence{" "}
                        {segment.confidence.value.toFixed(2)} — not comparable across engines
                      </span>
                    ) : (
                      <span className="transcript__confidence">
                        No confidence reported by this engine
                      </span>
                    )}
                    {segment.annotations.map((annotation, position) => {
                      const presentation = ANNOTATION_PRESENTATION[annotation.kind];
                      return (
                        <StatusBadge
                          key={`${annotation.kind}-${position}`}
                          tone={presentation.tone}
                          glyph={presentation.glyph}
                          label={presentation.label}
                          description={annotation.reason}
                        />
                      );
                    })}
                    {edits.drafts.has(segment.index) ? (
                      <StatusBadge
                        tone="running"
                        glyph="●"
                        label="Unsaved edit"
                        description="This text differs from the saved revision."
                      />
                    ) : null}
                  </div>
                </div>
              </div>
            );
          })}
        </div>
      </div>

      {nextCursor !== null ? (
        <Button glyph="⋯" disabled={loading} onClick={() => void loadMore()}>
          {loading ? "Loading…" : `Load more segments (${segments.length} of ${total})`}
        </Button>
      ) : null}
    </section>
  );
}

function HighlightedText({ text, query }: { text: string; query: string }) {
  const runs = highlightRuns(text, query);
  return (
    <>
      {runs.map((run, position) =>
        run.match ? <mark key={position}>{run.text}</mark> : <span key={position}>{run.text}</span>,
      )}
    </>
  );
}

async function writeToClipboard(text: string): Promise<boolean> {
  const clipboard = navigator.clipboard;
  if (!clipboard || typeof clipboard.writeText !== "function") return false;
  try {
    await clipboard.writeText(text);
    return true;
  } catch {
    return false;
  }
}

interface TranscriptSegments {
  segments: TranscriptSegment[];
  total: number;
  nextCursor: number | null;
  loading: boolean;
  error: CommandError | null;
  loadMore: () => Promise<void>;
  reload: () => Promise<void>;
}

/** Cursor-paged `get_transcript` reader; pages accumulate for the virtualizer. */
function useTranscriptSegments(revisionId: string): TranscriptSegments {
  const [segments, setSegments] = useState<TranscriptSegment[]>([]);
  const [total, setTotal] = useState(0);
  const [nextCursor, setNextCursor] = useState<number | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<CommandError | null>(null);
  const generation = useRef(0);

  const fetchPage = useCallback(
    async (cursor: number | null, append: boolean) => {
      const run = ++generation.current;
      setLoading(true);
      try {
        const page = await invokeCommand("get_transcript", {
          revision_id: revisionId,
          cursor,
          limit: TRANSCRIPT_PAGE_SIZE,
        });
        if (run !== generation.current) return;
        setSegments((current) => (append ? [...current, ...page.segments] : page.segments));
        setTotal(page.total_segments);
        setNextCursor(page.next_cursor);
        setError(null);
      } catch (caught) {
        if (run !== generation.current) return;
        setError(caught instanceof CommandError ? caught : new CommandError(toEnvelope(caught)));
      } finally {
        if (run === generation.current) setLoading(false);
      }
    },
    [revisionId],
  );

  useEffect(() => {
    setSegments([]);
    setNextCursor(null);
    void fetchPage(null, false);
  }, [fetchPage]);

  const cursorRef = useRef(nextCursor);
  cursorRef.current = nextCursor;

  return {
    segments,
    total,
    nextCursor,
    loading,
    error,
    loadMore: async () => {
      if (cursorRef.current === null) return;
      await fetchPage(cursorRef.current, true);
    },
    reload: () => fetchPage(null, false),
  };
}
