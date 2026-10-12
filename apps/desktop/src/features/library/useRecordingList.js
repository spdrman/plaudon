/**
 * Cursor-paged `list_recordings` state for the Library and Inbox screens.
 *
 * Events are hints only (§15.3): `useRefetchOnEvents` restarts the cursor
 * chain from page one instead of patching rows from an event payload, and a
 * dropped event (resync) does exactly the same thing.
 */
import { useCallback, useEffect, useMemo, useReducer, useRef, useState } from "react";
import { useCommandQuery } from "../../ipc/useCommand";
import { useRefetchOnEvents } from "../../app/useRefetchOnEvents";
import { appendPage, buildListRequest, libraryQueryReducer, querySignature, } from "./libraryQuery";
/** Committed-state hints that can change what the list should show. */
export const LIBRARY_EVENTS = [
    "recording.imported",
    "library.changed",
    "recording.source_updated",
    "transcript.completed",
];
const EMPTY_LOADED = { key: "", rows: [], total: 0, nextCursor: null };
export function useRecordingList(initial) {
    const [state, dispatch] = useReducer(libraryQueryReducer, initial);
    const [anchor, setAnchor] = useState(() => ({
        signature: querySignature(initial),
        cursor: null,
    }));
    const [generation, setGeneration] = useState(0);
    const [loaded, setLoaded] = useState(EMPTY_LOADED);
    const signature = querySignature(state);
    // A filter or sort change invalidates the cursor chain in the same render,
    // so we never issue a page request that mixes a new filter with an old cursor.
    const cursor = anchor.signature === signature ? anchor.cursor : null;
    const chainKey = `${signature}|${generation}`;
    const request = useMemo(() => buildListRequest(state, cursor), 
    // `signature` is the serialized filter+sort; `state` changes that do not
    // alter it cannot alter the request.
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [signature, cursor]);
    const { data, error, loading } = useCommandQuery("list_recordings", request, {
        deps: [chainKey, cursor],
    });
    const cursorRef = useRef(cursor);
    cursorRef.current = cursor;
    const chainKeyRef = useRef(chainKey);
    chainKeyRef.current = chainKey;
    const lastPageRef = useRef(null);
    useEffect(() => {
        if (!data || data === lastPageRef.current)
            return;
        lastPageRef.current = data;
        const key = chainKeyRef.current;
        const continues = cursorRef.current !== null;
        setLoaded((previous) => ({
            key,
            rows: continues && previous.key === key ? appendPage(previous.rows, data.items) : [...data.items],
            total: data.total,
            nextCursor: data.next_cursor,
        }));
    }, [data]);
    const reload = useCallback(() => {
        setAnchor((previous) => ({ ...previous, cursor: null }));
        setGeneration((value) => value + 1);
    }, []);
    useRefetchOnEvents(LIBRARY_EVENTS, reload);
    const loadMore = useCallback(() => {
        setLoaded((previous) => {
            if (previous.nextCursor === null)
                return previous;
            setAnchor({ signature: previous.key.split("|")[0] ?? "", cursor: previous.nextCursor });
            return previous;
        });
    }, []);
    const rows = loaded.key === chainKey ? loaded.rows : [];
    return {
        state,
        dispatch,
        rows,
        total: loaded.key === chainKey ? loaded.total : 0,
        hasMore: loaded.key === chainKey && loaded.nextCursor !== null,
        loading: loading && rows.length === 0,
        loadingMore: loading && cursor !== null,
        error,
        loadMore,
        reload,
    };
}
