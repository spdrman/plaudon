import { jsx as _jsx } from "react/jsx-runtime";
import { describe, expect, it } from "vitest";
import { screen, waitFor } from "@testing-library/react";
import { renderWithApp } from "../../testing/renderWithApp";
import { expectNoAxeViolations } from "../../testing/axe";
import { AppearanceSection } from "./AppearanceSection";
describe("AppearanceSection", () => {
    it("applies appearance and contrast to the document", async () => {
        const { user } = renderWithApp(_jsx(AppearanceSection, {}));
        await screen.findByRole("radio", { name: "Follow the system" });
        await user.click(screen.getByRole("radio", { name: "Dark" }));
        await waitFor(() => expect(document.documentElement.dataset.theme).toBe("dark"));
        await user.click(screen.getByRole("radio", { name: "High contrast" }));
        await waitFor(() => expect(document.documentElement.dataset.theme).toBe("hc-dark"));
        await user.click(screen.getByRole("radio", { name: "Light" }));
        await waitFor(() => expect(document.documentElement.dataset.theme).toBe("hc-light"));
        await user.click(screen.getByRole("radio", { name: "Standard" }));
        await waitFor(() => expect(document.documentElement.dataset.theme).toBe("light"));
        expect(screen.getByRole("radio", { name: "Light" })).toBeChecked();
        await expectNoAxeViolations();
    });
});
