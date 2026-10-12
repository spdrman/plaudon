import { jsx as _jsx, jsxs as _jsxs } from "react/jsx-runtime";
import { Button } from "../../components/Button";
import { EmptyState } from "../../components/EmptyState";
import { StatusBadge } from "../../components/StatusBadge";
import { ImportButton } from "../import/ImportButton";
import { formatDuration } from "./plaudPresentation";
/** The cloud recordings, with selection and the import action (§4.2.4–5). */
export function RecordingsList({ items, listing, selected, outcomes, busy, onToggle, onToggleAll, onImport, onLoadMore, }) {
    if (items.length === 0) {
        // §7.5: an empty account is advice, not an error.
        return (_jsx(EmptyState, { title: "No recordings found", body: listing.empty_account_advisory ?? "", actions: _jsx(ImportButton, {}) }));
    }
    const selectable = items.filter((item) => item.imported_recording_id === null);
    const allSelected = selectable.length > 0 && selectable.every((i) => selected.has(i.external_id));
    return (_jsxs("section", { className: "recordings", "aria-labelledby": "recordings-heading", children: [_jsxs("div", { className: "recordings__header", children: [_jsx("h2", { id: "recordings-heading", className: "recordings__title", children: "PLAUD recordings" }), listing.completeness === "possibly_incomplete" ? (_jsx(StatusBadge, { tone: "warning", glyph: "!", label: "May be incomplete" })) : (_jsx(StatusBadge, { tone: "success", glyph: "\u2713", label: "Complete" }))] }), _jsxs("label", { className: "recordings__select-all", children: [_jsx("input", { type: "checkbox", checked: allSelected, disabled: selectable.length === 0 || busy, onChange: onToggleAll }), "Select all"] }), _jsx("ul", { className: "recordings__list", children: items.map((item) => {
                    const outcome = outcomes.get(item.external_id);
                    const inLibrary = item.imported_recording_id !== null;
                    const duration = formatDuration(item.duration_ms);
                    return (_jsxs("li", { className: "recordings__row", children: [_jsxs("label", { children: [_jsx("input", { type: "checkbox", checked: selected.has(item.external_id), disabled: inLibrary || busy, onChange: () => onToggle(item.external_id) }), _jsx("span", { className: "recordings__name", children: item.display_name })] }), duration ? _jsx("span", { className: "recordings__meta", children: duration }) : null, inLibrary ? _jsx(StatusBadge, { tone: "success", glyph: "\u2713", label: "In library" }) : null, outcome?.outcome === "imported" ? (_jsx(StatusBadge, { tone: "success", glyph: "\u2713", label: "Imported" })) : null, outcome?.outcome === "already_imported" ? (_jsx(StatusBadge, { tone: "neutral", glyph: "=", label: "Already in library" })) : null, outcome?.outcome === "failed" ? (_jsxs("p", { className: "recordings__failure", role: "alert", children: [_jsx(StatusBadge, { tone: "danger", glyph: "\u2715", label: "Failed" }), outcome.error.message] })) : null] }, item.external_id));
                }) }), _jsxs("div", { className: "recordings__actions", children: [_jsxs(Button, { variant: "primary", disabled: busy || selected.size === 0, onClick: onImport, children: ["Import selected (", selected.size, ")"] }), listing.next_cursor ? (_jsx(Button, { disabled: busy, onClick: onLoadMore, children: "Load more recordings" })) : null] })] }));
}
