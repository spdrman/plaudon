import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { App } from "./app/App";
import { AppStateProvider } from "./app/AppState";
import { createTauriTransport, setTransport } from "./ipc/client";

async function bootstrap(): Promise<void> {
  if (import.meta.env.DEV && new URLSearchParams(window.location.search).has("harness")) {
    // Development/e2e harness: in-memory backend, never bundled into release.
    const { installHarness } = await import("./testing/installHarness");
    installHarness();
  } else {
    setTransport(
      createTauriTransport({
        invoke: (cmd, args) => invoke(cmd, args),
        listen: (event, handler) => listen(event, (e) => handler({ payload: e.payload })),
      }),
    );
  }

  const container = document.getElementById("root");
  if (!container) throw new Error("missing #root");
  createRoot(container).render(
    <StrictMode>
      <AppStateProvider>
        <App />
      </AppStateProvider>
    </StrictMode>,
  );
}

void bootstrap();
