/**
 * Destructive deletion dialog (J1.4) that shows exactly what will be removed,
 * passes the confirmation token, and handles the round-trip disclosure flow.
 */
import { useEffect, useState } from "react";
import { Button } from "../../components/Button";
import { Dialog } from "../../components/Dialog";
import { useCommandMutation } from "../../ipc/useCommand";
import type { DeletionDisclosureDto } from "../../ipc/contract";
import { CommandErrorNotice } from "./errorNotice";

export function DeleteRecordingDialog({
  recordingId,
  open,
  onClose,
  onDeleted,
}: {
  recordingId: string;
  open: boolean;
  onClose: () => void;
  onDeleted: () => void;
}) {
  const [disclosure, setDisclosure] = useState<DeletionDisclosureDto | null>(null);
  const getDisclosure = useCommandMutation("get_deletion_disclosure");
  const deleteRecording = useCommandMutation("delete_recording");

  useEffect(() => {
    if (open && !disclosure) {
      getDisclosure
        .mutateAsync({ recording_id: recordingId })
        .then(setDisclosure)
        .catch(() => {
          // Error shown by CommandErrorNotice
        });
    }
  }, [open, recordingId, disclosure, getDisclosure]);

  useEffect(() => {
    if (!open) {
      setDisclosure(null);
    }
  }, [open]);

  const handleConfirmDelete = async () => {
    if (!disclosure) return;

    try {
      await deleteRecording.mutateAsync({
        recording_id: recordingId,
        confirmation_token: disclosure.confirmation_token,
      });
      onDeleted();
      onClose();
    } catch (err) {
      // Error shown by CommandErrorNotice
    }
  };

  return (
    <Dialog
      open={open}
      title={`Delete "${disclosure?.title ?? "recording"}"`}
      onClose={onClose}
      destructive
      footer={
        <>
          <Button variant="quiet" onClick={onClose}>
            Cancel
          </Button>
          <Button
            variant="danger"
            onClick={handleConfirmDelete}
            disabled={!disclosure || deleteRecording.isPending}
          >
            {deleteRecording.isPending ? "Deleting..." : "Delete permanently"}
          </Button>
        </>
      }
    >
      {disclosure ? (
        <div style={{ display: "flex", flexDirection: "column", gap: "1.5rem" }}>
          <p>
            This will permanently delete <strong>{disclosure.title}</strong> and cannot be undone.
          </p>

          {disclosure.has_annotation && (
            <p style={{ color: "var(--color-warning)" }}>
              ⚠ This recording has a note that will also be deleted.
            </p>
          )}

          <section>
            <h3>What will be removed</h3>
            <ul>
              {disclosure.categories.map((category, i) => (
                <li key={i}>
                  {category.label}: <strong>{category.count}</strong>
                </li>
              ))}
            </ul>
          </section>

          {disclosure.media.length > 0 && (
            <section>
              <h3>Media files</h3>
              <ul>
                {disclosure.media.map((item, i) => (
                  <li key={i}>
                    {item.quality_label} ({(item.byte_length / 1024 / 1024).toFixed(2)} MB)
                    {item.shared_with_other_recordings ? (
                      <span style={{ color: "var(--color-neutral)" }}>
                        {" "}
                        — kept, shared with another recording
                      </span>
                    ) : item.bytes_will_be_removed ? (
                      <span style={{ color: "var(--color-danger)" }}> — will be deleted</span>
                    ) : (
                      <span style={{ color: "var(--color-neutral)" }}> — will be kept</span>
                    )}
                  </li>
                ))}
              </ul>
            </section>
          )}

          <section>
            <p>
              <strong>Storage freed:</strong>{" "}
              {(disclosure.bytes_to_free / 1024 / 1024).toFixed(2)} MB
            </p>
          </section>

          {disclosure.limits.length > 0 && (
            <section>
              <h3>Limits</h3>
              <ul>
                {disclosure.limits.map((limit, i) => (
                  <li key={i}>{limit}</li>
                ))}
              </ul>
            </section>
          )}

          <CommandErrorNotice mutation={getDisclosure} />
          <CommandErrorNotice mutation={deleteRecording} />
        </div>
      ) : (
        <div>
          <p>Loading deletion disclosure...</p>
          <CommandErrorNotice mutation={getDisclosure} />
        </div>
      )}
    </Dialog>
  );
}
