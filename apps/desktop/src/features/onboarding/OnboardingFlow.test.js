import { jsx as _jsx } from "react/jsx-runtime";
import { describe, expect, it, vi } from "vitest";
import { screen, waitFor } from "@testing-library/react";
import { FakeBackend } from "../../testing/fakeBackend";
import { renderWithApp } from "../../testing/renderWithApp";
import { expectNoAxeViolations } from "../../testing/axe";
import { OnboardingFlow } from "./OnboardingFlow";
async function setup() {
    const backend = new FakeBackend();
    const capabilities = (await backend.invoke("get_capabilities", {}));
    const onCompleted = vi.fn();
    const rendered = renderWithApp(_jsx(OnboardingFlow, { capabilities: capabilities, onCompleted: onCompleted }), { backend });
    await screen.findByRole("heading", { level: 1, name: "Set up Plaudon" });
    return { ...rendered, backend, onCompleted, capabilities };
}
async function next(user, times = 1) {
    for (let i = 0; i < times; i += 1) {
        await user.click(screen.getByRole("button", { name: "Next" }));
    }
}
describe("OnboardingFlow", () => {
    it("opens on the privacy step, states the local-by-default posture and invokes nothing", async () => {
        const { backend } = await setup();
        expect(screen.getByText("Step 1 of 6")).toBeInTheDocument();
        expect(screen.getByRole("heading", { level: 2, name: "Your recordings stay on this computer" })).toBeInTheDocument();
        expect(screen.getByText(/stay on this computer unless you explicitly choose a cloud AI provider/i)).toBeInTheDocument();
        expect(screen.getByText(/Cloud processing is opt-in/i)).toBeInTheDocument();
        expect(screen.getByText(/What would leave this machine if you opt in later/i)).toBeInTheDocument();
        // Nothing is user-initiated yet: only the capability read ran.
        expect(backend.calls.map((call) => call.command)).toEqual(backend.calls.map(() => "get_capabilities"));
        expect(backend.calls.length).toBeGreaterThan(0);
        await expectNoAxeViolations();
    });
    it("sends the local/cloud choice with explicit confirmation and says cloud grants nothing", async () => {
        const { user, backend } = await setup();
        await next(user);
        expect(screen.getByText(/choosing this now grants nothing by itself/i)).toBeInTheDocument();
        await expectNoAxeViolations();
        await user.click(screen.getByRole("radio", { name: /Allow cloud AI later/i }));
        await waitFor(() => expect(backend.calls.at(-1)).toEqual({
            command: "set_local_only_mode",
            args: { enabled: false, confirmed: true },
        }));
        await user.click(screen.getByRole("radio", { name: /Local only/i }));
        await waitFor(() => expect(backend.calls.at(-1)).toEqual({
            command: "set_local_only_mode",
            args: { enabled: true, confirmed: true },
        }));
    });
    it("lists every engine with status, reason and disk facts, and offers no download in local-only mode", async () => {
        const { user } = await setup();
        await next(user, 2);
        expect(screen.getAllByText("Whisper (local)").length).toBeGreaterThan(0);
        expect(screen.getByText("Apple Speech (on-device)")).toBeInTheDocument();
        // Unsupported engines are never hidden; the reason is shown instead.
        expect(screen.getByText("Requires macOS 26 or newer on Apple silicon.")).toBeInTheDocument();
        expect(screen.getByText(/Unsupported/)).toBeInTheDocument();
        expect(screen.getByText(/base\.en — 141 MB · not installed/)).toBeInTheDocument();
        expect(screen.getByText(/Needs 141 MB, 205 GB free on this disk\./)).toBeInTheDocument();
        // No download control exists at all, and the reason is disclosed.
        const buttons = screen.getAllByRole("button").map((b) => b.textContent ?? "");
        expect(buttons.some((label) => /download|install/i.test(label))).toBe(false);
        expect(screen.getByText(/Local-only mode is on, so Plaudon downloads nothing/i)).toBeInTheDocument();
        expect(screen.getByText(/Installing models happens in Settings/i)).toBeInTheDocument();
        await expectNoAxeViolations();
    });
    it("moves focus to the step heading when the step changes", async () => {
        const { user } = await setup();
        await next(user);
        const heading = screen.getByRole("heading", {
            level: 2,
            name: "Local only, or allow cloud AI later",
        });
        expect(heading).toHaveFocus();
    });
    it("does nothing destructive on Escape", async () => {
        const { user, onCompleted } = await setup();
        await next(user);
        await user.keyboard("{Escape}");
        expect(screen.getByRole("heading", { level: 2, name: "Local only, or allow cloud AI later" })).toBeInTheDocument();
        expect(onCompleted).not.toHaveBeenCalled();
    });
    it("hands the key over once and keeps it nowhere afterwards", async () => {
        const { user, backend } = await setup();
        await next(user, 3);
        expect(screen.getByRole("heading", { level: 2, name: "Optional: add an OpenRouter key" })).toBeInTheDocument();
        await expectNoAxeViolations();
        const input = screen.getByLabelText("OpenRouter API key");
        await user.type(input, "sk-or-secret-123");
        await user.click(screen.getByRole("button", { name: "Save key" }));
        await screen.findByText("Key saved to the system keychain.");
        expect(backend.receivedSecrets).toEqual(["sk-or-secret-123"]);
        expect(input).toHaveValue("");
        expect(document.body.innerHTML).not.toContain("sk-or-secret-123");
        const caps = (await backend.invoke("get_capabilities", {}));
        expect(caps.openrouter_key_present).toBe(true);
        expect(JSON.stringify(caps)).not.toContain("sk-or-secret-123");
    });
    it("skips every account step and still reaches the import-audio finish path", async () => {
        const { user, backend, onCompleted } = await setup();
        await next(user, 2);
        await user.click(screen.getByRole("radio", { name: "Whisper (local)" }));
        await next(user);
        await user.click(screen.getByRole("button", { name: "Skip this step" }));
        expect(screen.getByRole("heading", { level: 2, name: "Where your audio comes from" })).toBeInTheDocument();
        expect(screen.getAllByText("Not in this build")).toHaveLength(2);
        await expectNoAxeViolations();
        await user.click(screen.getByRole("button", { name: "Continue to import audio" }));
        expect(screen.getByRole("heading", { level: 2, name: "Finish setup" })).toBeInTheDocument();
        expect(screen.getByText("Whisper (local)")).toBeInTheDocument();
        await expectNoAxeViolations();
        await user.click(screen.getByRole("button", { name: "Finish setup" }));
        await waitFor(() => expect(onCompleted).toHaveBeenCalledTimes(1));
        expect(backend.calls.at(-1)).toEqual({
            command: "complete_onboarding",
            args: {
                default_stt_engine_id: "whisper.cpp",
                default_text_engine_id: null,
                local_only_mode: true,
                notifications: backend.preferences.notifications,
            },
        });
        const preferences = onCompleted.mock.calls[0]?.[0];
        expect(preferences).toMatchObject({
            onboarding_completed: true,
            default_stt_engine_id: "whisper.cpp",
            local_only_mode: true,
        });
        expect(backend.receivedSecrets).toEqual([]);
    });
    it("skips the source step as well and still completes with the cloud choice", async () => {
        const { user, backend, onCompleted } = await setup();
        await next(user);
        await user.click(screen.getByRole("radio", { name: /Allow cloud AI later/i }));
        await next(user, 2);
        await user.click(screen.getByRole("button", { name: "Skip this step" }));
        await user.click(screen.getByRole("button", { name: "Skip this step" }));
        expect(screen.getByRole("heading", { level: 2, name: "Finish setup" })).toBeInTheDocument();
        await user.click(screen.getByRole("button", { name: "Finish setup" }));
        await waitFor(() => expect(onCompleted).toHaveBeenCalledTimes(1));
        expect(backend.calls.at(-1)?.args).toMatchObject({
            local_only_mode: false,
            default_stt_engine_id: null,
        });
    });
});
