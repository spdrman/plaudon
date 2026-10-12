import { FakeBackend } from "./fakeBackend";
import { setTransport } from "../ipc/client";

declare global {
  interface Window {
    /** Exposed only by the dev harness so e2e tests can drive the fake backend. */
    __plaudonHarness?: FakeBackend;
    /** Paths the harness file picker returns; set by a test before importing. */
    __plaudonPickerPaths?: string[];
    /** Destination the harness save dialog returns. */
    __plaudonSavePath?: string;
    __TAURI_INTERNALS__?: { invoke: (cmd: string, payload?: unknown) => Promise<unknown> };
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
  installDialogBridge();
  window.__plaudonHarness = backend;
  return backend;
}

/**
 * Minimal stand-in for the Tauri IPC bridge so the OS file picker and save
 * dialog resolve in the browser harness. Only the two dialog commands the UI
 * is allowed to use are answered; everything else rejects, which is what makes
 * the drag-and-drop code fall back to DOM events outside Tauri.
 */
function installDialogBridge(): void {
  window.__plaudonPickerPaths ??= ["/Users/tester/Audio/standup.mp3"];
  window.__plaudonSavePath ??= "/Users/tester/Desktop/transcript.txt";
  window.__TAURI_INTERNALS__ = {
    invoke: async (command: string) => {
      if (command === "plugin:dialog|open") return window.__plaudonPickerPaths ?? null;
      if (command === "plugin:dialog|save") return window.__plaudonSavePath ?? null;
      throw new Error(`harness: command ${command} is not available outside Tauri`);
    },
  };
}
