import { defineConfig, devices } from "@playwright/test";

/**
 * Desktop UI end-to-end suite.
 *
 * The suite drives the real production React bundle in Chromium with the
 * in-memory backend harness installed (`?harness=1`), so it is hermetic: no
 * network, no provider, no Tauri process. Tauri-specific behaviour (command
 * validation, archive writes) is covered by the Rust integration tests.
 */
export default defineConfig({
  testDir: "./e2e",
  fullyParallel: true,
  forbidOnly: !!process.env.CI,
  retries: process.env.CI ? 1 : 0,
  reporter: process.env.CI ? [["list"], ["html", { open: "never" }]] : [["list"]],
  use: {
    baseURL: "http://127.0.0.1:5273",
    trace: "retain-on-failure",
    // Desktop viewport: the app explicitly does not target narrow screens (§5.6).
    viewport: { width: 1440, height: 900 },
    ...devices["Desktop Chrome"],
  },
  webServer: {
    command: "npm run dev",
    url: "http://127.0.0.1:5273",
    reuseExistingServer: !process.env.CI,
    timeout: 120_000,
  },
});
