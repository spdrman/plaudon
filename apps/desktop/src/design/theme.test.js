import { describe, expect, it, vi } from "vitest";
import { applyThemeToDocument, effectiveTheme, initialThemeState, themeReducer, watchSystemAppearance, } from "./theme";
const base = initialThemeState;
describe("theme reducer", () => {
    it("follows the OS when appearance is system", () => {
        expect(effectiveTheme(base)).toBe("light");
        const dark = themeReducer(base, { type: "system-appearance-changed", dark: true });
        expect(effectiveTheme(dark)).toBe("dark");
    });
    it("lets an explicit appearance override the OS", () => {
        const state = themeReducer(themeReducer(base, { type: "system-appearance-changed", dark: true }), { type: "set-appearance", appearance: "light" });
        expect(effectiveTheme(state)).toBe("light");
    });
    it("maps high contrast onto the matching high-contrast theme", () => {
        const hc = themeReducer(base, { type: "set-contrast", contrast: "high" });
        expect(effectiveTheme(hc)).toBe("hc-light");
        expect(effectiveTheme(themeReducer(hc, { type: "system-appearance-changed", dark: true }))).toBe("hc-dark");
    });
    it("returns the identical state object for no-op actions", () => {
        expect(themeReducer(base, { type: "set-appearance", appearance: "system" })).toBe(base);
    });
    it("applies tokens and color-scheme to the document", () => {
        const theme = applyThemeToDocument(document, {
            ...base,
            appearance: "dark",
        });
        expect(theme).toBe("dark");
        expect(document.documentElement.dataset.theme).toBe("dark");
        expect(document.documentElement.style.colorScheme).toBe("dark");
    });
});
describe("watchSystemAppearance", () => {
    it("reports the current value and subsequent changes, then unsubscribes", () => {
        const listeners = new Set();
        const win = {
            matchMedia: () => ({
                matches: true,
                addEventListener: (_, cb) => listeners.add(cb),
                removeEventListener: (_, cb) => listeners.delete(cb),
            }),
        };
        const onChange = vi.fn();
        const stop = watchSystemAppearance(win, onChange);
        expect(onChange).toHaveBeenCalledWith(true);
        for (const cb of listeners)
            cb({ matches: false });
        expect(onChange).toHaveBeenCalledWith(false);
        stop();
        expect(listeners.size).toBe(0);
    });
});
