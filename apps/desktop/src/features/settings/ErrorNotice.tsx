import "./settings.css";
import type { ReactNode } from "react";
import type { ErrorEnvelope } from "../../ipc/contract";
import { Button } from "../../components/Button";
import { useAppState } from "../../app/AppState";

export interface ErrorNoticeProps {
  envelope: ErrorEnvelope;
  /** Retry affordance; also the fallback for a retryable envelope with no action. */
  onRetry?: () => void;
  /** Jump to the engine/model picker for `CHOOSE_ANOTHER_MODEL`. */
  onChooseAnotherModel?: () => void;
  children?: ReactNode;
}

/**
 * §15.4 / §5.4: we render the envelope message plus the control its `action`
 * implies. `action` may be absent; a retryable envelope then falls back to a
 * Retry control. Stack text is never shown — only the diagnostic id.
 */
export function ErrorNotice({
  envelope,
  onRetry,
  onChooseAnotherModel,
  children,
}: ErrorNoticeProps) {
  const { navigate } = useAppState();
  const action = envelope.action ?? null;

  const retryControl =
    onRetry != null ? (
      <Button variant="primary" onClick={onRetry}>
        Retry
      </Button>
    ) : null;

  const control = (() => {
    switch (action) {
      case "RETRY_IMPORT":
      case "WAIT_AND_RETRY":
        return retryControl;
      case "CHOOSE_ANOTHER_MODEL":
        return onChooseAnotherModel ? (
          <Button onClick={onChooseAnotherModel}>Choose another model</Button>
        ) : null;
      case "FREE_DISK_SPACE":
        return <Button onClick={() => navigate({ name: "settings" })}>Open storage settings</Button>;
      case "REVIEW_LOCAL_ONLY_MODE":
        return (
          <Button onClick={() => navigate({ name: "settings" })}>Review local-only mode</Button>
        );
      case "REVIEW_CONSENT":
      case "OPEN_SETTINGS":
        return <Button onClick={() => navigate({ name: "settings" })}>Open settings</Button>;
      case "OPEN_DIAGNOSTICS":
        return <Button onClick={() => navigate({ name: "settings.activity" })}>Open diagnostics</Button>;
      case null:
        return envelope.retryable ? retryControl : null;
      default:
        return envelope.retryable ? retryControl : null;
    }
  })();

  return (
    <div className="error-notice" role="alert">
      <p className="error-notice__message">
        <span aria-hidden="true">! </span>
        {envelope.message}
      </p>
      <p className="error-notice__diagnostic">Diagnostic id: {envelope.diagnostic_id}</p>
      {control || children ? (
        <div className="error-notice__actions">
          {control}
          {children}
        </div>
      ) : null}
    </div>
  );
}
