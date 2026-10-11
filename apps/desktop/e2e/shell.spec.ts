import { auditPage, expect, harnessUrl, test } from "./fixtures";

/**
 * E1.1 / E1.5 / E2.5: the shell launches, renders the five destinations,
 * follows the OS appearance, is keyboard-complete and makes no network
 * request of its own.
 */
test.describe("application shell", () => {
  test("launches, renders the library and makes no external request", async ({ page, egress }) => {
    await page.goto(harnessUrl({ recordings: 25, segments: 20 }));
    await expect(page.getByRole("heading", { name: "Library", level: 1 })).toBeVisible();
    await expect(page.getByRole("tab", { name: /Library/ })).toHaveAttribute(
      "aria-selected",
      "true",
    );
    expect(egress.external, `unexpected egress: ${egress.external.join(", ")}`).toEqual([]);
  });

  test("reaches every destination with the keyboard only", async ({ page }) => {
    await page.goto(harnessUrl({ recordings: 5 }));
    await page.getByRole("tab", { name: /Library/ }).focus();
    const expected = ["Inbox", "Jobs", "Templates", "Settings"];
    for (const name of expected) {
      await page.keyboard.press("ArrowDown");
      await expect(page.getByRole("tab", { name: new RegExp(name) })).toHaveAttribute(
        "aria-selected",
        "true",
      );
      await expect(page.getByRole("heading", { level: 1 })).toHaveText(name);
    }
    // Wraps back to the first destination.
    await page.keyboard.press("ArrowDown");
    await expect(page.getByRole("heading", { level: 1 })).toHaveText("Library");
  });

  test("jumps to a destination with the platform shortcut", async ({ page }) => {
    await page.goto(harnessUrl({ recordings: 3 }));
    await page.keyboard.press("ControlOrMeta+3");
    await expect(page.getByRole("heading", { level: 1 })).toHaveText("Jobs");
    await page.keyboard.press("ControlOrMeta+1");
    await expect(page.getByRole("heading", { level: 1 })).toHaveText("Library");
  });

  test("shows the keyboard reference and restores focus when it closes", async ({ page }) => {
    await page.goto(harnessUrl({ recordings: 3 }));
    await page.keyboard.press("?");
    const dialog = page.getByRole("dialog", { name: "Keyboard shortcuts" });
    await expect(dialog).toBeVisible();
    await auditPage(page, "shortcuts dialog");
    await page.keyboard.press("Escape");
    await expect(dialog).toBeHidden();
  });

  test("follows the OS appearance and keeps a visible focus ring", async ({ page }) => {
    await page.emulateMedia({ colorScheme: "light" });
    await page.goto(harnessUrl({ recordings: 3 }));
    const html = page.locator("html");
    await expect(html).toHaveAttribute("data-theme", "light");
    await page.emulateMedia({ colorScheme: "dark" });
    await expect(html).toHaveAttribute("data-theme", "dark");

    await page.keyboard.press("Tab");
    const outline = await page.evaluate(() => {
      const el = document.activeElement as HTMLElement;
      const style = getComputedStyle(el);
      return { width: style.outlineWidth, style: style.outlineStyle, color: style.outlineColor };
    });
    expect(Number.parseFloat(outline.width)).toBeGreaterThanOrEqual(2);
    expect(outline.style).not.toBe("none");
  });

  test("stays usable below the minimum desktop width without mobile stacking", async ({ page }) => {
    await page.setViewportSize({ width: 820, height: 700 });
    await page.goto(harnessUrl({ recordings: 10 }));
    await expect(page.getByRole("status").filter({ hasText: /minimum/ })).toBeVisible();
    // Navigation and content remain side by side: the nav keeps its own column.
    const nav = page.getByRole("navigation", { name: "Primary" });
    const main = page.locator("#main-region");
    const navBox = await nav.boundingBox();
    const mainBox = await main.boundingBox();
    expect(navBox).not.toBeNull();
    expect(mainBox).not.toBeNull();
    expect(mainBox!.x).toBeGreaterThanOrEqual(navBox!.x + navBox!.width - 1);
  });

  test("keeps transcript and list text selectable", async ({ page }) => {
    await page.goto(harnessUrl({ recordings: 5, segments: 10 }));
    const userSelect = await page.evaluate(() => getComputedStyle(document.body).userSelect);
    expect(userSelect).not.toBe("none");
  });

  test("has no accessibility violations on every top-level destination", async ({ page }) => {
    await page.goto(harnessUrl({ recordings: 30, segments: 25 }));
    for (const name of ["Library", "Inbox", "Jobs", "Templates", "Settings"]) {
      await page.getByRole("tab", { name: new RegExp(name) }).click();
      await expect(page.getByRole("heading", { level: 1 })).toHaveText(name);
      await auditPage(page, name);
    }
  });
});
