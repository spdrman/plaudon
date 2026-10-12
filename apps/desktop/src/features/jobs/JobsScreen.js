import { jsx as _jsx, jsxs as _jsxs } from "react/jsx-runtime";
import { useCallback, useMemo, useState } from "react";
import "./jobs.css";
import { JOB_STATES } from "../../ipc/contract";
import { useCommandMutation, useCommandQuery } from "../../ipc/useCommand";
import { useAppState } from "../../app/AppState";
import { useRefetchOnEvents } from "../../app/useRefetchOnEvents";
import { Button } from "../../components/Button";
import { EmptyState } from "../../components/EmptyState";
import { JobRow } from "./JobRow";
import { JOB_KIND_LABELS, JOB_STATE_PRESENTATION } from "./jobPresentation";
const JOB_EVENTS = [
    "job.updated",
    "job.failed",
    "transcript.completed",
    "recording.imported",
];
const JOB_KINDS = Object.keys(JOB_KIND_LABELS);
export function JobsScreen({ focusJobId, onOpenTarget }) {
    const { capabilities, navigate, pushToast } = useAppState();
    const [states, setStates] = useState([]);
    const [kinds, setKinds] = useState([]);
    const query = useCommandQuery("list_jobs", { cursor: null, limit: 50, states, kinds }, { deps: [states.join(","), kinds.join(",")] });
    const { data, error, loading, refetch } = query;
    const refetchJobs = useCallback(() => {
        void refetch();
    }, [refetch]);
    useRefetchOnEvents(JOB_EVENTS, refetchJobs);
    const cancel = useCommandMutation("cancel_job");
    const retry = useCommandMutation("retry_job");
    const busy = cancel.pending || retry.pending;
    const counts = data?.state_counts ?? {};
    const countedStates = useMemo(() => JOB_STATES.filter((state) => (counts[state] ?? 0) > 0 || states.includes(state)), [counts, states]);
    const onCancel = useCallback(async (job) => {
        try {
            await cancel.run({ job_id: job.job_id });
        }
        catch {
            // The envelope is rendered from `cancel.error`; nothing to derive here.
        }
        void refetch();
    }, [cancel, refetch]);
    const onRetry = useCallback(async (job, costAcknowledged) => {
        try {
            await retry.run({ job_id: job.job_id, cost_acknowledged: costAcknowledged });
        }
        catch {
            // Rendered below from `retry.error`.
        }
        void refetch();
    }, [retry, refetch]);
    const onControl = useCallback((intent, job) => {
        switch (intent) {
            case "keep_queued":
                pushToast({
                    tone: "info",
                    title: "Kept queued",
                    body: `${job.label} stays in the queue and will not be sent anywhere else.`,
                });
                return;
            case "open_storage_settings":
            case "free_space":
            case "open_ai_settings":
            case "choose_local_engine":
            case "review_local_only_mode":
            case "review_consent":
            case "open_settings":
            case "open_permission_settings":
            case "open_plaud_connect":
            case "open_cli_setup":
            case "open_diagnostics":
            case "open_device_diagnostics":
            case "refresh_source_link":
            case "charge_device":
                navigate({ name: "settings" });
                return;
            case "retry":
            case "retry_with_cost_acknowledgement":
            case "retry_import":
                // Cost acknowledgement is only ever granted through the row dialog.
                if (job.cost_acknowledgement_required)
                    return;
                void onRetry(job, false);
                return;
        }
    }, [navigate, onRetry, pushToast]);
    const items = data?.items ?? [];
    const mutationError = cancel.error ?? retry.error;
    return (_jsxs("section", { className: "jobs", "aria-labelledby": "jobs-heading", children: [_jsx("h2", { id: "jobs-heading", className: "jobs__heading", children: "Jobs" }), _jsxs("div", { className: "jobs__filters", children: [_jsxs("fieldset", { className: "jobs__filter-group", children: [_jsx("legend", { className: "jobs__filter-legend", children: "Filter by state" }), countedStates.length === 0 ? (_jsx("p", { className: "jobs__hint", children: "No job states to filter yet." })) : (countedStates.map((state) => {
                                const active = states.includes(state);
                                return (_jsxs(Button, { "aria-pressed": active, onClick: () => setStates((current) => current.includes(state)
                                        ? current.filter((s) => s !== state)
                                        : [...current, state]), children: [JOB_STATE_PRESENTATION[state].label, " (", counts[state] ?? 0, ")"] }, state));
                            }))] }), _jsxs("fieldset", { className: "jobs__filter-group", children: [_jsx("legend", { className: "jobs__filter-legend", children: "Filter by kind" }), JOB_KINDS.map((kind) => {
                                const active = kinds.includes(kind);
                                return (_jsx(Button, { "aria-pressed": active, onClick: () => setKinds((current) => current.includes(kind) ? current.filter((k) => k !== kind) : [...current, kind]), children: JOB_KIND_LABELS[kind] }, kind));
                            })] })] }), _jsx("p", { className: "jobs__status", role: "status", children: loading ? "Loading jobs…" : `${items.length} job${items.length === 1 ? "" : "s"} shown` }), error ? (_jsxs("div", { className: "jobs__error", role: "alert", children: [_jsx("p", { children: error.envelope.message }), _jsx(Button, { onClick: refetchJobs, children: "Retry" })] })) : null, mutationError ? (_jsx("p", { className: "jobs__error", role: "alert", children: mutationError.envelope.message })) : null, !loading && !error && items.length === 0 ? (_jsx(EmptyState, { title: "No jobs", body: "Nothing is being processed right now. Imports and transcriptions appear here while they run, and stay here afterwards.", actions: _jsx(Button, { onClick: refetchJobs, children: "Refresh" }) })) : null, items.length > 0 ? (_jsx("ul", { className: "jobs__list", children: items.map((job) => (_jsx(JobRow, { job: job, focused: job.job_id === focusJobId, busy: busy, freeBytes: capabilities?.storage.free_bytes ?? null, onOpenTarget: onOpenTarget, onControl: onControl, onCancel: (target) => void onCancel(target), onRetry: (target, acknowledged) => void onRetry(target, acknowledged) }, job.job_id))) })) : null] }));
}
