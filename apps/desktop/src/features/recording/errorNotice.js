import { jsx as _jsx, jsxs as _jsxs } from "react/jsx-runtime";
import { Button } from "../../components/Button";
import { useAppState } from "../../app/AppState";
const ACTION_AFFORDANCES = {
    OPEN_PLAUD_CONNECT: { label: "Open PLAUD sources", destination: "settings" },
    OPEN_CLI_SETUP: { label: "Open CLI setup", destination: "settings" },
    OPEN_SETTINGS: { label: "Open settings", destination: "settings" },
    OPEN_PERMISSION_SETTINGS: { label: "Open permission settings", destination: "settings" },
    RETRY_IMPORT: { label: "Retry import" },
    REFRESH_SOURCE_LINK: { label: "Refresh source link" },
    REVIEW_CONSENT: { label: "Review consent", destination: "settings" },
    FREE_DISK_SPACE: { label: "Open storage settings", destination: "settings" },
    REVIEW_LOCAL_ONLY_MODE: { label: "Review local-only mode", destination: "settings" },
    OPEN_DEVICE_DIAGNOSTICS: { label: "Open device diagnostics", destination: "settings" },
    CHARGE_DEVICE: { label: "Charge the device" },
    OPEN_DIAGNOSTICS: { label: "Open diagnostics", destination: "settings" },
    CHOOSE_ANOTHER_MODEL: { label: "Choose another engine", destination: "settings" },
    WAIT_AND_RETRY: { label: "Retry" },
};
export function CommandErrorNotice({ error, onRetry, children }) {
    const { navigate } = useAppState();
    const envelope = error.envelope;
    const action = envelope.action ?? null;
    const affordance = action ? ACTION_AFFORDANCES[action] : null;
    // No declared affordance: a retryable error still gets a Retry control.
    const showRetry = onRetry !== undefined && (action === null || action === "WAIT_AND_RETRY" || envelope.retryable);
    return (_jsxs("div", { className: "error-notice", role: "alert", children: [_jsx("p", { className: "error-notice__message", children: envelope.message }), _jsxs("div", { className: "error-notice__actions", children: [showRetry ? (_jsx(Button, { glyph: "\u21BB", onClick: onRetry, children: "Retry" })) : null, affordance && affordance.label !== "Retry" ? (_jsx(Button, { onClick: () => {
                            if (affordance.destination === "settings")
                                navigate({ name: "settings" });
                            else if (affordance.destination === "activity")
                                navigate({ name: "settings.activity" });
                            else
                                onRetry?.();
                        }, children: affordance.label })) : null, children] }), envelope.diagnostic_id ? (_jsxs("p", { className: "error-notice__diagnostic", children: ["Diagnostic reference: ", envelope.diagnostic_id] })) : null] }));
}
