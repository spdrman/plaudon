import { auditPage, expect, harnessUrl, test } from "./fixtures";

/**
 * E2.1 / E2.2: review a recording. Playback is keyboard-operable and states
 * position as text, a transcript without alignment shows no fabricated
 * timeline, and edits create a new revision without mutating the original.
 */
test.describe("recording review", () => {
  test("plays, seeks and reports position as text using the keyboard only", async ({ page }) => {
    await page.goto(`${harnessUrl({ recordings: 3, segments: 30 })}#/library/rec-00001`);
    const player = page.getByRole("group", { name: /player|playback/i }).first();
    await expect(player).toBeVisible();

    const slider = page.getByRole("slider", { name: /position|seek/i }).first();
    await expect(slider).toHaveAttribute("aria-valuetext", /of/);
    await slider.focus();
    const before = await slider.getAttribute("aria-valuenow");
    await page.keyboard.press("ArrowRight");
    await expect(slider).not.toHaveAttribute("aria-valuenow", before ?? "");
    await page.keyboard.press("Home");
    await expect(slider).toHaveAttribute("aria-valuenow", "0");

    const back = page.getByRole("button", { name: /back 10|−10|-10/i }).first();
    await back.focus();
    await page.keyboard.press("Enter");
    await expect(slider).toHaveAttribute("aria-valuenow", "0");

    await auditPage(page, "recording detail");
  });

  test("shows an honest indeterminate state when the duration is unknown", async ({ page }) => {
    await page.goto(harnessUrl({ recordings: 1, segments: 5 }));
    const id = await page.evaluate(() => {
      const rec = window.__plaudonHarness!.addRecording({
        title: "Voice memo without duration",
        durationMs: null,
        alignment: "none",
        segments: 6,
      });
      window.__plaudonHarness!.emit("library.changed");
      return rec.recording_id;
    });
    await page.evaluate((target) => {
      window.location.hash = `#/library/${target}`;
    }, id);

    await expect(page.getByText(/duration unknown/i)).toBeVisible();
    await expect(page.getByRole("slider", { name: /position|seek/i })).toHaveCount(0);
    await expect(page.getByText("0:00 of 0:00")).toHaveCount(0);
    // No alignment means no timestamp chips and no seek affordance.
    await expect(page.getByRole("button", { name: /^\d+:\d\d$/ })).toHaveCount(0);
    await auditPage(page, "recording without timeline");
  });

  test("edits create a new revision and keep the original retrievable", async ({ page }) => {
    await page.goto(`${harnessUrl({ recordings: 2, segments: 12 })}#/library/rec-00001`);
    const firstSegment = page.getByRole("listitem").filter({ hasText: "Segment 1" }).first();
    await firstSegment.scrollIntoViewIfNeeded();
    await firstSegment.dblclick();
    const editor = page.getByRole("textbox").first();
    await editor.fill("Corrected opening line");
    await page.keyboard.press("Escape");

    await firstSegment.dblclick();
    await page.getByRole("textbox").first().fill("Corrected opening line");
    await page.getByRole("button", { name: /^Save/ }).first().click();

    const result = await page.evaluate(async () => {
      const harness = window.__plaudonHarness!;
      const rec = harness.recordings.find((r) => r.recording_id === "rec-00001")!;
      const original = rec.revisions[0]!;
      const page1 = (await harness.invoke("get_transcript", {
        revision_id: original.revision_id,
        cursor: 0,
        limit: 5,
      })) as { segments: Array<{ text: string }> };
      return {
        revisionCount: rec.revisions.length,
        originalFirstSegment: page1.segments[0]!.text,
        head: rec.current_revision_id,
        originalId: original.revision_id,
      };
    });
    expect(result.revisionCount).toBe(2);
    expect(result.originalFirstSegment).toContain("Segment 1");
    expect(result.head).not.toBe(result.originalId);
  });

  test("offers a non-destructive resolution when the base revision changed", async ({ page }) => {
    await page.goto(`${harnessUrl({ recordings: 2, segments: 8 })}#/library/rec-00002`);
    const segment = page.getByRole("listitem").filter({ hasText: "Segment 1" }).first();
    await segment.dblclick();
    await page.getByRole("textbox").first().fill("My local correction");

    // Another writer commits a newer revision in the meantime.
    await page.evaluate(async () => {
      const harness = window.__plaudonHarness!;
      await harness.invoke("save_transcript_revision", {
        recording_id: "rec-00002",
        base_revision_id: harness.recordings.find((r) => r.recording_id === "rec-00002")!
          .current_revision_id,
        edits: [{ index: 1, text: "Someone else edited this" }],
      });
    });

    await page.getByRole("button", { name: /^Save/ }).first().click();
    await expect(page.getByText(/changed since you opened it/i)).toBeVisible();
    await expect(page.getByRole("button", { name: /reload newest revision/i })).toBeVisible();
    await expect(page.getByRole("button", { name: /copy my changes/i })).toBeVisible();
    await expect(page.getByText("My local correction")).toBeVisible();
  });
});
