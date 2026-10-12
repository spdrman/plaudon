import { EVENT_KINDS, type EventKind, type UiEvent } from "./contract";
import { getTransport, type Transport, type Unlisten } from "./client";

export interface EventBusListener {
  /** Called for every in-order event. */
  onEvent(event: UiEvent): void;
  /**
   * Called when the stream lost events (sequence gap) or when the bus
   * reconnects. Subscribers must refetch authoritative state (§15.3).
   */
  onResync(reason: ResyncReason): void;
}

export interface ResyncReason {
  kind: "sequence_gap" | "subscribed" | "transport_error";
  expectedSeq: number;
  receivedSeq: number | null;
}

/**
 * Subscribes to the typed event stream and detects dropped events.
 *
 * Events are hints only: a gap, an out-of-order delivery or a backend restart
 * triggers `onResync` so the UI refetches committed state instead of rendering
 * derived or stale data.
 */
export class EventBus {
  #transport: Transport;
  #listeners = new Set<EventBusListener>();
  #unlisten: Unlisten[] = [];
  #lastSeq = 0;
  #started = false;

  constructor(transport: Transport = getTransport()) {
    this.#transport = transport;
  }

  get lastSeq(): number {
    return this.#lastSeq;
  }

  async start(kinds: readonly EventKind[] = EVENT_KINDS): Promise<void> {
    if (this.#started) return;
    this.#started = true;
    try {
      for (const kind of kinds) {
        const off = await this.#transport.listen(kind, (event) => this.#ingest(event));
        this.#unlisten.push(off);
      }
    } catch (error) {
      this.#started = false;
      this.#emitResync({ kind: "transport_error", expectedSeq: this.#lastSeq + 1, receivedSeq: null });
      throw error;
    }
    this.#emitResync({ kind: "subscribed", expectedSeq: this.#lastSeq + 1, receivedSeq: null });
  }

  stop(): void {
    for (const off of this.#unlisten) off();
    this.#unlisten = [];
    this.#started = false;
  }

  subscribe(listener: EventBusListener): () => void {
    this.#listeners.add(listener);
    return () => this.#listeners.delete(listener);
  }

  #ingest(event: UiEvent): void {
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
    for (const listener of [...this.#listeners]) listener.onEvent(event);
  }

  #emitResync(reason: ResyncReason): void {
    for (const listener of [...this.#listeners]) listener.onResync(reason);
  }
}
