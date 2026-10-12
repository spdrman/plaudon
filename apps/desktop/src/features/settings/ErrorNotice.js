import { jsx as _jsx, jsxs as _jsxs } from "react/jsx-runtime";
import "./settings.css";
import { Button } from "../../components/Button";
import { useAppState } from "../../app/AppState";
/**
 * §15.4 / §5.4: we render the envelope message plus the control its `action`
 * implies. `action` may be absent; a retryable envelope then falls back to a
 * Retry control. Stack text is never shown — only the diagnostic id.
 */
export function ErrorNotice({ envelope, onRetry, onChooseAnotherModel, children, }) {
    const { navigate } = useAppState();
    const action = envelope.action ?? null;
    const retryControl = onRetry != null ? (_jsx(Button, { variant: "primary", onClick: onRetry, children: "Retry" })) : null;
    const control = (() => {
        switch (action) {
            case "RETRY_IMPORT":
            case "WAIT_AND_RETRY":
                return retryControl;
            case "CHOOSE_ANOTHER_MODEL":
                return onChooseAnotherModel ? (_jsx(Button, { onClick: onChooseAnotherModel, children: "Choose another model" })) : null;
            case "FREE_DISK_SPACE":
                return _jsx(Button, { onClick: () => navigate({ name: "settings" }), children: "Open storage settings" });
            case "REVIEW_LOCAL_ONLY_MODE":
                return (_jsx(Button, { onClick: () => navigate({ name: "settings" }), children: "Review local-only mode" }));
            case "REVIEW_CONSENT":
            case "OPEN_SETTINGS":
                return _jsx(Button, { onClick: () => navigate({ name: "settings" }), children: "Open settings" });
            case "OPEN_DIAGNOSTICS":
                return _jsx(Button, { onClick: () => navigate({ name: "settings.activity" }), children: "Open diagnostics" });
            case null:
                return envelope.retryable ? retryControl : null;
            default:
                return envelope.retryable ? retryControl : null;
        }
    })();
    return (_jsxs("div", { className: "error-notice", role: "alert", children: [_jsxs("p", { className: "error-notice__message", children: [_jsx("span", { "aria-hidden": "true", children: "! " }), envelope.message] }), _jsxs("p", { className: "error-notice__diagnostic", children: ["Diagnostic id: ", envelope.diagnostic_id] }), control || children ? (_jsxs("div", { className: "error-notice__actions", children: [control, children] })) : null] }));
}
