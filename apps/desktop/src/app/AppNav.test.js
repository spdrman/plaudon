import { jsx as _jsx, Fragment as _Fragment, jsxs as _jsxs } from "react/jsx-runtime";
import { describe, expect, it } from "vitest";
import { screen } from "@testing-library/react";
import { AppNav } from "./AppNav";
import { renderWithApp } from "../testing/renderWithApp";
import { DESTINATIONS, DESTINATION_LABELS } from "./router";
import { expectNoAxeViolations } from "../testing/axe";
describe("AppNav", () => {
    it("exposes the five top-level destinations as a tab list", async () => {
        renderWithApp(_jsx(AppNav, {}));
        const tabs = await screen.findAllByRole("tab");
        expect(tabs.map((t) => t.textContent)).toEqual(DESTINATIONS.map((d) => expect.stringContaining(DESTINATION_LABELS[d])));
    });
    it("keeps exactly one tab stop and moves selection with the arrow keys", async () => {
        const { user } = renderWithApp(_jsx(AppNav, {}));
        const tabs = await screen.findAllByRole("tab");
        expect(tabs.filter((t) => t.getAttribute("tabindex") === "0")).toHaveLength(1);
        tabs[0].focus();
        await user.keyboard("{ArrowDown}");
        expect(screen.getByRole("tab", { selected: true })).toHaveTextContent(DESTINATION_LABELS[DESTINATIONS[1]]);
        await user.keyboard("{ArrowUp}");
        expect(screen.getByRole("tab", { selected: true })).toHaveTextContent(DESTINATION_LABELS[DESTINATIONS[0]]);
        await user.keyboard("{End}");
        expect(screen.getByRole("tab", { selected: true })).toHaveTextContent(DESTINATION_LABELS[DESTINATIONS[DESTINATIONS.length - 1]]);
    });
    it("activates a destination with the mouse", async () => {
        const { user } = renderWithApp(_jsx(AppNav, {}));
        await user.click(screen.getByRole("tab", { name: /Jobs/ }));
        expect(screen.getByRole("tab", { name: /Jobs/ })).toHaveAttribute("aria-selected", "true");
    });
    it("names every destination in text, not by icon alone", async () => {
        renderWithApp(_jsx(AppNav, {}));
        for (const destination of DESTINATIONS) {
            expect(await screen.findByRole("tab", { name: new RegExp(DESTINATION_LABELS[destination]) })).toBeInTheDocument();
        }
    });
    it("has no accessibility violations", async () => {
        // `aria-controls` points at the shell's main region, so the audited tree
        // must contain it exactly as the app renders it.
        const { container } = renderWithApp(_jsxs(_Fragment, { children: [_jsx(AppNav, {}), _jsx("main", { id: "main-region", tabIndex: -1, children: _jsx("h2", { children: "Library" }) })] }));
        await expectNoAxeViolations(container);
    });
});
