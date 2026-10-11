import { useCallback, useEffect, useRef, useState } from "react";
import type { CommandMap, CommandName } from "./contract";
import { CommandError, invokeCommand } from "./client";

export interface CommandQuery<T> {
  data: T | null;
  error: CommandError | null;
  loading: boolean;
  /** Refetch authoritative state; always safe to call after a dropped event. */
  refetch: () => Promise<void>;
}

/**
 * Fetches committed backend state. The UI never derives state from events;
 * an event only triggers `refetch` (§15.3).
 */
export function useCommandQuery<K extends CommandName>(
  command: K,
  args: CommandMap[K]["req"],
  options: { enabled?: boolean; deps?: unknown[] } = {},
): CommandQuery<CommandMap[K]["res"]> {
  const enabled = options.enabled ?? true;
  const [data, setData] = useState<CommandMap[K]["res"] | null>(null);
  const [error, setError] = useState<CommandError | null>(null);
  const [loading, setLoading] = useState(enabled);
  const argsRef = useRef(args);
  argsRef.current = args;
  const generation = useRef(0);
  const key = JSON.stringify(options.deps ?? args);

  const refetch = useCallback(async () => {
    if (!enabled) return;
    const run = ++generation.current;
    setLoading(true);
    try {
      const result = await invokeCommand(command, argsRef.current);
      if (run !== generation.current) return;
      setData(result);
      setError(null);
    } catch (caught) {
      if (run !== generation.current) return;
      setError(caught instanceof CommandError ? caught : new CommandError({
        code: "INTERNAL_ERROR",
        message: String(caught),
        retryable: false,
        action: null,
        diagnostic_id: "",
      }));
    } finally {
      if (run === generation.current) setLoading(false);
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

export interface CommandMutation<K extends CommandName> {
  run: (args: CommandMap[K]["req"]) => Promise<CommandMap[K]["res"]>;
  pending: boolean;
  error: CommandError | null;
  reset: () => void;
}

export function useCommandMutation<K extends CommandName>(command: K): CommandMutation<K> {
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<CommandError | null>(null);
  const run = useCallback(
    async (args: CommandMap[K]["req"]) => {
      setPending(true);
      setError(null);
      try {
        return await invokeCommand(command, args);
      } catch (caught) {
        const err =
          caught instanceof CommandError
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
      } finally {
        setPending(false);
      }
    },
    [command],
  );
  return { run, pending, error, reset: () => setError(null) };
}
