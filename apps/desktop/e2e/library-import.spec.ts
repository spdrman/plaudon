import { auditPage, expect, gotoApp, test } from "./fixtures";

/**
 * E1.4 / E1.5: the library lists, searches, filters and virtualizes; import
 * reaches `import_files` with validated paths and never resolves a duplicate
 * silently.
 */
test.describe("library", () => {
  test("renders the required columns and virtualizes a large library", async ({ page }) => {
    await gotoApp(page, { recordings: 5000, segments: 0 });
    const grid = page.getByRole("grid").first();
    await expect(grid).toBeVisible();

    for (const column of [
      "Title",
      "Recorded",
      "Duration",
      "Source",
      "Import status",
      "Transcription",
      "Engine",
      "Last error",
    ]) {
      await expect(
        grid.getByRole("columnheader", { name: new RegExp(column, "i") }).first(),
      ).toBeVisible();
    }

    const renderedRows = await grid.getByRole("row").count();
    expect(renderedRows).toBeGreaterThan(1);
    expect(renderedRows, "a virtualized grid must not render 5000 rows").toBeLessThan(200);
    await expect(grid).toHaveAttribute("aria-rowcount", /\d{4,}/);
    await auditPage(page, "library populated");
  });

  test("searches and filters without stalling", async ({ page }) => {
    await gotoApp(page, { recordings: 400, segments: 0 });
    await page.getByLabel("Search recordings").fill("Weekly sync 123");
    await expect(page.getByRole("status").filter({ hasText: /1 / }).first()).toBeVisible({
      timeout: 10_000,
    });
    await expect(page.getByRole("gridcell", { name: /Weekly sync 123/ }).first()).toBeVisible();
  });

  test("offers corrective actions when the library is empty", async ({ page }) => {
    await gotoApp(page, { recordings: 0 });
    await expect(page.getByRole("button", { name: /import an audio file/i })).toBeVisible();
    await expect(page.getByRole("button", { name: /connect plaud/i })).toBeVisible();
    await auditPage(page, "library empty");
  });

  test("refetches after a dropped library event instead of rendering stale rows", async ({
    page,
  }) => {
    await gotoApp(page, { recordings: 3, segments: 0 });
    const before = await page.evaluate(
      () => window.__plaudonHarness!.calls.filter((c) => c.command === "list_recordings").length,
    );
    await page.evaluate(() => {
      const harness = window.__plaudonHarness!;
      harness.addRecording({ title: "Arrived while offline" });
      harness.dropNextEvent = true;
      harness.emit("library.changed");
    });
    await expect(page.getByRole("gridcell", { name: /Arrived while offline/ })).toBeVisible();
    const after = await page.evaluate(
      () => window.__plaudonHarness!.calls.filter((c) => c.command === "list_recordings").length,
    );
    expect(after).toBeGreaterThan(before);
  });
});

test.describe("manual import", () => {
  test("imports the files chosen in the OS picker", async ({ page }) => {
    await gotoApp(page, { recordings: 0 });
    await page.evaluate(() => {
      window.__plaudonPickerPaths = [
        "/Users/tester/Audio/board-meeting.mp3",
        "/Users/tester/Audio/notes.wav",
      ];
    });
    await page.getByRole("button", { name: /import audio/i }).click();
    await expect(page.getByRole("gridcell", { name: /board-meeting/ })).toBeVisible();
    const call = await page.evaluate(() => {
      const args = window.__plaudonHarness!.calls.find((c) => c.command === "import_files")?.args;
      if (!args || typeof args !== "object") return null;
      const paths = "paths" in args && Array.isArray(args.paths) ? args.paths.map(String) : [];
      const policy = "duplicate_policy" in args ? String(args.duplicate_policy) : "";
      return { paths, policy };
    });
    expect(call).not.toBeNull();
    expect(call!.paths).toEqual([
      "/Users/tester/Audio/board-meeting.mp3",
      "/Users/tester/Audio/notes.wav",
    ]);
    expect(call!.policy).toBe("ask");
  });

  test("reports per-file outcomes for a mixed batch and never aborts it", async ({ page }) => {
    await gotoApp(page, { recordings: 0 });
    await page.evaluate(() => {
      window.__plaudonPickerPaths = [
        "/Users/tester/Audio/good.mp3",
        "/Users/tester/Audio/notes.txt",
        "/Users/tester/Audio/; rm -rf x.mp3",
      ];
    });
    await page.getByRole("button", { name: /import audio/i }).click();
    await expect(page.getByText(/not a supported audio file/i)).toBeVisible();
    await expect(page.getByRole("gridcell", { name: /good/ })).toBeVisible();
    // The hostile name is rendered as inert text.
    await expect(page.getByText("; rm -rf x", { exact: false }).first()).toBeVisible();
  });

  test("asks before importing a duplicate and honours the choice", async ({ page }) => {
    await gotoApp(page, { recordings: 0 });
    await page.evaluate(() => {
      window.__plaudonPickerPaths = ["/Users/tester/Audio/standup.mp3"];
    });
    await page.getByRole("button", { name: /import audio/i }).click();
    await expect(page.getByRole("gridcell", { name: /standup/ })).toBeVisible();

    // The same bytes again: the app must ask rather than import or skip silently.
    await page.getByRole("button", { name: /import audio/i }).click();
    const dialog = page.getByRole("dialog");
    await expect(dialog).toBeVisible();
    await expect(dialog).toContainText(/content|hash/i);
    await auditPage(page, "duplicate dialog");
    await dialog.getByRole("button", { name: /import anyway/i }).click();

    const policies = await page.evaluate(() =>
      window
        .__plaudonHarness!.calls.filter((c) => c.command === "import_files")
        .map((c) =>
          c.args && typeof c.args === "object" && "duplicate_policy" in c.args
            ? String(c.args.duplicate_policy)
            : "",
        ),
    );
    expect(policies).toContain("import_anyway");
  });
});
