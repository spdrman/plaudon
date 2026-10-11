import { describe, expect, it } from "vitest";
import {
  COLOR_ROLES,
  MIN_DESKTOP_WIDTH,
  applyTokens,
  resolveTokens,
  type EffectiveTheme,
} from "./tokens";

const THEMES: EffectiveTheme[] = ["light", "dark", "hc-light", "hc-dark"];

describe("design tokens", () => {
  it("defines every semantic color role in every theme", () => {
    for (const theme of THEMES) {
      const tokens = resolveTokens(theme);
      for (const role of COLOR_ROLES) {
        expect(tokens[role], `${theme}.${role}`).toBeTruthy();
      }
    }
  });

  it("gives light and dark distinct canvas and text colors", () => {
    expect(resolveTokens("light")["color-bg-canvas"]).not.toBe(
      resolveTokens("dark")["color-bg-canvas"],
    );
    expect(resolveTokens("light")["color-text-primary"]).not.toBe(
      resolveTokens("dark")["color-text-primary"],
    );
  });

  it("keeps a visible focus ring in every theme and widens it in high contrast", () => {
    for (const theme of THEMES) {
      const tokens = resolveTokens(theme);
      expect(tokens["color-focus-ring"]).toBeTruthy();
      expect(Number.parseInt(tokens["focus-ring-width"] ?? "0", 10)).toBeGreaterThanOrEqual(2);
    }
    expect(resolveTokens("hc-light")["focus-ring-width"]).toBe("3px");
    expect(resolveTokens("hc-dark")["focus-ring-width"]).toBe("3px");
  });

  it("meets WCAG 2.2 AA 4.5:1 text contrast for primary text on canvas and surface", () => {
    for (const theme of THEMES) {
      const t = resolveTokens(theme);
      for (const bg of ["color-bg-canvas", "color-bg-surface", "color-bg-raised"] as const) {
        expect(contrast(t["color-text-primary"]!, t[bg]!), `${theme} ${bg}`).toBeGreaterThanOrEqual(
          4.5,
        );
        expect(
          contrast(t["color-text-secondary"]!, t[bg]!),
          `${theme} secondary on ${bg}`,
        ).toBeGreaterThanOrEqual(4.5);
      }
    }
  });

  it("meets 4.5:1 for every status color on the surface background", () => {
    for (const theme of THEMES) {
      const t = resolveTokens(theme);
      for (const role of COLOR_ROLES.filter((r) => r.startsWith("color-status-"))) {
        expect(contrast(t[role]!, t["color-bg-surface"]!), `${theme} ${role}`).toBeGreaterThanOrEqual(
          4.5,
        );
      }
    }
  });

  it("meets 3:1 non-text contrast for borders and the focus ring", () => {
    for (const theme of THEMES) {
      const t = resolveTokens(theme);
      expect(contrast(t["color-border-strong"]!, t["color-bg-surface"]!)).toBeGreaterThanOrEqual(3);
      expect(contrast(t["color-focus-ring"]!, t["color-bg-surface"]!)).toBeGreaterThanOrEqual(3);
    }
  });

  it("applies tokens as CSS custom properties and records the theme", () => {
    const el = document.createElement("div");
    applyTokens(el, "dark");
    expect(el.dataset.theme).toBe("dark");
    expect(el.style.getPropertyValue("--color-bg-canvas")).toBe(
      resolveTokens("dark")["color-bg-canvas"],
    );
    expect(el.style.getPropertyValue("--space-5")).toBe("16px");
  });

  it("keeps a desktop-class minimum width (no mobile viewport assumption)", () => {
    expect(MIN_DESKTOP_WIDTH).toBeGreaterThanOrEqual(800);
  });
});

function channel(value: number): number {
  const c = value / 255;
  return c <= 0.03928 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
}

function luminance(hex: string): number {
  const m = /^#([0-9a-f]{6})$/i.exec(hex.trim());
  if (!m) throw new Error(`expected opaque hex color, got ${hex}`);
  const n = Number.parseInt(m[1]!, 16);
  return (
    0.2126 * channel((n >> 16) & 0xff) +
    0.7152 * channel((n >> 8) & 0xff) +
    0.0722 * channel(n & 0xff)
  );
}

function contrast(a: string, b: string): number {
  const la = luminance(a);
  const lb = luminance(b);
  return (Math.max(la, lb) + 0.05) / (Math.min(la, lb) + 0.05);
}
