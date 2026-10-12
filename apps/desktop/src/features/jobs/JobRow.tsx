import { useEffect, useRef, useState } from "react";
import type { JobSummary } from "../../ipc/contract";
import { Button } from "../../components/Button";
import { Dialog } from "../../components/Dialog";
import { Progress } from "../../components/Progress";
import { StatusBadge } from "../../components/StatusBadge";
import {
  CANCEL_DISCLOSURE,
  JOB_KIND_LABELS,
  JOB_STATE_PRESENTATION,
  envelopeActionControl,
  errorGuidance,
  jobElapsedLabel,
  type ActionControl,
  type ControlIntent,
} from "./jobPresentation";

export interface JobRowProps {
  job: JobSummary;
  /** Scroll to and focus this row (deep link from a notification or route). */
  focused: boolean;
  busy: boolean;
  /** Free bytes reported by capabilities; `null` when unknown. */
  freeBytes: number | null;
  onOpenTarget: (target: { kind: "recording" | "job" | "export"; id: string }) => void;
  onControl: (intent: ControlIntent, job: JobSummary) => void;
  onCancel: (job: JobSummary) => void;
  onRetry: (job: JobSummary, costAcknowledged: boolean) => void;
}

export function JobRow({
  job,
  focused,
  busy,
  freeBytes,
  onOpenTarget,
  onControl,
  onCancel,
  onRetry,
}: JobRowProps) {
  const rowRef = useRef<HTMLLIElement | null>(null);
  const [cancelOpen, setCancelOpen] = useState(false);
  const [costOpen, setCostOpen] = useState(false);
  const [costAcknowledged, setCostAcknowledged] = useState(false);

  useEffect(() => {
    if (!focused) return;
    const node = rowRef.current;
    if (!node) return;
    node.scrollIntoView?.({ block: "nearest" });
    node.focus();
  }, [focused]);

  const presentation = JOB_STATE_PRESENTATION[job.state];
  const guidance = errorGuidance(job.error?.code);
  const secondary = job.error
    ? envelopeActionControl(job.error.action, job.error.retryable)
    : null;
  const costAckRequired = presentation.requiresCostAck || job.cost_acknowledgement_required;
  const retryAllowed = presentation.canRetry && !(guidance?.suppressRetry ?? false);

  const controls: ActionControl[] = [];
  for (const control of guidance?.controls ?? []) {
    if (control.intent === "retry" && !retryAllowed) continue;
    controls.push(control);
  }
  if (secondary && !controls.some((c) => c.label === secondary.label)) {
    if (!(secondary.intent === "retry" && !retryAllowed)) controls.push(secondary);
  }

  const kindLabel = JOB_KIND_LABELS[job.kind];
  const elapsed = jobElapsedLabel(job.started_at, job.finished_at);
  const showProgress = !presentation.terminal && job.state !== "cloud_outcome_unknown";
  const outputLink = job.output_link;

  return (
    <li className="job-row" ref={rowRef} tabIndex={-1} data-job-id={job.job_id}>
      <div className="job-row__head">
        <div className="job-row__identity">
          <span className="job-row__kind">{kindLabel}</span>
          <span className="job-row__label">{job.label}</span>
        </div>
        <StatusBadge
          tone={presentation.tone}
          glyph={presentation.glyph}
          label={presentation.label}
          description={presentation.description}
        />
      </div>

      <dl className="job-row__facts">
        <div className="job-row__fact">
          <dt>Stage</dt>
          <dd>{job.stage ?? "Not reported"}</dd>
        </div>
        <div className="job-row__fact">
          <dt>Elapsed</dt>
          <dd>{elapsed}</dd>
        </div>
      </dl>

      {showProgress ? (
        <Progress label={`${kindLabel} progress for ${job.label}`} value={job.progress} />
      ) : null}

      {job.state === "cancelling" || job.state === "cancelled" ? (
        <p className="job-row__note">{CANCEL_DISCLOSURE}</p>
      ) : null}

      {job.state === "cloud_outcome_unknown" ? (
        <section className="job-row__panel" aria-label={`Cloud outcome unknown for ${job.label}`}>
          <h2 className="job-row__panel-title">Cloud request outcome unknown</h2>
          <p>
            The provider response was lost after the request was dispatched. The request may
            already have been accepted and billed, so it is never retried automatically.
          </p>
          <p>
            Provider request id:{" "}
            <span className="job-row__mono">{job.provider_request_id ?? "not reported"}</span>
          </p>
        </section>
      ) : null}

      {job.error ? (
        <div className="job-row__error" role="group" aria-label={`Error for ${job.label}`}>
          <p className="job-row__error-title">
            <span aria-hidden="true">! </span>
            {guidance?.title ?? "This job stopped"}
          </p>
          <p className="job-row__error-message">{job.error.message}</p>
          {guidance?.showCapacity ? (
            <dl className="job-row__facts">
              <div className="job-row__fact">
                <dt>Needed</dt>
                <dd>{job.error.message}</dd>
              </div>
              <div className="job-row__fact">
                <dt>Available</dt>
                <dd>{freeBytes === null ? "Unknown" : formatBytes(freeBytes)}</dd>
              </div>
            </dl>
          ) : null}
          {guidance?.suppressRetry ? (
            <p className="job-row__note">
              Billable retries are stopped until your provider credits are restored.
            </p>
          ) : null}
          <div className="job-row__actions">
            {controls.map((control) => (
              <Button
                key={`${control.intent}-${control.label}`}
                onClick={() =>
                  control.intent === "retry"
                    ? startRetry()
                    : control.intent === "keep_queued"
                      ? onControl("keep_queued", job)
                      : onControl(control.intent, job)
                }
                disabled={busy}
              >
                {control.label}
              </Button>
            ))}
          </div>
        </div>
      ) : null}

      <div className="job-row__actions">
        {presentation.canCancel ? (
          <Button onClick={() => setCancelOpen(true)} disabled={busy}>
            Cancel
          </Button>
        ) : null}
        {retryAllowed && !job.error ? (
          <Button onClick={startRetry} disabled={busy}>
            Retry
          </Button>
        ) : null}
        {outputLink ? (
          <Button variant="quiet" onClick={() => onOpenTarget(outputLink)}>
            Open result
          </Button>
        ) : null}
      </div>

      <Dialog
        open={cancelOpen}
        destructive
        title={`Cancel ${kindLabel.toLowerCase()}?`}
        description={<p>{CANCEL_DISCLOSURE}</p>}
        onClose={() => setCancelOpen(false)}
        footer={
          <>
            <Button onClick={() => setCancelOpen(false)}>Keep running</Button>
            <Button
              variant="danger"
              onClick={() => {
                setCancelOpen(false);
                onCancel(job);
              }}
            >
              Cancel this job
            </Button>
          </>
        }
      />

      <Dialog
        open={costOpen}
        title="This retry may be billed again"
        description={
          <p>
            The earlier request may already have been accepted and billed by the provider. Retrying
            sends a new request, which may be billed a second time.
          </p>
        }
        onClose={closeCostDialog}
        footer={
          <>
            <Button onClick={closeCostDialog}>Keep as is</Button>
            <Button
              variant="primary"
              disabled={!costAcknowledged}
              onClick={() => {
                closeCostDialog();
                onRetry(job, true);
              }}
            >
              Retry and accept the cost
            </Button>
          </>
        }
      >
        <p className="job-row__panel-title">
          Provider request id:{" "}
          <span className="job-row__mono">{job.provider_request_id ?? "not reported"}</span>
        </p>
        <label className="job-row__check">
          <input
            type="checkbox"
            checked={costAcknowledged}
            onChange={(event) => setCostAcknowledged(event.target.checked)}
          />
          I understand this retry may incur a new charge.
        </label>
      </Dialog>
    </li>
  );

  function startRetry(): void {
    if (costAckRequired) {
      setCostAcknowledged(false);
      setCostOpen(true);
      return;
    }
    onRetry(job, false);
  }

  function closeCostDialog(): void {
    setCostOpen(false);
    setCostAcknowledged(false);
  }
}

const UNITS = ["bytes", "KB", "MB", "GB", "TB"] as const;

export function formatBytes(bytes: number): string {
  let value = bytes;
  let unit = 0;
  while (value >= 1000 && unit < UNITS.length - 1) {
    value /= 1000;
    unit += 1;
  }
  const rounded = unit === 0 ? String(Math.round(value)) : value.toFixed(1);
  return `${rounded} ${UNITS[unit]}`;
}
