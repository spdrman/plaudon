import { useEffect } from "react";
import { useAppState } from "./AppState";
/**
 * Refetches authoritative state when one of `kinds` arrives, and
 * unconditionally when the bus reports a resync (dropped event / reconnect).
 */
export function useRefetchOnEvents(kinds, refetch) {
    const { onLibraryChanged } = useAppState();
    useEffect(() => {
        const wanted = new Set(kinds);
        return onLibraryChanged((event) => {
            if (event === null || wanted.has(event.kind))
                refetch();
        });
        // `kinds` is a stable literal tuple at every call site.
    }, [onLibraryChanged, refetch, kinds.join("|")]);
}
