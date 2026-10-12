export interface ProgressProps {
  label: string;
  /** 0..1, or null when the backend reports no measurable progress. */
  value: number | null;
  /** Extra text shown next to the bar, e.g. elapsed time. */
  detail?: string;
}

/**
 * Non-blocking progress (§5.3). An unknown ratio renders an honest
 * indeterminate bar; we never invent a percentage.
 */
export function Progress({ label, value, detail }: ProgressProps) {
  const pct = value === null ? null : Math.max(0, Math.min(1, value));
  const text = pct === null ? "in progress, amount unknown" : `${Math.round(pct * 100)}%`;
  return (
    <div className="progress">
      <div
        className="progress__track"
        role="progressbar"
        aria-label={label}
        aria-valuemin={pct === null ? undefined : 0}
        aria-valuemax={pct === null ? undefined : 100}
        aria-valuenow={pct === null ? undefined : Math.round(pct * 100)}
        aria-valuetext={text}
      >
        <div
          className={`progress__fill${pct === null ? " progress__fill--indeterminate" : ""}`}
          style={pct === null ? undefined : { width: `${pct * 100}%` }}
        />
      </div>
      <span className="field__hint">
        {text}
        {detail ? ` · ${detail}` : ""}
      </span>
    </div>
  );
}
