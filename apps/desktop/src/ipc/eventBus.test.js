import { describe, expect, it, vi } from "vitest";
import { EventBus } from "./eventBus";
function harness() {
    const handlers = new Map();
    const transport = {
        invoke: async () => ({}),
        listen: async (kind, handler) => {
            handlers.set(kind, handler);
            return () => handlers.delete(kind);
        },
    };
    const emit = (seq, kind = "job.updated") => {
        handlers.get(kind)?.({
            seq,
            kind,
            occurred_at: "2026-01-01T00:00:00Z",
            job_id: null,
            recording_id: null,
            payload: {},
        });
    };
    return { transport, emit, handlers };
}
describe("EventBus", () => {
    it("delivers in-order events and asks for a resync on subscribe", async () => {
        const { transport, emit } = harness();
        const bus = new EventBus(transport);
        const onEvent = vi.fn();
        const onResync = vi.fn();
        bus.subscribe({ onEvent, onResync });
        await bus.start(["job.updated"]);
        expect(onResync).toHaveBeenCalledWith(expect.objectContaining({ kind: "subscribed", expectedSeq: 1 }));
        emit(1);
        emit(2);
        expect(onEvent).toHaveBeenCalledTimes(2);
        expect(bus.lastSeq).toBe(2);
    });
    it("requests a refetch instead of delivering a gapped event", async () => {
        const { transport, emit } = harness();
        const bus = new EventBus(transport);
        const onEvent = vi.fn();
        const onResync = vi.fn();
        bus.subscribe({ onEvent, onResync });
        await bus.start(["job.updated"]);
        onResync.mockClear();
        emit(1);
        emit(5);
        expect(onEvent).toHaveBeenCalledTimes(1);
        expect(onResync).toHaveBeenCalledWith({
            kind: "sequence_gap",
            expectedSeq: 2,
            receivedSeq: 5,
        });
        // Recovery continues from the observed sequence, no permanent desync.
        emit(6);
        expect(onEvent).toHaveBeenCalledTimes(2);
    });
    it("ignores duplicate and replayed deliveries", async () => {
        const { transport, emit } = harness();
        const bus = new EventBus(transport);
        const onEvent = vi.fn();
        const onResync = vi.fn();
        bus.subscribe({ onEvent, onResync });
        await bus.start(["job.updated"]);
        emit(1);
        emit(1);
        emit(1);
        expect(onEvent).toHaveBeenCalledTimes(1);
    });
    it("is monotonic for arbitrary interleavings (property)", async () => {
        const { transport, emit } = harness();
        const bus = new EventBus(transport);
        const seen = [];
        bus.subscribe({ onEvent: (e) => seen.push(e.seq), onResync: () => undefined });
        await bus.start(["job.updated"]);
        const sequences = [1, 2, 2, 3, 7, 4, 8, 9, 9, 10];
        for (const seq of sequences)
            emit(seq);
        for (let i = 1; i < seen.length; i += 1) {
            expect(seen[i]).toBeGreaterThan(seen[i - 1]);
        }
        expect(bus.lastSeq).toBe(Math.max(...sequences));
    });
    it("stops listening when stopped", async () => {
        const { transport, handlers } = harness();
        const bus = new EventBus(transport);
        await bus.start(["job.updated", "library.changed"]);
        expect(handlers.size).toBe(2);
        bus.stop();
        expect(handlers.size).toBe(0);
    });
});
