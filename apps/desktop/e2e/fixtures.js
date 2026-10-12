import { test as base, expect } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";
export function harnessUrl(query = {}) {
    const params = new URLSearchParams({ harness: "1" });
    if (query.recordings !== undefined)
        params.set("recordings", String(query.recordings));
    if (query.segments !== undefined)
        params.set("segments", String(query.segments));
    if (query.onboarded !== undefined)
        params.set("onboarded", query.onboarded ? "1" : "0");
    if (query.notifications)
        params.set("notifications", query.notifications);
    return `/?${params.toString()}`;
}
export const test = base.extend({
    egress: async ({ page }, use) => {
        const log = { external: [], all: [] };
        page.on("request", (request) => {
            const url = request.url();
            log.all.push(url);
            // `plaudon-media:` (and its Windows `http://plaudon-media.localhost`
            // form) is the custom scheme the Rust side intercepts in-process; it
            // never reaches a network stack.
            const local = /^(data:|blob:|about:|plaudon-media:)/.test(url) ||
                url.startsWith("http://127.0.0.1:5273") ||
                url.startsWith("http://plaudon-media.localhost");
            if (!local) {
                log.external.push(url);
            }
        });
        await use(log);
    },
});
export { expect };
/**
 * Navigates to the app with the in-memory backend and waits until the harness
 * is installed and the shell has rendered, so `page.evaluate` can drive it.
 */
export async function gotoApp(page, query = {}, hash = "") {
    await page.goto(`${harnessUrl(query)}${hash}`);
    await page.waitForFunction(() => Boolean(window.__plaudonHarness));
    await page.getByRole("navigation", { name: "Primary" }).or(page.getByRole("heading", { level: 1 })).first().waitFor();
}
/** Runs an axe WCAG 2.2 AA audit against the current page. */
export async function auditPage(page, context) {
    const results = await new AxeBuilder({ page })
        .withTags(["wcag2a", "wcag2aa", "wcag21a", "wcag21aa", "wcag22aa"])
        .analyze();
    const summary = results.violations
        .map((v) => `${v.id} (${v.impact}): ${v.help}\n    ${v.nodes.map((n) => n.target.join(" ")).join("\n    ")}`)
        .join("\n  ");
    expect(results.violations, `${context ?? "page"} has axe violations:\n  ${summary}`).toHaveLength(0);
}
/** Walks the tab order and returns the accessible names of the stops. */
export async function tabOrder(page, steps) {
    const names = [];
    for (let i = 0; i < steps; i += 1) {
        await page.keyboard.press("Tab");
        names.push(await page.evaluate(() => {
            const el = document.activeElement;
            if (!el || el === document.body)
                return "(body)";
            return (el.getAttribute("aria-label") ??
                el.textContent?.trim().slice(0, 40) ??
                el.tagName.toLowerCase());
        }));
    }
    return names;
}
