import { test as base, expect, type Page } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";

export interface HarnessQuery {
  /** Number of seeded recordings. */
  recordings?: number;
  /** Segments per seeded transcript. */
  segments?: number;
  /** Whether onboarding is already completed. */
  onboarded?: boolean;
  /** Simulated OS notification permission. */
  notifications?: "granted" | "denied" | "not_requested" | "unsupported";
}

export function harnessUrl(query: HarnessQuery = {}): string {
  const params = new URLSearchParams({ harness: "1" });
  if (query.recordings !== undefined) params.set("recordings", String(query.recordings));
  if (query.segments !== undefined) params.set("segments", String(query.segments));
  if (query.onboarded !== undefined) params.set("onboarded", query.onboarded ? "1" : "0");
  if (query.notifications) params.set("notifications", query.notifications);
  return `/?${params.toString()}`;
}

/**
 * Every request the page makes is recorded. The app is local-first: the only
 * legitimate origins are the dev server itself and `data:`/`blob:` URLs.
 */
export interface EgressLog {
  /** Requests to anything other than the local dev server. */
  external: string[];
  all: string[];
}

export const test = base.extend<{ egress: EgressLog }>({
  egress: async ({ page }, use) => {
    const log: EgressLog = { external: [], all: [] };
    page.on("request", (request) => {
      const url = request.url();
      log.all.push(url);
      if (!/^(data:|blob:|about:)/.test(url) && !url.startsWith("http://127.0.0.1:5273")) {
        log.external.push(url);
      }
    });
    await use(log);
  },
});

export { expect };

/** Runs an axe WCAG 2.2 AA audit against the current page. */
export async function auditPage(page: Page, context?: string) {
  const results = await new AxeBuilder({ page })
    .withTags(["wcag2a", "wcag2aa", "wcag21a", "wcag21aa", "wcag22aa"])
    .analyze();
  const summary = results.violations
    .map(
      (v) =>
        `${v.id} (${v.impact}): ${v.help}\n    ${v.nodes.map((n) => n.target.join(" ")).join("\n    ")}`,
    )
    .join("\n  ");
  expect(results.violations, `${context ?? "page"} has axe violations:\n  ${summary}`).toHaveLength(
    0,
  );
}

/** Walks the tab order and returns the accessible names of the stops. */
export async function tabOrder(page: Page, steps: number): Promise<string[]> {
  const names: string[] = [];
  for (let i = 0; i < steps; i += 1) {
    await page.keyboard.press("Tab");
    names.push(
      await page.evaluate(() => {
        const el = document.activeElement as HTMLElement | null;
        if (!el || el === document.body) return "(body)";
        return (
          el.getAttribute("aria-label") ??
          el.textContent?.trim().slice(0, 40) ??
          el.tagName.toLowerCase()
        );
      }),
    );
  }
  return names;
}
