import { applyTokens } from "./tokens";
export const initialThemeState = {
    appearance: "system",
    contrast: "standard",
    systemDark: false,
};
export function themeReducer(state, action) {
    switch (action.type) {
        case "set-appearance":
            return state.appearance === action.appearance
                ? state
                : { ...state, appearance: action.appearance };
        case "set-contrast":
            return state.contrast === action.contrast ? state : { ...state, contrast: action.contrast };
        case "system-appearance-changed":
            return state.systemDark === action.dark ? state : { ...state, systemDark: action.dark };
        default: {
            const never = action;
            throw new Error(`unknown theme action: ${JSON.stringify(never)}`);
        }
    }
}
export function effectiveTheme(state) {
    const dark = state.appearance === "system" ? state.systemDark : state.appearance === "dark";
    if (state.contrast === "high")
        return dark ? "hc-dark" : "hc-light";
    return dark ? "dark" : "light";
}
export function applyThemeToDocument(doc, state) {
    const theme = effectiveTheme(state);
    applyTokens(doc.documentElement, theme);
    doc.documentElement.style.colorScheme = theme.endsWith("dark") ? "dark" : "light";
    return theme;
}
/** Subscribes to OS appearance changes; returns an unsubscribe function. */
export function watchSystemAppearance(win, onChange) {
    const query = win.matchMedia("(prefers-color-scheme: dark)");
    onChange(query.matches);
    const listener = (event) => onChange(event.matches);
    query.addEventListener("change", listener);
    return () => query.removeEventListener("change", listener);
}
/** Subscribes to the OS high-contrast / forced-colors signal. */
export function watchSystemContrast(win, onChange) {
    const query = win.matchMedia("(prefers-contrast: more), (forced-colors: active)");
    onChange(query.matches);
    const listener = (event) => onChange(event.matches);
    query.addEventListener("change", listener);
    return () => query.removeEventListener("change", listener);
}
