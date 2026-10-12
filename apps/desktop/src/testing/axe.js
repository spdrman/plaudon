import axe, {} from "axe-core";
/**
 * WCAG 2.2 AA rule set used by every screen test (§5.3, E2.5).
 * `color-contrast` is kept on: tokens are applied to the test document so the
 * computed colors are the real ones.
 */
export const WCAG_22_AA = {
    runOnly: {
        type: "tag",
        values: ["wcag2a", "wcag2aa", "wcag21a", "wcag21aa", "wcag22aa", "best-practice"],
    },
    rules: {
        // jsdom has no layout engine, so geometry-dependent rules cannot be judged here;
        // the Playwright suite runs them against the real webview.
        "target-size": { enabled: false },
        region: { enabled: false },
        "scrollable-region-focusable": { enabled: false },
    },
};
export async function runAxe(context = document.body, options = WCAG_22_AA) {
    return axe.run(context, options);
}
export function formatViolations(results) {
    return results.violations
        .map((v) => `${v.id} (${v.impact ?? "unknown"}): ${v.help}\n  ` +
        v.nodes.map((n) => n.target.join(" ")).join("\n  "))
        .join("\n");
}
export async function expectNoAxeViolations(context = document.body, options = WCAG_22_AA) {
    const results = await runAxe(context, options);
    if (results.violations.length > 0) {
        throw new Error(`axe found ${results.violations.length} violation(s):\n${formatViolations(results)}`);
    }
}
