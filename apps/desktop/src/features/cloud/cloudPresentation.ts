/**
 * Pure presentation rules for the cloud surfaces.
 *
 * Every label here is derived from a field the backend actually sent. Where a
 * field is `null` or `unknown` the label says so; no default is invented, no
 * missing price becomes zero and no unverified model is described as checked
 * (§8.6, §8.6A, §10.2, §10.4, §10.5, §10.8).
 */
import type {
  CostValue,
  ListModelsResponse,
  ModelCard,
  ModelPricing,
  ModelPrivacy,
  ProcessingStage,
  RequestFamily,
  RetentionPosture,
  TriState,
  VerificationReason,
} from "../../ipc/contract";
import type { StatusTone } from "../../components/StatusBadge";
import { formatMoney } from "./money";

/** §8.6: which OpenRouter request contract the model was shown to implement. */
export const REQUEST_FAMILY_LABELS: Record<RequestFamily, string> = {
  multipart_stt: "Dedicated speech-to-text endpoint",
  audio_chat: "Chat request carrying audio",
  unsupported: "No supported request contract",
  unknown: "Request contract not established",
};

/** §8.6A: the machine reason a model is not verified, in plain words. */
export const VERIFICATION_REASON_LABELS: Record<VerificationReason, string> = {
  not_in_allowlist: "It is not on the tested allowlist.",
  no_observed_exchange: "No request and response has been observed for it.",
  catalog_only: "It appears in the provider catalogue only; nothing has been tested.",
  privacy_route_unresolved: "Its privacy route could not be resolved for this exact endpoint.",
  no_request_builder: "No request builder implements its endpoint contract.",
  user_supplied_custom_id: "You typed this model id, so nothing has been tested for it.",
};

export const RETENTION_LABELS: Record<RetentionPosture, string> = {
  zero_retention: "Documented zero retention",
  limited_retention: "Limited retention",
  provider_default: "Provider default retention",
  unknown: "Retention unknown",
};

export const TRI_STATE_LABELS: Record<TriState, string> = {
  yes: "Yes",
  no: "No",
  unknown: "Unknown",
};

export const DEGRADED_REASON_LABELS: Record<
  NonNullable<ListModelsResponse["degraded_reason"]>,
  string
> = {
  offline: "the provider catalogue could not be reached",
  local_only_mode: "local-only mode is on, so the catalogue was not fetched",
  no_key: "no OpenRouter key is saved, so the catalogue was not fetched",
  catalog_error: "the provider catalogue could not be read",
};

export const STAGE_LABELS: Record<ProcessingStage, string> = {
  transcribe: "Transcription",
  diarize: "Speaker diarization",
  normalize: "Transcript normalization",
  section_summary: "Section summaries",
  consolidated_summary: "Consolidated summary",
  structured_extraction: "Structured extraction",
  citation_verification: "Citation verification",
  tool_run: "Transcript tool run",
};

/**
 * §10.5: zero retention still means the content is processed off the device,
 * and approving it now does not delete anything a provider already holds.
 */
export const ZDR_CAVEAT =
  "Zero retention still processes your content outside this device, and it does not retroactively delete provider data, exports, logs or backups created earlier.";

/**
 * ADR-0022: a model can only become `verified` by an observed exchange, so the
 * check sends this much bundled fixture audio as a real, billable request.
 */
export const CAPABILITY_CHECK_CLIP_MS = 5_000;

/** What the capability check actually does, stated before it is run. */
export const CAPABILITY_CHECK_DISCLOSURE =
  "Running the check sends about 5 seconds of bundled test audio to this model through the provider. It is a real request: it may be billed, and it is the only way the model can become Verified.";

export interface CostPresentation {
  /** Short label; always states which of the three confidences it is. */
  label: string;
  /** Longer sentence for assistive technology and inline notes. */
  detail: string;
  tone: StatusTone;
  /** Text glyph so confidence is never signalled by colour alone (§5.3). */
  glyph: string;
}

/** §10.4: an estimate is never rendered as an amount that was charged. */
export function costPresentation(cost: CostValue): CostPresentation {
  if (cost.confidence === "unknown" || cost.amount === null) {
    return {
      label: "Cost unknown",
      detail: "No cost could be derived for this run, so none is shown.",
      tone: "neutral",
      glyph: "?",
    };
  }
  if (cost.confidence === "estimated") {
    return {
      label: `Estimated ${formatMoney(cost.amount)}`,
      detail: "This is an estimate before dispatch, not an amount you have been charged.",
      tone: "warning",
      glyph: "≈",
    };
  }
  return {
    label: `Final ${formatMoney(cost.amount)}`,
    detail: "Reported by the provider for this request.",
    tone: "success",
    glyph: "=",
  };
}

/** §10.4: a missing unit price is stated as unknown, never printed as zero. */
export function pricingLabel(pricing: ModelPricing): string {
  if (pricing.amount_per_unit === null) {
    return `Price unknown (${pricing.unit_label})`;
  }
  return `${formatMoney(pricing.amount_per_unit)} — ${pricing.unit_label}`;
}

/** §10.8: relay plus the downstream provider, or an explicit non-disclosure. */
export function privacyRouteLabel(privacy: ModelPrivacy): string {
  const downstream = privacy.downstream_provider ?? "provider not disclosed";
  return `${privacy.relay} → ${downstream}`;
}

export function retentionLabel(privacy: ModelPrivacy): string {
  if (privacy.retention === "limited_retention" && privacy.retention_days !== null) {
    return `${RETENTION_LABELS.limited_retention} (${privacy.retention_days} days)`;
  }
  return RETENTION_LABELS[privacy.retention];
}

export interface VerificationPresentation {
  label: "Verified" | "Not verified";
  tone: StatusTone;
  glyph: string;
  /** Machine reason in plain words; empty string when verified. */
  reason: string;
}

/** §8.6A: `Verified` means our smoke test passed, nothing weaker. */
export function verificationPresentation(model: ModelCard): VerificationPresentation {
  if (model.verification === "verified") {
    return { label: "Verified", tone: "success", glyph: "✓", reason: "" };
  }
  return {
    label: "Not verified",
    tone: "warning",
    glyph: "!",
    reason:
      model.verification_reason === null
        ? "No reason was reported."
        : VERIFICATION_REASON_LABELS[model.verification_reason],
  };
}

/** Why a model cannot be dispatched to right now; `null` when it can. */
export function unselectableReason(model: ModelCard): string | null {
  if (model.selectable) return null;
  if (model.verification === "not_verified") {
    return `This model cannot be used yet: ${verificationPresentation(model).reason}`;
  }
  return "This model cannot be used for this stage right now.";
}

/**
 * §10.2 advanced custom model id. A model the user typed has been tested in no
 * way at all, so every fact about it is reported as unknown and it is never
 * marked selectable.
 */
export function customModelCard(modelId: string, engine: string): ModelCard {
  return {
    model_id: modelId,
    display_name: modelId,
    engine,
    request_family: "unknown",
    verification: "not_verified",
    verification_reason: "user_supplied_custom_id",
    fixture_id: null,
    validated_at: null,
    features: {
      segments: "unknown",
      word_timestamps: "unknown",
      diarization: "unknown",
      language_detection: "unknown",
    },
    locales: null,
    max_input_bytes: null,
    max_duration_ms: null,
    pricing: { basis: "other", unit_label: "no pricing retrieved", amount_per_unit: null },
    privacy: {
      relay: engine,
      downstream_provider: null,
      retention: "unknown",
      retention_days: null,
      zero_retention_available: false,
    },
    selectable: false,
  };
}
