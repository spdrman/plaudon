import { jsx as _jsx, jsxs as _jsxs } from "react/jsx-runtime";
export function StatusBadge({ tone, glyph, label, description }) {
    return (_jsxs("span", { className: `status-badge status-badge--${tone}`, "data-tone": tone, title: description ?? label, children: [_jsx("span", { className: "status-badge__glyph", "aria-hidden": "true", children: glyph }), _jsx("span", { children: label }), description ? _jsx("span", { className: "sr-only", children: description }) : null] }));
}
