import { jsx as _jsx } from "react/jsx-runtime";
import { describe, expect, it } from "vitest";
import { screen, waitFor, within } from "@testing-library/react";
import { renderWithApp } from "../../testing/renderWithApp";
import { expectNoAxeViolations } from "../../testing/axe";
import { SettingsScreen } from "./SettingsScreen";
async function setup() {
    const rendered = renderWithApp(_jsx(SettingsScreen, {}), { route: { name: "settings" } });
    // The shell header owns the page <h1>; a screen heading is level 2.
    await screen.findByRole("heading", { level: 2, name: "Settings" });
    await screen.findByText("/Users/tester/Library/Application Support/Plaudon");
    return rendered;
}
describe("SettingsScreen", () => {
    it("renders every section as a landmark with read-only storage facts", async () => {
        await setup();
        for (const title of [
            "Appearance",
            "Privacy & network",
            "AI key",
            "Notifications",
            "Storage",
            "Activity history",
        ]) {
            expect(screen.getByRole("heading", { level: 2, name: title })).toBeInTheDocument();
            expect(screen.getByRole("region", { name: title })).toBeInTheDocument();
        }
        const storage = screen.getByRole("region", { name: "Storage" });
        expect(within(storage).getByText("205 GB")).toBeInTheDocument();
        expect(within(storage).getByText("466 GB")).toBeInTheDocument();
        // The real controls of each section are present, not placeholders.
        expect(screen.getByRole("radio", { name: "Dark" })).toBeInTheDocument();
        expect(screen.getByRole("checkbox", { name: /Local-only mode/i })).toBeInTheDocument();
        expect(screen.getByLabelText("Set OpenRouter API key")).toBeInTheDocument();
        await expectNoAxeViolations();
    });
    it("navigates to the activity history", async () => {
        const { user } = await setup();
        await user.click(screen.getByRole("button", { name: "Open activity history" }));
        await waitFor(() => expect(window.location.hash).toContain("settings/activity"));
    });
});
