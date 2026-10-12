import type { CommandMap, CommandName, ErrorEnvelope, EventKind, UiEvent } from "./contract";
import { ERROR_ACTIONS } from "./contract";

export type Unlisten = () => void;

/**
 * The only way the UI talks to Rust. Tests and the e2e harness swap the
 * transport; production uses the Tauri IPC bridge.
 */
export interface Transport {
  invoke(command: CommandName, args: unknown): Promise<unknown>;
  listen(kind: EventKind, handler: (event: UiEvent) => void): Promise<Unlisten>;
}

export class CommandError extends Error {
  readonly envelope: ErrorEnvelope;

  constructor(envelope: ErrorEnvelope) {
    super(envelope.message);
    this.name = "CommandError";
    this.envelope = envelope;
  }

  get code(): string {
    return this.envelope.code;
  }

  get retryable(): boolean {
    return this.envelope.retryable;
  }
}

const UNKNOWN_ERROR_CODE = "INTERNAL_ERROR";

/** Normalizes anything thrown by the transport into a §15.4 envelope. */
export function toEnvelope(error: unknown): ErrorEnvelope {
  if (error instanceof CommandError) return error.envelope;
  if (isEnvelope(error)) return error;
  const message =
    error instanceof Error ? error.message : typeof error === "string" ? error : "Unexpected error.";
  return {
    code: UNKNOWN_ERROR_CODE,
    message,
    retryable: false,
    action: null,
    diagnostic_id: "",
  };
}

export function isEnvelope(value: unknown): value is ErrorEnvelope {
  if (typeof value !== "object" || value === null) return false;
  const v = value as Record<string, unknown>;
  return (
    typeof v.code === "string" &&
    typeof v.message === "string" &&
    typeof v.retryable === "boolean" &&
    (v.action === null ||
      v.action === undefined ||
      (typeof v.action === "string" && (ERROR_ACTIONS as readonly string[]).includes(v.action))) &&
    typeof v.diagnostic_id === "string"
  );
}

let transport: Transport | null = null;

export function setTransport(next: Transport | null): void {
  transport = next;
}

export function getTransport(): Transport {
  if (!transport) {
    throw new Error("IPC transport is not configured; call setTransport() during bootstrap.");
  }
  return transport;
}

export async function invokeCommand<K extends CommandName>(
  command: K,
  args: CommandMap[K]["req"],
): Promise<CommandMap[K]["res"]> {
  try {
    const result = await getTransport().invoke(command, args);
    return result as CommandMap[K]["res"];
  } catch (error) {
    throw new CommandError(toEnvelope(error));
  }
}

/** Production transport backed by Tauri 2 IPC. */
export function createTauriTransport(api: {
  invoke: (cmd: string, args?: Record<string, unknown>) => Promise<unknown>;
  listen: (event: string, handler: (e: { payload: unknown }) => void) => Promise<Unlisten>;
}): Transport {
  return {
    invoke: (command, args) => api.invoke(command, { request: args }),
    listen: (kind, handler) =>
      api.listen(kind, (event) => {
        handler(event.payload as UiEvent);
      }),
  };
}
