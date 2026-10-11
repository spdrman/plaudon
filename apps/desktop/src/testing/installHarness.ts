import { FakeBackend } from "./fakeBackend";
import { setTransport } from "../ipc/client";

declare global {
  interface Window {
    /** Exposed only by the dev harness so e2e tests can drive the fake backend. */
    __plaudonHarness?: FakeBackend;
  }
}

export interface HarnessOptions {
  recordings?: number;
  segments?: number;
  onboarded?: boolean;
  notificationPermission?: "granted" | "denied" | "not_requested" | "unsupported";
}

/**
 * Installs the in-memory backend as the IPC transport. Only called from
 * `main.tsx` under `import.meta.env.DEV` with an explicit `?harness=1` flag,
 * and from the Playwright suite.
 */
export function installHarness(options: HarnessOptions = {}): FakeBackend {
  const params = new URLSearchParams(window.location.search);
  const recordings = options.recordings ?? Number(params.get("recordings") ?? 12);
  const segments = options.segments ?? Number(params.get("segments") ?? 40);
  const onboarded = options.onboarded ?? params.get("onboarded") !== "0";
  const backend = new FakeBackend({
    recordings,
    segmentsPerTranscript: segments,
    osNotificationPermission:
      options.notificationPermission ??
      ((params.get("notifications") as HarnessOptions["notificationPermission"]) ?? "granted"),
  });
  backend.preferences = {
    ...backend.preferences,
    onboarding_completed: onboarded,
    default_stt_engine_id: onboarded ? "whisper.cpp" : null,
  };
  setTransport(backend);
  window.__plaudonHarness = backend;
  return backend;
}
