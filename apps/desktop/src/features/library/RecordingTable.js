import { jsx as _jsx, jsxs as _jsxs } from "react/jsx-runtime";
/**
 * Virtualized recordings grid (§5.2, §5.3, §5.6).
 *
 * - Every §5.2 column is present, including the last error message.
 * - Status is text + glyph via `StatusBadge`, never color alone (§5.3).
 * - `aria-rowcount` reports the whole matching set, not the rendered window.
 * - Rows use a roving tabindex; Up/Down/Home/End move, Enter opens, Space
 *   toggles selection, Shift extends it, Cmd/Ctrl+A selects the loaded rows.
 * - No `user-select` override: text selection keeps working (§5.6).
 */
import { useCallback, useEffect, useRef, useState, } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";
import { StatusBadge } from "../../components/StatusBadge";
import { Button } from "../../components/Button";
import { IMPORT_STATUS_BADGE, TRANSCRIPTION_STATUS_BADGE, ariaSortFor, describeSelection, formatDateTime, formatDuration, rangeIds, } from "./libraryQuery";
/** Row height is owned by the virtualizer, so it lives in TS, not CSS. */
export const ROW_HEIGHT = 36;
const OVERSCAN = 6;
/**
 * Used when the scroll container reports a zero-sized rect — a pre-layout
 * frame, a hidden pane, or a DOM without a layout engine. Without it the
 * virtualizer would render nothing at all.
 */
