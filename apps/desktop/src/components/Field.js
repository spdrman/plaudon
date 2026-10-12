import { jsx as _jsx, jsxs as _jsxs } from "react/jsx-runtime";
import { useId } from "react";
function FieldShell({ label, hint, error, children }) {
    const controlId = useId();
    const hintId = `${controlId}-hint`;
    const errorId = `${controlId}-error`;
    const describedBy = [hint ? hintId : null, error ? errorId : null].filter(Boolean).join(" ");
    return (_jsxs("div", { className: "field", children: [_jsx("label", { className: "field__label", htmlFor: controlId, children: label }), children({ controlId, describedBy: describedBy || undefined }), hint ? (_jsx("span", { className: "field__hint", id: hintId, children: hint })) : null, error ? (_jsxs("span", { className: "field__error", id: errorId, children: [_jsx("span", { "aria-hidden": "true", children: "! " }), error] })) : null] }));
}
export function TextField({ label, hint, error, ...rest }) {
    return (_jsx(FieldShell, { label: label, hint: hint, error: error, children: ({ controlId, describedBy }) => (_jsx("input", { ...rest, id: controlId, className: "field__control", "aria-describedby": describedBy, "aria-invalid": error ? true : undefined })) }));
}
export function SelectField({ label, hint, error, children, ...rest }) {
    return (_jsx(FieldShell, { label: label, hint: hint, error: error, children: ({ controlId, describedBy }) => (_jsx("select", { ...rest, id: controlId, className: "field__control", "aria-describedby": describedBy, children: children })) }));
}
