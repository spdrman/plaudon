import { describe, expect, it, vi } from "vitest";
import { screen, within } from "@testing-library/react";
import { useState } from "react";
import { renderWithApp } from "../../testing/renderWithApp";
import { expectNoAxeViolations } from "../../testing/axe";
import { FakeBackend } from "../../testing/fakeBackend";
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
