import { jsx as _jsx, Fragment as _Fragment, jsxs as _jsxs } from "react/jsx-runtime";
import { describe, expect, it } from "vitest";
import { screen, waitFor } from "@testing-library/react";
import { FakeBackend } from "../../testing/fakeBackend";
import { renderWithApp } from "../../testing/renderWithApp";
import { expectNoAxeViolations } from "../../testing/axe";
import { ActivityBell } from "./ActivityBell";
import { useAppState } from "../../app/AppState";
import { formatRoute } from "../../app/router";
function BellHarness() {
    const { route } = useAppState();
    return (_jsxs(_Fragment, { children: [_jsx(ActivityBell, {}), _jsx("p", { "data-testid": "route", children: formatRoute(route) })] }));
}
describe("ActivityBell", () => {
    it("names itself with the unread count and keeps it current", async () => {
        const backend = new FakeBackend();
        backend.addActivity({ category: "import_finished", title: "Import finished" });
        backend.addActivity({ category: "requires_attention", title: "Requires attention" });
        backend.addActivity({ category: "import_finished", title: "Import finished", read: true });
        renderWithApp(_jsx(ActivityBell, {}), { backend });
        expect(await screen.findByRole("button", { name: "Activity history, 2 unread" })).toBeInTheDocument();
        backend.addActivity({ category: "requires_attention", title: "Requires attention" });
        backend.emit("activity.added");
        await waitFor(() => expect(screen.getByRole("button", { name: "Activity history, 3 unread" })).toBeInTheDocument());
        await expectNoAxeViolations();
    });
    it("navigates to the activity history on click and on Enter", async () => {
        const backend = new FakeBackend();
        backend.addActivity({ category: "import_finished", title: "Import finished" });
        const { user } = renderWithApp(_jsx(BellHarness, {}), { backend });
        const bell = await screen.findByRole("button", { name: "Activity history, 1 unread" });
        await user.click(bell);
        expect(screen.getByTestId("route")).toHaveTextContent("#/settings/activity");
        window.location.hash = "#/library";
        bell.focus();
        await user.keyboard("{Enter}");
        expect(screen.getByTestId("route")).toHaveTextContent("#/settings/activity");
    });
});
