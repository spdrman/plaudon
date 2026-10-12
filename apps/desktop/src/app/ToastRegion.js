import { jsxs as _jsxs, jsx as _jsx } from "react/jsx-runtime";
import { useAppState } from "./AppState";
import { Button } from "../components/Button";
/**
 * Toasts are announced through a polite live region and never steal focus
 * (§5.3). Every toast is also recorded in the activity history by the backend.
 */
export function ToastRegion() {
    const { toasts, dismissToast } = useAppState();
    return (_jsx("div", { className: "toast-region", role: "status", "aria-live": "polite", "aria-relevant": "additions text", "aria-label": "Notifications", children: toasts.map((toast) => (_jsxs("div", { className: "toast", "data-tone": toast.tone, children: [_jsxs("span", { className: "toast__title", children: [_jsxs("span", { "aria-hidden": "true", children: [TONE_GLYPH[toast.tone], " "] }), toast.title] }), toast.body ? _jsx("span", { className: "toast__body", children: toast.body }) : null, _jsx("div", { className: "dialog__actions", children: _jsx(Button, { variant: "quiet", onClick: () => dismissToast(toast.id), children: "Dismiss" }) })] }, toast.id))) }));
}
const TONE_GLYPH = {
    info: "i",
    success: "OK",
    warning: "!",
    danger: "X",
};
