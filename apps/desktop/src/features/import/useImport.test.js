import { jsx as _jsx, jsxs as _jsxs } from "react/jsx-runtime";
import { describe, expect, it, vi } from "vitest";
import { readFileSync, readdirSync } from "node:fs";
import { join } from "node:path";
import { screen, waitFor } from "@testing-library/react";
import { open } from "@tauri-apps/plugin-dialog";
import { renderWithApp } from "../../testing/renderWithApp";
import { FakeBackend, envelope } from "../../testing/fakeBackend";
import { ToastRegion } from "../../app/ToastRegion";
import { ImportButton } from "./ImportButton";
import { ImportDropTarget } from "./ImportDropTarget";
import { describeBatch, mergeOutcomes, summarizeOutcomes, toPathStrings } from "./useImport";
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn() }));
const openPicker = vi.mocked(open);
function importRequests(backend) {
    return backend.calls
        .filter((call) => call.command === "import_files")
        .map((call) => call.args);
}
function renderImport(backend = new FakeBackend()) {
    return renderWithApp(_jsxs(ImportDropTarget, { children: [_jsx(ImportButton, {}), _jsx(ToastRegion, {})] }), { backend });
}
describe("import path handling", () => {
    it("forwards only usable path strings, byte for byte", () => {
        expect(toPathStrings(null)).toEqual([]);
        expect(toPathStrings("/tmp/one.mp3")).toEqual(["/tmp/one.mp3"]);
        expect(toPathStrings(["/tmp/a.mp3", "", 7, { path: "/tmp/b.mp3" }, { nope: 1 }])).toEqual([
            "/tmp/a.mp3",
            "/tmp/b.mp3",
        ]);
        const hostile = "/tmp/\u202Egpm.3pm.mp3";
        expect(toPathStrings([hostile])).toEqual([hostile]);
    });
    it("replaces an outcome when a file is decided again and keeps the order", () => {
        const first = [
            { outcome: "duplicate", path: "/a.mp3", sha256: "x", existing_recording_id: "rec-1", existing_title: "A" },
            { outcome: "accepted", path: "/b.mp3", job_id: "job-1" },
        ];
        const merged = mergeOutcomes(first, [{ outcome: "accepted", path: "/a.mp3", job_id: "job-2" }]);
        expect(merged.map((item) => [item.path, item.outcome])).toEqual([
            ["/a.mp3", "accepted"],
            ["/b.mp3", "accepted"],
        ]);
    });
    it("summarizes a batch without hiding the bad news", () => {
        const counts = summarizeOutcomes([
            { outcome: "accepted", path: "/a.mp3", job_id: "job-1" },
            { outcome: "rejected", path: "/b.txt", error: envelope("UNSUPPORTED_MEDIA", "no") },
        ]);
        expect(counts).toEqual({ accepted: 1, duplicate: 0, skipped: 0, rejected: 1 });
        const toast = describeBatch(counts);
        expect(toast.body).toContain("1 imported");
        expect(toast.body).toContain("1 rejected");
        expect(toast.tone).toBe("warning");
    });
});
describe("ImportButton", () => {
    it("opens the OS picker filtered to audio and imports what it returns", async () => {
        openPicker.mockResolvedValue(["/Users/t/one.mp3", "/Users/t/two.m4a"]);
        const { backend, user } = renderImport();
        await user.click(screen.getByRole("button", { name: /Import audio/ }));
        await waitFor(() => expect(importRequests(backend)).toHaveLength(1));
        expect(importRequests(backend)[0]).toEqual({
            paths: ["/Users/t/one.mp3", "/Users/t/two.m4a"],
            duplicate_policy: "ask",
        });
        const options = openPicker.mock.calls[0][0];
        expect(options.multiple).toBe(true);
        expect(options.filters[0].extensions).toEqual(["wav", "mp3", "m4a", "flac", "ogg", "opus"]);
    });
    it("imports nothing when the picker is cancelled", async () => {
        openPicker.mockResolvedValue(null);
        const { backend, user } = renderImport();
        await user.click(screen.getByRole("button", { name: /Import audio/ }));
        await waitFor(() => expect(openPicker).toHaveBeenCalled());
        expect(importRequests(backend)).toHaveLength(0);
    });
    it("announces the finished batch with a toast", async () => {
        openPicker.mockResolvedValue(["/Users/t/one.mp3"]);
        const { user } = renderImport();
        await user.click(screen.getByRole("button", { name: /Import audio/ }));
        expect(await screen.findByText("Import finished")).toBeInTheDocument();
        expect(screen.getByText("1 imported.")).toBeInTheDocument();
    });
    it("keeps the rest of a batch when one file is rejected", async () => {
        openPicker.mockResolvedValue(["/Users/t/good.mp3", "/Users/t/notes.txt"]);
        const { backend, user } = renderImport();
        await user.click(screen.getByRole("button", { name: /Import audio/ }));
        expect(await screen.findByText(/notes\.txt is not a supported audio file/)).toBeInTheDocument();
        expect(screen.getByText("/Users/t/good.mp3")).toBeInTheDocument();
        expect(screen.getAllByText("Imported").length).toBeGreaterThan(0);
        expect(screen.getByText("Rejected")).toBeInTheDocument();
        expect(backend.recordings).toHaveLength(1);
        expect(backend.recordings[0].media.original_file_name).toBe("good.mp3");
    });
    it("renders hostile file names as inert text and invokes no other command", async () => {
        const hostile = [
            "/tmp/; rm -rf /.mp3",
            `/tmp/${"a".repeat(300)}.mp3`,
            "/tmp/CON.mp3",
            "/tmp/\u202Egpm.3pm.mp3",
            "/tmp/<script>alert(1)</script>.mp3",
        ];
        openPicker.mockResolvedValue(hostile);
        const { backend, container, user } = renderImport();
        await user.click(screen.getByRole("button", { name: /Import audio/ }));
        await waitFor(() => expect(importRequests(backend)).toHaveLength(1));
        expect(importRequests(backend)[0].paths).toEqual(hostile);
        for (const path of hostile)
            expect(screen.getByText(path)).toBeInTheDocument();
        expect(container.innerHTML).not.toContain("<script");
        expect(container.innerHTML).toContain("&lt;script&gt;");
        const other = backend.calls.filter((call) => call.command !== "import_files" && call.command !== "get_capabilities");
        expect(other).toEqual([]);
    });
    it("never uses dangerouslySetInnerHTML in the import or library feature code", () => {
        const roots = [join(__dirname), join(__dirname, "..", "library")];
        const sources = roots.flatMap((root) => readdirSync(root)
            .filter((name) => /\.tsx?$/.test(name) && !name.includes(".test."))
            .map((name) => readFileSync(join(root, name), "utf8")));
        expect(sources.length).toBeGreaterThan(5);
        for (const source of sources)
            expect(source).not.toContain("dangerouslySetInnerHTML");
    });
});
