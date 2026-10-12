import type {
  PlaudCompatibility,
  PlaudConnectorState,
  PlaudNextAction,
} from "../../ipc/contract";

/**
 * PLAUD's own documentation for the official command line tool (§4.2.2). The
 * app links to it and never installs anything itself.
 */
export const PLAUD_CLI_DOCS_URL = "https://support.plaud.ai/hc/en-us/articles/57751026815257-Plaud-CLI";

/**
 * One word per connector state. These are labels for a state, not sentences:
 * everything that explains a state comes from Rust (§7.6).
 */
export const STATE_LABELS: Record<PlaudConnectorState, string> = {
  unconfigured: "Not connected",
  cli_detected: "Tool found",
  auth_required: "Sign-in needed",
  authenticated: "Signed in",
  enumerating: "Listing",
  ready: "Ready",
  expired_auth: "Session expired",
  incompatible_version: "Untested version",
  error: "Stopped",
};

/** The compatibility verdict word (§7.1). */
export const COMPATIBILITY_LABELS: Record<PlaudCompatibility, string> = {
  unknown: "Unknown",
  tested: "Tested",
  untested: "Untested",
};

/** The operation a control triggers; the screen decides how to run it. */
export type ControlRun = "connect" | "retry" | "review_local_only_mode";

export type NextActionControl =
  | { kind: "link"; label: string; href: string }
  | { kind: "action"; label: string; run: ControlRun }
  | { kind: "import_manually" };

/**
 * §5.4 / §4.2: every `next_action` becomes one concrete control. The label is
 * the name of the control; the sentence that says *why* is the diagnostics
 * detail, rendered verbatim next to it.
 */
export function nextActionControl(action: PlaudNextAction): NextActionControl | null {
  switch (action.kind) {
    case "none":
      return null;
    case "install_cli":
      return { kind: "link", label: "Open PLAUD's installation guide", href: PLAUD_CLI_DOCS_URL };
    case "install_tested_version":
      return {
        kind: "link",
        label: "Open PLAUD's installation guide",
        href: PLAUD_CLI_DOCS_URL,
      };
    case "sign_in":
      return { kind: "action", label: "Sign in to PLAUD", run: "connect" };
    case "retry":
    case "wait_and_retry":
      return { kind: "action", label: "Try again", run: "retry" };
    case "review_local_only_mode":
      return { kind: "action", label: "Review Local-only Mode", run: "review_local_only_mode" };
    case "import_manually":
      return { kind: "import_manually" };
    case "check_account_and_region":
      return { kind: "action", label: "Sign in again", run: "connect" };
    default: {
      const never: never = action;
      throw new Error(`unhandled next action ${JSON.stringify(never)}`);
    }
  }
}

/** Whether the connector can list recordings from this state. */
export function canList(state: PlaudConnectorState): boolean {
  return state === "authenticated" || state === "ready" || state === "enumerating";
}

/** `m:ss` / `h:mm:ss`, or null when the source did not report a duration. */
export function formatDuration(ms: number | null): string | null {
  if (ms === null) return null;
  const total = Math.round(ms / 1000);
  const hours = Math.floor(total / 3600);
  const minutes = Math.floor((total % 3600) / 60);
  const seconds = String(total % 60).padStart(2, "0");
  return hours > 0 ? `${hours}:${String(minutes).padStart(2, "0")}:${seconds}` : `${minutes}:${seconds}`;
}