const FALLBACK_VIEWPORT = { width: 960, height: 640 };
const COLUMNS = [
    {
        key: "title",
        label: "Title",
        sort: "title",
        render: (row) => _jsx("span", { className: "rec-grid__title", children: row.title }),
    },
    { key: "recorded", label: "Recorded", sort: "recorded_at", render: (row) => formatDateTime(row.recorded_at) },
    { key: "imported", label: "Imported", sort: "imported_at", render: (row) => formatDateTime(row.imported_at) },
    { key: "duration", label: "Duration", sort: "duration_ms", render: (row) => formatDuration(row.duration_ms) },
    { key: "source", label: "Source", render: (row) => row.source.label },
    {
        key: "import",
        label: "Import status",
        render: (row) => _jsx(StatusBadge, { ...IMPORT_STATUS_BADGE[row.import_status] }),
    },
    {
        key: "transcription",
        label: "Transcription",
        render: (row) => _jsx(StatusBadge, { ...TRANSCRIPTION_STATUS_BADGE[row.transcription_status] }),
    },
    { key: "engine", label: "Engine", render: (row) => row.processing_engine ?? "Not set" },
    {
        key: "error",
        label: "Last error",
        render: (row) => row.last_error ? _jsx("span", { className: "rec-grid__error", children: row.last_error.message }) : "None",
    },
];
export function RecordingTable({ label, rows, rowCount, sort, onSort, onOpenRecording, hasMore = false, loadingMore = false, onLoadMore, }) {
    const viewportRef = useRef(null);
    const [activeIndex, setActiveIndex] = useState(0);
    const [selected, setSelected] = useState(() => new Set());
    const anchorRef = useRef(0);
    const pendingFocus = useRef(false);
    const virtualizer = useVirtualizer({
        count: rows.length,
        getScrollElement: () => viewportRef.current,
        estimateSize: () => ROW_HEIGHT,
        overscan: OVERSCAN,
        observeElementRect: observeViewportRect,
    });
    const virtualItems = virtualizer.getVirtualItems();
    useEffect(() => {
        const ids = new Set(rows.map((row) => row.recording_id));
        setSelected((previous) => {
            if (previous.size === 0)
                return previous;
            const next = new Set([...previous].filter((id) => ids.has(id)));
            return next.size === previous.size ? previous : next;
        });
        setActiveIndex((previous) => Math.min(previous, Math.max(0, rows.length - 1)));
    }, [rows]);
    useEffect(() => {
        if (!pendingFocus.current)
            return;
        const row = viewportRef.current?.querySelector(`[data-row-index="${activeIndex}"]`);
        if (!row)
            return;
        pendingFocus.current = false;
        row.focus();
    });
    const lastRenderedIndex = virtualItems[virtualItems.length - 1]?.index ?? -1;
    useEffect(() => {
        if (!hasMore || loadingMore || !onLoadMore)
            return;
        if (rows.length > 0 && lastRenderedIndex >= rows.length - 1)
            onLoadMore();
    }, [hasMore, loadingMore, onLoadMore, lastRenderedIndex, rows.length]);
    const moveTo = useCallback((index, extend) => {
        if (rows.length === 0)
            return;
        const target = Math.max(0, Math.min(rows.length - 1, index));
        if (extend) {
            const ids = rangeIds(rows, anchorRef.current, target);
            setSelected((previous) => new Set([...previous, ...ids]));
        }
        else {
            anchorRef.current = target;
        }
        setActiveIndex(target);
        pendingFocus.current = true;
        virtualizer.scrollToIndex(target);
    }, [rows, virtualizer]);
    const toggle = useCallback((recordingId) => {
        setSelected((previous) => {
            const next = new Set(previous);
            if (next.has(recordingId))
                next.delete(recordingId);
            else
                next.add(recordingId);
            return next;
        });
    }, []);
    const onRowKeyDown = useCallback((event, index) => {
        const row = rows[index];
        if (!row)
            return;
        if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "a") {
            event.preventDefault();
            setSelected(new Set(rows.map((item) => item.recording_id)));
            return;
        }
        switch (event.key) {
            case "ArrowDown":
                event.preventDefault();
                moveTo(index + 1, event.shiftKey);
                break;
            case "ArrowUp":
                event.preventDefault();
                moveTo(index - 1, event.shiftKey);
                break;
            case "Home":
                event.preventDefault();
                moveTo(0, event.shiftKey);
                break;
            case "End":
                event.preventDefault();
                moveTo(rows.length - 1, event.shiftKey);
                break;
            case "Enter":
                event.preventDefault();
                onOpenRecording(row.recording_id);
                break;
            case " ":
                event.preventDefault();
                toggle(row.recording_id);
                break;
            default:
                break;
        }
    }, [moveTo, onOpenRecording, rows, toggle]);
    const onRowClick = useCallback((event, index) => {
        const row = rows[index];
        if (!row)
            return;
        setActiveIndex(index);
        if (event.shiftKey) {
            const ids = rangeIds(rows, anchorRef.current, index);
            setSelected((previous) => new Set([...previous, ...ids]));
            return;
        }
        anchorRef.current = index;
        if (event.metaKey || event.ctrlKey)
            toggle(row.recording_id);
        else
            setSelected(new Set([row.recording_id]));
    }, [rows, toggle]);
    return (_jsxs("div", { className: "rec-grid-shell", children: [_jsxs("div", { className: "rec-grid", role: "grid", "aria-label": label, "aria-rowcount": rowCount + 1, "aria-colcount": COLUMNS.length, "aria-multiselectable": "true", children: [_jsx("div", { className: "rec-grid__row rec-grid__row--head", role: "row", "aria-rowindex": 1, children: COLUMNS.map((column, index) => {
                            const field = column.sort;
                            return (_jsx("div", { role: "columnheader", className: "rec-grid__cell rec-grid__cell--head", "aria-colindex": index + 1, "aria-sort": field ? ariaSortFor(sort, field) : undefined, children: field ? (_jsxs("button", { type: "button", className: "rec-grid__sort", onClick: () => onSort(field), children: [column.label, _jsx("span", { "aria-hidden": "true", className: "rec-grid__sort-glyph", children: sort.sortField === field ? (sort.sortDirection === "asc" ? "▲" : "▼") : "↕" })] })) : (column.label) }, column.key));
                        }) }), _jsx("div", { className: "rec-grid__viewport", role: "presentation", ref: viewportRef, children: _jsx("div", { className: "rec-grid__sizer", role: "presentation", style: { height: virtualizer.getTotalSize() }, children: virtualItems.map((item) => {
                                const row = rows[item.index];
                                if (!row)
                                    return null;
                                return (_jsx("div", { role: "row", className: "rec-grid__row", "aria-rowindex": item.index + 2, "aria-selected": selected.has(row.recording_id), "data-row-index": item.index, tabIndex: item.index === activeIndex ? 0 : -1, style: { height: ROW_HEIGHT, transform: `translateY(${item.start}px)` }, onKeyDown: (event) => onRowKeyDown(event, item.index), onClick: (event) => onRowClick(event, item.index), onDoubleClick: () => onOpenRecording(row.recording_id), onFocus: () => setActiveIndex(item.index), children: COLUMNS.map((column, index) => (_jsx("div", { role: "gridcell", className: "rec-grid__cell", "aria-colindex": index + 1, children: column.render(row) }, column.key))) }, row.recording_id));
                            }) }) })] }), _jsxs("div", { className: "rec-grid__footer", children: [_jsxs("p", { className: "rec-grid__count", role: "status", children: ["Showing ", rows.length, " of ", rowCount, " recordings (snapshot at query time)"] }), hasMore ? (_jsx(Button, { onClick: onLoadMore, disabled: loadingMore, children: loadingMore ? "Loading more…" : "Load more recordings" })) : null] }), _jsx("div", { className: "sr-only", role: "status", children: describeSelection(selected.size, rows.length) })] }));
}
function observeViewportRect(instance, cb) {
    const element = instance.scrollElement;
    if (!element)
        return;
    const measure = () => {
        const rect = element.getBoundingClientRect();
        cb({
            width: Math.round(rect.width) || FALLBACK_VIEWPORT.width,
            height: Math.round(rect.height) || FALLBACK_VIEWPORT.height,
        });
    };
    measure();
    const observerCtor = element.ownerDocument.defaultView?.ResizeObserver;
    if (!observerCtor)
        return;
    const observer = new observerCtor(measure);
    observer.observe(element, { box: "border-box" });
    return () => observer.disconnect();
}
