import { describe, expect, it } from "vitest";
import { screen, waitFor, within } from "@testing-library/react";
import { renderWithApp } from "../../testing/renderWithApp";
import { expectNoAxeViolations } from "../../testing/axe";
import { SettingsScreen } from "./SettingsScreen";

async function setup() {
  const rendered = renderWithApp(<SettingsScreen />, { route: { name: "settings" } });
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
      "Cloud spending",
      "Cloud consent",
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

  it("exposes the cloud budget and consent controls, keyboard reachable", async () => {
    const { user, backend } = await setup();
    backend.addConsent({ model_id: "acme/scribe-2", scope_kind: "model_policy" });

    const budget = screen.getByRole("region", { name: "Cloud spending" });
    expect(within(budget).getByLabelText("Per-job limit (USD)")).toBeInTheDocument();
    expect(within(budget).getByLabelText("Monthly limit (USD)")).toBeInTheDocument();
    // §10.4: the enforcement note is rendered verbatim from the backend.
    expect(
      within(budget).getByText(
        /Local budgets cannot prevent charges made elsewhere with the same key\./,
      ),
    ).toBeInTheDocument();

    // Every control in the new sections is reachable with the keyboard alone.
    const perJob = within(budget).getByLabelText("Per-job limit (USD)");
    perJob.focus();
    expect(perJob).toHaveFocus();
    await user.tab();
    expect(within(budget).getByLabelText("Monthly limit (USD)")).toHaveFocus();
    await user.tab();
    expect(within(budget).getByRole("button", { name: "Save budget" })).toHaveFocus();

    expect(
      within(screen.getByRole("region", { name: "Cloud consent" })).getByText(
        "No cloud consent has been granted.",
      ),
    ).toBeInTheDocument();

    await expectNoAxeViolations();
  });
});
