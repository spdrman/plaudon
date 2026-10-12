import { useAppState } from "./AppState";
import { Button } from "../components/Button";

/**
 * Toasts are announced through a polite live region and never steal focus
 * (§5.3). Every toast is also recorded in the activity history by the backend.
 */
export function ToastRegion() {
  const { toasts, dismissToast } = useAppState();
  return (
    <div
      className="toast-region"
      role="status"
      aria-live="polite"
      aria-relevant="additions text"
      aria-label="Notifications"
    >
      {toasts.map((toast) => (
        <div className="toast" key={toast.id} data-tone={toast.tone}>
          <span className="toast__title">
            <span aria-hidden="true">{TONE_GLYPH[toast.tone]} </span>
            {toast.title}
          </span>
          {toast.body ? <span className="toast__body">{toast.body}</span> : null}
          <div className="dialog__actions">
            <Button variant="quiet" onClick={() => dismissToast(toast.id)}>
              Dismiss
            </Button>
          </div>
        </div>
      ))}
    </div>
  );
}

const TONE_GLYPH: Record<string, string> = {
  info: "i",
  success: "OK",
  warning: "!",
  danger: "X",
};
