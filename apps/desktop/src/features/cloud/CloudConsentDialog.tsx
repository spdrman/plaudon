/**
 * Cloud AI consent (§4.4). Before the first cloud operation we disclose the
 * provider and model, what leaves the device, the retention posture, the
 * estimated charge when it is measurable, and an editable spending cap. The
 * user then picks one of the three §4.4 scopes. The versioned disclosure id is
 * stored with the decision, and the §10.5 zero-retention caveat is always
 * shown — approving now never deletes data a provider already holds.
 */
import "./cloud.css";
import { useState } from "react";
import type {
  ConsentContentType,
  ConsentPurpose,
  ConsentScopeKind,
  CostValue,
  ModelCard,
} from "../../ipc/contract";
import { useAppState } from "../../app/AppState";
import { useCommandMutation } from "../../ipc/useCommand";
import { Button } from "../../components/Button";
import { Dialog } from "../../components/Dialog";
import { TextField } from "../../components/Field";
import { ErrorNotice } from "../settings/ErrorNotice";
import { CostView } from "./CostView";
import { ZDR_CAVEAT, privacyRouteLabel, retentionLabel } from "./cloudPresentation";

/** Version of the privacy statement these words correspond to (§4.4 #5). */
export const CONSENT_DISCLOSURE_VERSION = "2026-02-01";

const CONTENT_TYPE_LABELS: Record<ConsentContentType, string> = {
  audio_bytes: "The recording's audio bytes are uploaded.",
  transcript_text: "The transcript text is uploaded.",
  metadata_only: "Only metadata is sent; no audio or transcript leaves the device.",
};

const PURPOSE_LABELS: Record<ConsentPurpose, string> = {
  transcription: "Transcription",
  summarization: "Summarization",
  transcript_tool: "Transcript tool",
  credential_validation: "Credential validation",
};

const SCOPES: Array<{ kind: ConsentScopeKind; label: string; description: string }> = [
  {
    kind: "ask_every_time",
    label: "Ask every time",
    description:
      "Records the preference only. It does not authorise this run, so nothing will be sent now.",
  },
  {
    kind: "recording",
    label: "Allow for this recording",
    description: "Covers further cloud runs for this recording with this model.",
  },
  {
    kind: "model_policy",
    label: "Allow for this model policy",
    description: "Covers any recording processed with this model until you revoke it.",
  },
];

export interface CloudConsentDialogProps {
  open: boolean;
  model: ModelCard;
  purpose: ConsentPurpose;
  contentType: ConsentContentType;
  /** Required to offer the per-recording scope; `null` hides that choice. */
  recordingId: string | null;
  /** Pre-dispatch estimate; `unknown` renders as "estimate unavailable". */
  estimatedCost: CostValue;
  onClose: () => void;
  /**
   * Called after the grant is stored. `authorized` is false for
   * `ask_every_time`, which never authorises a dispatch: the caller must not
   * retry the operation in that case.
   */
  onGranted: (result: { scopeKind: ConsentScopeKind; authorized: boolean }) => void;
}

export function CloudConsentDialog({
  open,
  model,
  purpose,
  contentType,
  recordingId,
  estimatedCost,
  onClose,
  onGranted,
}: CloudConsentDialogProps) {
  const { refreshCapabilities } = useAppState();
  const grant = useCommandMutation("grant_cloud_consent");
  const setBudget = useCommandMutation("set_cloud_budget");
  const [scopeKind, setScopeKind] = useState<ConsentScopeKind>("ask_every_time");
  const [cap, setCap] = useState("");
  const [requireZdr, setRequireZdr] = useState(false);

  const scopes = SCOPES.filter((scope) => scope.kind !== "recording" || recordingId !== null);
  const authorizes = scopeKind !== "ask_every_time";
  const pending = grant.pending || setBudget.pending;

  async function onApprove(): Promise<void> {
    try {
      const trimmedCap = cap.trim();
      if (trimmedCap.length > 0) {
        await setBudget.run({
          per_job_limit: trimmedCap,
          monthly_limit: null,
          currency: "USD",
        });
      }
      await grant.run({
        purpose,
        content_type: contentType,
        scope: { kind: scopeKind, recording_id: scopeKind === "recording" ? recordingId : null },
        model_id: model.model_id,
        downstream_provider: model.privacy.downstream_provider,
        require_zero_retention: requireZdr,
        disclosure_version: CONSENT_DISCLOSURE_VERSION,
      });
      await refreshCapabilities();
      onGranted({ scopeKind, authorized: authorizes });
    } catch {
      /* rendered from grant.error / setBudget.error */
    }
  }

  return (
    <Dialog
      open={open}
      title="Send this content to a cloud provider?"
      description={
        <p>
          {PURPOSE_LABELS[purpose]} would be performed by a model reached through{" "}
          {model.privacy.relay}. {CONTENT_TYPE_LABELS[contentType]}
        </p>
      }
      onClose={onClose}
      footer={
        <>
          <Button onClick={onClose}>Cancel</Button>
          <Button variant="primary" onClick={() => void onApprove()} disabled={pending}>
            {authorizes ? "Approve and continue" : "Save preference without sending"}
          </Button>
        </>
      }
    >
      <dl className="cloud-consent__facts">
        <dt>Model</dt>
        <dd>
          {model.display_name} ({model.model_id})
        </dd>
        <dt>Provider route</dt>
        <dd>{privacyRouteLabel(model.privacy)}</dd>
        <dt>Data transferred</dt>
        <dd>{CONTENT_TYPE_LABELS[contentType]}</dd>
        <dt>Retention posture</dt>
        <dd>{retentionLabel(model.privacy)}</dd>
        <dt>Estimated charge</dt>
        <dd>
          {estimatedCost.confidence === "unknown" ? (
            "Estimate unavailable for this model's pricing basis."
          ) : (
            <CostView cost={estimatedCost} />
          )}
        </dd>
      </dl>

      <div className="cloud-consent__section">
        <TextField
          label="Spending cap for a single run (USD)"
          inputMode="decimal"
          value={cap}
          onChange={(event) => setCap(event.target.value)}
          hint="Saved as your per-job budget. Leave empty to keep the current limit."
        />
        <label className="cloud-consent__scope">
          <input
            type="checkbox"
            checked={requireZdr}
            onChange={(event) => setRequireZdr(event.target.checked)}
          />
          <span>
            Require documented zero retention. If it cannot be confirmed for this exact route, the
            request is refused rather than sent.
          </span>
        </label>
        <p className="cloud-consent__caveat">{ZDR_CAVEAT}</p>
      </div>

      <fieldset className="cloud-consent__section">
        <legend>How long should this apply?</legend>
        {scopes.map((scope) => (
          <label className="cloud-consent__scope" key={scope.kind}>
            <input
              type="radio"
              name="cloud-consent-scope"
              value={scope.kind}
              checked={scopeKind === scope.kind}
              onChange={() => setScopeKind(scope.kind)}
            />
            <span>
              <strong>{scope.label}</strong>
              <span className="cloud-consent__caveat"> {scope.description}</span>
            </span>
          </label>
        ))}
      </fieldset>

      <p className="cloud-consent__version">Disclosure version {CONSENT_DISCLOSURE_VERSION}</p>

      {grant.error ? <ErrorNotice envelope={grant.error.envelope} /> : null}
      {setBudget.error ? <ErrorNotice envelope={setBudget.error.envelope} /> : null}
    </Dialog>
  );
}
