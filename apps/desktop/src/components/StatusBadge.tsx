/**
 * Status is never communicated by color alone (§5.3): every badge renders a
 * text label plus a monochrome glyph, and the color role is supplementary.
 */
export type StatusTone = "neutral" | "running" | "waiting" | "success" | "warning" | "danger";

export interface StatusBadgeProps {
  tone: StatusTone;
  /** Short glyph (text, not color) duplicating the tone for color-blind users. */
  glyph: string;
  label: string;
  /** Optional longer description exposed to assistive technology. */
  description?: string;
}

export function StatusBadge({ tone, glyph, label, description }: StatusBadgeProps) {
  return (
    <span
      className={`status-badge status-badge--${tone}`}
      data-tone={tone}
      title={description ?? label}
    >
      <span className="status-badge__glyph" aria-hidden="true">
        {glyph}
      </span>
      <span>{label}</span>
      {description ? <span className="sr-only">{description}</span> : null}
    </span>
  );
}
