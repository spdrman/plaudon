import { useCallback, useEffect, useRef, useState } from "react";
import { CommandError, invokeCommand } from "./client";
/**
 * Fetches committed backend state. The UI never derives state from events;
 * an event only triggers `refetch` (§15.3).
 */
export function useCommandQuery(command, args, options = {}) {
    const enabled = options.enabled ?? true;
    const [data, setData] = useState(null);
    const [error, setError] = useState(null);
    const [loading, setLoading] = useState(enabled);
    const argsRef = useRef(args);
    argsRef.current = args;
    const generation = useRef(0);
    const key = JSON.stringify(options.deps ?? args);
    const refetch = useCallback(async () => {
        if (!enabled)
            return;
        const run = ++generation.current;
        setLoading(true);
        try {
            const result = await invokeCommand(command, argsRef.current);
            if (run !== generation.current)
                return;
            setData(result);
            setError(null);
        }
        catch (caught) {
            if (run !== generation.current)
                return;
            setError(caught instanceof CommandError ? caught : new CommandError({
                code: "INTERNAL_ERROR",
                message: String(caught),
                retryable: false,
                action: null,
                diagnostic_id: "",
            }));
        }
        finally {
            if (run === generation.current)
                setLoading(false);
        }
    }, [command, enabled]);
    useEffect(() => {
        if (!enabled) {
            setLoading(false);
            return;
        }
        void refetch();
        // `key` captures the caller-declared dependency set.
    }, [refetch, enabled, key]);
    return { data, error, loading, refetch };
}
export function useCommandMutation(command) {
    const [pending, setPending] = useState(false);
    const [error, setError] = useState(null);
    const run = useCallback(async (args) => {
        setPending(true);
        setError(null);
        try {
            return await invokeCommand(command, args);
        }
        catch (caught) {
            const err = caught instanceof CommandError
                ? caught
                : new CommandError({
                    code: "INTERNAL_ERROR",
                    message: String(caught),
                    retryable: false,
                    action: null,
                    diagnostic_id: "",
                });
            setError(err);
            throw err;
        }
        finally {
            setPending(false);
        }
    }, [command]);
    return { run, pending, error, reset: () => setError(null) };
}
