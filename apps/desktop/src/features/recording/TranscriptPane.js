import { jsx as _jsx, jsxs as _jsxs, Fragment as _Fragment } from "react/jsx-runtime";
/**
 * Transcript viewer and editor (§4.5, §5.2, §5.3, §8.5, §8.6A).
 *
 * The list is virtualized and paged through `get_transcript`; edits are a
 * draft overlay that never touches the loaded revision, and a save produces a
 * new revision through `save_transcript_revision`.
 */
import { useCallback, useEffect, useMemo, useRef, useState, } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";
import { Button } from "../../components/Button";
import { Dialog } from "../../components/Dialog";
import { StatusBadge } from "../../components/StatusBadge";
import { TextField } from "../../components/Field";
import { CommandError, invokeCommand, toEnvelope } from "../../ipc/client";
import { useCommandMutation } from "../../ipc/useCommand";
import { formatClock } from "./playerState";
import { createEditState, findMatches, formatPassage, highlightRuns, redo, setSegmentText, textOf, toEdits, undo, undoRedoIntent, } from "./transcriptEditing";
import { CommandErrorNotice } from "./errorNotice";
/** §15.2 paging window for `get_transcript`. */
export const TRANSCRIPT_PAGE_SIZE = 200;
const ROW_ESTIMATE_PX = 96;
const ANNOTATION_PRESENTATION = {
    low_confidence: { label: "Low confidence", glyph: "!", tone: "warning" },
    possible_hallucination: { label: "Possible hallucination", glyph: "?", tone: "warning" },
    inaudible: { label: "Inaudible", glyph: "~", tone: "neutral" },
    edited: { label: "Edited", glyph: "✎", tone: "running" },
};
export function TranscriptPane({ recordingId, revision, editable, onSeek, onDirtyChange, onSaved, onReloadRequested, }) {
    const { segments, total, nextCursor, loading, error, loadMore, reload } = useTranscriptSegments(revision.revision_id);
    const [edits, setEdits] = useState(createEditState);
    const [editingIndex, setEditingIndex] = useState(null);
    const [selected, setSelected] = useState(new Set());
    const [query, setQuery] = useState("");
    const [matchCursor, setMatchCursor] = useState(0);
    const [copyStatus, setCopyStatus] = useState("");
    const [discardPrompt, setDiscardPrompt] = useState(false);
    const save = useCommandMutation("save_transcript_revision");
    const [saveError, setSaveError] = useState(null);
    const scrollRef = useRef(null);
    // Drafts are only ever cleared by an explicit user action: picking another
    // revision, discarding them, or a successful save (§4.5 no silent overwrite).
    useEffect(() => {
        setEdits(createEditState());
        setEditingIndex(null);
        sessionRef.current = null;
        setSelected(new Set());
        setSaveError(null);
    }, [revision.revision_id]);
    useEffect(() => {
        onDirtyChange?.(edits.drafts.size);
    }, [edits, onDirtyChange]);
    const virtualizer = useVirtualizer({
        count: segments.length,
        getScrollElement: () => scrollRef.current,
        estimateSize: () => ROW_ESTIMATE_PX,
        overscan: 6,
        measureElement: (element) => {
            // Without a layout engine (tests) the estimate stays authoritative.
            const measured = element.getBoundingClientRect().height;
            return measured > 0 ? measured : ROW_ESTIMATE_PX;
        },
    });
    const virtualRows = virtualizer.getVirtualItems();
    const lastRenderedIndex = virtualRows[virtualRows.length - 1]?.index ?? -1;
    useEffect(() => {
        if (nextCursor === null || loading)
            return;
        if (segments.length > 0 && lastRenderedIndex >= segments.length - 1)
            void loadMore();
    }, [lastRenderedIndex, nextCursor, loading, segments.length, loadMore]);
    const matches = useMemo(() => findMatches(segments, edits, query), [segments, edits, query]);
    const matchTotal = matches.reduce((sum, match) => sum + match.count, 0);
    const activeMatch = matches[Math.min(matchCursor, Math.max(0, matches.length - 1))];
    const goToMatch = (next) => {
        if (matches.length === 0)
            return;
        const wrapped = ((next % matches.length) + matches.length) % matches.length;
        setMatchCursor(wrapped);
        const target = matches[wrapped];
        if (!target)
            return;
        const position = segments.findIndex((segment) => segment.index === target.index);
        if (position >= 0)
            virtualizer.scrollToIndex(position, { align: "center" });
    };
    // Index whose editor already pushed a history entry in this editing session.
    const sessionRef = useRef(null);
    const openEditor = (index) => {
        sessionRef.current = null;
        setEditingIndex(index);
    };
    const closeEditor = () => {
        sessionRef.current = null;
        setEditingIndex(null);
    };
    const commitText = (segment, text) => {
        const coalesce = sessionRef.current === segment.index;
        setEdits((current) => setSegmentText(current, segment.index, segment.text, text, { coalesce }));
        sessionRef.current = segment.index;
    };
    /** Escape discards the current editing session without touching older edits. */
    const cancelEditor = (segmentIndex) => {
        if (sessionRef.current === segmentIndex)
            setEdits((current) => undo(current));
        closeEditor();
    };
    const onPaneKeyDown = (event) => {
        const intent = undoRedoIntent(event);
        if (!intent)
            return;
        event.preventDefault();
        setEdits((current) => (intent === "undo" ? undo(current) : redo(current)));
    };
    const copySelection = async () => {
        const chosen = segments.filter((segment) => selected.has(segment.index));
        if (chosen.length === 0)
            return;
        const text = formatPassage(chosen, edits);
        const written = await writeToClipboard(text);
        setCopyStatus(written
            ? `Copied ${chosen.length} segment${chosen.length === 1 ? "" : "s"} to the clipboard.`
            : "This system did not allow copying to the clipboard.");
    };
    const copyMyChanges = async () => {
        const changed = segments.filter((segment) => edits.drafts.has(segment.index));
        const written = await writeToClipboard(formatPassage(changed, edits));
        setCopyStatus(written
            ? "Copied your unsaved changes to the clipboard."
            : "This system did not allow copying to the clipboard.");
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
        }
        catch (caught) {
            // Drafts are deliberately kept: a failed save never discards user text.
            setSaveError(caught instanceof CommandError ? caught : new CommandError(toEnvelope(caught)));
        }
    };
    const dirtyCount = edits.drafts.size;
    const conflict = saveError?.envelope.code === "CONFLICT";
    if (error) {
        return (_jsx("div", { className: "transcript", children: _jsx(CommandErrorNotice, { error: error, onRetry: () => void reload() }) }));
    }
    return (_jsxs("section", { className: "transcript", "aria-label": "Transcript", onKeyDown: onPaneKeyDown, children: [_jsxs("div", { className: "transcript__toolbar", children: [_jsxs("div", { className: "transcript__search", children: [_jsx(TextField, { label: "Search transcript", type: "search", value: query, onChange: (event) => {
                                    setQuery(event.target.value);
                                    setMatchCursor(0);
                                } }), _jsx(Button, { glyph: "\u2191", disabled: matches.length === 0, onClick: () => goToMatch(matchCursor - 1), children: "Previous match" }), _jsx(Button, { glyph: "\u2193", disabled: matches.length === 0, onClick: () => goToMatch(matchCursor + 1), children: "Next match" })] }), _jsxs("div", { className: "transcript__tools", children: [_jsx(Button, { glyph: "\u29C9", disabled: selected.size === 0, onClick: () => void copySelection(), children: "Copy passage" }), editable ? (_jsx(Button, { variant: "primary", glyph: "\u2713", disabled: dirtyCount === 0 || save.pending, onClick: () => void runSave(), children: save.pending ? "Saving…" : `Save ${dirtyCount} edited segment${dirtyCount === 1 ? "" : "s"}` })) : null] })] }), _jsxs("p", { className: "transcript__status", role: "status", "aria-live": "polite", children: [query.trim().length === 0
                        ? `${segments.length} of ${total} segments loaded.`
                        : matchTotal === 0
                            ? `No matches in the ${segments.length} segments loaded of ${total}.`
                            : `Match ${matchCursor + 1} of ${matches.length} segments, ${matchTotal} occurrence${matchTotal === 1 ? "" : "s"} in the ${segments.length} segments loaded of ${total}.`, copyStatus ? ` ${copyStatus}` : ""] }), saveError ? (_jsx(CommandErrorNotice, { error: saveError, onRetry: conflict ? undefined : () => void runSave(), children: conflict ? (_jsxs(_Fragment, { children: [_jsx(Button, { glyph: "\u21BB", onClick: () => edits.drafts.size > 0 ? setDiscardPrompt(true) : onReloadRequested(), children: "Reload newest revision" }), _jsx(Button, { glyph: "\u29C9", onClick: () => void copyMyChanges(), children: "Copy my changes" })] })) : null })) : null, !editable ? (_jsx("p", { className: "transcript__readonly", children: "This revision is read-only. Switch to the current revision to make edits." })) : null, _jsx("div", { className: "transcript__scroller virtual-list", ref: scrollRef, "data-testid": "transcript-scroller", children: _jsx("div", { className: "virtual-list__sizer", role: "list", style: { height: `${virtualizer.getTotalSize()}px` }, children: virtualRows.map((row) => {
                        const segment = segments[row.index];
                        if (!segment)
                            return null;
                        const text = textOf(edits, segment);
                        const timed = segment.alignment_origin !== "none" && segment.start_ms !== null;
                        const isMatch = activeMatch?.index === segment.index;
                        return (_jsxs("div", { role: "listitem", "data-index": row.index, ref: virtualizer.measureElement, className: `virtual-list__item transcript__row${isMatch ? " transcript__row--active" : ""}`, style: { transform: `translateY(${row.start}px)` }, "data-testid": `segment-${segment.index}`, children: [_jsxs("div", { className: "transcript__gutter", children: [_jsxs("label", { className: "transcript__select", children: [_jsx("input", { type: "checkbox", checked: selected.has(segment.index), onChange: (event) => {
                                                        setSelected((current) => {
                                                            const next = new Set(current);
                                                            if (event.target.checked)
                                                                next.add(segment.index);
                                                            else
                                                                next.delete(segment.index);
                                                            return next;
                                                        });
                                                    } }), _jsxs("span", { className: "sr-only", children: ["Select segment ", segment.index + 1, " for copying"] })] }), timed && segment.start_ms !== null ? (_jsxs("button", { type: "button", className: "transcript__chip", onClick: () => onSeek(segment.start_ms ?? 0), "aria-label": segment.alignment_origin === "derived"
                                                ? `Play from ${formatClock(segment.start_ms)}, approximate (derived) timing`
                                                : `Play from ${formatClock(segment.start_ms)}`, children: [_jsx("span", { "aria-hidden": "true", children: formatClock(segment.start_ms) }), segment.alignment_origin === "derived" ? (_jsx("span", { className: "transcript__chip-note", "aria-hidden": "true", children: "approximate (derived)" })) : null] })) : null] }), _jsxs("div", { className: "transcript__body", children: [editingIndex === segment.index ? (_jsx("textarea", { className: "transcript__editor", "aria-label": `Edit segment ${segment.index + 1}`, value: text, autoFocus: true, onChange: (event) => commitText(segment, event.target.value), onKeyDown: (event) => {
                                                if (event.key === "Escape") {
                                                    event.stopPropagation();
                                                    cancelEditor(segment.index);
                                                }
                                            }, onBlur: closeEditor })) : editable ? (_jsx("div", { className: "transcript__text", role: "button", tabIndex: 0, "aria-label": `Segment ${segment.index + 1}: ${text}. Press Enter to edit.`, onDoubleClick: () => openEditor(segment.index), onKeyDown: (event) => {
                                                if (event.key === "Enter") {
                                                    event.preventDefault();
                                                    openEditor(segment.index);
                                                }
                                            }, children: _jsx(HighlightedText, { text: text, query: query }) })) : (_jsx("p", { className: "transcript__text transcript__text--readonly", children: _jsx(HighlightedText, { text: text, query: query }) })), _jsxs("div", { className: "transcript__signals", children: [segment.confidence ? (_jsxs("span", { className: "transcript__confidence", children: [segment.confidence.engine_id, " confidence", " ", segment.confidence.value.toFixed(2), " \u2014 not comparable across engines"] })) : (_jsx("span", { className: "transcript__confidence", children: "No confidence reported by this engine" })), segment.annotations.map((annotation, position) => {
                                                    const presentation = ANNOTATION_PRESENTATION[annotation.kind];
                                                    return (_jsx(StatusBadge, { tone: presentation.tone, glyph: presentation.glyph, label: presentation.label, description: annotation.reason }, `${annotation.kind}-${position}`));
                                                }), edits.drafts.has(segment.index) ? (_jsx(StatusBadge, { tone: "running", glyph: "\u25CF", label: "Unsaved edit", description: "This text differs from the saved revision." })) : null] })] })] }, segment.index));
                    }) }) }), nextCursor !== null ? (_jsx(Button, { glyph: "\u22EF", disabled: loading, onClick: () => void loadMore(), children: loading ? "Loading…" : `Load more segments (${segments.length} of ${total})` })) : null, _jsx(Dialog, { open: discardPrompt, destructive: true, title: "Discard your unsaved edits?", description: `Reloading replaces what you see with the newest revision. Your ${edits.drafts.size} edited segment${edits.drafts.size === 1 ? "" : "s"} will be lost unless you copy them first.`, onClose: () => setDiscardPrompt(false), footer: _jsxs(_Fragment, { children: [_jsx(Button, { onClick: () => setDiscardPrompt(false), children: "Keep editing" }), _jsx(Button, { glyph: "\u29C9", onClick: () => void copyMyChanges(), children: "Copy my changes" }), _jsx(Button, { variant: "danger", onClick: () => {
                                setDiscardPrompt(false);
                                onReloadRequested();
                            }, children: "Discard and reload" })] }) })] }));
}
function HighlightedText({ text, query }) {
    const runs = highlightRuns(text, query);
    return (_jsx(_Fragment, { children: runs.map((run, position) => run.match ? _jsx("mark", { children: run.text }, position) : _jsx("span", { children: run.text }, position)) }));
}
async function writeToClipboard(text) {
    const clipboard = navigator.clipboard;
    if (!clipboard || typeof clipboard.writeText !== "function")
        return false;
    try {
        await clipboard.writeText(text);
        return true;
    }
    catch {
        return false;
    }
}
/** Cursor-paged `get_transcript` reader; pages accumulate for the virtualizer. */
function useTranscriptSegments(revisionId) {
    const [segments, setSegments] = useState([]);
    const [total, setTotal] = useState(0);
    const [nextCursor, setNextCursor] = useState(null);
    const [loading, setLoading] = useState(true);
    const [error, setError] = useState(null);
    const generation = useRef(0);
    const fetchPage = useCallback(async (cursor, append) => {
        const run = ++generation.current;
        setLoading(true);
        try {
            const page = await invokeCommand("get_transcript", {
                revision_id: revisionId,
                cursor,
                limit: TRANSCRIPT_PAGE_SIZE,
            });
            if (run !== generation.current)
                return;
            setSegments((current) => (append ? [...current, ...page.segments] : page.segments));
            setTotal(page.total_segments);
            setNextCursor(page.next_cursor);
            setError(null);
        }
        catch (caught) {
            if (run !== generation.current)
                return;
            setError(caught instanceof CommandError ? caught : new CommandError(toEnvelope(caught)));
        }
        finally {
            if (run === generation.current)
                setLoading(false);
        }
    }, [revisionId]);
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
            if (cursorRef.current === null)
                return;
            await fetchPage(cursorRef.current, true);
        },
        reload: () => fetchPage(null, false),
    };
}
