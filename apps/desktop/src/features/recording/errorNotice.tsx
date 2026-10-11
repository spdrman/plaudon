/**
 * §15.4 error rendering for the recording detail view: always the envelope
 * message plus the control its `action` implies. Stack text and diagnostics
 * payloads are never shown — only the diagnostic id the user can quote.
 */
import type { ReactNode } from "react";
import { Button } from "../../components/Button";
import { useAppState } from "../../app/AppState";
import type { CommandError } from "../../ipc/client";
import type { ErrorAction } from "../../ipc/contract";

interface ActionAffordance {
  label: string;
  /** Where the corrective control takes the user, when it is a destination. */
  destination?: "settings" | "activity";
}

const ACTION_AFFORDANCES: Record<ErrorAction, ActionAffordance> = {
  OPEN_PLAUD_CONNECT: { label: "Open PLAUD sources", destination: "settings" },
  OPEN_CLI_SETUP: { label: "Open CLI setup", destination: "settings" },
  OPEN_SETTINGS: { label: "Open settings", destination: "settings" },
  OPEN_PERMISSION_SETTINGS: { label: "Open permission settings", destination: "settings" },
  RETRY_IMPORT: { label: "Retry import" },
  REFRESH_SOURCE_LINK: { label: "Refresh source link" },
  REVIEW_CONSENT: { label: "Review consent", destination: "settings" },
  FREE_DISK_SPACE: { label: "Open storage settings", destination: "settings" },
  REVIEW_LOCAL_ONLY_MODE: { label: "Review local-only mode", destination: "settings" },
  OPEN_DEVICE_DIAGNOSTICS: { label: "Open device diagnostics", destination: "settings" },
  CHARGE_DEVICE: { label: "Charge the device" },
  OPEN_DIAGNOSTICS: { label: "Open diagnostics", destination: "settings" },
  CHOOSE_ANOTHER_MODEL: { label: "Choose another engine", destination: "settings" },
  WAIT_AND_RETRY: { label: "Retry" },
};

export interface CommandErrorNoticeProps {
  error: CommandError;
  /** Invoked by the Retry control; omit when retrying is meaningless here. */
  onRetry?: () => void;
  /** Extra resolution controls, e.g. the non-destructive conflict choices. */
  children?: ReactNode;
}

export function CommandErrorNotice({ error, onRetry, children }: CommandErrorNoticeProps) {
  const { navigate } = useAppState();
  const envelope = error.envelope;
  const action = envelope.action ?? null;
  const affordance = action ? ACTION_AFFORDANCES[action] : null;
  // No declared affordance: a retryable error still gets a Retry control.
  const showRetry =
    onRetry !== undefined && (action === null || action === "WAIT_AND_RETRY" || envelope.retryable);

  return (
    <div className="error-notice" role="alert">
      <p className="error-notice__message">{envelope.message}</p>
      <div className="error-notice__actions">
        {showRetry ? (
          <Button glyph="↻" onClick={onRetry}>
            Retry
          </Button>
        ) : null}
        {affordance && affordance.label !== "Retry" ? (
          <Button
            onClick={() => {
              if (affordance.destination === "settings") navigate({ name: "settings" });
              else if (affordance.destination === "activity") navigate({ name: "settings.activity" });
              else onRetry?.();
            }}
          >
            {affordance.label}
          </Button>
        ) : null}
        {children}
      </div>
      {envelope.diagnostic_id ? (
        <p className="error-notice__diagnostic">Diagnostic reference: {envelope.diagnostic_id}</p>
      ) : null}
    </div>
  );
}
