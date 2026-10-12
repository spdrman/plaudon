import { jsx as _jsx, jsxs as _jsxs } from "react/jsx-runtime";
/**
 * Non-blocking progress (§5.3). An unknown ratio renders an honest
 * indeterminate bar; we never invent a percentage.
 */
export function Progress({ label, value, detail }) {
    const pct = value === null ? null : Math.max(0, Math.min(1, value));
    const text = pct === null ? "in progress, amount unknown" : `${Math.round(pct * 100)}%`;
    return (_jsxs("div", { className: "progress", children: [_jsx("div", { className: "progress__track", role: "progressbar", "aria-label": label, "aria-valuemin": pct === null ? undefined : 0, "aria-valuemax": pct === null ? undefined : 100, "aria-valuenow": pct === null ? undefined : Math.round(pct * 100), "aria-valuetext": text, children: _jsx("div", { className: `progress__fill${pct === null ? " progress__fill--indeterminate" : ""}`, style: pct === null ? undefined : { width: `${pct * 100}%` } }) }), _jsxs("span", { className: "field__hint", children: [text, detail ? ` · ${detail}` : ""] })] }));
}
