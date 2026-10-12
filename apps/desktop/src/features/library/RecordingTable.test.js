import { jsx as _jsx } from "react/jsx-runtime";
import { describe, expect, it, vi } from "vitest";
import { screen, within } from "@testing-library/react";
import { renderWithApp } from "../../testing/renderWithApp";
import { expectNoAxeViolations } from "../../testing/axe";
import { RecordingTable } from "./RecordingTable";
import { createLibraryQuery } from "./libraryQuery";
function makeRow(index, overrides = {}) {
    return {
        recording_id: `rec-${index}`,
        title: `Recording ${index}`,
        recorded_at: "2026-02-01T09:00:00Z",
        imported_at: "2026-02-01T10:00:00Z",
        duration_ms: 61_000,
        source: { source_kind: "manual_import", label: "Manual import" },
        import_status: "imported",
        transcription_status: "completed",
        processing_engine: "whisper.cpp",
        last_error: null,
        tags: [],
        byte_length: 2048,
        sha256: "b".repeat(64),
        ...overrides,
    };
}
function renderTable(rows, onOpen = vi.fn(), rowCount = rows.length) {
    const result = renderWithApp(_jsx(RecordingTable, { label: "Recordings", rows: rows, rowCount: rowCount, sort: createLibraryQuery(), onSort: vi.fn(), onOpenRecording: onOpen }));
    return { ...result, onOpen };
}
function dataRows() {
    return screen.getAllByRole("row").filter((row) => row.hasAttribute("data-row-index"));
}
describe("RecordingTable", () => {
    it("renders every §5.2 column including the last error message", () => {
        renderTable([
            makeRow(1, {
                title: "Quarterly review",
                processing_engine: "apple.speech",
                last_error: {
                    code: "STORAGE_FULL",
                    message: "Not enough space to finish transcription.",
                    retryable: true,
                    action: "FREE_DISK_SPACE",
                    diagnostic_id: "diag-storage",
                },
            }),
        ]);
        for (const header of [
            "Title",
            "Recorded",
            "Imported",
            "Duration",
            "Source",
            "Import status",
            "Transcription",
            "Engine",
            "Last error",
        ]) {
            expect(screen.getByRole("columnheader", { name: new RegExp(header, "i") })).toBeInTheDocument();
        }
        const row = dataRows()[0];
        expect(within(row).getByText("Quarterly review")).toBeInTheDocument();
        expect(within(row).getByText("Manual import")).toBeInTheDocument();
        expect(within(row).getByText("apple.speech")).toBeInTheDocument();
        expect(within(row).getByText("1:01")).toBeInTheDocument();
        expect(within(row).getByText("Not enough space to finish transcription.")).toBeInTheDocument();
    });
    it("states an unknown duration and a missing engine instead of fabricating values", () => {
        renderTable([makeRow(1, { duration_ms: null, processing_engine: null, recorded_at: null })]);
        const row = dataRows()[0];
        // Both the missing recorded-at and the missing duration say so in words.
        expect(within(row).getAllByText("Unknown")).toHaveLength(2);
        expect(within(row).getByText("Not set")).toBeInTheDocument();
        expect(within(row).queryByText("0:00")).not.toBeInTheDocument();
    });
    it("never communicates a status by color alone", () => {
        const { container } = renderTable([
            makeRow(1, { import_status: "failed", transcription_status: "stale" }),
        ]);
        const badges = Array.from(container.querySelectorAll(".status-badge"));
        expect(badges.length).toBeGreaterThan(0);
        for (const badge of badges) {
            expect(badge.textContent?.replace(/\s/g, "").length ?? 0).toBeGreaterThan(1);
        }
        expect(screen.getByText("Import failed")).toBeInTheDocument();
        expect(screen.getByText("Stale")).toBeInTheDocument();
    });
    it("reports the whole matching set with aria-rowcount, not the rendered window", () => {
        renderTable([makeRow(1), makeRow(2)], vi.fn(), 100_000);
        expect(screen.getByRole("grid")).toHaveAttribute("aria-rowcount", "100001");
        expect(screen.getByRole("grid")).toHaveAttribute("aria-multiselectable", "true");
    });
    it("moves with Up/Down/Home/End using a roving tabindex", async () => {
        const { user } = renderTable([makeRow(1), makeRow(2), makeRow(3)]);
        const rows = dataRows();
        expect(rows[0]).toHaveAttribute("tabindex", "0");
        expect(rows[1]).toHaveAttribute("tabindex", "-1");
        rows[0].focus();
        await user.keyboard("{ArrowDown}");
        expect(dataRows()[1]).toHaveFocus();
        await user.keyboard("{End}");
        expect(dataRows()[2]).toHaveFocus();
        await user.keyboard("{Home}");
        expect(dataRows()[0]).toHaveFocus();
        await user.keyboard("{ArrowUp}");
        expect(dataRows()[0]).toHaveFocus();
    });
    it("opens a recording with Enter and with a double click, never by rendering detail itself", async () => {
        const onOpen = vi.fn();
        const { user } = renderTable([makeRow(1), makeRow(2)], onOpen);
        dataRows()[1].focus();
        await user.keyboard("{Enter}");
        expect(onOpen).toHaveBeenCalledWith("rec-2");
        await user.dblClick(dataRows()[0]);
        expect(onOpen).toHaveBeenCalledWith("rec-1");
    });
    it("toggles selection with Space, extends it with Shift and selects all with Cmd+A", async () => {
        const { user } = renderTable([makeRow(1), makeRow(2), makeRow(3), makeRow(4)]);
        dataRows()[0].focus();
        await user.keyboard(" ");
        expect(dataRows()[0]).toHaveAttribute("aria-selected", "true");
        expect(screen.getByText("1 of 4 loaded recordings selected.")).toBeInTheDocument();
        await user.keyboard("{Shift>}{ArrowDown}{ArrowDown}{/Shift}");
        expect(dataRows()[2]).toHaveAttribute("aria-selected", "true");
        expect(screen.getByText("3 of 4 loaded recordings selected.")).toBeInTheDocument();
        await user.keyboard("{Meta>}a{/Meta}");
        expect(screen.getByText("4 of 4 loaded recordings selected.")).toBeInTheDocument();
    });
    it("extends selection with Shift+Click from the last clicked row", async () => {
        const { user } = renderTable([makeRow(1), makeRow(2), makeRow(3)]);
        await user.click(dataRows()[0]);
        await user.keyboard("{Shift>}");
        await user.click(dataRows()[2]);
        await user.keyboard("{/Shift}");
        expect(screen.getByText("3 of 3 loaded recordings selected.")).toBeInTheDocument();
    });
    it("virtualizes a large page: the DOM holds far fewer rows than the data", () => {
        const rows = Array.from({ length: 2_000 }, (_, index) => makeRow(index));
        renderTable(rows, vi.fn(), 100_000);
        expect(dataRows().length).toBeLessThan(200);
        expect(dataRows().length).toBeGreaterThan(0);
        expect(screen.getByRole("grid")).toHaveAttribute("aria-rowcount", "100001");
    });
    it("has no axe violations", async () => {
        renderTable([makeRow(1), makeRow(2, { duration_ms: null })]);
        await expectNoAxeViolations();
    });
});
