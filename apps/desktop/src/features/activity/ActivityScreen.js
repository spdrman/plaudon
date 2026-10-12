import { jsx as _jsx, jsxs as _jsxs } from "react/jsx-runtime";
import { useCallback, useState } from "react";
import "./activity.css";
import { useCommandMutation, useCommandQuery } from "../../ipc/useCommand";
import { useRefetchOnEvents } from "../../app/useRefetchOnEvents";
import { Button } from "../../components/Button";
import { EmptyState } from "../../components/EmptyState";
import { StatusBadge } from "../../components/StatusBadge";
const ACTIVITY_EVENTS = [
    "activity.added",
    "job.updated",
    "recording.imported",
    "transcript.completed",
];
const PAGE_SIZE = 25;
export const CATEGORY_PRESENTATION = {
    import_finished: { label: "Import finished", glyph: "IN", tone: "success" },
    transcription_finished: { label: "Transcription finished", glyph: "TR", tone: "success" },
    requires_attention: { label: "Requires attention", glyph: "!", tone: "warning" },
    cloud_budget_exceeded: { label: "Cloud budget exceeded", glyph: "$", tone: "danger" },
};
/**
 * §5.5 / §22(13): the in-app activity history records every completion and
 * attention event, whether or not the OS delivered a notification.
 */
export function ActivityScreen({ onOpenTarget }) {
    const [extra, setExtra] = useState([]);
    const [tailCursor, setTailCursor] = useState(null);
    const { data, error, loading, refetch } = useCommandQuery("list_activity", {
        cursor: null,
        limit: PAGE_SIZE,
    });
    const more = useCommandMutation("list_activity");
    const reload = useCallback(() => {
        setExtra([]);
        setTailCursor(null);
        void refetch();
    }, [refetch]);
    useRefetchOnEvents(ACTIVITY_EVENTS, reload);
    const items = [...(data?.items ?? []), ...extra];
    const nextCursor = tailCursor ?? data?.next_cursor ?? null;
    const loadMore = useCallback(async () => {
        if (!nextCursor)
            return;
        try {
            const page = await more.run({ cursor: nextCursor, limit: PAGE_SIZE });
            setExtra((current) => [...current, ...page.items]);
            setTailCursor(page.next_cursor);
        }
        catch {
            // Rendered from `more.error` below.
        }
    }, [more, nextCursor]);
    return (_jsxs("section", { className: "activity", "aria-labelledby": "activity-heading", children: [_jsx("h2", { id: "activity-heading", className: "activity__heading", children: "Activity history" }), _jsx("p", { className: "activity__intro", children: "Every completion and attention event is recorded here, even when desktop notifications are turned off or denied by the operating system." }), _jsx("p", { className: "activity__status", role: "status", children: loading
                    ? "Loading activity…"
                    : `${items.length} entr${items.length === 1 ? "y" : "ies"}, ${data?.unread_count ?? 0} unread` }), error ? (_jsxs("div", { className: "activity__error", role: "alert", children: [_jsx("p", { children: error.envelope.message }), _jsx(Button, { onClick: reload, children: "Retry" })] })) : null, more.error ? (_jsx("p", { className: "activity__error", role: "alert", children: more.error.envelope.message })) : null, !loading && !error && items.length === 0 ? (_jsx(EmptyState, { title: "No activity yet", body: "Imports, finished transcriptions and anything needing your attention are listed here.", actions: _jsx(Button, { onClick: reload, children: "Refresh" }) })) : null, items.length > 0 ? (_jsx("ul", { className: "activity__list", children: items.map((entry) => (_jsx(ActivityRow, { entry: entry, onOpenTarget: onOpenTarget }, entry.activity_id))) })) : null, nextCursor ? (_jsx(Button, { onClick: () => void loadMore(), disabled: more.pending, children: "Load older activity" })) : null] }));
}
function ActivityRow({ entry, onOpenTarget, }) {
    const category = CATEGORY_PRESENTATION[entry.category];
    const target = entry.target;
    return (_jsxs("li", { className: "activity-row", children: [_jsxs("div", { className: "activity-row__head", children: [_jsx(StatusBadge, { tone: category.tone, glyph: category.glyph, label: category.label }), _jsx("span", { className: "activity-row__read", children: entry.read ? "Read" : "Unread" })] }), _jsx("p", { className: "activity-row__title", children: entry.title }), entry.body ? _jsx("p", { className: "activity-row__body", children: entry.body }) : null, _jsxs("p", { className: "activity-row__meta", children: [_jsx("time", { dateTime: entry.occurred_at, children: formatTimestamp(entry.occurred_at) }), " · ", entry.notification_delivered
                        ? "Desktop notification delivered"
                        : `Desktop notification not delivered${entry.notification_suppressed_reason
                            ? `: ${entry.notification_suppressed_reason}`
                            : ""}`] }), target ? (_jsx(Button, { variant: "quiet", onClick: () => onOpenTarget(target), children: target.kind === "recording"
                    ? "Open recording"
                    : target.kind === "job"
                        ? "Open job"
                        : "Open settings" })) : null] }));
}
export function formatTimestamp(iso) {
    const parsed = Date.parse(iso);
    if (Number.isNaN(parsed))
        return iso;
    return new Date(parsed).toLocaleString(undefined, {
        dateStyle: "medium",
        timeStyle: "short",
    });
}
