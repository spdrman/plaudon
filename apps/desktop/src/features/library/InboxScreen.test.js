import { jsx as _jsx } from "react/jsx-runtime";
import { describe, expect, it, vi } from "vitest";
import { screen } from "@testing-library/react";
import { renderWithApp } from "../../testing/renderWithApp";
import { FakeBackend } from "../../testing/fakeBackend";
import { expectNoAxeViolations } from "../../testing/axe";
import { InboxScreen } from "./InboxScreen";
function dataRows() {
    return screen.queryAllByRole("row").filter((row) => row.hasAttribute("data-row-index"));
}
function listRequests(backend) {
    return backend.calls
        .filter((call) => call.command === "list_recordings")
        .map((call) => call.args);
}
function seedInbox() {
    const backend = new FakeBackend();
    const importing = backend.addRecording({ title: "Still importing" });
    importing.import_status = "importing";
    backend.addRecording({ title: "Waiting to transcribe", transcriptionStatus: "queued" });
    backend.addRecording({ title: "Finished long ago", segments: 2 });
    return backend;
}
describe("InboxScreen", () => {
    it("queries both inbox branches sorted by newest import", async () => {
        const backend = seedInbox();
        renderWithApp(_jsx(InboxScreen, { onOpenRecording: vi.fn() }), { backend });
        await screen.findByRole("grid");
        const requests = listRequests(backend);
        expect(requests.some((request) => request.filter.import_status.join(",") === "pending,importing,failed" &&
            request.sort.field === "imported_at" &&
            request.sort.direction === "desc")).toBe(true);
        expect(requests.some((request) => request.filter.transcription_status.join(",") === "none,queued,running" &&
            request.sort.field === "imported_at" &&
            request.sort.direction === "desc")).toBe(true);
    });
    it("shows pending work once each and leaves finished recordings out", async () => {
        const backend = seedInbox();
        const { container } = renderWithApp(_jsx(InboxScreen, { onOpenRecording: vi.fn() }), { backend });
        expect(await screen.findByText("Still importing")).toBeInTheDocument();
        expect(screen.getAllByText("Still importing")).toHaveLength(1);
        expect(screen.getByText("Waiting to transcribe")).toBeInTheDocument();
        expect(screen.queryByText("Finished long ago")).not.toBeInTheDocument();
        expect(dataRows()).toHaveLength(2);
        await expectNoAxeViolations(container);
    });
    it("opens a recording through the callback", async () => {
        const onOpen = vi.fn();
        const backend = seedInbox();
        const { user } = renderWithApp(_jsx(InboxScreen, { onOpenRecording: onOpen }), { backend });
        await screen.findByRole("grid");
        dataRows()[0].focus();
        await user.keyboard("{Enter}");
        expect(onOpen).toHaveBeenCalledTimes(1);
    });
    it("has its own empty state with a next step", async () => {
        const { container } = renderWithApp(_jsx(InboxScreen, { onOpenRecording: vi.fn() }), {
            recordings: 0,
        });
        expect(await screen.findByText("Nothing waiting")).toBeInTheDocument();
        expect(screen.getByRole("button", { name: "Import an audio file" })).toBeInTheDocument();
        expect(screen.getByRole("button", { name: "Go to Library" })).toBeInTheDocument();
        await expectNoAxeViolations(container);
    });
    it("refetches both branches after a dropped event", async () => {
        const backend = seedInbox();
        renderWithApp(_jsx(InboxScreen, { onOpenRecording: vi.fn() }), { backend });
        await screen.findByRole("grid");
        backend.addRecording({ title: "Just dropped in", transcriptionStatus: "queued" });
        backend.dropNextEvent = true;
        backend.emit("library.changed");
        expect(await screen.findByText("Just dropped in")).toBeInTheDocument();
    });
});
