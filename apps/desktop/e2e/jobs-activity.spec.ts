import { auditPage, expect, gotoApp, test } from "./fixtures";

/**
 * E2.3 / E2.4: job states are explicit, a possibly-billed request never
 * auto-retries, and completion events stay visible in the app even when the
 * OS refuses notifications.
 */
test.describe("jobs", () => {
  test("renders every job state distinctly and never by colour alone", async ({ page }) => {
    await gotoApp(page, { recordings: 2 });
    const states = await page.evaluate(() => {
      const harness = window.__plaudonHarness!;
      const all = [
        "queued",
        "preflight",
        "waiting_for_user",
        "waiting_for_network",
        "waiting_for_model",
        "waiting_for_source",
        "waiting_for_authorization",
        "running_local",
        "cloud_ready",
        "cloud_sending",
        "cloud_outcome_unknown",
        "verifying",
        "committing",
        "cancelling",
        "completed",
        "failed_retryable",
        "failed_terminal",
        "cancelled",
      ] as const;
      for (const state of all) {
        harness.addJob({ kind: "transcribe", state, label: `Job ${state}` });
      }
      harness.emit("job.updated");
      return all;
    });

    await page.getByRole("tab", { name: /Jobs/ }).click();
    const labels = new Set<string>();
    for (const state of states) {
      const row = page.getByRole("listitem").filter({ hasText: `Job ${state}` }).first();
      await expect(row).toBeVisible();
      const badge = row.locator("[data-tone]").first();
      const text = (await badge.innerText()).trim();
      expect(text.length, `state ${state} must carry a text label`).toBeGreaterThan(0);
      labels.add(text);
    }
    expect(labels.size, "each job state needs its own label").toBe(states.length);
    await auditPage(page, "jobs with every state");
  });

  test("requires explicit cost acknowledgement before retrying an uncertain cloud request", async ({
    page,
  }) => {
    await gotoApp(page, { recordings: 1 });
    await page.evaluate(() => {
      const harness = window.__plaudonHarness!;
      harness.addJob({
        kind: "transcribe",
        state: "cloud_outcome_unknown",
        label: "Cloud transcription",
        cost_acknowledgement_required: true,
        provider_request_id: "req_01HX",
      });
      harness.emit("job.updated");
    });
    await page.getByRole("tab", { name: /Jobs/ }).click();
    const row = page.getByRole("listitem").filter({ hasText: "Cloud transcription" }).first();
    await expect(row).toContainText(/billed|charge/i);
    await expect(row).toContainText("req_01HX");

    await row.getByRole("button", { name: /retry/i }).first().click();
    const dialog = page.getByRole("dialog");
    await expect(dialog).toBeVisible();
    const confirm = dialog.getByRole("button", { name: /retry/i }).last();
    await expect(confirm).toBeDisabled();
    await dialog.getByRole("checkbox").first().check();
    await expect(confirm).toBeEnabled();
    await confirm.click();

    const acknowledgements = await page.evaluate(() =>
      window
        .__plaudonHarness!.calls.filter((c) => c.command === "retry_job")
        .map((c) =>
          c.args && typeof c.args === "object" && "cost_acknowledged" in c.args
            ? c.args.cost_acknowledged === true
            : null,
        ),
    );
    expect(acknowledgements).toEqual([true]);
  });

  test("shows the empty state with a corrective action", async ({ page }) => {
    await gotoApp(page, { recordings: 0 });
    await page.getByRole("tab", { name: /Jobs/ }).click();
    await expect(page.getByText(/no jobs/i)).toBeVisible();
    await auditPage(page, "jobs empty");
  });
});

test.describe("activity history", () => {
  test("records completions even when the OS denies notifications", async ({ page }) => {
    await gotoApp(page, { recordings: 1, notifications: "denied" });
    await page.evaluate(() => {
      const harness = window.__plaudonHarness!;
      harness.addActivity({
        category: "import_finished",
        title: "Import finished",
        body: "1 recording was added to your library.",
      });
      harness.emit("activity.added");
    });
    await page.getByRole("tab", { name: /Settings/ }).click();
    await page.locator("#main-region").getByRole("button", { name: /open activity history/i }).click();
    await expect(page.getByText("Import finished").first()).toBeVisible();
    await expect(page.getByText(/not permitted|denied|suppressed/i).first()).toBeVisible();
    await auditPage(page, "activity history");
  });

  test("announces unread activity in the header", async ({ page }) => {
    await gotoApp(page, { recordings: 1 });
    await page.evaluate(() => {
      window.__plaudonHarness!.addActivity({
        category: "transcription_finished",
        title: "Transcription finished",
        body: "1 recording finished transcribing.",
      });
      window.__plaudonHarness!.emit("activity.added");
    });
    await expect(page.getByRole("button", { name: /activity history/i })).toContainText(/1/);
  });
});
