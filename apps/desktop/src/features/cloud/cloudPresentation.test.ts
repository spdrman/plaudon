import { describe, expect, it } from "vitest";
import type { ModelCard } from "../../ipc/contract";
import {
  costPresentation,
  customModelCard,
  pricingLabel,
  privacyRouteLabel,
  retentionLabel,
  unselectableReason,
  verificationPresentation,
} from "./cloudPresentation";
import { estimateDispatchCost, formatMoney, parseMicros } from "./money";

const BASE: ModelCard = customModelCard("vendor/model", "openrouter");

function withPricing(pricing: Partial<ModelCard["pricing"]>): ModelCard {
  return { ...BASE, pricing: { ...BASE.pricing, ...pricing } };
}

describe("cost presentation", () => {
  it("never renders an estimate as a finalized amount", () => {
    const estimated = costPresentation({
      confidence: "estimated",
      amount: { amount: "0.006000", currency: "USD" },
    });
    expect(estimated.label).toBe("Estimated USD 0.006000");
    expect(estimated.label).not.toMatch(/Final/);
    expect(estimated.glyph).toBe("≈");

    const finalized = costPresentation({
      confidence: "finalized",
      amount: { amount: "0.006000", currency: "USD" },
    });
    expect(finalized.label).toBe("Final USD 0.006000");
    expect(finalized.glyph).not.toBe(estimated.glyph);
  });

  it("renders an unknown cost as unknown rather than zero", () => {
    const unknown = costPresentation({ confidence: "unknown", amount: null });
    expect(unknown.label).toBe("Cost unknown");
    expect(unknown.label).not.toMatch(/0/);
  });

  it("treats a missing amount as unknown even if the confidence claims otherwise", () => {
    expect(costPresentation({ confidence: "finalized", amount: null }).label).toBe("Cost unknown");
  });
});

describe("money", () => {
  it("keeps the exact decimal it was given", () => {
    expect(formatMoney({ amount: "0.004212", currency: "USD" })).toBe("USD 0.004212");
  });

  it("rejects an amount that is not a plain decimal", () => {
    expect(parseMicros("1.5")).toBe(1_500_000);
    expect(parseMicros("lots")).toBeNull();
    expect(parseMicros("-1")).toBeNull();
    expect(parseMicros("1.1234567")).toBeNull();
  });

  it("estimates only from a duration-based unit price", () => {
    const perSecond = withPricing({
      basis: "per_audio_second",
      unit_label: "USD per audio second",
      amount_per_unit: { amount: "0.000100", currency: "USD" },
    });
    expect(estimateDispatchCost(perSecond, 60_000)).toEqual({
      confidence: "estimated",
      amount: { amount: "0.006000", currency: "USD" },
    });
    // No duration, no estimate — and never a zero one.
    expect(estimateDispatchCost(perSecond, null)).toEqual({ confidence: "unknown", amount: null });
    // Token pricing cannot be estimated before a request is built.
    const perToken = withPricing({
      basis: "per_token",
      unit_label: "USD per token",
      amount_per_unit: { amount: "0.000002", currency: "USD" },
    });
    expect(estimateDispatchCost(perToken, 60_000)).toEqual({
      confidence: "unknown",
      amount: null,
    });
  });
});

describe("model facts", () => {
  it("states an unknown unit price as unknown", () => {
    expect(pricingLabel(BASE.pricing)).toBe("Price unknown (no pricing retrieved)");
    expect(
      pricingLabel({
        basis: "per_token",
        unit_label: "USD per token",
        amount_per_unit: { amount: "0.000002", currency: "USD" },
      }),
    ).toBe("USD 0.000002 — USD per token");
  });

  it("never guesses the downstream provider", () => {
    expect(privacyRouteLabel(BASE.privacy)).toBe("openrouter → provider not disclosed");
    expect(
      privacyRouteLabel({ ...BASE.privacy, downstream_provider: "Acme" }),
    ).toBe("openrouter → Acme");
  });

  it("reports the retention window only when it is known", () => {
    expect(retentionLabel(BASE.privacy)).toBe("Retention unknown");
    expect(
      retentionLabel({ ...BASE.privacy, retention: "limited_retention", retention_days: 30 }),
    ).toBe("Limited retention (30 days)");
    expect(retentionLabel({ ...BASE.privacy, retention: "limited_retention" })).toBe(
      "Limited retention",
    );
  });

  it("marks a user supplied id as not verified and not dispatchable", () => {
    const card = customModelCard("acme/anything", "openrouter");
    expect(card.verification).toBe("not_verified");
    expect(card.verification_reason).toBe("user_supplied_custom_id");
    expect(card.selectable).toBe(false);
    expect(verificationPresentation(card).label).toBe("Not verified");
    expect(unselectableReason(card)).toMatch(/You typed this model id/);
  });

  it("has no blocking reason for a selectable model", () => {
    expect(unselectableReason({ ...BASE, selectable: true })).toBeNull();
  });
});
