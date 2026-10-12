/**
 * Speaker management panel for a recording (spec §8.7, §17.6).
 */
import { useCallback } from "react";
import { useListSpeakers, useRenameSpeaker, useConfirmSpeaker, useResetSpeaker } from "./useSpeakerCommands";
import { SpeakerControls } from "./SpeakerControls";
import { EmptyState } from "../../components/EmptyState";
import { CommandErrorNotice } from "../recording/errorNotice";
import "./speakers.css";

export interface SpeakerManagementPanelProps {
  recordingId: string;
}

export function SpeakerManagementPanel({ recordingId }: SpeakerManagementPanelProps) {
  const { data, error, loading, refetch } = useListSpeakers(recordingId);
  const rename = useRenameSpeaker();
  const confirm = useConfirmSpeaker();
  const reset = useResetSpeaker();

  const handleRename = useCallback(
    async (speakerId: string, name: string) => {
      await rename.run({ recording_id: recordingId, speaker_id: speakerId, name });
      await refetch();
    },
    [recordingId, rename, refetch],
  );

  const handleConfirm = useCallback(
    async (speakerId: string) => {
      await confirm.run({ recording_id: recordingId, speaker_id: speakerId });
      await refetch();
    },
    [recordingId, confirm, refetch],
  );

  const handleReset = useCallback(
    async (speakerId: string) => {
      await reset.run({ recording_id: recordingId, speaker_id: speakerId });
      await refetch();
    },
    [recordingId, reset, refetch],
  );

  if (error) {
    return (
      <div className="speaker-management-panel">
        <CommandErrorNotice error={error} onRetry={refetch} />
      </div>
    );
  }

  if (loading || !data) {
    return (
      <div className="speaker-management-panel">
        <p role="status">Loading speakers...</p>
      </div>
    );
  }

  if (data.speakers.length === 0) {
    return (
      <div className="speaker-management-panel">
        <EmptyState title="No speakers" body="No speakers found for this recording" />
      </div>
    );
  }

  return (
    <div className="speaker-management-panel">
      <h3>Speakers</h3>
      <ul className="speaker-list" aria-label="Recording speakers">
        {data.speakers.map((speaker) => (
          <li key={speaker.speaker_id} className="speaker-list-item">
            <SpeakerControls
              speaker={speaker}
              onRename={handleRename}
              onConfirm={handleConfirm}
              onReset={handleReset}
              disabled={rename.pending || confirm.pending || reset.pending}
            />
          </li>
        ))}
      </ul>
    </div>
  );
}
