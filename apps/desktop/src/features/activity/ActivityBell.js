import { jsx as _jsx } from "react/jsx-runtime";
import { useCallback } from "react";
import "./activity.css";
import { useCommandQuery } from "../../ipc/useCommand";
import { useAppState } from "../../app/AppState";
import { useRefetchOnEvents } from "../../app/useRefetchOnEvents";
import { Button } from "../../components/Button";
const BELL_EVENTS = [
    "activity.added",
    "job.updated",
    "recording.imported",
    "transcript.completed",
];
/** Header entry point to the activity history, with the unread count in its name. */
export function ActivityBell() {
    const { navigate } = useAppState();
    const { data, refetch } = useCommandQuery("list_activity", { cursor: null, limit: 1 });
    const refresh = useCallback(() => {
        void refetch();
    }, [refetch]);
    useRefetchOnEvents(BELL_EVENTS, refresh);
    const unread = data?.unread_count ?? 0;
    return (_jsx(Button, { variant: "quiet", glyph: "\u2022", className: "activity-bell", "aria-label": `Activity history, ${unread} unread`, onClick: () => navigate({ name: "settings.activity" }), children: _jsx("span", { "aria-hidden": "true", children: unread }) }));
}
