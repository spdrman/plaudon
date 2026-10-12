import type { PlaudImportOutcome, PlaudItemSummary, PlaudListResponse } from "../../ipc/contract";
import { Button } from "../../components/Button";
import { EmptyState } from "../../components/EmptyState";
import { StatusBadge } from "../../components/StatusBadge";
import { ImportButton } from "../import/ImportButton";
import { formatDuration } from "./plaudPresentation";

export interface RecordingsListProps {
  items: PlaudItemSummary[];
  listing: Pick<PlaudListResponse, "completeness" | "empty_account_advisory" | "next_cursor">;
  selected: ReadonlySet<string>;
  outcomes: ReadonlyMap<string, PlaudImportOutcome>;
  busy: boolean;
  onToggle: (externalId: string) => void;
  onToggleAll: () => void;
  onImport: () => void;
  onLoadMore: () => void;
}

/** The cloud recordings, with selection and the import action (§4.2.4–5). */
export function RecordingsList({
  items,
  listing,
  selected,
  outcomes,
  busy,
  onToggle,
  onToggleAll,
  onImport,
  onLoadMore,
}: RecordingsListProps) {
  if (items.length === 0) {
    // §7.5: an empty account is advice, not an error.
    return (
      <EmptyState
        title="No recordings found"
        body={listing.empty_account_advisory ?? ""}
        actions={<ImportButton />}
      />
    );
  }

  const selectable = items.filter((item) => item.imported_recording_id === null);
  const allSelected = selectable.length > 0 && selectable.every((i) => selected.has(i.external_id));

  return (
    <section className="recordings" aria-labelledby="recordings-heading">
      <div className="recordings__header">
        <h2 id="recordings-heading" className="recordings__title">
          PLAUD recordings
        </h2>
        {listing.completeness === "possibly_incomplete" ? (
          <StatusBadge tone="warning" glyph="!" label="May be incomplete" />
        ) : (
          <StatusBadge tone="success" glyph="✓" label="Complete" />
        )}
      </div>

      <label className="recordings__select-all">
        <input
          type="checkbox"
          checked={allSelected}
          disabled={selectable.length === 0 || busy}
          onChange={onToggleAll}
        />
        Select all
      </label>

      <ul className="recordings__list">
        {items.map((item) => {
          const outcome = outcomes.get(item.external_id);
          const inLibrary = item.imported_recording_id !== null;
          const duration = formatDuration(item.duration_ms);
          return (
            <li key={item.external_id} className="recordings__row">
              <label>
                <input
                  type="checkbox"
                  checked={selected.has(item.external_id)}
                  disabled={inLibrary || busy}
                  onChange={() => onToggle(item.external_id)}
                />
                <span className="recordings__name">{item.display_name}</span>
              </label>
              {duration ? <span className="recordings__meta">{duration}</span> : null}
              {inLibrary ? <StatusBadge tone="success" glyph="✓" label="In library" /> : null}
              {outcome?.outcome === "imported" ? (
                <StatusBadge tone="success" glyph="✓" label="Imported" />
              ) : null}
              {outcome?.outcome === "already_imported" ? (
                <StatusBadge tone="neutral" glyph="=" label="Already in library" />
              ) : null}
              {outcome?.outcome === "failed" ? (
                <p className="recordings__failure" role="alert">
                  <StatusBadge tone="danger" glyph="✕" label="Failed" />
                  {outcome.error.message}
                </p>
              ) : null}
            </li>
          );
        })}
      </ul>

      <div className="recordings__actions">
        <Button variant="primary" disabled={busy || selected.size === 0} onClick={onImport}>
          Import selected ({selected.size})
        </Button>
        {listing.next_cursor ? (
          <Button disabled={busy} onClick={onLoadMore}>
            Load more recordings
          </Button>
        ) : null}
      </div>
    </section>
  );
}
