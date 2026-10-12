import { describe, expect, it } from "vitest";
import { screen, within } from "@testing-library/react";
import { renderWithApp } from "../../testing/renderWithApp";
import { expectNoAxeViolations } from "../../testing/axe";
import { FakeBackend } from "../../testing/fakeBackend";
import { CloudConsentSection } from "./CloudConsentSection";

function seeded(): FakeBackend {
  const backend = new FakeBackend({ recordings: 1 });
  backend.addConsent({
    model_id: "acme/scribe-2",
    purpose: "transcription",
    content_type: "audio_bytes",
    scope_kind: "model_policy",
    downstream_provider: "Acme Speech",
    required_retention: "limited_retention",
  });
  return backend;
}

describe("CloudConsentSection", () => {
  it("lists a grant with the facts it was given under", async () => {
    renderWithApp(<CloudConsentSection />, { backend: seeded() });
    const item = (await screen.findByText("acme/scribe-2")).closest("li")!;

    expect(within(item).getByText("Active")).toBeInTheDocument();
    expect(within(item).getByText("Transcription")).toBeInTheDocument();
    expect(within(item).getByText("Audio bytes")).toBeInTheDocument();
    expect(within(item).getByText("This model policy")).toBeInTheDocument();
    expect(within(item).getByText("Acme Speech")).toBeInTheDocument();
    expect(within(item).getByText("Limited retention")).toBeInTheDocument();
    expect(within(item).getByText("2026-02-01")).toBeInTheDocument();
  });

  it("revokes a grant and keeps it on record as revoked", async () => {
    const backend = seeded();
    const { user } = renderWithApp(<CloudConsentSection />, { backend });
    await screen.findByText("acme/scribe-2");

    await user.click(screen.getByRole("button", { name: /Revoke consent for acme\/scribe-2/ }));

    expect(await screen.findByTitle(/^Revoked on /)).toBeInTheDocument();
    expect(backend.consents[0]?.revoked_at).not.toBeNull();
    // The record stays visible; it is not deleted.
    expect(screen.getByText("acme/scribe-2")).toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: /Revoke consent for acme\/scribe-2/ }),
    ).not.toBeInTheDocument();
  });

  it("does not claim a revocation recalls content already sent", async () => {
    renderWithApp(<CloudConsentSection />, { backend: seeded() });
    await screen.findByText("acme/scribe-2");
    expect(
      screen.getByText(
        /It does not recall content already sent or delete anything a provider already holds\./,
      ),
    ).toBeInTheDocument();
  });

  it("says so when nothing has been granted", async () => {
    renderWithApp(<CloudConsentSection />, { backend: new FakeBackend() });
    expect(await screen.findByText("No cloud consent has been granted.")).toBeInTheDocument();
  });

  it("has no accessibility violations", async () => {
    renderWithApp(<CloudConsentSection />, { backend: seeded() });
    await screen.findByText("acme/scribe-2");
    await expectNoAxeViolations();
  });
});
