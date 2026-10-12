/**
 * Speaker rename, confirm and reset controls (spec §8.7, §17.6).
 * 
 * Renaming is display-time only; stored transcript text is never mutated.
 * Visual distinction between inferred clusters and confirmed identities.
 */
import { useState } from "react";
import { Button } from "../../components/Button";
import type { Speaker } from "../../ipc/contract";

export interface SpeakerControlsProps {
  speaker: Speaker;
  onRename: (speakerId: string, name: string) => Promise<void>;
  onConfirm: (speakerId: string) => Promise<void>;
  onReset: (speakerId: string) => Promise<void>;
  disabled?: boolean;
}

export function SpeakerControls({
  speaker,
  onRename,
  onConfirm,
  onReset,
  disabled = false,
}: SpeakerControlsProps) {
  const [isEditing, setIsEditing] = useState(false);
  const [editValue, setEditValue] = useState(speaker.label);
  const [pending, setPending] = useState(false);

  const handleRename = async () => {
    if (editValue.trim() === speaker.label) {
      setIsEditing(false);
      return;
    }
    setPending(true);
    try {
      await onRename(speaker.speaker_id, editValue.trim());
      setIsEditing(false);
    } finally {
      setPending(false);
    }
  };

  const handleConfirm = async () => {
    setPending(true);
    try {
      await onConfirm(speaker.speaker_id);
    } finally {
      setPending(false);
    }
  };

  const handleReset = async () => {
    setPending(true);
    try {
      await onReset(speaker.speaker_id);
      setEditValue(speaker.original_label);
    } finally {
      setPending(false);
    }
  };

  return (
    <div
      className={speaker.confirmed ? "speaker-confirmed" : "speaker-inferred"}
      aria-label={`Speaker: ${speaker.label}${speaker.confirmed ? " (confirmed)" : " (inferred cluster)"}`}
    >
      {isEditing ? (
        <div role="group" aria-label="Rename speaker">
          <input
            type="text"
            value={editValue}
            onChange={(e) => setEditValue(e.target.value)}
            disabled={pending || disabled}
            aria-label="New speaker name"
            maxLength={120}
          />
          <Button onClick={handleRename} disabled={pending || disabled} aria-label="Save name">
            Save
          </Button>
          <Button
            onClick={() => {
              setIsEditing(false);
              setEditValue(speaker.label);
            }}
            disabled={pending || disabled}
            aria-label="Cancel rename"
          >
            Cancel
          </Button>
        </div>
      ) : (
        <div role="group" aria-label={`Speaker ${speaker.label} controls`}>
          <span className={speaker.confirmed ? "confirmed-label" : "inferred-label"}>
            {speaker.label}
            {!speaker.confirmed && <span className="qualifier"> (inferred cluster)</span>}
          </span>
          <Button
            onClick={() => setIsEditing(true)}
            disabled={pending || disabled}
            aria-label={`Rename ${speaker.label}`}
          >
            Rename
          </Button>
          {speaker.label !== speaker.original_label && (
            <>
              <Button
                onClick={handleConfirm}
                disabled={pending || disabled || speaker.confirmed}
                aria-label={`Confirm ${speaker.label} as identity`}
              >
                {speaker.confirmed ? "Confirmed" : "Confirm"}
              </Button>
              <Button
                onClick={handleReset}
                disabled={pending || disabled}
                aria-label={`Reset to ${speaker.original_label}`}
              >
                Reset
              </Button>
            </>
          )}
        </div>
      )}
    </div>
  );
}
