/**
 * Renders a §15.4 envelope the way §5.4 requires: the human message plus the
 * one control the declared `action` implies. Never a stack trace, never a bare
 * code, and never a dead end — an envelope with no action but `retryable` still
 * offers Retry.
 */
import type { ErrorEnvelope } from "../../ipc/contract";
import { Button } from "../../components/Button";
import { useAppState } from "../../app/AppState";

export type ErrorControl =
  | { kind: "none" }
  | { kind: "retry"; label: string }
  | { kind: "settings"; label: string };

/**
 * Maps an envelope to its corrective control. `action` is optional in the
 * contract: Rust omits it for codes with no affordance.
 */
export function envelopeActionControl(envelope: ErrorEnvelope): ErrorControl {
  switch (envelope.action ?? null) {
    case "RETRY_IMPORT":
      return { kind: "retry", label: "Retry import" };
    case "WAIT_AND_RETRY":
      return { kind: "retry", label: "Retry" };
    case "REFRESH_SOURCE_LINK":
      return { kind: "retry", label: "Refresh and retry" };
    case "FREE_DISK_SPACE":
      return { kind: "settings", label: "Open storage settings" };
    case "CHOOSE_ANOTHER_MODEL":
      return { kind: "settings", label: "Choose another engine" };
    case "REVIEW_LOCAL_ONLY_MODE":
      return { kind: "settings", label: "Review local-only mode" };
    case "REVIEW_CONSENT":
      return { kind: "settings", label: "Review cloud consent" };
    case "OPEN_SETTINGS":
      return { kind: "settings", label: "Open settings" };
    case "OPEN_PLAUD_CONNECT":
      return { kind: "settings", label: "Open PLAUD connection settings" };
    case "OPEN_CLI_SETUP":
      return { kind: "settings", label: "Open PLAUD CLI setup" };
    case "OPEN_PERMISSION_SETTINGS":
      return { kind: "settings", label: "Open permission settings" };
    case "OPEN_DEVICE_DIAGNOSTICS":
    case "OPEN_DIAGNOSTICS":
      return { kind: "settings", label: "Open diagnostics" };
    case "CHARGE_DEVICE":
      return { kind: "none" };
    default:
      return envelope.retryable ? { kind: "retry", label: "Retry" } : { kind: "none" };
  }
}

export interface ErrorNoticeProps {
  envelope: ErrorEnvelope;
  onRetry?: () => void;
  /** Short statement of what failed, e.g. "Recordings could not be loaded." */
  context?: string;
}

export function ErrorNotice({ envelope, onRetry, context }: ErrorNoticeProps) {
  const { navigate } = useAppState();
  const control = envelopeActionControl(envelope);
  return (
    <div className="error-notice" role="alert">
      <p className="error-notice__message">
        <span aria-hidden="true" className="status-badge__glyph">
          ✕
        </span>
        {context ? `${context} ` : ""}
        {envelope.message}
      </p>
      <div className="error-notice__actions">
        {control.kind === "retry" && onRetry ? (
          <Button variant="primary" onClick={onRetry}>
            {control.label}
          </Button>
        ) : null}
        {control.kind === "settings" ? (
          <Button onClick={() => navigate({ name: "settings" })}>{control.label}</Button>
        ) : null}
      </div>
      {envelope.diagnostic_id ? (
        <p className="error-notice__diagnostic">Diagnostic id: {envelope.diagnostic_id}</p>
      ) : null}
    </div>
  );
}
