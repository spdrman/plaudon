import { jsx as _jsx, jsxs as _jsxs } from "react/jsx-runtime";
import { Button } from "../../components/Button";
import { useAppState } from "../../app/AppState";
/**
 * Maps an envelope to its corrective control. `action` is optional in the
 * contract: Rust omits it for codes with no affordance.
 */
export function envelopeActionControl(envelope) {
    switch (envelope.action ?? null) {
        case "RETRY_IMPORT":
            return { kind: "retry", label: "Retry import" };
        case "WAIT_AND_RETRY":
            return { kind: "retry", label: "Retry" };
        case "REFRESH_SOURCE_LINK":
            return { kind: "retry", label: "Refresh and retry" };
        case "FREE_DISK_SPACE":
            return { kind: "settings", label: "Open storage settings" };
        case "CHOOSE_ANOTHER_MODEL":
            return { kind: "settings", label: "Choose another engine" };
        case "REVIEW_LOCAL_ONLY_MODE":
            return { kind: "settings", label: "Review local-only mode" };
        case "REVIEW_CONSENT":
            return { kind: "settings", label: "Review cloud consent" };
        case "OPEN_SETTINGS":
            return { kind: "settings", label: "Open settings" };
        case "OPEN_PLAUD_CONNECT":
            return { kind: "settings", label: "Open PLAUD connection settings" };
        case "OPEN_CLI_SETUP":
            return { kind: "settings", label: "Open PLAUD CLI setup" };
        case "OPEN_PERMISSION_SETTINGS":
            return { kind: "settings", label: "Open permission settings" };
        case "OPEN_DEVICE_DIAGNOSTICS":
        case "OPEN_DIAGNOSTICS":
            return { kind: "settings", label: "Open diagnostics" };
        case "CHARGE_DEVICE":
            return { kind: "none" };
        default:
            return envelope.retryable ? { kind: "retry", label: "Retry" } : { kind: "none" };
    }
}
export function ErrorNotice({ envelope, onRetry, context }) {
    const { navigate } = useAppState();
    const control = envelopeActionControl(envelope);
    return (_jsxs("div", { className: "error-notice", role: "alert", children: [_jsxs("p", { className: "error-notice__message", children: [_jsx("span", { "aria-hidden": "true", className: "status-badge__glyph", children: "\u2715" }), context ? `${context} ` : "", envelope.message] }), _jsxs("div", { className: "error-notice__actions", children: [control.kind === "retry" && onRetry ? (_jsx(Button, { variant: "primary", onClick: onRetry, children: control.label })) : null, control.kind === "settings" ? (_jsx(Button, { onClick: () => navigate({ name: "settings" }), children: control.label })) : null] }), envelope.diagnostic_id ? (_jsxs("p", { className: "error-notice__diagnostic", children: ["Diagnostic id: ", envelope.diagnostic_id] })) : null] }));
}
