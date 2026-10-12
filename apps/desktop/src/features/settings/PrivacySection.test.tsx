import { describe, expect, it } from "vitest";
import { screen, waitFor, within } from "@testing-library/react";
import { envelope, FakeBackend } from "../../testing/fakeBackend";
import { renderWithApp } from "../../testing/renderWithApp";
import { expectNoAxeViolations } from "../../testing/axe";
import { PrivacySection } from "./PrivacySection";

describe("PrivacySection", () => {
  it("confirms the change and renders the enforced policy returned by the gate", async () => {
    const { user, backend } = renderWithApp(<PrivacySection />);
    const toggle = await screen.findByRole("checkbox", { name: /Local-only mode/i });
    expect(screen.getByText(/Cloud AI is opt-in/i)).toBeInTheDocument();
    expect(toggle).toBeChecked();
    await expectNoAxeViolations();

    await user.click(toggle);
    await waitFor(() =>
      expect(backend.calls).toContainEqual({
        command: "set_local_only_mode",
        args: { enabled: false, confirmed: true },
      }),
    );
    expect(
      await screen.findByText(
        "Enforced policy: Outbound access allowed only to providers you enable.",
      ),
    ).toBeInTheDocument();
    expect(
      within(screen.getByRole("list", { name: "Allowed hosts" })).getByText("openrouter.ai"),
    ).toBeInTheDocument();

    await user.click(screen.getByRole("checkbox", { name: /Local-only mode/i }));
    await screen.findByText("Enforced policy: All outbound network access is denied.");
    expect(screen.getByText("No hosts are reachable.")).toBeInTheDocument();
  });

  it("renders the envelope message and a retry control when the change fails", async () => {
    const backend = new FakeBackend();
    backend.failures.set(
      "set_local_only_mode",
      envelope("STORAGE_FULL", "The policy could not be written.", {
        retryable: true,
        action: "WAIT_AND_RETRY",
      }),
    );
    const { user } = renderWithApp(<PrivacySection />, { backend });
    const toggle = await screen.findByRole("checkbox", { name: /Local-only mode/i });

    await user.click(toggle);
    const alert = await screen.findByRole("alert");
    expect(within(alert).getByText(/The policy could not be written\./)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Retry" })).toBeInTheDocument();

    backend.failures.clear();
    await user.click(screen.getByRole("button", { name: "Retry" }));
    await screen.findByText(
      "Enforced policy: Outbound access allowed only to providers you enable.",
    );
  });
});
