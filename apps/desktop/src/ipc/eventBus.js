import { EVENT_KINDS } from "./contract";
import { getTransport } from "./client";
/**
 * Subscribes to the typed event stream and detects dropped events.
 *
 * Events are hints only: a gap, an out-of-order delivery or a backend restart
 * triggers `onResync` so the UI refetches committed state instead of rendering
 * derived or stale data.
 */
export class EventBus {
    #transport;
    #listeners = new Set();
    #unlisten = [];
    #lastSeq = 0;
    #started = false;
    constructor(transport = getTransport()) {
        this.#transport = transport;
    }
    get lastSeq() {
        return this.#lastSeq;
    }
    async start(kinds = EVENT_KINDS) {
        if (this.#started)
            return;
        this.#started = true;
        try {
            for (const kind of kinds) {
                const off = await this.#transport.listen(kind, (event) => this.#ingest(event));
                this.#unlisten.push(off);
            }
        }
        catch (error) {
            this.#started = false;
            this.#emitResync({ kind: "transport_error", expectedSeq: this.#lastSeq + 1, receivedSeq: null });
            throw error;
        }
        this.#emitResync({ kind: "subscribed", expectedSeq: this.#lastSeq + 1, receivedSeq: null });
    }
    stop() {
        for (const off of this.#unlisten)
            off();
        this.#unlisten = [];
        this.#started = false;
    }
    subscribe(listener) {
        this.#listeners.add(listener);
        return () => this.#listeners.delete(listener);
    }
    #ingest(event) {
        const expected = this.#lastSeq + 1;
        if (event.seq <= this.#lastSeq) {
            // Duplicate or replayed delivery: ignore, state is already at least this new.
            return;
        }
        if (event.seq !== expected) {
            this.#lastSeq = event.seq;
            this.#emitResync({ kind: "sequence_gap", expectedSeq: expected, receivedSeq: event.seq });
            return;
        }
        this.#lastSeq = event.seq;
        for (const listener of [...this.#listeners])
            listener.onEvent(event);
    }
    #emitResync(reason) {
        for (const listener of [...this.#listeners])
            listener.onResync(reason);
    }
}
