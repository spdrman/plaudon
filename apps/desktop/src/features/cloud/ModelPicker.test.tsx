import { describe, expect, it, vi } from "vitest";
import { screen, within } from "@testing-library/react";
import { useState } from "react";
import { renderWithApp } from "../../testing/renderWithApp";
import { expectNoAxeViolations } from "../../testing/axe";
import { FakeBackend, envelope } from "../../testing/fakeBackend";
import type { ModelCard } from "../../ipc/contract";
import { ModelPicker } from "./ModelPicker";

function Harness({ onSelect }: { onSelect?: (model: ModelCard) => void }) {
  const [selected, setSelected] = useState<ModelCard | null>(null);
  return (
    <ModelPicker
      stage="transcribe"
      selectedModelId={selected?.model_id ?? null}
      onSelect={(model) => {
        setSelected(model);
        onSelect?.(model);
      }}
    />
  );
}

function choiceFor(modelId: string): HTMLElement {
  const node = document.querySelector<HTMLElement>(`[data-model-id="${CSS.escape(modelId)}"]`);
  if (!node) throw new Error(`no model choice rendered for ${modelId}`);
  return node;
}

describe("ModelPicker", () => {
  it("shows the request family, verification reason, price, route and retention of every model", async () => {
    const backend = new FakeBackend();
    renderWithApp(<Harness />, { backend });
    await screen.findByRole("radio", { name: /Scribe 2/ });

    const verified = within(choiceFor("acme/scribe-2"));
    expect(verified.getAllByText(/Verified/).length).toBeGreaterThan(0);
    expect(verified.getByText("Dedicated speech-to-text endpoint")).toBeInTheDocument();
    expect(verified.getByText("USD 0.000100 — USD per audio second")).toBeInTheDocument();
    expect(verified.getByText("openrouter → Acme Speech")).toBeInTheDocument();
    expect(verified.getByText("Limited retention (30 days)")).toBeInTheDocument();

    // Catalogue-only entries stay visible with the machine reason, not hidden.
    const catalogOnly = within(choiceFor("vendor/catalog-audio-xl"));
    expect(
      catalogOnly.getAllByText(/It appears in the provider catalogue only; nothing has been tested\./)
        .length,
    ).toBeGreaterThan(0);
    expect(catalogOnly.getByText("openrouter → provider not disclosed")).toBeInTheDocument();
    expect(catalogOnly.getByText("Retention unknown")).toBeInTheDocument();
    expect(catalogOnly.getByText("Request contract not established")).toBeInTheDocument();
  });

  it("states an unknown price as unknown and never as a zero amount", async () => {
    const backend = new FakeBackend();
    renderWithApp(<Harness />, { backend });
    await screen.findByRole("radio", { name: /Scribe 2/ });

    const catalogOnly = within(choiceFor("vendor/catalog-audio-xl"));
    expect(catalogOnly.getByText("Price unknown (pricing not published)")).toBeInTheDocument();
    expect(catalogOnly.queryByText(/0\.000000/)).not.toBeInTheDocument();
    expect(catalogOnly.queryByText(/\$0\.00/)).not.toBeInTheDocument();
  });

  it("disables a model that cannot be dispatched to and explains why", async () => {
    const backend = new FakeBackend();
    const onSelect = vi.fn();
    const { user } = renderWithApp(<Harness onSelect={onSelect} />, { backend });
    await screen.findByRole("radio", { name: /Scribe 2/ });

    const blocked = screen.getByRole("radio", { name: /Nimbus Audio Chat/ });
    expect(blocked).toBeDisabled();
    expect(
      within(choiceFor("nimbus/audio-chat-preview")).getByText(
        /cannot be used yet: Its privacy route could not be resolved/,
      ),
    ).toBeInTheDocument();

    await user.click(blocked);
    expect(onSelect).not.toHaveBeenCalled();
    expect(blocked).not.toBeChecked();
  });

  it("marks a user supplied custom model id as not verified and not dispatchable", async () => {
    const backend = new FakeBackend();
    const { user } = renderWithApp(<Harness />, { backend });
    await screen.findByRole("radio", { name: /Scribe 2/ });

    await user.type(screen.getByLabelText("Advanced: custom model ID"), "some-vendor/brand-new");
    await user.click(screen.getByRole("button", { name: "Add custom model ID" }));

    const custom = within(choiceFor("some-vendor/brand-new"));
    expect(custom.getAllByText(/Not verified/).length).toBeGreaterThan(0);
    expect(
      custom.getAllByText(/You typed this model id, so nothing has been tested for it\./).length,
    ).toBeGreaterThan(0);
    expect(custom.getByText("Price unknown (no pricing retrieved)")).toBeInTheDocument();
    expect(screen.getByRole("radio", { name: /some-vendor\/brand-new/ })).toBeDisabled();
  });

  it("says the list came from the pinned allowlist when discovery degraded", async () => {
    const backend = new FakeBackend({ catalogDegradedReason: "offline" });
    renderWithApp(<Harness />, { backend });
    await screen.findByRole("radio", { name: /Scribe 2/ });

    expect(
      screen.getByText(
        /Showing the pinned allowlist because the provider catalogue could not be reached/,
      ),
    ).toBeInTheDocument();
    expect(screen.queryByText(/Live provider catalogue/)).not.toBeInTheDocument();
  });

  it("marks the per-task default without changing it", async () => {
    const backend = new FakeBackend();
    const { user } = renderWithApp(<Harness />, { backend });
    await screen.findByRole("radio", { name: /Scribe 2/ });

    expect(within(choiceFor("acme/scribe-2")).getByText("Task default")).toBeInTheDocument();
    await user.click(screen.getByRole("radio", { name: /Scribe Mini/ }));
    expect(backend.defaultModelByStage.transcribe).toBe("acme/scribe-2");
  });

  it("has no accessibility violations", async () => {
    const backend = new FakeBackend();
    renderWithApp(<Harness />, { backend });
    await screen.findByRole("radio", { name: /Scribe 2/ });
    await expectNoAxeViolations();
  });
});

