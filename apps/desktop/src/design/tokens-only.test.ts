import { readFileSync, readdirSync, statSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";

/**
 * §5.6 / E1.1: components consume design tokens only. This guard scans every
 * stylesheet except the token definition itself for literal colors.
 */
const SRC = join(process.cwd(), "src");

function cssFiles(dir: string): string[] {
  const out: string[] = [];
  for (const entry of readdirSync(dir)) {
    const full = join(dir, entry);
    if (statSync(full).isDirectory()) {
      if (entry === "node_modules") continue;
      out.push(...cssFiles(full));
    } else if (entry.endsWith(".css")) {
      out.push(full);
    }
  }
  return out;
}

const LITERAL_COLOR =
  /(#[0-9a-fA-F]{3,8}\b)|(\brgba?\()|(\bhsla?\()|(?<![-\w])(red|blue|green|orange|yellow|purple|black|white|grey|gray)(?![-\w])/;

describe("stylesheets use design tokens only", () => {
  const files = cssFiles(SRC);

  it("finds the stylesheets", () => {
    expect(files.length).toBeGreaterThan(2);
  });

  it.each(files.map((f) => [f.slice(SRC.length)] as const))("%s has no literal color", (relative) => {
    const file = join(SRC, relative);
    const offenders: string[] = [];
    for (const [index, line] of readFileSync(file, "utf8").split("\n").entries()) {
      const code = line.split("/*")[0] ?? "";
      if (LITERAL_COLOR.test(code)) offenders.push(`${index + 1}: ${line.trim()}`);
    }
    expect(offenders, `literal colors must come from src/design/tokens.ts`).toEqual([]);
  });

  it.each(files.map((f) => [f.slice(SRC.length)] as const))(
    "%s uses spacing tokens for padding, margin and gap",
    (relative) => {
      const file = join(SRC, relative);
      const offenders: string[] = [];
      for (const [index, line] of readFileSync(file, "utf8").split("\n").entries()) {
        const match = /^\s*(padding|margin|gap|row-gap|column-gap)(-[a-z]+)?\s*:\s*([^;]+);/.exec(line);
        if (!match) continue;
        const value = match[3]!;
        // Hairlines and the 1px clip used by the screen-reader-only utility are
        // layout primitives, not spacing choices.
        const literalPx =
          /(?<![-\w])-?\d+px\b/.test(value) &&
          !value.includes("var(--") &&
          !/^-?(0|1)px$/.test(value.trim());
        if (literalPx) offenders.push(`${index + 1}: ${line.trim()}`);
      }
      expect(offenders, "use var(--space-N) for spacing").toEqual([]);
    },
  );
});
