/**
 * Hooks for speaker management commands (spec §8.7, §17.6).
 */
import { useCommandQuery, useCommandMutation } from "../../ipc/useCommand";

export function useListSpeakers(recordingId: string, options: { enabled?: boolean } = {}) {
  return useCommandQuery(
    "list_speakers",
    { recording_id: recordingId },
    { enabled: options.enabled ?? true, deps: [recordingId] },
  );
}

export function useRenameSpeaker() {
  return useCommandMutation("rename_speaker");
}

export function useConfirmSpeaker() {
  return useCommandMutation("confirm_speaker");
}

export function useResetSpeaker() {
  return useCommandMutation("reset_speaker");
}
