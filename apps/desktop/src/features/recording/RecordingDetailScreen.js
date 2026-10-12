import { jsx as _jsx, jsxs as _jsxs, Fragment as _Fragment } from "react/jsx-runtime";
/**
 * Recording detail (§5.2): header with provenance and privacy, playback,
 * transcript, and the Summary/Actions/Decisions/History pane.
 */
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import "./recording.css";
import { Button } from "../../components/Button";
import { Dialog } from "../../components/Dialog";
import { EmptyState } from "../../components/EmptyState";
import { SelectField } from "../../components/Field";
import { StatusBadge } from "../../components/StatusBadge";
import { useAppState } from "../../app/AppState";
import { useRefetchOnEvents } from "../../app/useRefetchOnEvents";
import { useCommandMutation, useCommandQuery } from "../../ipc/useCommand";
import { AudioPlayer, usePlayerController, usePlayerShortcuts } from "./AudioPlayer";
import { TranscriptPane } from "./TranscriptPane";
import { RevisionSwitcher, revisionLabel } from "./RevisionSwitcher";
import { CommandErrorNotice } from "./errorNotice";
const DETAIL_EVENTS = ["transcript.completed", "recording.source_updated", "library.changed"];
const TABS = [
    { id: "summary", label: "Summary" },
    { id: "actions", label: "Actions" },
    { id: "decisions", label: "Decisions" },
    { id: "history", label: "History" },
];
const OUTPUT_KIND_BY_TAB = {
    summary: "summary",
    actions: "actions",
    decisions: "decisions",
};
const TRANSCRIPTION_PRESENTATION = {
    none: { label: "Not transcribed", glyph: "–", tone: "neutral" },
    queued: { label: "Queued", glyph: "…", tone: "waiting" },
    running: { label: "Transcribing", glyph: "▸", tone: "running" },
    completed: { label: "Transcribed", glyph: "✓", tone: "success" },
    failed: { label: "Transcription failed", glyph: "✕", tone: "danger" },
    stale: { label: "Transcript stale", glyph: "⟳", tone: "warning" },
};
const EXPORT_FORMATS = [
    { value: "txt", label: "Plain text (.txt)" },
    { value: "markdown", label: "Markdown (.md)" },
    { value: "json", label: "Structured JSON (.json)" },
];
export function RecordingDetailScreen({ recordingId, onBack, }) {
    const { preferences, pushToast } = useAppState();
    const { data: detail, error, loading, refetch } = useCommandQuery("get_recording", { recording_id: recordingId }, { deps: [recordingId] });
    useRefetchOnEvents(DETAIL_EVENTS, refetch);
    const controller = usePlayerController(detail?.duration_ms ?? null);
    const shortcuts = usePlayerShortcuts(controller);
    useEffect(() => {
        const onKeyDown = (event) => shortcuts(event);
        window.addEventListener("keydown", onKeyDown);
        return () => window.removeEventListener("keydown", onKeyDown);
    }, [shortcuts]);
    const [selectedRevisionId, setSelectedRevisionId] = useState(null);
    /**
     * Revision the user is editing. While it is set, an incoming
     * `transcript.completed` (or any other refetch) must not move the pane to a
     * newer head: that would discard unsaved text silently (§4.5).
     */
    const [pinnedRevisionId, setPinnedRevisionId] = useState(null);
    const [staleOutputIds, setStaleOutputIds] = useState([]);
    const [tab, setTab] = useState("summary");
    useEffect(() => {
        setSelectedRevisionId(null);
        setPinnedRevisionId(null);
        setStaleOutputIds([]);
    }, [recordingId]);
    const revisions = useMemo(() => detail?.revisions ?? [], [detail]);
    const activeRevision = revisions.find((revision) => revision.revision_id === selectedRevisionId) ??
        revisions.find((revision) => revision.revision_id === pinnedRevisionId) ??
        revisions.find((revision) => revision.revision_id === detail?.current_revision_id) ??
        null;
    // Read inside the dirty callback, which must pin whatever is on screen now.
    const displayedRevisionId = useRef(null);
    displayedRevisionId.current = activeRevision?.revision_id ?? null;
    const onDirtyChange = useCallback((dirtyCount) => {
        setPinnedRevisionId((current) => {
            if (dirtyCount === 0)
                return null;
            return current ?? displayedRevisionId.current;
        });
    }, []);
    if (error) {
        return (_jsxs("div", { className: "recording", children: [_jsx("header", { className: "recording__header", children: _jsx(Button, { glyph: "\u2190", onClick: onBack, children: "Back to library" }) }), _jsx(CommandErrorNotice, { error: error, onRetry: () => void refetch() })] }));
    }
    if (!detail) {
        return (_jsxs("div", { className: "recording", children: [_jsx("header", { className: "recording__header", children: _jsx(Button, { glyph: "\u2190", onClick: onBack, children: "Back to library" }) }), _jsx("p", { role: "status", "aria-live": "polite", children: loading ? "Loading recording…" : "This recording is not available." })] }));
    }
    const status = TRANSCRIPTION_PRESENTATION[detail.transcription_status];
    const localOnly = preferences?.local_only_mode ?? true;
    return (_jsxs("div", { className: "recording", children: [_jsxs("header", { className: "recording__header", children: [_jsxs("div", { className: "recording__header-row", children: [_jsx(Button, { glyph: "\u2190", onClick: onBack, children: "Back to library" }), _jsx(ExportControl, { detail: detail, revisionId: activeRevision?.revision_id ?? null, onExported: (jobId) => pushToast({ tone: "info", title: "Export queued", body: `Job ${jobId}` }) })] }), _jsx("h2", { className: "recording__title", children: detail.title }), _jsx("p", { className: "recording__title-note", children: "Renaming is not part of this build: no command persists a new title, so the title is shown exactly as it was imported." }), _jsxs("div", { className: "recording__badges", children: [_jsx(StatusBadge, { tone: status.tone, glyph: status.glyph, label: status.label }), localOnly ? (_jsx(StatusBadge, { tone: "success", glyph: "\u25C6", label: "Local only", description: "Local-only mode is on: this recording is never sent to a provider." })) : (_jsx(StatusBadge, { tone: "warning", glyph: "\u25C7", label: "Cloud eligible", description: "Cloud processing is allowed; each cloud run still asks for consent." })), detail.processing_engine ? (_jsx(StatusBadge, { tone: "neutral", glyph: "\u2699", label: `Engine ${detail.processing_engine}` })) : null] }), _jsxs("dl", { className: "recording__provenance", children: [_jsxs("div", { children: [_jsx("dt", { children: "Source" }), _jsx("dd", { children: detail.source.label })] }), _jsxs("div", { children: [_jsx("dt", { children: "Original file" }), _jsx("dd", { children: detail.media.original_file_name })] }), _jsxs("div", { children: [_jsx("dt", { children: "Content hash (SHA-256)" }), _jsx("dd", { children: _jsxs("code", { children: [detail.sha256.slice(0, 12), "\u2026"] }) })] }), _jsxs("div", { children: [_jsx("dt", { children: "Imported" }), _jsx("dd", { children: detail.imported_at })] })] }), detail.last_error ? (_jsxs("p", { className: "recording__last-error", role: "status", children: ["Last error: ", detail.last_error.message] })) : null] }), _jsxs("div", { className: "recording__layout", children: [_jsxs("div", { className: "recording__main", children: [_jsx(AudioPlayer, { recordingId: recordingId, controller: controller, title: detail.title }), activeRevision ? (_jsxs(_Fragment, { children: [_jsx(RevisionSwitcher, { revisions: revisions, selectedRevisionId: activeRevision.revision_id, headRevisionId: detail.current_revision_id, onSelect: (revisionId) => {
                                            setPinnedRevisionId(null);
                                            setSelectedRevisionId(revisionId);
                                        } }), pinnedRevisionId !== null &&
                                        detail.current_revision_id !== null &&
                                        detail.current_revision_id !== activeRevision.revision_id ? (_jsxs("p", { className: "recording__pinned", role: "status", "aria-live": "polite", children: [_jsx("span", { "aria-hidden": "true", children: "\u2691 " }), "A newer revision was saved elsewhere. Your edits are kept; saving will show resolution options."] })) : null, _jsx(TranscriptPane, { recordingId: recordingId, revision: activeRevision, editable: activeRevision.revision_id === (pinnedRevisionId ?? detail.current_revision_id), onSeek: (positionMs) => controller.seekTo(positionMs), onDirtyChange: onDirtyChange, onSaved: (response) => {
                                            setPinnedRevisionId(null);
                                            setSelectedRevisionId(response.revision_id);
                                            setStaleOutputIds(response.stale_output_ids);
                                            void refetch();
                                            pushToast({
                                                tone: "success",
                                                title: "Transcript revision saved",
                                                body: "The original machine transcript is unchanged and still available.",
                                            });
                                        }, onReloadRequested: () => {
                                            setPinnedRevisionId(null);
                                            setSelectedRevisionId(null);
                                            void refetch();
                                        } })] })) : (_jsx(EmptyState, { title: "No transcript yet", body: "This recording has not been transcribed, so there is nothing to read or edit here. Start a transcription from the Jobs screen once an engine and model are installed." }))] }), _jsx(DetailPane, { detail: detail, tab: tab, onTabChange: setTab, staleOutputIds: staleOutputIds })] })] }));
}
function DetailPane({ detail, tab, onTabChange, staleOutputIds }) {
    const tabRefs = useRef({});
    const moveTab = (direction) => {
        const position = TABS.findIndex((entry) => entry.id === tab);
        const next = TABS[(position + direction + TABS.length) % TABS.length];
        if (!next)
            return;
        onTabChange(next.id);
        tabRefs.current[next.id]?.focus();
    };
    return (_jsxs("aside", { className: "recording__pane", "aria-label": "Recording details", children: [_jsx("div", { className: "recording__tabs", role: "tablist", "aria-label": "Recording outputs", children: TABS.map((entry) => (_jsx("button", { type: "button", role: "tab", id: `tab-${entry.id}`, "aria-selected": tab === entry.id, "aria-controls": `panel-${entry.id}`, tabIndex: tab === entry.id ? 0 : -1, className: `recording__tab${tab === entry.id ? " recording__tab--active" : ""}`, ref: (node) => {
                        tabRefs.current[entry.id] = node;
                    }, onClick: () => onTabChange(entry.id), onKeyDown: (event) => {
                        if (event.key === "ArrowRight") {
                            event.preventDefault();
                            moveTab(1);
                        }
                        else if (event.key === "ArrowLeft") {
                            event.preventDefault();
                            moveTab(-1);
                        }
                    }, children: entry.label }, entry.id))) }), _jsx("div", { className: "recording__panel", role: "tabpanel", id: `panel-${tab}`, "aria-labelledby": `tab-${tab}`, tabIndex: 0, children: tab === "history" ? (_jsx(RevisionHistory, { detail: detail })) : (_jsx(OutputList, { detail: detail, kind: OUTPUT_KIND_BY_TAB[tab], staleOutputIds: staleOutputIds })) })] }));
}
function OutputList({ detail, kind, staleOutputIds, }) {
    const outputs = detail.outputs.filter((output) => output.kind === kind);
    if (outputs.length === 0) {
        return (_jsxs("p", { className: "recording__empty-output", children: ["No ", kind, " output exists for this recording yet. Generating one needs a text model, which this build does not run."] }));
    }
    return (_jsx("ul", { className: "recording__outputs", children: outputs.map((output) => {
            // §4.5: an output generated from a superseded revision needs regeneration.
            const stale = output.stale || staleOutputIds.includes(output.output_id);
            return (_jsxs("li", { children: [_jsxs("p", { children: ["Generated ", output.created_at] }), _jsxs("p", { className: "recording__output-source", children: ["From revision ", output.source_revision_id] }), stale ? (_jsx(StatusBadge, { tone: "warning", glyph: "\u27F3", label: "Needs regeneration", description: "The transcript changed after this output was produced." })) : (_jsx(StatusBadge, { tone: "success", glyph: "\u2713", label: "Current" }))] }, output.output_id));
        }) }));
}
function RevisionHistory({ detail }) {
    if (detail.revisions.length === 0) {
        return _jsx("p", { className: "recording__empty-output", children: "No transcript revisions exist yet." });
    }
    return (_jsx("ol", { className: "recording__revisions", children: detail.revisions.map((revision) => (_jsxs("li", { children: [_jsx("p", { children: revisionLabel(revision) }), _jsxs("p", { className: "recording__output-source", children: [revision.engine_id ?? "engine unknown", " \u00B7 ", revision.segment_count, " segments \u00B7", " ", revision.alignment_origin === "none"
                            ? "no timestamps"
                            : `${revision.alignment_origin} timestamps`] }), revision.revision_id === detail.current_revision_id ? (_jsx(StatusBadge, { tone: "success", glyph: "\u2713", label: "Current revision" })) : (_jsx(StatusBadge, { tone: "neutral", glyph: "\u25A3", label: "Superseded revision" }))] }, revision.revision_id))) }));
}
function ExportControl({ detail, revisionId, onExported, }) {
    const [open, setOpen] = useState(false);
    const [format, setFormat] = useState("markdown");
    const [problem, setProblem] = useState(null);
    const exportRecording = useCommandMutation("export_recording");
    const start = async () => {
        setProblem(null);
        if (!revisionId)
            return;
        try {
            // Exception to the static-import rule, as in `features/import/useImport.ts`:
            // `@tauri-apps/plugin-dialog` only resolves inside the Tauri webview, and
            // the browser harness and tests substitute it at this boundary.
            const { save } = await import("@tauri-apps/plugin-dialog");
            const destination = await save({
                title: "Export transcript",
                defaultPath: `${detail.title}.${format === "markdown" ? "md" : format}`,
            });
            if (!destination)
                return;
            const response = await exportRecording.run({
                recording_id: detail.recording_id,
                revision_id: revisionId,
                format,
                destination,
            });
            setOpen(false);
            onExported(response.job_id);
        }
        catch (caught) {
            setProblem(caught instanceof Error ? caught.message : "The export could not be started.");
        }
    };
    return (_jsxs(_Fragment, { children: [_jsx(Button, { glyph: "\u2197", disabled: revisionId === null, onClick: () => setOpen(true), children: "Export\u2026" }), revisionId === null ? (_jsx("span", { className: "field__hint", children: "Export needs a transcript revision." })) : null, _jsxs(Dialog, { open: open, title: "Export transcript", description: "Choose a format, then pick where to save the file.", onClose: () => setOpen(false), footer: _jsxs(_Fragment, { children: [_jsx(Button, { onClick: () => setOpen(false), children: "Cancel" }), _jsx(Button, { variant: "primary", disabled: exportRecording.pending, onClick: () => void start(), children: exportRecording.pending ? "Starting…" : "Choose destination…" })] }), children: [_jsx(SelectField, { label: "Format", value: format, onChange: (event) => setFormat(event.target.value), children: EXPORT_FORMATS.map((entry) => (_jsx("option", { value: entry.value, children: entry.label }, entry.value))) }), exportRecording.error ? (_jsx(CommandErrorNotice, { error: exportRecording.error, onRetry: () => void start() })) : null, problem ? (_jsx("p", { className: "error-notice__message", role: "alert", children: problem })) : null] })] }));
}
