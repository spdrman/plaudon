import { jsx as _jsx, jsxs as _jsxs } from "react/jsx-runtime";
/**
 * Duplicate resolution for manual import (§7.4).
 *
 * The match is by content hash, which the dialog says out loud: a renamed copy
 * still matches and two different recordings with the same file name do not.
 * Every file needs an explicit decision — nothing is imported or skipped for
 * the user.
 */
import { Dialog } from "../../components/Dialog";
import { Button } from "../../components/Button";
export function DuplicateDialog({ open, duplicates, pending = false, onChoose, onDecideLater, }) {
    return (_jsx(Dialog, { open: open && duplicates.length > 0, title: duplicates.length === 1
            ? "This audio is already in your library"
            : `${duplicates.length} files are already in your library`, description: _jsx("p", { className: "import-duplicate__explainer", children: "Matched by content hash (SHA-256) of the audio itself, not by file name. A renamed copy of the same recording still matches, and a different recording with the same name does not." }), onClose: onDecideLater, footer: _jsx(Button, { onClick: onDecideLater, children: "Decide later" }), children: _jsx("ul", { className: "import-duplicate__list", children: duplicates.map((duplicate) => (_jsxs("li", { className: "import-duplicate__item", children: [_jsx("p", { className: "import-duplicate__path", children: duplicate.path }), _jsxs("p", { className: "import-duplicate__match", children: ["Matches ", _jsx("strong", { children: duplicate.existing_title }), " \u00B7 content hash", " ", _jsxs("span", { className: "import-duplicate__hash", children: [duplicate.sha256.slice(0, 12), "\u2026"] })] }), _jsxs("div", { className: "import-duplicate__choices", children: [_jsx(Button, { variant: "primary", disabled: pending, onClick: () => onChoose(duplicate.path, "import_anyway"), children: "Import anyway as a separate recording" }), _jsx(Button, { disabled: pending, onClick: () => onChoose(duplicate.path, "open_existing"), children: "Open existing recording" }), _jsx(Button, { disabled: pending, onClick: () => onChoose(duplicate.path, "skip"), children: "Skip" })] })] }, duplicate.path))) }) }));
}
