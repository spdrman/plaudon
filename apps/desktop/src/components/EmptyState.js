import { jsx as _jsx, jsxs as _jsxs } from "react/jsx-runtime";
export function EmptyState({ title, body, actions }) {
    return (_jsxs("div", { className: "empty-state", role: "group", "aria-label": title, children: [_jsx("h2", { className: "empty-state__title", children: title }), _jsx("p", { className: "empty-state__body", children: body }), actions ? _jsx("div", { className: "empty-state__actions", children: actions }) : null] }));
}
