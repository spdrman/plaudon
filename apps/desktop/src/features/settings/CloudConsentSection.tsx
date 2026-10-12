/**
 * Cloud consent register (§4.4 #6, §16.3). Every grant stays visible after it
 * is withdrawn, marked as revoked with the time it happened, so the record of
 * what was once allowed is never quietly erased.
 */
import "./settings.css";
import "../cloud/cloud.css";
import type { ConsentSummary } from "../../ipc/contract";
import { useAppState } from "../../app/AppState";
import { useCommandMutation } from "../../ipc/useCommand";
import { Button } from "../../components/Button";
import { StatusBadge } from "../../components/StatusBadge";
import { RETENTION_LABELS } from "../cloud/cloudPresentation";
import { ErrorNotice } from "./ErrorNotice";

const SCOPE_LABELS: Record<ConsentSummary["scope_kind"], string> = {
  ask_every_time: "Ask every time",
  recording: "This recording only",
  model_policy: "This model policy",
};

const PURPOSE_LABELS: Record<ConsentSummary["purpose"], string> = {
  transcription: "Transcription",
  summarization: "Summarization",
  transcript_tool: "Transcript tool",
  credential_validation: "Credential validation",
};

const CONTENT_LABELS: Record<ConsentSummary["content_type"], string> = {
  audio_bytes: "Audio bytes",
  transcript_text: "Transcript text",
  metadata_only: "Metadata only",
};

export function CloudConsentSection() {
  const { capabilities, refreshCapabilities } = useAppState();
  const revoke = useCommandMutation("revoke_cloud_consent");
  const grants = capabilities?.cloud_consents ?? [];

  async function onRevoke(consentId: string): Promise<void> {
    try {
      await revoke.run({ consent_id: consentId });
      await refreshCapabilities();
    } catch {
      /* rendered from revoke.error */
    }
  }

  return (
    <>
      <p className="settings-section__note">
        Revoking stops future cloud runs under that grant. It does not recall content already sent
        or delete anything a provider already holds.
      </p>
      {grants.length === 0 ? (
        <p className="settings-section__note">No cloud consent has been granted.</p>
      ) : (
        <ul className="cloud-consents">
          {grants.map((grant) => (
            <li className="cloud-consents__item" key={grant.consent_id}>
              <div className="cloud-consents__row">
                <span>{grant.model_id}</span>
                <StatusBadge
                  tone={grant.revoked_at === null ? "success" : "neutral"}
                  glyph={grant.revoked_at === null ? "✓" : "✕"}
                  label={grant.revoked_at === null ? "Active" : "Revoked"}
                  description={
                    grant.revoked_at === null
                      ? undefined
                      : `Revoked on ${grant.revoked_at}`
                  }
                />
              </div>
              <dl className="settings-facts">
                <dt>Purpose</dt>
                <dd>{PURPOSE_LABELS[grant.purpose]}</dd>
                <dt>Data allowed</dt>
                <dd>{CONTENT_LABELS[grant.content_type]}</dd>
                <dt>Scope</dt>
                <dd>
                  {SCOPE_LABELS[grant.scope_kind]}
                  {grant.recording_id === null ? "" : ` (${grant.recording_id})`}
                </dd>
                <dt>Downstream provider</dt>
                <dd>{grant.downstream_provider ?? "Not disclosed"}</dd>
                <dt>Required retention</dt>
                <dd>
                  {grant.required_retention === null
                    ? "Not recorded"
                    : RETENTION_LABELS[grant.required_retention]}
                </dd>
                <dt>Granted</dt>
                <dd>{grant.granted_at}</dd>
                <dt>Expires</dt>
                <dd>{grant.expires_at ?? "No expiry recorded"}</dd>
                {grant.revoked_at === null ? null : (
                  <>
                    <dt>Revoked</dt>
                    <dd>{grant.revoked_at}</dd>
                  </>
                )}
                <dt>Disclosure version</dt>
                <dd>{grant.disclosure_version}</dd>
              </dl>
              {grant.revoked_at === null ? (
                <div>
                  <Button
                    variant="danger"
                    disabled={revoke.pending}
                    onClick={() => void onRevoke(grant.consent_id)}
                  >
                    Revoke consent for {grant.model_id}
                  </Button>
                </div>
              ) : null}
            </li>
          ))}
        </ul>
      )}
      {revoke.error ? <ErrorNotice envelope={revoke.error.envelope} /> : null}
    </>
  );
}
