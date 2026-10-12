import { jsx as _jsx, jsxs as _jsxs } from "react/jsx-runtime";
import { useCallback, useEffect, useId, useRef } from "react";
const FOCUSABLE = 'a[href], button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])';
/**
 * Modal dialog with focus trap and focus restoration (WCAG 2.4.3 / 2.1.2).
 */
export function Dialog({ open, title, description, children, footer, onClose, destructive = false, }) {
    const dialogRef = useRef(null);
    const restoreRef = useRef(null);
    const titleId = useId();
    const descId = useId();
    useEffect(() => {
        if (!open)
            return;
        restoreRef.current = document.activeElement;
        const node = dialogRef.current;
        const first = node?.querySelector(FOCUSABLE) ?? node;
        first?.focus();
        return () => {
            restoreRef.current?.focus?.();
        };
    }, [open]);
    const onKeyDown = useCallback((event) => {
        if (event.key === "Escape") {
            event.stopPropagation();
            onClose();
            return;
        }
        if (event.key !== "Tab")
            return;
        const node = dialogRef.current;
        if (!node)
            return;
        const items = Array.from(node.querySelectorAll(FOCUSABLE)).filter((el) => el.offsetParent !== null || el === document.activeElement);
        if (items.length === 0)
            return;
        const first = items[0];
        const last = items[items.length - 1];
        const active = document.activeElement;
        if (event.shiftKey && (active === first || !node.contains(active))) {
            event.preventDefault();
            last.focus();
        }
        else if (!event.shiftKey && active === last) {
            event.preventDefault();
            first.focus();
        }
    }, [onClose]);
    if (!open)
        return null;
    return (_jsx("div", { className: "dialog__backdrop", "data-testid": "dialog-backdrop", children: _jsxs("div", { className: "dialog", role: destructive ? "alertdialog" : "dialog", "aria-modal": "true", "aria-labelledby": titleId, "aria-describedby": description ? descId : undefined, ref: dialogRef, onKeyDown: onKeyDown, tabIndex: -1, children: [_jsx("h2", { className: "dialog__title", id: titleId, children: title }), description ? _jsx("div", { id: descId, children: description }) : null, children, footer ? _jsx("div", { className: "dialog__actions", children: footer }) : null] }) }));
}
