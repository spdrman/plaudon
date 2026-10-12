import { applyTokens, type EffectiveTheme } from "./tokens";

/** User appearance preference; `system` follows the OS (§5.6 light/dark system integration). */
export type AppearancePreference = "system" | "light" | "dark";
export type ContrastPreference = "standard" | "high";

export interface ThemeState {
  appearance: AppearancePreference;
  contrast: ContrastPreference;
  /** Last observed OS appearance. */
  systemDark: boolean;
}

export type ThemeAction =
  | { type: "set-appearance"; appearance: AppearancePreference }
  | { type: "set-contrast"; contrast: ContrastPreference }
  | { type: "system-appearance-changed"; dark: boolean };

export const initialThemeState: ThemeState = {
  appearance: "system",
  contrast: "standard",
  systemDark: false,
};

export function themeReducer(state: ThemeState, action: ThemeAction): ThemeState {
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
      const never: never = action;
      throw new Error(`unknown theme action: ${JSON.stringify(never)}`);
    }
  }
}

export function effectiveTheme(state: ThemeState): EffectiveTheme {
  const dark = state.appearance === "system" ? state.systemDark : state.appearance === "dark";
  if (state.contrast === "high") return dark ? "hc-dark" : "hc-light";
  return dark ? "dark" : "light";
}

export function applyThemeToDocument(doc: Document, state: ThemeState): EffectiveTheme {
  const theme = effectiveTheme(state);
  applyTokens(doc.documentElement, theme);
  doc.documentElement.style.colorScheme = theme.endsWith("dark") ? "dark" : "light";
  return theme;
}

/** Subscribes to OS appearance changes; returns an unsubscribe function. */
export function watchSystemAppearance(win: Window, onChange: (dark: boolean) => void): () => void {
  const query = win.matchMedia("(prefers-color-scheme: dark)");
  onChange(query.matches);
  const listener = (event: MediaQueryListEvent) => onChange(event.matches);
  query.addEventListener("change", listener);
  return () => query.removeEventListener("change", listener);
}

/** Subscribes to the OS high-contrast / forced-colors signal. */
export function watchSystemContrast(win: Window, onChange: (high: boolean) => void): () => void {
  const query = win.matchMedia("(prefers-contrast: more), (forced-colors: active)");
  onChange(query.matches);
  const listener = (event: MediaQueryListEvent) => onChange(event.matches);
  query.addEventListener("change", listener);
  return () => query.removeEventListener("change", listener);
}
