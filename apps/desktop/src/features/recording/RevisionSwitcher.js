import { jsxs as _jsxs, jsx as _jsx } from "react/jsx-runtime";
/**
 * Transcript revision picker (§4.5 "display original and edited transcript
 * revisions distinctly", §8.6).
 */
import { SelectField } from "../../components/Field";
import { StatusBadge } from "../../components/StatusBadge";
/** Machine output is the immutable original; human revisions are user edits. */
export function revisionLabel(revision) {
    const base = revision.kind === "machine"
        ? "Original (machine)"
        : `Edited by you · ${formatCreatedAt(revision.created_at)}`;
    return revision.superseded ? `${base} · superseded` : base;
}
function formatCreatedAt(iso) {
    const at = new Date(iso);
    return Number.isNaN(at.getTime()) ? "date unknown" : at.toISOString().slice(0, 16).replace("T", " ");
}
export function RevisionSwitcher({ revisions, selectedRevisionId, headRevisionId, onSelect, }) {
    if (revisions.length === 0)
        return null;
    const selected = revisions.find((r) => r.revision_id === selectedRevisionId) ?? null;
    const isHead = selected !== null && selected.revision_id === headRevisionId;
    return (_jsxs("div", { className: "revision-switcher", children: [_jsx(SelectField, { label: "Transcript revision", value: selectedRevisionId ?? "", onChange: (event) => onSelect(event.target.value), hint: selected
                    ? `${selected.engine_id ?? "engine unknown"} · ${selected.model_id ?? "model unknown"} · ${selected.segment_count} segments`
                    : undefined, children: revisions.map((revision) => (_jsxs("option", { value: revision.revision_id, children: [revisionLabel(revision), revision.revision_id === headRevisionId ? " · current" : ""] }, revision.revision_id))) }), selected && !isHead ? (_jsx(StatusBadge, { tone: "neutral", glyph: "\u25A3", label: "Read-only revision", description: "Older revisions are kept exactly as they were; edit the current revision instead." })) : null] }));
}
