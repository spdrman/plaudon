import { jsx as _jsx, jsxs as _jsxs, Fragment as _Fragment } from "react/jsx-runtime";
import { useEffect, useRef, useState } from "react";
import { Button } from "../../components/Button";
import { Dialog } from "../../components/Dialog";
import { Progress } from "../../components/Progress";
import { StatusBadge } from "../../components/StatusBadge";
import { CANCEL_DISCLOSURE, JOB_KIND_LABELS, JOB_STATE_PRESENTATION, envelopeActionControl, errorGuidance, jobElapsedLabel, } from "./jobPresentation";
export function JobRow({ job, focused, busy, freeBytes, onOpenTarget, onControl, onCancel, onRetry, }) {
    const rowRef = useRef(null);
    const [cancelOpen, setCancelOpen] = useState(false);
    const [costOpen, setCostOpen] = useState(false);
    const [costAcknowledged, setCostAcknowledged] = useState(false);
    useEffect(() => {
        if (!focused)
            return;
        const node = rowRef.current;
        if (!node)
            return;
        node.scrollIntoView?.({ block: "nearest" });
        node.focus();
    }, [focused]);
    const presentation = JOB_STATE_PRESENTATION[job.state];
    const guidance = errorGuidance(job.error?.code);
    const secondary = job.error
        ? envelopeActionControl(job.error.action, job.error.retryable)
        : null;
    const costAckRequired = presentation.requiresCostAck || job.cost_acknowledgement_required;
    const retryAllowed = presentation.canRetry && !(guidance?.suppressRetry ?? false);
    const controls = [];
    for (const control of guidance?.controls ?? []) {
        if (control.intent === "retry" && !retryAllowed)
            continue;
        controls.push(control);
    }
    if (secondary && !controls.some((c) => c.label === secondary.label)) {
        if (!(secondary.intent === "retry" && !retryAllowed))
            controls.push(secondary);
    }
    const kindLabel = JOB_KIND_LABELS[job.kind];
    const elapsed = jobElapsedLabel(job.started_at, job.finished_at);
    const showProgress = !presentation.terminal && job.state !== "cloud_outcome_unknown";
    const outputLink = job.output_link;
    return (_jsxs("li", { className: "job-row", ref: rowRef, tabIndex: -1, "data-job-id": job.job_id, children: [_jsxs("div", { className: "job-row__head", children: [_jsxs("div", { className: "job-row__identity", children: [_jsx("span", { className: "job-row__kind", children: kindLabel }), _jsx("span", { className: "job-row__label", children: job.label })] }), _jsx(StatusBadge, { tone: presentation.tone, glyph: presentation.glyph, label: presentation.label, description: presentation.description })] }), _jsxs("dl", { className: "job-row__facts", children: [_jsxs("div", { className: "job-row__fact", children: [_jsx("dt", { children: "Stage" }), _jsx("dd", { children: job.stage ?? "Not reported" })] }), _jsxs("div", { className: "job-row__fact", children: [_jsx("dt", { children: "Elapsed" }), _jsx("dd", { children: elapsed })] })] }), showProgress ? (_jsx(Progress, { label: `${kindLabel} progress for ${job.label}`, value: job.progress })) : null, job.state === "cancelling" || job.state === "cancelled" ? (_jsx("p", { className: "job-row__note", children: CANCEL_DISCLOSURE })) : null, job.state === "cloud_outcome_unknown" ? (_jsxs("section", { className: "job-row__panel", "aria-label": `Cloud outcome unknown for ${job.label}`, children: [_jsx("h2", { className: "job-row__panel-title", children: "Cloud request outcome unknown" }), _jsx("p", { children: "The provider response was lost after the request was dispatched. The request may already have been accepted and billed, so it is never retried automatically." }), _jsxs("p", { children: ["Provider request id:", " ", _jsx("span", { className: "job-row__mono", children: job.provider_request_id ?? "not reported" })] })] })) : null, job.error ? (_jsxs("div", { className: "job-row__error", role: "group", "aria-label": `Error for ${job.label}`, children: [_jsxs("p", { className: "job-row__error-title", children: [_jsx("span", { "aria-hidden": "true", children: "! " }), guidance?.title ?? "This job stopped"] }), _jsx("p", { className: "job-row__error-message", children: job.error.message }), guidance?.showCapacity ? (_jsxs("dl", { className: "job-row__facts", children: [_jsxs("div", { className: "job-row__fact", children: [_jsx("dt", { children: "Needed" }), _jsx("dd", { children: job.error.message })] }), _jsxs("div", { className: "job-row__fact", children: [_jsx("dt", { children: "Available" }), _jsx("dd", { children: freeBytes === null ? "Unknown" : formatBytes(freeBytes) })] })] })) : null, guidance?.suppressRetry ? (_jsx("p", { className: "job-row__note", children: "Billable retries are stopped until your provider credits are restored." })) : null, _jsx("div", { className: "job-row__actions", children: controls.map((control) => (_jsx(Button, { onClick: () => control.intent === "retry"
                                ? startRetry()
                                : control.intent === "keep_queued"
                                    ? onControl("keep_queued", job)
                                    : onControl(control.intent, job), disabled: busy, children: control.label }, `${control.intent}-${control.label}`))) })] })) : null, _jsxs("div", { className: "job-row__actions", children: [presentation.canCancel ? (_jsx(Button, { onClick: () => setCancelOpen(true), disabled: busy, children: "Cancel" })) : null, retryAllowed && !job.error ? (_jsx(Button, { onClick: startRetry, disabled: busy, children: "Retry" })) : null, outputLink ? (_jsx(Button, { variant: "quiet", onClick: () => onOpenTarget(outputLink), children: "Open result" })) : null] }), _jsx(Dialog, { open: cancelOpen, destructive: true, title: `Cancel ${kindLabel.toLowerCase()}?`, description: _jsx("p", { children: CANCEL_DISCLOSURE }), onClose: () => setCancelOpen(false), footer: _jsxs(_Fragment, { children: [_jsx(Button, { onClick: () => setCancelOpen(false), children: "Keep running" }), _jsx(Button, { variant: "danger", onClick: () => {
                                setCancelOpen(false);
                                onCancel(job);
                            }, children: "Cancel this job" })] }) }), _jsxs(Dialog, { open: costOpen, title: "This retry may be billed again", description: _jsx("p", { children: "The earlier request may already have been accepted and billed by the provider. Retrying sends a new request, which may be billed a second time." }), onClose: closeCostDialog, footer: _jsxs(_Fragment, { children: [_jsx(Button, { onClick: closeCostDialog, children: "Keep as is" }), _jsx(Button, { variant: "primary", disabled: !costAcknowledged, onClick: () => {
                                closeCostDialog();
                                onRetry(job, true);
                            }, children: "Retry and accept the cost" })] }), children: [_jsxs("p", { className: "job-row__panel-title", children: ["Provider request id:", " ", _jsx("span", { className: "job-row__mono", children: job.provider_request_id ?? "not reported" })] }), _jsxs("label", { className: "job-row__check", children: [_jsx("input", { type: "checkbox", checked: costAcknowledged, onChange: (event) => setCostAcknowledged(event.target.checked) }), "I understand this retry may incur a new charge."] })] })] }));
    function startRetry() {
        if (costAckRequired) {
            setCostAcknowledged(false);
            setCostOpen(true);
            return;
        }
        onRetry(job, false);
    }
    function closeCostDialog() {
        setCostOpen(false);
        setCostAcknowledged(false);
    }
}
const UNITS = ["bytes", "KB", "MB", "GB", "TB"];
export function formatBytes(bytes) {
    let value = bytes;
    let unit = 0;
    while (value >= 1000 && unit < UNITS.length - 1) {
        value /= 1000;
        unit += 1;
    }
    const rounded = unit === 0 ? String(Math.round(value)) : value.toFixed(1);
    return `${rounded} ${UNITS[unit]}`;
}
