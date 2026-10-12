import { jsx as _jsx, jsxs as _jsxs } from "react/jsx-runtime";
/**
 * Window-level drag-and-drop import (§5.2, §7.4) and the shared import state
 * for everything below it (the toolbar button, the empty states).
 *
 * Inside Tauri the real drop paths arrive on `onDragDropEvent`; in the browser
 * harness the DOM `drop` event is the fallback. Either way only strings are
 * forwarded to `import_files` — a `File` object never crosses the boundary.
 */
import { useCallback, useEffect, useRef, useState } from "react";
import { StatusBadge } from "../../components/StatusBadge";
import { Button } from "../../components/Button";
import { Progress } from "../../components/Progress";
import { useAppState } from "../../app/AppState";
import { envelopeActionControl } from "../library/ErrorNotice";
import { DuplicateDialog } from "./DuplicateDialog";
import { AUDIO_EXTENSIONS, ImportContext, OUTCOME_BADGE, toPathStrings, useImportController, } from "./useImport";
import "./import.css";
/**
 * Paths for a browser `FileList`. A web `File` exposes no filesystem path, so
 * the harness falls back to the name; Rust still rejects anything that is not
 * an approved, resolvable path (§15.1).
 */
export function filesToPaths(files) {
    if (!files)
        return [];
    const paths = [];
    for (const file of Array.from(files)) {
        const candidate = "path" in file ? file.path : undefined;
        paths.push(typeof candidate === "string" && candidate.length > 0 ? candidate : file.name);
    }
    return paths;
}
export function ImportDropTarget({ children }) {
    const controller = useImportController();
    const { importPaths, setDragging, dragging } = controller;
    const [batch, setBatch] = useState([]);
    const [decideLater, setDecideLater] = useState(false);
    const tauriActive = useRef(false);
    const dragDepth = useRef(0);
    const startBatch = useCallback((paths) => {
        if (paths.length === 0)
            return;
        setBatch(paths);
        setDecideLater(false);
        void importPaths(paths, "ask");
    }, [importPaths]);
    useEffect(() => {
        let unlisten;
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
                if (cancelled)
                    off();
                else {
                    unlisten = off;
                    tauriActive.current = true;
                }
            }
            catch {
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
        const onDragEnter = (event) => {
            if (tauriActive.current)
                return;
            event.preventDefault();
            dragDepth.current += 1;
            setDragging(true);
        };
        const onDragOver = (event) => {
            if (tauriActive.current)
                return;
            // Required for the window to accept a drop at all.
            event.preventDefault();
            setDragging(true);
        };
        const onDragLeave = (event) => {
            if (tauriActive.current)
                return;
            event.preventDefault();
            dragDepth.current = Math.max(0, dragDepth.current - 1);
            if (dragDepth.current === 0)
                setDragging(false);
        };
        const onDrop = (event) => {
            if (tauriActive.current)
                return;
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
    const onChoose = useCallback((path, choice) => {
        void controller.resolveDuplicate(path, choice);
    }, [controller]);
    return (_jsxs(ImportContext.Provider, { value: controller, children: [children, dragging ? (_jsxs("div", { className: "import-drop__overlay", "data-testid": "import-drop-overlay", children: [_jsxs("p", { className: "import-drop__overlay-title", children: [_jsx("span", { "aria-hidden": "true", children: "\u2913 " }), "Drop audio files to import"] }), _jsx("p", { className: "import-drop__overlay-hint", children: AUDIO_EXTENSIONS.map((extension) => extension.toUpperCase()).join(", ") })] })) : null, _jsx("div", { className: "sr-only", role: "status", children: dragging ? "Drop audio files to import." : controller.announcement }), _jsx(ImportResultsPanel, { controller: controller, batch: batch, onClearBatch: () => setBatch([]) }), _jsx(DuplicateDialog, { open: !decideLater, duplicates: controller.duplicates, pending: controller.pending, onChoose: onChoose, onDecideLater: () => setDecideLater(true) })] }));
}
/** Per-file outcomes for the batch: a rejected file never hides the rest. */
function ImportResultsPanel({ controller, batch, onClearBatch }) {
    const { navigate } = useAppState();
    const { results, pending, importPaths } = controller;
    const paths = batch.length > 0 ? batch : results.map((result) => result.path);
    if (paths.length === 0)
        return null;
    return (_jsxs("section", { className: "import-results", "aria-label": "Import results", children: [_jsxs("header", { className: "import-results__header", children: [_jsx("h2", { className: "import-results__title", children: "Import" }), _jsx(Button, { variant: "quiet", onClick: () => {
                            controller.clearResults();
                            onClearBatch();
                        }, children: "Dismiss" })] }), pending ? _jsx(Progress, { label: "Importing files", value: null }) : null, _jsx("ul", { className: "import-results__list", children: paths.map((path) => {
                    const outcome = results.find((result) => result.path === path);
                    return (_jsxs("li", { className: "import-results__item", children: [_jsx("span", { className: "import-results__path", children: path }), outcome ? (_jsx(StatusBadge, { ...OUTCOME_BADGE[outcome.outcome] })) : (_jsx(StatusBadge, { tone: "running", glyph: "\u27F3", label: pending ? "Importing" : "Waiting", description: "No outcome reported yet" })), _jsx(OutcomeDetail, { outcome: outcome, onRetry: () => void importPaths([path], "ask"), onOpenSettings: () => navigate({ name: "settings" }) })] }, path));
                }) })] }));
}
function OutcomeDetail({ outcome, onRetry, onOpenSettings }) {
    if (!outcome)
        return null;
    if (outcome.outcome === "rejected") {
        const control = envelopeActionControl(outcome.error);
        return (_jsxs("div", { className: "import-results__detail", children: [_jsx("p", { className: "import-results__error", children: outcome.error.message }), control.kind === "retry" ? (_jsx(Button, { variant: "quiet", onClick: onRetry, children: control.label })) : null, control.kind === "settings" ? (_jsx(Button, { variant: "quiet", onClick: onOpenSettings, children: control.label })) : null] }));
    }
    if (outcome.outcome === "skipped") {
        return (_jsxs("p", { className: "import-results__detail", children: ["Kept the recording you already have (", outcome.existing_recording_id, ")."] }));
    }
    if (outcome.outcome === "duplicate") {
        return (_jsxs("p", { className: "import-results__detail", children: ["Same audio as ", _jsx("strong", { children: outcome.existing_title }), " by content hash. Choose what to do."] }));
    }
    return null;
}
