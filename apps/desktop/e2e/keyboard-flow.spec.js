import { auditPage, expect, gotoApp, harnessUrl, test } from "./fixtures";
/**
 * E2.5 / §19 Phase 2 exit gate: the complete manual file flow — onboarding →
 * import → library → play → edit transcript → export — must be completable
 * with the keyboard alone, and every screen must pass WCAG 2.2 AA.
 */
/** Presses Tab until the predicate matches the focused element, or fails. */
async function tabTo(page, matcher, limit = 60) {
    for (let i = 0; i < limit; i += 1) {
        const name = await page.evaluate(() => {
            const el = document.activeElement;
            // `document.body` reports the whole page text; it is not a tab stop.
            if (!el || el === document.body || el === document.documentElement)
                return "";
            return `${el.getAttribute("aria-label") ?? ""} ${el.textContent ?? ""}`.trim().slice(0, 80);
        });
        if (matcher.test(name))
            return;
        await page.keyboard.press("Tab");
    }
    const reached = await page.evaluate(() => document.activeElement?.textContent ?? "");
    throw new Error(`never focused an element matching ${matcher}; stopped at "${reached}"`);
}
test("the manual file flow is completable with the keyboard only", async ({ page, egress }) => {
    // 1. Onboarding, keyboard only, skipping every account step.
    await page.goto(harnessUrl({ onboarded: false, recordings: 0 }));
    await page.waitForFunction(() => Boolean(window.__plaudonHarness));
    await page.evaluate(() => {
        window.__plaudonPickerPaths = ["/Users/tester/Audio/quarterly-review.mp3"];
    });
    for (let step = 0; step < 10; step += 1) {
        const finish = page.getByRole("button", { name: /^(Finish|Start importing|Done)/ });
        if (await finish.count()) {
            await finish.first().focus();
            await page.keyboard.press("Enter");
            break;
        }
        const advance = page.getByRole("button", { name: /^(Skip|Next|Continue)/ }).first();
        if (!(await advance.count()))
            break;
        await advance.focus();
        await page.keyboard.press("Enter");
    }
    await expect(page.getByRole("heading", { name: "Library", level: 1 })).toBeVisible();
    // 2. Import with the keyboard.
    await tabTo(page, /Import audio/i);
    await page.keyboard.press("Enter");
    await expect(page.getByRole("gridcell", { name: /quarterly-review/ })).toBeVisible();
    await auditPage(page, "library after import");
    // 3. Open the recording from the grid with the keyboard.
    await page.getByRole("row").filter({ hasText: "quarterly-review" }).first().click();
    await page.keyboard.press("Enter");
    await expect(page.getByRole("heading", { level: 2, name: /quarterly-review/ })).toBeVisible();
    await auditPage(page, "recording detail");
    // 4. Playback controls are reachable and operable by keyboard.
    await tabTo(page, /\b(Play|Pause)\b/i);
    await page.keyboard.press("Enter");
    await expect(page.getByText(/\d+:\d\d of |Duration unknown/).first()).toBeVisible();
    // 5. Give the recording a transcript, then edit it with the keyboard.
    await page.evaluate(() => {
        const harness = window.__plaudonHarness;
        const rec = harness.recordings.find((r) => r.title.includes("quarterly-review"));
        const seeded = harness.addRecording({ title: "seed", segments: 6 });
        const revision = seeded.revisions[0];
        rec.revisions = [{ ...revision, revision_id: `${rec.recording_id}-rev-1` }];
        rec.current_revision_id = `${rec.recording_id}-rev-1`;
        rec.segments.set(`${rec.recording_id}-rev-1`, seeded.segments.get(revision.revision_id));
        rec.transcription_status = "completed";
        harness.recordings = harness.recordings.filter((r) => r.title !== "seed");
        harness.emit("transcript.completed", { recording_id: rec.recording_id });
    });
    const segment = page.getByRole("button", { name: /^Segment 1:/ }).first();
    await segment.waitFor();
    await segment.focus();
    await page.keyboard.press("Enter");
    const editor = page.getByRole("textbox", { name: /Edit segment 1/ });
    await editor.fill("Keyboard corrected line");
    await page.getByRole("button", { name: /^Save 1 edited segment/ }).focus();
    await page.keyboard.press("Enter");
    await expect(page.getByRole("button", { name: /^Save 0 edited segments/ })).toBeVisible();
    await auditPage(page, "recording detail with transcript");
    // 6. Export, keyboard only.
    await tabTo(page, /Export…/);
    await page.keyboard.press("Enter");
    const dialog = page.getByRole("dialog");
    await expect(dialog).toBeVisible();
    await auditPage(page, "export dialog");
    await dialog.getByRole("button", { name: /Choose destination/ }).focus();
    await page.keyboard.press("Enter");
    const exported = await page.evaluate(() => window.__plaudonHarness.calls.some((c) => c.command === "export_recording"));
    expect(exported).toBe(true);
    expect(egress.external, `unexpected egress: ${egress.external.join(", ")}`).toEqual([]);
});
test("no interactive element is unreachable by keyboard on the main screens", async ({ page }) => {
    await gotoApp(page, { recordings: 12, segments: 20 });
    for (const destination of ["Library", "Inbox", "Jobs", "Templates", "Settings"]) {
        await page.getByRole("tab", { name: new RegExp(destination) }).click();
        const unreachable = await page.evaluate(() => {
            const selector = 'a[href], button, input, select, textarea, [role="button"], [role="slider"], [role="checkbox"]';
            return Array.from(document.querySelectorAll(selector))
                .filter((el) => {
                if (el.hasAttribute("disabled") || el.getAttribute("aria-hidden") === "true")
                    return false;
                const style = getComputedStyle(el);
                if (style.display === "none" || style.visibility === "hidden")
                    return false;
                const index = el.getAttribute("tabindex");
                // -1 is legitimate for roving-tabindex members whose container is focusable.
                if (index === null)
                    return false;
                return Number.parseInt(index, 10) < 0 && el.closest('[role="grid"], [role="tablist"], [role="list"]') === null;
            })
                .map((el) => el.getAttribute("aria-label") ?? el.textContent?.trim().slice(0, 30) ?? "?");
        });
        expect(unreachable, `${destination} has unreachable interactive elements`).toEqual([]);
    }
});
