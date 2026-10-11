import type { ReactNode } from "react";

export interface EmptyStateProps {
  title: string;
  body: ReactNode;
  /** Corrective actions required by §5.4 — an empty state always offers a next step. */
  actions?: ReactNode;
}

export function EmptyState({ title, body, actions }: EmptyStateProps) {
  return (
    <div className="empty-state" role="group" aria-label={title}>
      <h2 className="empty-state__title">{title}</h2>
      <p className="empty-state__body">{body}</p>
      {actions ? <div className="empty-state__actions">{actions}</div> : null}
    </div>
  );
}
