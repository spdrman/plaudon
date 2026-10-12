import { jsx as _jsx } from "react/jsx-runtime";
import { describe, expect, it } from "vitest";
import { act, screen, waitFor } from "@testing-library/react";
import { App } from "./App";
import { renderWithApp } from "../testing/renderWithApp";
import { FakeBackend } from "../testing/fakeBackend";
import { expectNoAxeViolations } from "../testing/axe";
import { MIN_DESKTOP_WIDTH } from "../design/tokens";
function onboarded(options = {}) {
    const backend = new FakeBackend({
        recordings: options.recordings ?? 3,
        segmentsPerTranscript: options.segments ?? 6,
    });
    backend.preferences = { ...backend.preferences, onboarding_completed: true };
    return backend;
}
describe("App shell", () => {
    it("shows onboarding until it is completed, then the library", async () => {
        const backend = new FakeBackend({ recordings: 0 });
        renderWithApp(_jsx(App, {}), { backend });
        expect((await screen.findAllByText(/stays? on this computer/i)).length).toBeGreaterThan(0);
        expect(screen.queryByRole("tab", { name: /Library/ })).not.toBeInTheDocument();
        expect(screen.queryByRole("button", { name: /Import audio$/ })).not.toBeInTheDocument();
    });
    it("renders the five destinations and the persistent import control once onboarded", async () => {
        renderWithApp(_jsx(App, {}), { backend: onboarded() });
        for (const name of ["Library", "Inbox", "Jobs", "Templates", "Settings"]) {
            expect(await screen.findByRole("tab", { name: new RegExp(name) })).toBeInTheDocument();
        }
        expect(screen.getByRole("button", { name: /Import audio/ })).toBeInTheDocument();
        expect(screen.getAllByRole("heading", { level: 1 })).toHaveLength(1);
    });
    it("surfaces a start-up failure with a retry instead of an empty window", async () => {
        const backend = onboarded();
        backend.failures.set("get_capabilities", {
            code: "STORAGE_UNAVAILABLE",
            message: "The library folder could not be opened.",
            retryable: true,
            action: "OPEN_DIAGNOSTICS",
            diagnostic_id: "d9",
        });
        const { user } = renderWithApp(_jsx(App, {}), { backend });
        expect(await screen.findByText(/library folder could not be opened/i)).toBeInTheDocument();
        backend.failures.delete("get_capabilities");
        await user.click(screen.getByRole("button", { name: /try again/i }));
        expect(await screen.findByRole("tab", { name: /Library/ })).toBeInTheDocument();
    });
    it("opens the keyboard reference with ? and closes it with Escape", async () => {
        const { user } = renderWithApp(_jsx(App, {}), { backend: onboarded() });
        await screen.findByRole("tab", { name: /Library/ });
        await user.keyboard("?");
        const dialog = await screen.findByRole("dialog", { name: "Keyboard shortcuts" });
        expect(dialog).toBeInTheDocument();
        await user.keyboard("{Escape}");
        await waitFor(() => expect(screen.queryByRole("dialog", { name: "Keyboard shortcuts" })).not.toBeInTheDocument());
    });
    it("navigates to the recording a clicked notification refers to", async () => {
        const backend = onboarded();
        renderWithApp(_jsx(App, {}), { backend });
        await screen.findByRole("tab", { name: /Library/ });
        await act(async () => {
            backend.emit("ui.activation_requested", {
                recording_id: "rec-00002",
                payload: { target: { kind: "recording", id: "rec-00002" } },
            });
        });
        expect(await screen.findByRole("heading", { level: 1, name: "Recording" })).toBeInTheDocument();
    });
    it("ignores an activation event without a usable target", async () => {
        const backend = onboarded();
        renderWithApp(_jsx(App, {}), { backend });
        await screen.findByRole("tab", { name: /Library/ });
        await act(async () => {
            backend.emit("ui.activation_requested", { payload: { target: { kind: "nope", id: 7 } } });
        });
        expect(screen.getByRole("heading", { level: 1, name: "Library" })).toBeInTheDocument();
    });
    it("warns below the minimum desktop width without stacking mobile-style", async () => {
        const originalWidth = window.innerWidth;
        Object.defineProperty(window, "innerWidth", {
            configurable: true,
            value: MIN_DESKTOP_WIDTH - 120,
        });
        renderWithApp(_jsx(App, {}), { backend: onboarded() });
        await screen.findByRole("tab", { name: /Library/ });
        expect(screen.getByText(new RegExp(`narrower than the ${MIN_DESKTOP_WIDTH}px minimum`))).toBeInTheDocument();
        // The navigation keeps its own column; nothing collapses into a stack.
        expect(screen.getByRole("navigation", { name: "Primary" })).toBeInTheDocument();
        Object.defineProperty(window, "innerWidth", { configurable: true, value: originalWidth });
    });
    it("has no accessibility violations on the default screen", async () => {
        const { container } = renderWithApp(_jsx(App, {}), { backend: onboarded() });
        await screen.findByRole("tab", { name: /Library/ });
        await expectNoAxeViolations(container);
    });
});
