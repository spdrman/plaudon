/**
 * Cloud spending (§10.4, §10.7): per-job and monthly local limits, the
 * month-to-date figure with its estimate/finalized/unknown split intact, and
 * the backend's fixed enforcement note explaining that a local budget cannot
 * stop charges made elsewhere with the same key.
 */
import "./settings.css";
import "../cloud/cloud.css";
import { useEffect, useState, type FormEvent } from "react";
import { useCommandMutation, useCommandQuery } from "../../ipc/useCommand";
import { Button } from "../../components/Button";
import { TextField } from "../../components/Field";
import { CostView } from "../cloud/CostView";
import { formatMoney } from "../cloud/money";
import { ErrorNotice } from "./ErrorNotice";

/** Calendar month containing `now`, as the half-open range `get_usage` takes. */
export function monthWindow(now: Date): { from: string; to: string } {
  const from = new Date(Date.UTC(now.getUTCFullYear(), now.getUTCMonth(), 1));
  const to = new Date(Date.UTC(now.getUTCFullYear(), now.getUTCMonth() + 1, 1));
  return { from: from.toISOString(), to: to.toISOString() };
}

export function CloudBudgetSection() {
  // The month is fixed on mount so a long-lived view does not silently reframe.
  const [range] = useState(() => monthWindow(new Date()));
  const { data, error, loading, refetch } = useCommandQuery("get_usage", range);
  const save = useCommandMutation("set_cloud_budget");
  const [perJob, setPerJob] = useState("");
  const [monthly, setMonthly] = useState("");

  const budget = data?.budget ?? null;
  useEffect(() => {
    if (!budget) return;
    setPerJob(budget.per_job_limit?.amount ?? "");
    setMonthly(budget.monthly_limit?.amount ?? "");
  }, [budget]);

  async function onSubmit(event: FormEvent): Promise<void> {
    event.preventDefault();
    try {
      await save.run({
        per_job_limit: perJob.trim() === "" ? null : perJob.trim(),
        monthly_limit: monthly.trim() === "" ? null : monthly.trim(),
        currency: "USD",
      });
      await refetch();
    } catch {
      /* rendered from save.error */
    }
  }

  if (error) {
    return <ErrorNotice envelope={error.envelope} onRetry={() => void refetch()} />;
  }

  return (
    <div className="cloud-budget">
      {data === null ? (
        <p role="status" aria-live="polite">
          {loading ? "Loading cloud spending…" : "Cloud spending is not available."}
        </p>
      ) : (
        <>
          <dl className="settings-facts">
            <dt>Spent this month</dt>
            <dd>
              <CostView cost={data.budget.month_to_date} showDetail />
            </dd>
            <dt>Runs with no derivable cost</dt>
            <dd>
              {data.totals.unknown_count === 0
                ? "None"
                : `${data.totals.unknown_count} (not counted in the figures above)`}
            </dd>
            <dt>Finalized so far</dt>
            <dd>
              {data.totals.finalized === null
                ? "No finalized amount has been reported yet"
                : formatMoney(data.totals.finalized)}
            </dd>
            <dt>Estimated only</dt>
            <dd>
              {data.totals.estimated === null
                ? "No estimate-only runs"
                : `${formatMoney(data.totals.estimated)} (estimate, not charged amounts)`}
            </dd>
            <dt>Per-job limit</dt>
            <dd>
              {data.budget.per_job_limit === null
                ? "No limit set"
                : formatMoney(data.budget.per_job_limit)}
            </dd>
            <dt>Monthly limit</dt>
            <dd>
              {data.budget.monthly_limit === null
                ? "No limit set"
                : formatMoney(data.budget.monthly_limit)}
            </dd>
            <dt>Local enforcement</dt>
            <dd>
              {data.budget.blocks_dispatch
                ? "A locally calculable limit is set, so a run that would exceed it is stopped before anything is sent."
                : "No local limit is set, so no run is stopped here."}
            </dd>
          </dl>

          <form className="cloud-budget__form" onSubmit={(event) => void onSubmit(event)}>
            <TextField
              label="Per-job limit (USD)"
              inputMode="decimal"
              value={perJob}
              onChange={(event) => setPerJob(event.target.value)}
              hint="Leave empty for no per-job limit."
            />
            <TextField
              label="Monthly limit (USD)"
              inputMode="decimal"
              value={monthly}
              onChange={(event) => setMonthly(event.target.value)}
              hint="Leave empty for no monthly limit."
            />
            <Button type="submit" variant="primary" disabled={save.pending}>
              Save budget
            </Button>
          </form>

          <p className="cloud-budget__note">{data.budget.enforcement_note}</p>
        </>
      )}
      {save.error ? <ErrorNotice envelope={save.error.envelope} /> : null}
    </div>
  );
}
