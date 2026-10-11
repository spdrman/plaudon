import { useCallback, useMemo, useState } from "react";
import "./jobs.css";
import type { JobKind, JobState, JobSummary } from "../../ipc/contract";
import { JOB_STATES } from "../../ipc/contract";
import { useCommandMutation, useCommandQuery } from "../../ipc/useCommand";
import { useAppState } from "../../app/AppState";
import { useRefetchOnEvents } from "../../app/useRefetchOnEvents";
import { Button } from "../../components/Button";
import { EmptyState } from "../../components/EmptyState";
import { JobRow } from "./JobRow";
import { JOB_KIND_LABELS, JOB_STATE_PRESENTATION, type ControlIntent } from "./jobPresentation";

const JOB_EVENTS = [
  "job.updated",
  "job.failed",
  "transcript.completed",
  "recording.imported",
] as const;

const JOB_KINDS = Object.keys(JOB_KIND_LABELS) as JobKind[];

export interface JobsScreenProps {
  /** Row to scroll to and focus, from a deep link. */
  focusJobId: string | null;
  onOpenTarget: (target: { kind: "recording" | "job" | "export"; id: string }) => void;
}

export function JobsScreen({ focusJobId, onOpenTarget }: JobsScreenProps) {
  const { capabilities, navigate, pushToast } = useAppState();
  const [states, setStates] = useState<JobState[]>([]);
  const [kinds, setKinds] = useState<JobKind[]>([]);

  const query = useCommandQuery(
    "list_jobs",
    { cursor: null, limit: 50, states, kinds },
    { deps: [states.join(","), kinds.join(",")] },
  );
  const { data, error, loading, refetch } = query;
  const refetchJobs = useCallback(() => {
    void refetch();
  }, [refetch]);
  useRefetchOnEvents(JOB_EVENTS, refetchJobs);

  const cancel = useCommandMutation("cancel_job");
  const retry = useCommandMutation("retry_job");
  const busy = cancel.pending || retry.pending;

  const counts = data?.state_counts ?? {};
  const countedStates = useMemo(
    () => JOB_STATES.filter((state) => (counts[state] ?? 0) > 0 || states.includes(state)),
    [counts, states],
  );

  const onCancel = useCallback(
    async (job: JobSummary) => {
      try {
        await cancel.run({ job_id: job.job_id });
      } catch {
        // The envelope is rendered from `cancel.error`; nothing to derive here.
      }
      void refetch();
    },
    [cancel, refetch],
  );

  const onRetry = useCallback(
    async (job: JobSummary, costAcknowledged: boolean) => {
      try {
        await retry.run({ job_id: job.job_id, cost_acknowledged: costAcknowledged });
      } catch {
        // Rendered below from `retry.error`.
      }
      void refetch();
    },
    [retry, refetch],
  );

  const onControl = useCallback(
    (intent: ControlIntent, job: JobSummary) => {
      switch (intent) {
        case "keep_queued":
          pushToast({
            tone: "info",
            title: "Kept queued",
            body: `${job.label} stays in the queue and will not be sent anywhere else.`,
          });
          return;
        case "open_storage_settings":
        case "free_space":
        case "open_ai_settings":
        case "choose_local_engine":
        case "review_local_only_mode":
        case "review_consent":
        case "open_settings":
        case "open_permission_settings":
        case "open_plaud_connect":
        case "open_cli_setup":
        case "open_diagnostics":
        case "open_device_diagnostics":
        case "refresh_source_link":
        case "charge_device":
          navigate({ name: "settings" });
          return;
        case "retry":
        case "retry_with_cost_acknowledgement":
        case "retry_import":
          // Cost acknowledgement is only ever granted through the row dialog.
          if (job.cost_acknowledgement_required) return;
          void onRetry(job, false);
          return;
      }
    },
    [navigate, onRetry, pushToast],
  );

  const items = data?.items ?? [];
  const mutationError = cancel.error ?? retry.error;

  return (
    <section className="jobs" aria-labelledby="jobs-heading">
      <h1 id="jobs-heading" className="jobs__heading">
        Jobs
      </h1>

      <div className="jobs__filters">
        <fieldset className="jobs__filter-group">
          <legend className="jobs__filter-legend">Filter by state</legend>
          {countedStates.length === 0 ? (
            <p className="jobs__hint">No job states to filter yet.</p>
          ) : (
            countedStates.map((state) => {
              const active = states.includes(state);
              return (
                <Button
                  key={state}
                  aria-pressed={active}
                  onClick={() =>
                    setStates((current) =>
                      current.includes(state)
                        ? current.filter((s) => s !== state)
                        : [...current, state],
                    )
                  }
                >
                  {JOB_STATE_PRESENTATION[state].label} ({counts[state] ?? 0})
                </Button>
              );
            })
          )}
        </fieldset>
        <fieldset className="jobs__filter-group">
          <legend className="jobs__filter-legend">Filter by kind</legend>
          {JOB_KINDS.map((kind) => {
            const active = kinds.includes(kind);
            return (
              <Button
                key={kind}
                aria-pressed={active}
                onClick={() =>
                  setKinds((current) =>
                    current.includes(kind) ? current.filter((k) => k !== kind) : [...current, kind],
                  )
                }
              >
                {JOB_KIND_LABELS[kind]}
              </Button>
            );
          })}
        </fieldset>
      </div>

      <p className="jobs__status" role="status">
        {loading ? "Loading jobs…" : `${items.length} job${items.length === 1 ? "" : "s"} shown`}
      </p>

      {error ? (
        <div className="jobs__error" role="alert">
          <p>{error.envelope.message}</p>
          <Button onClick={refetchJobs}>Retry</Button>
        </div>
      ) : null}

      {mutationError ? (
        <p className="jobs__error" role="alert">
          {mutationError.envelope.message}
        </p>
      ) : null}

      {!loading && !error && items.length === 0 ? (
        <EmptyState
          title="No jobs"
          body="Nothing is being processed right now. Imports and transcriptions appear here while they run, and stay here afterwards."
          actions={<Button onClick={refetchJobs}>Refresh</Button>}
        />
      ) : null}

      {items.length > 0 ? (
        <ul className="jobs__list">
          {items.map((job) => (
            <JobRow
              key={job.job_id}
              job={job}
              focused={job.job_id === focusJobId}
              busy={busy}
              freeBytes={capabilities?.storage.free_bytes ?? null}
              onOpenTarget={onOpenTarget}
              onControl={onControl}
              onCancel={(target) => void onCancel(target)}
              onRetry={(target, acknowledged) => void onRetry(target, acknowledged)}
            />
          ))}
        </ul>
      ) : null}
    </section>
  );
}
