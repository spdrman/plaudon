import { describe, expect, it } from "vitest";
import { screen } from "@testing-library/react";
import { renderWithApp } from "../../testing/renderWithApp";
import { expectNoAxeViolations } from "../../testing/axe";
import { CLOUD_BUDGET_ENFORCEMENT_NOTE, FakeBackend, usd } from "../../testing/fakeBackend";
import { CloudBudgetSection, monthWindow } from "./CloudBudgetSection";

/** An instant inside the window the section asks for, so seeded rows are in range. */
function nowInWindow(): string {
  return new Date(Date.parse(monthWindow(new Date()).from) + 60_000).toISOString();
}

describe("CloudBudgetSection", () => {
  it("separates finalized, estimated and unknown spending", async () => {
    const backend = new FakeBackend();
    backend.addAttempt({
      model_id: "acme/scribe-2",
      created_at: nowInWindow(),
      cost: { confidence: "finalized", amount: usd(4_212) },
    });
    backend.addAttempt({
      model_id: "acme/scribe-2",
      created_at: nowInWindow(),
      cost: { confidence: "estimated", amount: usd(6_000) },
    });
    backend.addAttempt({
      model_id: "acme/scribe-mini",
      created_at: nowInWindow(),
      cost: { confidence: "unknown", amount: null },
    });

    renderWithApp(<CloudBudgetSection />, { backend });
    await screen.findByText(/Estimated USD 0\.010212/);

    // A month total containing an estimate is labelled as an estimate, never final.
    expect(screen.queryByText(/Final USD 0\.010212/)).not.toBeInTheDocument();
    expect(screen.getByText("USD 0.004212")).toBeInTheDocument();
    expect(screen.getByText("USD 0.006000 (estimate, not charged amounts)")).toBeInTheDocument();
    expect(screen.getByText("1 (not counted in the figures above)")).toBeInTheDocument();
  });

  it("renders an absent month-to-date figure as unknown, not as zero", async () => {
    const backend = new FakeBackend();
    renderWithApp(<CloudBudgetSection />, { backend });
    await screen.findByText("Cost unknown");
    expect(screen.queryByText(/USD 0\.000000/)).not.toBeInTheDocument();
    expect(screen.getByText("No finalized amount has been reported yet")).toBeInTheDocument();
  });

  it("saves per-job and monthly limits and states what local enforcement can do", async () => {
    const backend = new FakeBackend();
    const { user } = renderWithApp(<CloudBudgetSection />, { backend });
    await screen.findByText("Cost unknown");

    expect(screen.getByText(CLOUD_BUDGET_ENFORCEMENT_NOTE)).toBeInTheDocument();
    expect(screen.getByText(/No local limit is set, so no run is stopped here\./)).toBeInTheDocument();

    await user.type(screen.getByLabelText("Per-job limit (USD)"), "0.50");
    await user.type(screen.getByLabelText("Monthly limit (USD)"), "10");
    await user.click(screen.getByRole("button", { name: "Save budget" }));

    await screen.findByText(/A locally calculable limit is set/);
    expect(backend.budget.per_job_limit).toEqual({ amount: "0.500000", currency: "USD" });
    expect(backend.budget.monthly_limit).toEqual({ amount: "10.000000", currency: "USD" });
  });

  it("renders the backend's refusal of an unusable amount", async () => {
    const backend = new FakeBackend();
    const { user } = renderWithApp(<CloudBudgetSection />, { backend });
    await screen.findByText("Cost unknown");

    await user.type(screen.getByLabelText("Per-job limit (USD)"), "lots");
    await user.click(screen.getByRole("button", { name: "Save budget" }));

    expect(
      await screen.findByText("Enter an amount such as 2.50, or leave it empty."),
    ).toBeInTheDocument();
  });

  it("has no accessibility violations", async () => {
    const backend = new FakeBackend();
    renderWithApp(<CloudBudgetSection />, { backend });
    await screen.findByText("Cost unknown");
    await expectNoAxeViolations();
  });
});
