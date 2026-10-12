/**
 * Transcript revision picker (§4.5 "display original and edited transcript
 * revisions distinctly", §8.6).
 */
import { SelectField } from "../../components/Field";
import { StatusBadge } from "../../components/StatusBadge";
import type { TranscriptRevisionRef } from "../../ipc/contract";

export interface RevisionSwitcherProps {
  revisions: readonly TranscriptRevisionRef[];
  selectedRevisionId: string | null;
  /** The recording's current revision; everything else opens read-only. */
  headRevisionId: string | null;
  onSelect: (revisionId: string) => void;
}

/** Machine output is the immutable original; human revisions are user edits. */
export function revisionLabel(revision: TranscriptRevisionRef): string {
  const base =
    revision.kind === "machine"
      ? "Original (machine)"
      : `Edited by you · ${formatCreatedAt(revision.created_at)}`;
  return revision.superseded ? `${base} · superseded` : base;
}

function formatCreatedAt(iso: string): string {
  const at = new Date(iso);
  return Number.isNaN(at.getTime()) ? "date unknown" : at.toISOString().slice(0, 16).replace("T", " ");
}

export function RevisionSwitcher({
  revisions,
  selectedRevisionId,
  headRevisionId,
  onSelect,
}: RevisionSwitcherProps) {
  if (revisions.length === 0) return null;
  const selected = revisions.find((r) => r.revision_id === selectedRevisionId) ?? null;
  const isHead = selected !== null && selected.revision_id === headRevisionId;

  return (
    <div className="revision-switcher">
      <SelectField
        label="Transcript revision"
        value={selectedRevisionId ?? ""}
        onChange={(event) => onSelect(event.target.value)}
        hint={
          selected
            ? `${selected.engine_id ?? "engine unknown"} · ${selected.model_id ?? "model unknown"} · ${selected.segment_count} segments`
            : undefined
        }
      >
        {revisions.map((revision) => (
          <option key={revision.revision_id} value={revision.revision_id}>
            {revisionLabel(revision)}
            {revision.revision_id === headRevisionId ? " · current" : ""}
          </option>
        ))}
      </SelectField>
      {selected && !isHead ? (
        <StatusBadge
          tone="neutral"
          glyph="▣"
          label="Read-only revision"
          description="Older revisions are kept exactly as they were; edit the current revision instead."
        />
      ) : null}
    </div>
  );
}
