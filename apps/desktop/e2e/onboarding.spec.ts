import { auditPage, expect, harnessUrl, test } from "./fixtures";

/**
 * E1.3 / §4.1: first launch is offline-first. Every account step is skippable,
 * nothing downloads, and the user reaches a working import path.
 */
test.describe("first-run onboarding", () => {
  test("completes with every account step skipped and no egress", async ({ page, egress }) => {
    await page.goto(harnessUrl({ onboarded: false, recordings: 0 }));

    const heading = page.getByRole("heading", { level: 2 });
    await expect(heading).toBeVisible();
    await expect(page.getByText(/stay on this computer|stays on this computer/i)).toBeVisible();
    await auditPage(page, "onboarding privacy step");

    // Walk the flow using the keyboard only, skipping every account step.
    for (let step = 0; step < 8; step += 1) {
      const finish = page.getByRole("button", { name: /^(Finish|Start importing|Done)/ });
      if (await finish.count()) {
        await finish.first().click();
        break;
      }
      const skip = page.getByRole("button", { name: /^Skip/ });
      const next = page.getByRole("button", { name: /^(Next|Continue)/ });
      if (await skip.count()) {
        await skip.first().click();
      } else if (await next.count()) {
        await next.first().click();
      } else {
        break;
      }
    }

    await expect(page.getByRole("heading", { name: "Library", level: 1 })).toBeVisible();
    expect(egress.external, `unexpected egress: ${egress.external.join(", ")}`).toEqual([]);
  });

  test("never offers a model download while local-only mode is on", async ({ page }) => {
    await page.goto(harnessUrl({ onboarded: false }));
    const downloadControls = page.getByRole("button", { name: /download/i });
    await expect(downloadControls).toHaveCount(0);
  });

  test("keeps the API key out of the DOM after it is saved", async ({ page }) => {
    await page.goto(harnessUrl({ onboarded: false }));
    const secret = "sk-or-v1-e2e-secret-value";
    for (let step = 0; step < 8; step += 1) {
      const field = page.getByLabel(/API key/i);
      if (await field.count()) {
        await field.first().fill(secret);
        await page.getByRole("button", { name: /^(Save|Store)/ }).first().click();
        break;
      }
      const next = page.getByRole("button", { name: /^(Next|Continue|Skip)/ });
      if (!(await next.count())) break;
      await next.first().click();
    }
    const html = await page.content();
    expect(html).not.toContain(secret);
    const received = await page.evaluate(() => window.__plaudonHarness?.receivedSecrets ?? []);
    expect(received).toEqual([secret]);
  });
});