describe("capability check (ADR-0022)", () => {
  function cloudBackend(): FakeBackend {
    const backend = new FakeBackend();
    backend.enableCloud();
    return backend;
  }

  it("offers the check only on models that are not verified", async () => {
    renderWithApp(<Harness />, { backend: cloudBackend() });
    await screen.findByRole("radio", { name: /Scribe 2/ });

    expect(
      screen.queryByRole("button", { name: /Run capability check on Scribe 2/ }),
    ).not.toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: /Run capability check on Catalog Audio XL/ }),
    ).toBeInTheDocument();
  });

  it("sends nothing until the user explicitly confirms the billable check", async () => {
    const backend = cloudBackend();
    backend.addConsent({ model_id: "vendor/catalog-audio-xl", scope_kind: "model_policy" });
    const { user } = renderWithApp(<Harness />, { backend });
    await screen.findByRole("radio", { name: /Scribe 2/ });

    // Rendering alone never runs a check.
    expect(backend.calls.some((call) => call.command === "verify_model")).toBe(false);

    await user.click(
      screen.getByRole("button", { name: /Run capability check on Catalog Audio XL/ }),
    );
    const dialog = await screen.findByRole("dialog", {
      name: /Run the capability check on Catalog Audio XL/,
    });
    expect(
      within(dialog).getByText(/sends about 5 seconds of bundled test audio/),
    ).toBeInTheDocument();
    // Still nothing sent while the disclosure is on screen.
    expect(backend.calls.some((call) => call.command === "verify_model")).toBe(false);

    await user.click(
      within(dialog).getByRole("button", { name: "Send the test clip and accept the cost" }),
    );

    await screen.findByText(/Capability check on vendor\/catalog-audio-xl completed/);
    const calls = backend.calls.filter((call) => call.command === "verify_model");
    expect(calls).toHaveLength(1);
    expect(calls[0]?.args).toEqual({
      model_id: "vendor/catalog-audio-xl",
      task: "transcription",
      cost_acknowledged: true,
    });
  });

  it("asks for consent before the check and verifies the model afterwards", async () => {
    const backend = cloudBackend();
    const { user } = renderWithApp(<Harness />, { backend });
    await screen.findByRole("radio", { name: /Scribe 2/ });

    await user.click(
      screen.getByRole("button", { name: /Run capability check on Catalog Audio XL/ }),
    );
    await screen.findByRole("dialog", { name: /Send this content to a cloud provider/ });
    expect(backend.calls.some((call) => call.command === "verify_model")).toBe(false);

    await user.click(screen.getByRole("radio", { name: /Allow for this model policy/ }));
    await user.click(screen.getByRole("button", { name: "Approve and continue" }));
    await user.click(
      await screen.findByRole("button", { name: "Send the test clip and accept the cost" }),
    );

    await screen.findByText(/Capability check on vendor\/catalog-audio-xl completed/);
    const checked = within(choiceFor("vendor/catalog-audio-xl"));
    expect(checked.getAllByText(/Verified/).length).toBeGreaterThan(0);
    expect(checked.queryByText(/Not verified/)).not.toBeInTheDocument();
    expect(screen.getByRole("radio", { name: /Catalog Audio XL/ })).toBeEnabled();
  });

  it("renders an unknown check cost as unknown, never as zero", async () => {
    const backend = cloudBackend();
    backend.addConsent({ model_id: "vendor/catalog-audio-xl", scope_kind: "model_policy" });
    const { user } = renderWithApp(<Harness />, { backend });
    await screen.findByRole("radio", { name: /Scribe 2/ });

    await user.click(
      screen.getByRole("button", { name: /Run capability check on Catalog Audio XL/ }),
    );
    const dialog = await screen.findByRole("dialog", { name: /Run the capability check/ });
    // This model publishes no price, so no estimate is invented.
    expect(
      within(dialog).getByText(/estimate unavailable for this model's pricing basis/),
    ).toBeInTheDocument();

    await user.click(
      within(dialog).getByRole("button", { name: "Send the test clip and accept the cost" }),
    );

    const result = await screen.findByText(/Capability check on vendor\/catalog-audio-xl completed/);
    expect(within(result).getByText("Cost unknown")).toBeInTheDocument();
    expect(result.textContent).not.toMatch(/\$0\.00|USD 0\.000000/);
  });

  it("leaves the model Not verified and shows the envelope when the check fails", async () => {
    const backend = cloudBackend();
    backend.addConsent({ model_id: "vendor/catalog-audio-xl", scope_kind: "model_policy" });
    backend.failures.set(
      "verify_model",
      envelope("PROVIDER_REJECTED", "The provider rejected the test clip.", { retryable: false }),
    );
    const { user } = renderWithApp(<Harness />, { backend });
    await screen.findByRole("radio", { name: /Scribe 2/ });

    await user.click(
      screen.getByRole("button", { name: /Run capability check on Catalog Audio XL/ }),
    );
    await user.click(
      await screen.findByRole("button", { name: "Send the test clip and accept the cost" }),
    );

    expect(await screen.findByText("The provider rejected the test clip.")).toBeInTheDocument();
    const stillUnverified = within(choiceFor("vendor/catalog-audio-xl"));
    expect(stillUnverified.getAllByText(/Not verified/).length).toBeGreaterThan(0);
    expect(screen.getByRole("radio", { name: /Catalog Audio XL/ })).toBeDisabled();
  });

  it("stops a check that would exceed the per-job budget before sending", async () => {
    const backend = cloudBackend();
    backend.addConsent({ model_id: "nimbus/audio-chat-preview", scope_kind: "model_policy" });
    backend.setModels("transcribe", [
      ...(backend.models.transcribe ?? []).map((model) =>
        model.model_id === "nimbus/audio-chat-preview"
          ? {
              ...model,
              pricing: {
                basis: "per_audio_second" as const,
                unit_label: "USD per audio second",
                amount_per_unit: { amount: "1.000000", currency: "USD" },
              },
            }
          : model,
      ),
    ]);
    backend.budget = {
      ...backend.budget,
      per_job_limit: { amount: "0.001000", currency: "USD" },
      blocks_dispatch: true,
    };
    const { user } = renderWithApp(<Harness />, { backend });
    await screen.findByRole("radio", { name: /Scribe 2/ });

    await user.click(
      screen.getByRole("button", { name: /Run capability check on Nimbus Audio Chat/ }),
    );
    await user.click(
      await screen.findByRole("button", { name: "Send the test clip and accept the cost" }),
    );

    expect(
      await screen.findByText(/above your per-job limit of USD 0\.001000\. Nothing was sent\./),
    ).toBeInTheDocument();
    expect(backend.attempts).toHaveLength(0);
  });

  it("has no accessibility violations with the disclosure on screen", async () => {
    const backend = cloudBackend();
    backend.addConsent({ model_id: "vendor/catalog-audio-xl", scope_kind: "model_policy" });
    const { user } = renderWithApp(<Harness />, { backend });
    await screen.findByRole("radio", { name: /Scribe 2/ });
    await user.click(
      screen.getByRole("button", { name: /Run capability check on Catalog Audio XL/ }),
    );
    await screen.findByRole("dialog", { name: /Run the capability check/ });
    await expectNoAxeViolations();
  });
});
