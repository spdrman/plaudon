import { jsx as _jsx, Fragment as _Fragment, jsxs as _jsxs } from "react/jsx-runtime";
import { Button } from "../../components/Button";
import { StatusBadge } from "../../components/StatusBadge";
import { ImportButton } from "../import/ImportButton";
import { COMPATIBILITY_LABELS, STATE_LABELS, nextActionControl, } from "./plaudPresentation";
const STATE_TONE = {
    unconfigured: "neutral",
    cli_detected: "waiting",
    auth_required: "warning",
    authenticated: "success",
    enumerating: "running",
    ready: "success",
    expired_auth: "warning",
    incompatible_version: "warning",
    error: "danger",
};
/**
 * Connector diagnostics (K1.5, §4.2, §7.6), rendered from one `plaud_probe`
 * snapshot. The headline, detail, warnings and disconnect wording are Rust's
 * strings and are rendered verbatim: this component composes no sentence and
 * there is no vendor text anywhere in the payload to render.
 */
export function ConnectorPanel({ snapshot, busy, onRun, onDisconnect }) {
    const { diagnostics, status, disconnect } = snapshot;
    const control = nextActionControl(diagnostics.next_action);
    const usable = diagnostics.next_action.kind === "install_tested_version"
        ? diagnostics.next_action.usable_versions
        : [];
    return (_jsxs("section", { className: "connector", "aria-labelledby": "connector-heading", children: [_jsx("h2", { id: "connector-heading", className: "connector__headline", children: diagnostics.headline }), _jsx("p", { className: "connector__detail", children: diagnostics.detail }), _jsxs("dl", { className: "connector__facts", children: [_jsx("dt", { children: "State" }), _jsx("dd", { children: _jsx(StatusBadge, { tone: STATE_TONE[diagnostics.state], glyph: diagnostics.state === "ready" || diagnostics.state === "authenticated" ? "✓" : "•", label: STATE_LABELS[diagnostics.state] }) }), _jsx("dt", { children: "Tool version" }), _jsx("dd", { children: diagnostics.detected_version ?? "Not detected" }), _jsx("dt", { children: "Compatibility" }), _jsx("dd", { children: COMPATIBILITY_LABELS[diagnostics.compatibility] }), _jsx("dt", { children: "Found in" }), _jsx("dd", { children: diagnostics.program_origin ?? "Not found" }), status.last_error ? (_jsxs(_Fragment, { children: [_jsx("dt", { children: "Last error code" }), _jsx("dd", { children: _jsx("code", { children: status.last_error }) })] })) : null] }), diagnostics.warnings.length > 0 ? (_jsx("ul", { className: "connector__warnings", "aria-label": "Warnings", children: diagnostics.warnings.map((warning) => (_jsxs("li", { className: "connector__warning", children: [_jsx("span", { "aria-hidden": "true", className: "status-badge__glyph", children: "!" }), warning] }, warning))) })) : null, usable.length > 0 ? (_jsxs("p", { className: "connector__versions", children: ["Tested versions: ", _jsx("span", { children: usable.join(", ") })] })) : null, _jsxs("div", { className: "connector__actions", children: [control?.kind === "link" ? (_jsx("a", { className: "btn btn--primary", href: control.href, target: "_blank", rel: "noreferrer noopener", children: control.label })) : null, control?.kind === "action" ? (_jsx(Button, { variant: "primary", disabled: busy, onClick: () => onRun(control.run), children: control.label })) : null, control?.kind === "import_manually" ? _jsx(ImportButton, {}) : null, diagnostics.state === "unconfigured" ||
                        diagnostics.state === "incompatible_version" ? (_jsx(Button, { disabled: busy, onClick: () => onRun("retry"), children: "Check again" })) : null] }), diagnostics.state !== "unconfigured" ? (_jsxs("div", { className: "connector__disconnect", children: [_jsx("p", { className: "connector__disconnect-body", children: disconnect.body }), _jsx(Button, { variant: "danger", disabled: busy, onClick: onDisconnect, children: disconnect.label })] })) : null] }));
}
