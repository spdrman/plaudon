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
import type { DuplicateChoice, DuplicateOutcome } from "./useImport";

export interface DuplicateDialogProps {
  open: boolean;
  duplicates: readonly DuplicateOutcome[];
  pending?: boolean;
  onChoose: (path: string, choice: DuplicateChoice) => void;
  /** Leaves the files undecided and visible in the import results panel. */
  onDecideLater: () => void;
}

export function DuplicateDialog({
  open,
  duplicates,
  pending = false,
  onChoose,
  onDecideLater,
}: DuplicateDialogProps) {
  return (
    <Dialog
      open={open && duplicates.length > 0}
      title={
        duplicates.length === 1
          ? "This audio is already in your library"
          : `${duplicates.length} files are already in your library`
      }
      description={
        <p className="import-duplicate__explainer">
          Matched by content hash (SHA-256) of the audio itself, not by file name. A renamed copy of
          the same recording still matches, and a different recording with the same name does not.
        </p>
      }
      onClose={onDecideLater}
      footer={
        <Button onClick={onDecideLater}>
          Decide later
        </Button>
      }
    >
      <ul className="import-duplicate__list">
        {duplicates.map((duplicate) => (
          <li className="import-duplicate__item" key={duplicate.path}>
            <p className="import-duplicate__path">{duplicate.path}</p>
            <p className="import-duplicate__match">
              Matches <strong>{duplicate.existing_title}</strong> · content hash{" "}
              <span className="import-duplicate__hash">{duplicate.sha256.slice(0, 12)}…</span>
            </p>
            <div className="import-duplicate__choices">
              <Button
                variant="primary"
                disabled={pending}
                onClick={() => onChoose(duplicate.path, "import_anyway")}
              >
                Import anyway as a separate recording
              </Button>
              <Button disabled={pending} onClick={() => onChoose(duplicate.path, "open_existing")}>
                Open existing recording
              </Button>
              <Button disabled={pending} onClick={() => onChoose(duplicate.path, "skip")}>
                Skip
              </Button>
            </div>
          </li>
        ))}
      </ul>
    </Dialog>
  );
}
