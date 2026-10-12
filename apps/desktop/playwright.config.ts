import { defineConfig, devices } from "@playwright/test";

/**
 * Desktop UI end-to-end suite.
 *
 * The suite drives the real production React bundle in Chromium with the
 * in-memory backend harness installed (`?harness=1`), so it is hermetic: no
 * network, no provider, no Tauri process. Tauri-specific behaviour (command
 * validation, archive writes) is covered by the Rust integration tests.
 *
 * It drives a real Chromium, so it runs on the MARS container runner
 * (`npm run e2e` -> scripts/e2e-remote.sh), never on a workstation.
 */
if (!process.env.PLAUDON_E2E_ALLOW_LOCAL) {
  throw new Error(
    "Playwright drives a real browser: run it through scripts/e2e-remote.sh (npm run e2e). " +
      "Set PLAUDON_E2E_ALLOW_LOCAL=1 to force a local run.",
  );
}

export default defineConfig({
  testDir: "./e2e",
  fullyParallel: true,
  forbidOnly: !!process.env.CI,
  retries: process.env.CI ? 1 : 0,
  reporter: process.env.CI ? [["list"], ["html", { open: "never" }]] : [["list"]],
  use: {
    baseURL: "http://127.0.0.1:5273",
    trace: "retain-on-failure",
    ...devices["Desktop Chrome"],
    // Desktop viewport: the app explicitly does not target narrow screens (§5.6).
    viewport: { width: 1440, height: 900 },
  },
  webServer: {
    command: "npm run dev",
    url: "http://127.0.0.1:5273",
    reuseExistingServer: !process.env.CI,
    timeout: 120_000,
  },
});
