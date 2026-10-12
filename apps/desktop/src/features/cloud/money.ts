/**
 * Exact-decimal money handling for the cloud surfaces (§10.4).
 *
 * Amounts travel as plain decimal strings and are compared and summed as
 * integer micro-units, so nothing is ever rounded into existence. An amount
 * that is not known stays `null`; it is never substituted with zero.
 */
import type { CostValue, ModelCard, MoneyAmount } from "../../ipc/contract";

const MICROS = 1_000_000;

/** Parses a plain decimal string into integer micro-units; `null` when invalid. */
export function parseMicros(value: string): number | null {
  const trimmed = value.trim();
  if (!/^\d+(\.\d{1,6})?$/.test(trimmed)) return null;
  const [whole, fraction = ""] = trimmed.split(".");
  return Number(whole) * MICROS + Number(fraction.padEnd(6, "0"));
}

/** Renders integer micro-units back as a six-decimal string. */
export function formatMicros(micros: number): string {
  const whole = Math.trunc(micros / MICROS);
  const fraction = String(Math.abs(micros) % MICROS).padStart(6, "0");
  return `${whole}.${fraction}`;
}

export function usd(micros: number): MoneyAmount {
  return { amount: formatMicros(micros), currency: "USD" };
}

/**
 * Money is shown as currency code plus the exact decimal we were given. We do
 * not localise or round it: a shortened "$0.00" would claim a precision — and
 * for sub-cent amounts a value — the data does not carry.
 */
export function formatMoney(amount: MoneyAmount): string {
  return `${amount.currency} ${amount.amount}`;
}

/**
 * Pre-dispatch estimate used for the §4.4 disclosure. Only a duration-based
 * unit price is calculable before a request is built; token and request based
 * pricing stays `unknown` rather than being guessed at.
 */
export function estimateDispatchCost(model: ModelCard, durationMs: number | null): CostValue {
  const unit = model.pricing.amount_per_unit;
  if (model.pricing.basis !== "per_audio_second" || unit === null || durationMs === null) {
    return { confidence: "unknown", amount: null };
  }
  const perSecond = parseMicros(unit.amount);
  if (perSecond === null) return { confidence: "unknown", amount: null };
  return {
    confidence: "estimated",
    amount: {
      amount: formatMicros(Math.round((perSecond * durationMs) / 1000)),
      currency: unit.currency,
    },
  };
}
