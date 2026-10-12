import { useEffect } from "react";
import type { EventKind, UiEvent } from "../ipc/contract";
import { useAppState } from "./AppState";

/**
 * Refetches authoritative state when one of `kinds` arrives, and
 * unconditionally when the bus reports a resync (dropped event / reconnect).
 */
export function useRefetchOnEvents(kinds: readonly EventKind[], refetch: () => void): void {
  const { onLibraryChanged } = useAppState();
  useEffect(() => {
    const wanted = new Set<EventKind>(kinds);
    return onLibraryChanged((event: UiEvent | null) => {
      if (event === null || wanted.has(event.kind)) refetch();
    });
    // `kinds` is a stable literal tuple at every call site.
  }, [onLibraryChanged, refetch, kinds.join("|")]);
}
