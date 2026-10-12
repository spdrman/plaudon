import { describe, expect, it } from "vitest";
import { createEditState, findMatches, formatPassage, highlightRuns, redo, setSegmentText, textOf, toEdits, undo, undoRedoIntent, } from "./transcriptEditing";
function segment(index, text, startMs) {
    return {
        index,
        text,
        start_ms: startMs,
        end_ms: startMs === null ? null : startMs + 1_000,
        alignment_origin: startMs === null ? "none" : "native",
        confidence: { engine_id: "whisper.cpp", value: 0.42 },
        speaker_label: null,
        annotations: [],
    };
}
const SEGMENTS = [
    segment(0, "we agreed to ship the import path first", 0),
    segment(1, "the decision is to keep everything local", 42_000),
    segment(2, "action item, send the matrix", 126_000),
];
describe("draft model", () => {
    it("collects only changed segments in the save payload", () => {
        let state = createEditState();
        state = setSegmentText(state, 1, SEGMENTS[1].text, "the decision is to stay local");
        expect(toEdits(state)).toEqual([{ index: 1, text: "the decision is to stay local" }]);
        expect(textOf(state, SEGMENTS[1])).toBe("the decision is to stay local");
        expect(textOf(state, SEGMENTS[0])).toBe(SEGMENTS[0].text);
    });
    it("drops a draft that matches the stored text again", () => {
        let state = setSegmentText(createEditState(), 0, SEGMENTS[0].text, "changed");
        state = setSegmentText(state, 0, SEGMENTS[0].text, SEGMENTS[0].text);
        expect(toEdits(state)).toEqual([]);
    });
    it("coalesces one editing session into a single undo step", () => {
        const original = SEGMENTS[0].text;
        let state = setSegmentText(createEditState(), 0, original, "a");
        state = setSegmentText(state, 0, original, "ab", { coalesce: true });
        state = setSegmentText(state, 0, original, "abc", { coalesce: true });
        expect(textOf(undo(state), SEGMENTS[0])).toBe(original);
    });
});
describe("undo and redo", () => {
    it("restores the pre-edit text and replays it", () => {
        const original = SEGMENTS[2].text;
        let state = setSegmentText(createEditState(), 2, original, "action item, send the plan");
        state = undo(state);
        expect(textOf(state, SEGMENTS[2])).toBe(original);
        state = redo(state);
        expect(textOf(state, SEGMENTS[2])).toBe("action item, send the plan");
    });
    it("is a no-op at the ends of the stack", () => {
        const empty = createEditState();
        expect(undo(empty)).toBe(empty);
        expect(redo(empty)).toBe(empty);
    });
    it("reads Cmd/Ctrl+Z and the shifted redo", () => {
        expect(undoRedoIntent({ key: "z", metaKey: true })).toBe("undo");
        expect(undoRedoIntent({ key: "Z", ctrlKey: true, shiftKey: true })).toBe("redo");
        expect(undoRedoIntent({ key: "z" })).toBeNull();
        expect(undoRedoIntent({ key: "y", ctrlKey: true })).toBeNull();
    });
});
describe("search", () => {
    it("counts every occurrence and ignores case", () => {
        const matches = findMatches(SEGMENTS, createEditState(), "the");
        expect(matches.map((m) => m.index)).toEqual([0, 1, 2]);
        expect(matches[1]?.count).toBe(1);
        expect(findMatches(SEGMENTS, createEditState(), "   ")).toEqual([]);
    });
    it("searches the drafted text, not the stored text", () => {
        const state = setSegmentText(createEditState(), 0, SEGMENTS[0].text, "quarterly roadmap");
        expect(findMatches(SEGMENTS, state, "roadmap").map((m) => m.index)).toEqual([0]);
        expect(findMatches(SEGMENTS, state, "import")).toEqual([]);
    });
    it("splits text into plain and matched runs", () => {
        expect(highlightRuns("local and local", "local")).toEqual([
            { text: "local", match: true },
            { text: " and ", match: false },
            { text: "local", match: true },
        ]);
        expect(highlightRuns("unchanged", "")).toEqual([{ text: "unchanged", match: false }]);
    });
});
describe("copy passage", () => {
    it("prefixes each line with [m:ss]", () => {
        expect(formatPassage([SEGMENTS[1], SEGMENTS[2]])).toBe("[0:42] the decision is to keep everything local\n[2:06] action item, send the matrix");
    });
    it("omits timestamps when the revision has no timeline", () => {
        const untimed = segment(0, "no timeline here", null);
        expect(formatPassage([untimed])).toBe("no timeline here");
    });
    it("copies the edited text", () => {
        const state = setSegmentText(createEditState(), 1, SEGMENTS[1].text, "we keep it local");
        expect(formatPassage([SEGMENTS[1]], state)).toBe("[0:42] we keep it local");
    });
});
