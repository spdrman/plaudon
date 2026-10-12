/**
 * Pure transport model for the recording player (§4.5, §5.3).
 *
 * All clamping, stepping, speed and formatting math lives here so it can be
 * unit-tested without React or a media element. A null duration means the
 * media length is *unknown* (§8.6A) — it is never coerced to zero and the
 * caller must render an indeterminate transport instead.
 */
/** Selectable playback rates (§5.3 "adjustable playback speed"). */
export const PLAYBACK_SPEEDS = [0.5, 0.75, 1, 1.25, 1.5, 2];
export const DEFAULT_SPEED = 1;
/** §5.3 "jump ±10 seconds". */
export const SKIP_MS = 10_000;
/** Scrubber arrow-key step. */
export const ARROW_SEEK_MS = 5_000;
/** Scrubber PageUp/PageDown step. */
export const PAGE_SEEK_MS = 30_000;
/** Upper bound on waveform buckets; a long recording never decodes per-pixel. */
export const MAX_PEAK_BUCKETS = 2_000;
/** Buckets reduced per idle slice so the window never freezes. */
export const PEAK_CHUNK_BUCKETS = 200;
export function createPlayerState(durationMs = null) {
    return {
        positionMs: 0,
        durationMs: normalizeDuration(durationMs),
        playing: false,
        speed: DEFAULT_SPEED,
    };
}
/**
 * Accepts whatever the media element reports. `NaN`, `Infinity` and
 * non-positive values mean "unknown", which stays null.
 */
export function normalizeDuration(value) {
    if (value === null || value === undefined)
        return null;
    if (!Number.isFinite(value) || value <= 0)
        return null;
    return value;
}
/** Clamps at 0 and, when the length is known, at the duration. */
export function clampPosition(positionMs, durationMs) {
    if (!Number.isFinite(positionMs) || positionMs < 0)
        return 0;
    if (durationMs !== null && positionMs > durationMs)
        return durationMs;
    return positionMs;
}
/** Relative seek (±10 s, ±5 s, ±30 s) with the same boundary rules. */
export function seekBy(positionMs, deltaMs, durationMs) {
    return clampPosition(positionMs + deltaMs, durationMs);
}
function speedIndex(speed) {
    const index = PLAYBACK_SPEEDS.indexOf(speed);
    return index < 0 ? PLAYBACK_SPEEDS.indexOf(DEFAULT_SPEED) : index;
}
/** `.` / `,` stepping: stops at the ends rather than jumping 2× → 0.5×. */
export function stepSpeed(speed, direction) {
    const next = speedIndex(speed) + direction;
    const bounded = Math.min(PLAYBACK_SPEEDS.length - 1, Math.max(0, next));
    return PLAYBACK_SPEEDS[bounded];
}
/** Speed button: walks the list and wraps back to the slowest rate. */
export function cycleSpeed(speed) {
    const next = (speedIndex(speed) + 1) % PLAYBACK_SPEEDS.length;
    return PLAYBACK_SPEEDS[next];
}
/** `h:mm:ss`, dropping the hour component below one hour. */
export function formatClock(ms) {
    if (!Number.isFinite(ms) || ms <= 0)
        return "0:00";
    const totalSeconds = Math.floor(ms / 1000);
    const seconds = totalSeconds % 60;
    const minutes = Math.floor(totalSeconds / 60) % 60;
    const hours = Math.floor(totalSeconds / 3600);
    const ss = String(seconds).padStart(2, "0");
    if (hours > 0)
        return `${hours}:${String(minutes).padStart(2, "0")}:${ss}`;
    return `${minutes}:${ss}`;
}
/** Text shown next to the transport and used as the slider's `aria-valuetext`. */
export function formatPositionLabel(state) {
    const position = formatClock(state.positionMs);
    if (state.durationMs === null)
        return `${position} of an unknown duration`;
    return `${position} of ${formatClock(state.durationMs)}`;
}
/** Number of waveform buckets for a decoded sample count. */
export function peakBucketCount(sampleCount) {
    if (!Number.isFinite(sampleCount) || sampleCount <= 0)
        return 0;
    return Math.max(1, Math.min(MAX_PEAK_BUCKETS, Math.floor(sampleCount)));
}
/**
 * Reduces `[fromBucket, toBucket)` of a decoded channel into absolute peaks.
 * Written as an explicit range so the caller can spread the work across idle
 * slices instead of blocking the window on a long recording.
 */
export function computePeakRange(channel, out, fromBucket, toBucket) {
    const buckets = out.length;
    if (buckets === 0 || channel.length === 0)
        return;
    const samplesPerBucket = channel.length / buckets;
    const start = Math.max(0, fromBucket);
    const end = Math.min(buckets, toBucket);
    for (let bucket = start; bucket < end; bucket += 1) {
        const from = Math.floor(bucket * samplesPerBucket);
        const to = Math.min(channel.length, Math.max(from + 1, Math.floor((bucket + 1) * samplesPerBucket)));
        let peak = 0;
        for (let i = from; i < to; i += 1) {
            const value = Math.abs(channel[i] ?? 0);
            if (value > peak)
                peak = value;
        }
        out[bucket] = peak;
    }
}
export function playerReducer(state, action) {
    switch (action.type) {
        case "play":
            return state.playing ? state : { ...state, playing: true };
        case "pause":
            return state.playing ? { ...state, playing: false } : state;
        case "toggle":
            return { ...state, playing: !state.playing };
        case "ended":
            return {
                ...state,
                playing: false,
                positionMs: state.durationMs ?? state.positionMs,
            };
        case "seek-to":
            return { ...state, positionMs: clampPosition(action.positionMs, state.durationMs) };
        case "seek-by":
            return {
                ...state,
                positionMs: seekBy(state.positionMs, action.deltaMs, state.durationMs),
            };
        case "time-update":
            return { ...state, positionMs: clampPosition(action.positionMs, state.durationMs) };
        case "set-duration": {
            const durationMs = normalizeDuration(action.durationMs);
            if (durationMs === state.durationMs)
                return state;
            return { ...state, durationMs, positionMs: clampPosition(state.positionMs, durationMs) };
        }
        case "set-speed":
            return state.speed === action.speed ? state : { ...state, speed: action.speed };
        case "step-speed":
            return { ...state, speed: stepSpeed(state.speed, action.direction) };
        case "cycle-speed":
            return { ...state, speed: cycleSpeed(state.speed) };
        default: {
            const never = action;
            throw new Error(`unhandled player action ${JSON.stringify(never)}`);
        }
    }
}
