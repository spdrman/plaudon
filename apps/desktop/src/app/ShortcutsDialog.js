import { jsx as _jsx, jsxs as _jsxs } from "react/jsx-runtime";
import { Dialog } from "../components/Dialog";
import { Button } from "../components/Button";
import { SHORTCUTS, formatShortcut, isMacPlatform } from "./shortcuts";
export function ShortcutsDialog({ open, onClose }) {
    const mac = isMacPlatform(typeof navigator === "undefined" ? undefined : navigator);
    const groups = [...new Set(SHORTCUTS.map((s) => s.group))];
    return (_jsx(Dialog, { open: open, title: "Keyboard shortcuts", onClose: onClose, footer: _jsx(Button, { variant: "primary", onClick: onClose, children: "Close" }), children: groups.map((group) => (_jsxs("section", { "aria-label": group, children: [_jsx("h3", { children: group }), _jsx("dl", { className: "shortcut-list", children: SHORTCUTS.filter((s) => s.group === group).map((shortcut) => (_jsxs("div", { className: "shortcut-list__row", children: [_jsx("dt", { children: _jsx("kbd", { children: formatShortcut(shortcut.keys, mac) }) }), _jsx("dd", { children: shortcut.description })] }, `${group}-${shortcut.keys}-${shortcut.description}`))) })] }, group))) }));
}
