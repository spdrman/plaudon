import { ERROR_ACTIONS } from "./contract";
export class CommandError extends Error {
    envelope;
    constructor(envelope) {
        super(envelope.message);
        this.name = "CommandError";
        this.envelope = envelope;
    }
    get code() {
        return this.envelope.code;
    }
    get retryable() {
        return this.envelope.retryable;
    }
}
const UNKNOWN_ERROR_CODE = "INTERNAL_ERROR";
/** Normalizes anything thrown by the transport into a §15.4 envelope. */
export function toEnvelope(error) {
    if (error instanceof CommandError)
        return error.envelope;
    if (isEnvelope(error))
        return error;
    const message = error instanceof Error ? error.message : typeof error === "string" ? error : "Unexpected error.";
    return {
        code: UNKNOWN_ERROR_CODE,
        message,
        retryable: false,
        action: null,
        diagnostic_id: "",
    };
}
export function isEnvelope(value) {
    if (typeof value !== "object" || value === null)
        return false;
    const v = value;
    return (typeof v.code === "string" &&
        typeof v.message === "string" &&
        typeof v.retryable === "boolean" &&
        (v.action === null ||
            v.action === undefined ||
            (typeof v.action === "string" && ERROR_ACTIONS.includes(v.action))) &&
        typeof v.diagnostic_id === "string");
}
let transport = null;
export function setTransport(next) {
    transport = next;
}
export function getTransport() {
    if (!transport) {
        throw new Error("IPC transport is not configured; call setTransport() during bootstrap.");
    }
    return transport;
}
export async function invokeCommand(command, args) {
    try {
        const result = await getTransport().invoke(command, args);
        return result;
    }
    catch (error) {
        throw new CommandError(toEnvelope(error));
    }
}
/** Production transport backed by Tauri 2 IPC. */
export function createTauriTransport(api) {
    return {
        invoke: (command, args) => api.invoke(command, { request: args }),
        listen: (kind, handler) => api.listen(kind, (event) => {
            handler(event.payload);
        }),
    };
}
