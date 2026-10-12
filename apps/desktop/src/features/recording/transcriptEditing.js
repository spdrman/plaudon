import { formatClock } from "./playerState";
const EMPTY = new Map();
export function createEditState() {
    return { drafts: EMPTY, past: [], future: [] };
}
/**
 * Records `text` for `index`. Returning to the baseline text drops the draft
 * so the segment stops counting as dirty and is left out of `toEdits()`.
 *
 * `coalesce` folds the change into the previous history entry, so one editing
 * session is one undo step instead of one per keystroke.
 */
export function setSegmentText(state, index, baselineText, text, options = {}) {
    const current = state.drafts.get(index) ?? baselineText;
    if (current === text)
        return state;
    const drafts = new Map(state.drafts);
    if (text === baselineText)
        drafts.delete(index);
    else
        drafts.set(index, text);
    const coalesce = options.coalesce === true && state.past.length > 0;
    return { drafts, past: coalesce ? state.past : [...state.past, state.drafts], future: [] };
}
export function undo(state) {
    const previous = state.past[state.past.length - 1];
    if (previous === undefined)
        return state;
    return {
        drafts: previous,
        past: state.past.slice(0, -1),
        future: [...state.future, state.drafts],
    };
}
export function redo(state) {
    const next = state.future[state.future.length - 1];
    if (next === undefined)
        return state;
    return {
        drafts: next,
        past: [...state.past, state.drafts],
        future: state.future.slice(0, -1),
    };
}
/** Text to display/edit for a segment: the draft when dirty, else the stored text. */
export function textOf(state, segment) {
    return state.drafts.get(segment.index) ?? segment.text;
}
/** Payload for `save_transcript_revision`: only the segments the user changed. */
export function toEdits(state) {
    return [...state.drafts.entries()]
        .sort((a, b) => a[0] - b[0])
        .map(([index, text]) => ({ index, text }));
}
/** Cmd/Ctrl+Z, with Shift for redo (§5.3 "undo for transcript edits"). */
export function undoRedoIntent(event) {
    if (event.key.toLowerCase() !== "z")
        return null;
    if (!event.metaKey && !event.ctrlKey)
        return null;
    return event.shiftKey ? "redo" : "undo";
}
/** Case-insensitive literal search over the segments currently loaded. */
export function findMatches(segments, state, query) {
    const needle = query.trim().toLowerCase();
    if (needle.length === 0)
        return [];
    const matches = [];
    for (const segment of segments) {
        const haystack = textOf(state, segment).toLowerCase();
        let count = 0;
        let from = haystack.indexOf(needle);
        while (from !== -1) {
            count += 1;
            from = haystack.indexOf(needle, from + needle.length);
        }
        if (count > 0)
            matches.push({ index: segment.index, count });
    }
    return matches;
}
/** Splits text into alternating plain/matched runs for `<mark>` rendering. */
export function highlightRuns(text, query) {
    const needle = query.trim().toLowerCase();
    if (needle.length === 0)
        return [{ text, match: false }];
    const runs = [];
    const haystack = text.toLowerCase();
    let cursor = 0;
    let at = haystack.indexOf(needle);
    while (at !== -1) {
        if (at > cursor)
            runs.push({ text: text.slice(cursor, at), match: false });
        runs.push({ text: text.slice(at, at + needle.length), match: true });
        cursor = at + needle.length;
        at = haystack.indexOf(needle, cursor);
    }
    if (cursor < text.length)
        runs.push({ text: text.slice(cursor), match: false });
    return runs;
}
/* ------------------------------------------------------------------ copy */
/**
 * §5.3 "copy transcript passage": `[m:ss] text` per segment. Segments with no
 * timeline (`alignment_origin: 'none'`, `start_ms: null`) are copied as plain
 * text — we never invent a seek time (§8.6A).
 */
export function formatPassage(segments, state = createEditState()) {
    return segments
        .map((segment) => {
        const text = textOf(state, segment);
        if (segment.alignment_origin === "none" || segment.start_ms === null)
            return text;
        return `[${formatClock(segment.start_ms)}] ${text}`;
    })
        .join("\n");
}
