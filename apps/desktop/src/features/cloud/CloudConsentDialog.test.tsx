import { describe, expect, it, vi } from "vitest";
import { screen } from "@testing-library/react";
import { renderWithApp } from "../../testing/renderWithApp";
import { expectNoAxeViolations } from "../../testing/axe";
import { FakeBackend } from "../../testing/fakeBackend";
import type { ConsentScopeKind, CostValue, ModelCard } from "../../ipc/contract";
import { CONSENT_DISCLOSURE_VERSION, CloudConsentDialog } from "./CloudConsentDialog";

const ESTIMATE: CostValue = {
  confidence: "estimated",
  amount: { amount: "0.006000", currency: "USD" },
};

function modelFrom(backend: FakeBackend, modelId: string): ModelCard {
  const card = backend.models.transcribe?.find((model) => model.model_id === modelId);
  if (!card) throw new Error(`seeded catalogue has no ${modelId}`);
  return card;
}

function setup(
  options: {
    estimatedCost?: CostValue;
    onGranted?: (result: { scopeKind: ConsentScopeKind; authorized: boolean }) => void;
    modelId?: string;
  } = {},
) {
  const backend = new FakeBackend({ recordings: 1 });
  backend.enableCloud();
  const onGranted = options.onGranted ?? vi.fn();
  const rendered = renderWithApp(
    <CloudConsentDialog
      open
      model={modelFrom(backend, options.modelId ?? "acme/scribe-2")}
      purpose="transcription"
      contentType="audio_bytes"
      recordingId="rec-00001"
      estimatedCost={options.estimatedCost ?? ESTIMATE}
      onClose={() => {}}
      onGranted={onGranted}
    />,
    { backend },
  );
  return { ...rendered, backend, onGranted };
}

describe("CloudConsentDialog", () => {
  it("discloses every §4.4 item before the first cloud operation", async () => {
    setup();
    await screen.findByRole("dialog", { name: /Send this content to a cloud provider/ });

    // Provider and model.
    expect(screen.getByText("Scribe 2 (acme/scribe-2)")).toBeInTheDocument();
    expect(screen.getByText("openrouter → Acme Speech")).toBeInTheDocument();
    // Data transfer type.
    expect(screen.getAllByText("The recording's audio bytes are uploaded.").length).toBeGreaterThan(
      0,
    );
    // Retention posture.
    expect(screen.getByText("Limited retention (30 days)")).toBeInTheDocument();
    // Estimated charge, labelled as an estimate.
    expect(screen.getByText("Estimated USD 0.006000")).toBeInTheDocument();
    // Editable spending cap.
    expect(screen.getByLabelText("Spending cap for a single run (USD)")).toBeEnabled();
    // Versioned privacy statement.
    expect(
      screen.getByText(`Disclosure version ${CONSENT_DISCLOSURE_VERSION}`),
    ).toBeInTheDocument();
    // The ZDR caveat.
    expect(
      screen.getByText(/does not retroactively delete provider data, exports, logs or backups/),
    ).toBeInTheDocument();
  });

  it("offers the three consent scopes", async () => {
    setup();
    await screen.findByRole("dialog");
    for (const label of [
      "Ask every time",
      "Allow for this recording",
      "Allow for this model policy",
    ]) {
      expect(screen.getByRole("radio", { name: new RegExp(label) })).toBeInTheDocument();
    }
  });

  it("says an estimate is unavailable rather than showing a zero charge", async () => {
    setup({ estimatedCost: { confidence: "unknown", amount: null } });
    await screen.findByRole("dialog");
    expect(
      screen.getByText("Estimate unavailable for this model's pricing basis."),
    ).toBeInTheDocument();
    expect(screen.queryByText(/USD 0\.000000/)).not.toBeInTheDocument();
  });

  it("stores a model policy grant, the spending cap and the disclosure version", async () => {
    const { user, backend, onGranted } = setup();
    await screen.findByRole("dialog");

    await user.type(screen.getByLabelText("Spending cap for a single run (USD)"), "2.50");
    await user.click(screen.getByRole("radio", { name: /Allow for this model policy/ }));
    await user.click(screen.getByRole("button", { name: "Approve and continue" }));

    expect(onGranted).toHaveBeenCalledWith({ scopeKind: "model_policy", authorized: true });
    expect(backend.consents).toHaveLength(1);
    expect(backend.consents[0]).toMatchObject({
      model_id: "acme/scribe-2",
      scope_kind: "model_policy",
      content_type: "audio_bytes",
      disclosure_version: CONSENT_DISCLOSURE_VERSION,
      revoked_at: null,
    });
    expect(backend.budget.per_job_limit).toEqual({ amount: "2.500000", currency: "USD" });
  });

  it("records 'ask every time' without authorizing the dispatch", async () => {
    const { user, backend, onGranted } = setup();
    await screen.findByRole("dialog");

    expect(
      screen.getByRole("button", { name: "Save preference without sending" }),
    ).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Save preference without sending" }));

    expect(onGranted).toHaveBeenCalledWith({ scopeKind: "ask_every_time", authorized: false });
    expect(backend.consents[0]?.scope_kind).toBe("ask_every_time");
  });

  it("fails closed when a required zero-retention route cannot be resolved", async () => {
    const { user, backend, onGranted } = setup({ modelId: "vendor/catalog-audio-xl" });
    await screen.findByRole("dialog");

    await user.click(screen.getByRole("checkbox", { name: /Require documented zero retention/ }));
    await user.click(screen.getByRole("radio", { name: /Allow for this model policy/ }));
    await user.click(screen.getByRole("button", { name: "Approve and continue" }));

    expect(
      await screen.findByText(
        /Zero retention could not be confirmed for this exact route, so nothing was sent\./,
      ),
    ).toBeInTheDocument();
    expect(onGranted).not.toHaveBeenCalled();
    expect(backend.consents).toHaveLength(0);
  });

  it("has no accessibility violations", async () => {
    setup();
    await screen.findByRole("dialog");
    await expectNoAxeViolations();
  });
});
