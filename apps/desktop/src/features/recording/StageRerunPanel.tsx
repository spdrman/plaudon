/**
 * Per-recording model override and single-stage re-run (§5.2 processing sheet,
 * §10.2). Choosing a model here is stored against this recording only: the
 * task default is shown separately and is never written by this panel. The
 * estimated charge is shown before anything is dispatched, and a refusal from
 * Rust — consent, budget, local-only mode — is rendered from its envelope.
 */
import "../cloud/cloud.css";
import { useState } from "react";
import type {
  ModelCard,
  ProcessingStage,
  RecordingDetail,
  StartProcessingResponse,
} from "../../ipc/contract";
import { PROCESSING_STAGES } from "../../ipc/contract";
import { useAppState } from "../../app/AppState";
import { useCommandMutation } from "../../ipc/useCommand";
import { Button } from "../../components/Button";
import { SelectField } from "../../components/Field";
import { CloudConsentDialog } from "../cloud/CloudConsentDialog";
import { CostView } from "../cloud/CostView";
import { ModelPicker } from "../cloud/ModelPicker";
import {
  CONSENT_CONTENT_TYPE_BY_STAGE,
  CONSENT_PURPOSE_BY_STAGE,
  STAGE_LABELS,
} from "../cloud/cloudPresentation";
import { estimateDispatchCost } from "../cloud/money";
import { CommandErrorNotice } from "./errorNotice";

export interface StageRerunPanelProps {
  detail: RecordingDetail;
  /** Called with the queued job id so the caller can refresh its views. */
  onQueued: (response: StartProcessingResponse) => void;
}

export function StageRerunPanel({ detail, onQueued }: StageRerunPanelProps) {
  const { capabilities, pushToast } = useAppState();
  const [stage, setStage] = useState<ProcessingStage>("transcribe");
  const [model, setModel] = useState<ModelCard | null>(null);
  const [overrideSaved, setOverrideSaved] = useState<string | null>(null);
  const [consentOpen, setConsentOpen] = useState(false);
  const [dispatched, setDispatched] = useState<StartProcessingResponse | null>(null);

  const setPreference = useCommandMutation("set_model_preference");
  const start = useCommandMutation("start_processing");
  const estimate = model === null ? null : estimateDispatchCost(model, detail.duration_ms);
  const busy = setPreference.pending || start.pending;

  async function onSaveOverride(): Promise<void> {
    if (!model) return;
    try {
      const result = await setPreference.run({
        stage,
        scope: { kind: "recording", recording_id: detail.recording_id },
        engine: model.engine,
        model_id: model.model_id,
      });
      setOverrideSaved(result.model_id);
      pushToast({
        tone: "success",
        title: "Override saved for this recording",
        body: "Your default model for this task is unchanged.",
      });
    } catch {
      /* rendered from setPreference.error */
    }
  }

  /** An active grant covering this model for this recording or any recording. */
  function coveredByConsent(modelId: string): boolean {
    return (capabilities?.cloud_consents ?? []).some(
      (grant) =>
        grant.revoked_at === null &&
        grant.model_id === modelId &&
        (grant.scope_kind === "model_policy" ||
          (grant.scope_kind === "recording" && grant.recording_id === detail.recording_id)),
    );
  }

  /**
   * §4.4: the disclosure comes before the cloud operation, not after a refusal.
   * When no grant on record covers this model we open the dialog ourselves;
   * a refusal from Rust is still rendered if one arrives anyway.
   */
  async function onRerun(justGranted = false): Promise<void> {
    setDispatched(null);
    const needsConsent =
      model !== null && model.engine !== "whisper.cpp" && !coveredByConsent(model.model_id);
    if (!justGranted && needsConsent) {
      setConsentOpen(true);
      return;
    }
    try {
      const response = await start.run({
        recording_id: detail.recording_id,
        stage,
        model_override: model === null ? null : { engine: model.engine, model_id: model.model_id },
        cost_acknowledged: false,
        rerun: true,
      });
      setDispatched(response);
      onQueued(response);
    } catch {
      /* rendered from start.error, including CONSENT_REQUIRED */
    }
  }

  const consentRequired = start.error?.code === "CONSENT_REQUIRED";

  return (
    <section className="stage-rerun" aria-labelledby="stage-rerun-heading">
      <h2 className="stage-rerun__title" id="stage-rerun-heading">
        Re-run a stage
      </h2>
      <p className="stage-rerun__note">
        Only the stage you choose runs again. A model chosen here applies to this recording alone.
      </p>

      <div className="stage-rerun__row">
        <SelectField
          label="Stage"
          value={stage}
          onChange={(event) => {
            setStage(event.target.value as ProcessingStage);
            setModel(null);
            setOverrideSaved(null);
            setDispatched(null);
          }}
        >
          {PROCESSING_STAGES.map((value) => (
            <option key={value} value={value}>
              {STAGE_LABELS[value]}
            </option>
          ))}
        </SelectField>
      </div>

      <ModelPicker
        stage={stage}
        selectedModelId={model?.model_id ?? null}
        onSelect={setModel}
        legend={`Model for ${STAGE_LABELS[stage]} on this recording`}
      />

      <p className="stage-rerun__note">
        Estimated charge before dispatch:{" "}
        {estimate === null ? (
          "choose a model to see an estimate"
        ) : estimate.confidence === "unknown" ? (
          "estimate unavailable for this model's pricing basis"
        ) : (
          <CostView cost={estimate} />
        )}
      </p>

      <div className="stage-rerun__row">
        <Button onClick={() => void onSaveOverride()} disabled={busy || model === null}>
          Use this model for this recording only
        </Button>
        <Button variant="primary" onClick={() => void onRerun()} disabled={busy || model === null}>
          Re-run this stage
        </Button>
      </div>

      {overrideSaved !== null ? (
        <p className="stage-rerun__note" role="status">
          This recording will use {overrideSaved} for {STAGE_LABELS[stage]}. The task default is
          unchanged.
        </p>
      ) : null}

      {dispatched !== null ? (
        <p className="stage-rerun__note" role="status">
          Queued job {dispatched.job_id} for {STAGE_LABELS[stage]}. Estimated charge:{" "}
          <CostView cost={dispatched.estimated_cost} />
          {dispatched.blocked_reason === null ? null : ` ${dispatched.blocked_reason}`}
        </p>
      ) : null}

      {setPreference.error ? <CommandErrorNotice error={setPreference.error} /> : null}
      {start.error ? (
        <CommandErrorNotice error={start.error}>
          {consentRequired && model !== null ? (
            <Button variant="primary" onClick={() => setConsentOpen(true)}>
              Review and approve
            </Button>
          ) : null}
        </CommandErrorNotice>
      ) : null}

      {model !== null ? (
        <CloudConsentDialog
          open={consentOpen}
          model={model}
          purpose={CONSENT_PURPOSE_BY_STAGE[stage]}
          contentType={CONSENT_CONTENT_TYPE_BY_STAGE[stage]}
          recordingId={detail.recording_id}
          estimatedCost={estimate ?? { confidence: "unknown", amount: null }}
          onClose={() => setConsentOpen(false)}
          onGranted={(result) => {
            setConsentOpen(false);
            if (result.authorized) {
              start.reset();
              void onRerun(true);
            } else {
              pushToast({
                tone: "info",
                title: "Preference saved",
                body: "Ask every time does not authorise this run, so nothing was sent.",
              });
            }
          }}
        />
      ) : null}
    </section>
  );
}
