import { jsx as _jsx } from "react/jsx-runtime";
import { describe, expect, it } from "vitest";
import { screen, waitFor, within } from "@testing-library/react";
import { FakeBackend } from "../../testing/fakeBackend";
import { renderWithApp } from "../../testing/renderWithApp";
import { expectNoAxeViolations } from "../../testing/axe";
import { AiKeySection } from "./AiKeySection";
describe("AiKeySection", () => {
    it("hands the key over once, keeps it nowhere, and reports presence only", async () => {
        const { user, backend } = renderWithApp(_jsx(AiKeySection, {}));
        await screen.findByText("No key saved");
        await expectNoAxeViolations();
        const input = screen.getByLabelText("Set OpenRouter API key");
        await user.type(input, "sk-or-secret-xyz");
        await user.click(screen.getByRole("button", { name: "Set key" }));
        await screen.findByText("Key saved in the system keychain");
        expect(backend.receivedSecrets).toEqual(["sk-or-secret-xyz"]);
        expect(screen.getByLabelText("Replace OpenRouter API key")).toHaveValue("");
        expect(document.body.innerHTML).not.toContain("sk-or-secret-xyz");
        const caps = (await backend.invoke("get_capabilities", {}));
        expect(caps.openrouter_key_present).toBe(true);
        expect(JSON.stringify(caps)).not.toContain("sk-or-secret-xyz");
    });
    it("removes the key only after a destructive confirmation", async () => {
        const backend = new FakeBackend();
        backend.openRouterKeyPresent = true;
        const { user } = renderWithApp(_jsx(AiKeySection, {}), { backend });
        await screen.findByText("Key saved in the system keychain");
        const trigger = screen.getByRole("button", { name: "Remove key" });
        await user.click(trigger);
        const dialog = await screen.findByRole("alertdialog", { name: "Remove the OpenRouter key?" });
        expect(backend.calls.some((call) => call.command === "remove_openrouter_key")).toBe(false);
        await expectNoAxeViolations();
        // Cancelling keeps the key and restores focus to the trigger.
        await user.click(within(dialog).getByRole("button", { name: "Cancel" }));
        expect(backend.openRouterKeyPresent).toBe(true);
        await waitFor(() => expect(trigger).toHaveFocus());
        await user.click(screen.getByRole("button", { name: "Remove key" }));
        const confirm = await screen.findByRole("alertdialog");
        await user.click(within(confirm).getByRole("button", { name: "Remove key" }));
        await waitFor(() => expect(backend.openRouterKeyPresent).toBe(false));
        expect(await screen.findByText("No key saved")).toBeInTheDocument();
    });
});
