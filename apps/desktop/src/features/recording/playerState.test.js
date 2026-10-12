import { describe, expect, it } from "vitest";
import { MAX_PEAK_BUCKETS, PLAYBACK_SPEEDS, SKIP_MS, clampPosition, computePeakRange, createPlayerState, cycleSpeed, formatClock, formatPositionLabel, normalizeDuration, peakBucketCount, playerReducer, seekBy, stepSpeed, } from "./playerState";
describe("duration handling", () => {
    it("keeps an unknown duration unknown instead of defaulting to zero", () => {
        expect(normalizeDuration(null)).toBeNull();
        expect(normalizeDuration(Number.NaN)).toBeNull();
        expect(normalizeDuration(Number.POSITIVE_INFINITY)).toBeNull();
        expect(normalizeDuration(0)).toBeNull();
        expect(normalizeDuration(12_000)).toBe(12_000);
        expect(createPlayerState(null).durationMs).toBeNull();
    });
});
describe("seek clamping", () => {
    it("clamps at zero and at the duration", () => {
        expect(clampPosition(-5_000, 60_000)).toBe(0);
        expect(clampPosition(75_000, 60_000)).toBe(60_000);
        expect(clampPosition(30_000, 60_000)).toBe(30_000);
    });
    it("clamps only at zero when the duration is unknown", () => {
        expect(clampPosition(-1, null)).toBe(0);
        expect(clampPosition(9_999_999, null)).toBe(9_999_999);
    });
    it("holds ±10 s at both boundaries", () => {
        expect(seekBy(3_000, -SKIP_MS, 60_000)).toBe(0);
        expect(seekBy(55_000, SKIP_MS, 60_000)).toBe(60_000);
        expect(seekBy(0, SKIP_MS, 60_000)).toBe(10_000);
        expect(seekBy(60_000, SKIP_MS, 60_000)).toBe(60_000);
    });
});
describe("speed", () => {
    it("steps through the documented rates and stops at the ends", () => {
        expect(PLAYBACK_SPEEDS).toEqual([0.5, 0.75, 1, 1.25, 1.5, 2]);
        expect(stepSpeed(1, 1)).toBe(1.25);
        expect(stepSpeed(1, -1)).toBe(0.75);
        expect(stepSpeed(2, 1)).toBe(2);
        expect(stepSpeed(0.5, -1)).toBe(0.5);
    });
    it("cycles back to the slowest rate from the fastest", () => {
        expect(cycleSpeed(1.5)).toBe(2);
        expect(cycleSpeed(2)).toBe(0.5);
    });
});
describe("position formatting", () => {
    it("formats h:mm:ss, dropping hours below one hour", () => {
        expect(formatClock(0)).toBe("0:00");
        expect(formatClock(42_000)).toBe("0:42");
        expect(formatClock(725_000)).toBe("12:05");
        expect(formatClock(3_600_000)).toBe("1:00:00");
        expect(formatClock(3_725_000)).toBe("1:02:05");
        expect(formatClock(Number.NaN)).toBe("0:00");
    });
    it("never prints a fabricated total for an unknown duration", () => {
        const known = { ...createPlayerState(725_000), positionMs: 42_000 };
        expect(formatPositionLabel(known)).toBe("0:42 of 12:05");
        const unknown = { ...createPlayerState(null), positionMs: 42_000 };
        expect(formatPositionLabel(unknown)).toBe("0:42 of an unknown duration");
        expect(formatPositionLabel(unknown)).not.toContain("0:00");
    });
});
describe("playerReducer", () => {
    it("toggles playback and reports ending at the known duration", () => {
        const base = createPlayerState(60_000);
        const playing = playerReducer(base, { type: "toggle" });
        expect(playing.playing).toBe(true);
        const ended = playerReducer(playing, { type: "ended" });
        expect(ended).toMatchObject({ playing: false, positionMs: 60_000 });
    });
    it("clamps the position when a duration arrives or disappears", () => {
        const state = playerReducer(createPlayerState(null), {
            type: "time-update",
            positionMs: 90_000,
        });
        expect(state.positionMs).toBe(90_000);
        const shortened = playerReducer(state, { type: "set-duration", durationMs: 60_000 });
        expect(shortened.positionMs).toBe(60_000);
        const unknown = playerReducer(shortened, { type: "set-duration", durationMs: Number.NaN });
        expect(unknown.durationMs).toBeNull();
    });
    it("steps speed through the action", () => {
        const state = playerReducer(createPlayerState(1_000), { type: "step-speed", direction: 1 });
        expect(state.speed).toBe(1.25);
    });
});
describe("waveform peaks", () => {
    it("never computes more than the bucket budget", () => {
        expect(peakBucketCount(0)).toBe(0);
        expect(peakBucketCount(500)).toBe(500);
        expect(peakBucketCount(44_100 * 3_600)).toBe(MAX_PEAK_BUCKETS);
    });
    it("reduces only the requested slice so decoding can be chunked", () => {
        const channel = Float32Array.from({ length: 100 }, (_, i) => (i % 10 === 0 ? -0.8 : 0.1));
        const out = new Float32Array(10);
        computePeakRange(channel, out, 0, 4);
        expect(out[0]).toBeCloseTo(0.8);
        expect(out[3]).toBeCloseTo(0.8);
        // Untouched slice stays zero until its own chunk runs.
        expect(out[4]).toBe(0);
        computePeakRange(channel, out, 4, 10);
        expect(out[9]).toBeCloseTo(0.8);
    });
});
