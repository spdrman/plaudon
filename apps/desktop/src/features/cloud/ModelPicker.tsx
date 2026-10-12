/**
 * Model picker (§5.2, §10.2). Every model the backend returned is listed —
 * unavailable ones show a reason instead of disappearing — together with the
 * request family it was shown to implement, its verification state and the
 * machine reason behind it, its pricing unit, its resolved privacy route and
 * its retention posture.
 */
import "./cloud.css";
import { useId, useState } from "react";
import type { ModelCard, ProcessingStage } from "../../ipc/contract";
import { useCommandQuery } from "../../ipc/useCommand";
import { Button } from "../../components/Button";
import { TextField } from "../../components/Field";
import { StatusBadge } from "../../components/StatusBadge";
import { CommandErrorNotice } from "../recording/errorNotice";
import {
  DEGRADED_REASON_LABELS,
  REQUEST_FAMILY_LABELS,
  TRI_STATE_LABELS,
  customModelCard,
  pricingLabel,
  privacyRouteLabel,
  retentionLabel,
  unselectableReason,
  verificationPresentation,
} from "./cloudPresentation";

export interface ModelPickerProps {
  stage: ProcessingStage;
  /** Restricts the list to one engine; `null` lists every engine. */
  engine?: string | null;
  /** Currently chosen model id, or `null` when nothing is chosen yet. */
  selectedModelId: string | null;
  onSelect: (model: ModelCard) => void;
  /** Accessible name of the group of choices. */
  legend?: string;
}

export function ModelPicker({
  stage,
  engine = null,
  selectedModelId,
  onSelect,
  legend = "Model",
}: ModelPickerProps) {
  const groupName = useId();
  const [customId, setCustomId] = useState("");
  const [customCards, setCustomCards] = useState<ModelCard[]>([]);
  const { data, error, loading, refetch } = useCommandQuery(
    "list_models",
    { stage, engine },
    { deps: [stage, engine] },
  );

  if (error) {
    return <CommandErrorNotice error={error} onRetry={() => void refetch()} />;
  }

  const models = [...(data?.models ?? []), ...customCards];
  const defaultModelId = data?.default_model_id ?? null;

  return (
    <div className="model-picker">
      {data && data.degraded_reason !== null ? (
        <p className="model-picker__degraded" role="status">
          <span aria-hidden="true">⚑ </span>
          Showing the pinned allowlist because {DEGRADED_REASON_LABELS[data.degraded_reason]}. This
          list is not the live provider catalogue.
        </p>
      ) : null}
      {data && data.degraded_reason === null ? (
        <p className="model-picker__source">
          Live provider catalogue, retrieved {data.catalog_fetched_at ?? "at an unrecorded time"}.
        </p>
      ) : null}

      <fieldset className="model-picker__group">
        <legend className="model-picker__legend">{legend}</legend>
        {loading && models.length === 0 ? (
          <p role="status" aria-live="polite">
            Loading models…
          </p>
        ) : null}
        {!loading && models.length === 0 ? (
          <p role="status">No model is offered for this stage yet.</p>
        ) : null}
        {models.map((model) => (
          <ModelChoice
            key={`${model.engine}:${model.model_id}`}
            model={model}
            groupName={groupName}
            checked={model.model_id === selectedModelId}
            isDefault={model.model_id === defaultModelId}
            onSelect={() => onSelect(model)}
          />
        ))}
      </fieldset>

      <form
        className="model-picker__custom"
        onSubmit={(event) => {
          event.preventDefault();
          const trimmed = customId.trim();
          if (trimmed.length === 0) return;
          if (models.some((model) => model.model_id === trimmed)) return;
          setCustomCards((current) => [...current, customModelCard(trimmed, engine ?? "openrouter")]);
          setCustomId("");
        }}
      >
        <TextField
          label="Advanced: custom model ID"
          value={customId}
          onChange={(event) => setCustomId(event.target.value)}
          hint="A model id you type has never been tested here, so it is always listed as Not verified and cannot be dispatched to."
        />
        <Button type="submit">Add custom model ID</Button>
      </form>
    </div>
  );
}

interface ModelChoiceProps {
  model: ModelCard;
  groupName: string;
  checked: boolean;
  isDefault: boolean;
  onSelect: () => void;
}

function ModelChoice({ model, groupName, checked, isDefault, onSelect }: ModelChoiceProps) {
  const factsId = useId();
  const verification = verificationPresentation(model);
  const blocked = unselectableReason(model);

  return (
    <div className="model-choice" data-model-id={model.model_id}>
      <label className="model-choice__header">
        <input
          type="radio"
          name={groupName}
          value={model.model_id}
          checked={checked}
          disabled={blocked !== null}
          onChange={onSelect}
          aria-describedby={factsId}
        />
        <span className="model-choice__name">{model.display_name}</span>
        <StatusBadge
          tone={verification.tone}
          glyph={verification.glyph}
          label={verification.label}
          description={verification.reason || undefined}
        />
        {isDefault ? <StatusBadge tone="neutral" glyph="★" label="Task default" /> : null}
      </label>

      <dl className="model-choice__facts" id={factsId}>
        <div>
          <dt>Model id</dt>
          <dd className="model-choice__mono">{model.model_id}</dd>
        </div>
        <div>
          <dt>Request family</dt>
          <dd>{REQUEST_FAMILY_LABELS[model.request_family]}</dd>
        </div>
        <div>
          <dt>Verification</dt>
          <dd>
            {verification.label}
            {verification.reason ? ` — ${verification.reason}` : null}
            {model.verification === "verified" && model.validated_at !== null
              ? ` Checked ${model.validated_at}${
                  model.fixture_id === null ? "" : ` against ${model.fixture_id}`
                }.`
              : null}
          </dd>
        </div>
        <div>
          <dt>Price</dt>
          <dd>{pricingLabel(model.pricing)}</dd>
        </div>
        <div>
          <dt>Route</dt>
          <dd>{privacyRouteLabel(model.privacy)}</dd>
        </div>
        <div>
          <dt>Retention</dt>
          <dd>{retentionLabel(model.privacy)}</dd>
        </div>
        <div>
          <dt>Languages</dt>
          <dd>{model.locales === null ? "Unknown" : model.locales.join(", ")}</dd>
        </div>
        <div>
          <dt>Segments</dt>
          <dd>{TRI_STATE_LABELS[model.features.segments]}</dd>
        </div>
        <div>
          <dt>Word timestamps</dt>
          <dd>{TRI_STATE_LABELS[model.features.word_timestamps]}</dd>
        </div>
        <div>
          <dt>Diarization</dt>
          <dd>{TRI_STATE_LABELS[model.features.diarization]}</dd>
        </div>
        <div>
          <dt>Language detection</dt>
          <dd>{TRI_STATE_LABELS[model.features.language_detection]}</dd>
        </div>
      </dl>

      {blocked !== null ? (
        <p className="model-choice__blocked">
          <span aria-hidden="true">✕ </span>
          {blocked}
        </p>
      ) : null}
    </div>
  );
}
