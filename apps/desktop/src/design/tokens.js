/**
 * Design tokens (spec §5.6): spacing, typography, semantic colors, focus states.
 * This module is the single source of truth. Components consume CSS custom
 * properties only; no component may hard-code a color or spacing value.
 */
export const SPACE = {
    "space-0": "0px",
    "space-1": "2px",
    "space-2": "4px",
    "space-3": "8px",
    "space-4": "12px",
    "space-5": "16px",
    "space-6": "24px",
    "space-7": "32px",
    "space-8": "48px",
};
export const RADIUS = {
    "radius-sm": "3px",
    "radius-md": "6px",
    "radius-lg": "10px",
};
export const TYPOGRAPHY = {
    // System typography (§5.3 "platform-appropriate conventions").
    "font-sans": 'ui-sans-serif, -apple-system, "Segoe UI Variable Text", "Segoe UI", system-ui, sans-serif',
    "font-mono": 'ui-monospace, "SF Mono", "Cascadia Mono", Menlo, Consolas, monospace',
    "font-size-xs": "11px",
    "font-size-sm": "12px",
    "font-size-md": "13px",
    "font-size-lg": "15px",
    "font-size-xl": "19px",
    "font-size-2xl": "24px",
    "line-height-tight": "1.25",
    "line-height-normal": "1.5",
    "font-weight-regular": "400",
    "font-weight-medium": "550",
    "font-weight-bold": "650",
};
export const MOTION = {
    "duration-fast": "90ms",
    "duration-normal": "160ms",
};
/** Semantic color roles. Every theme must define every role. */
export const COLOR_ROLES = [
    "color-bg-canvas",
    "color-bg-surface",
    "color-bg-raised",
    "color-bg-selected",
    "color-bg-hover",
    "color-text-primary",
    "color-text-secondary",
    "color-text-inverse",
    "color-border-subtle",
    "color-border-strong",
    "color-accent",
    "color-accent-text",
    "color-status-neutral",
    "color-status-running",
    "color-status-waiting",
    "color-status-success",
    "color-status-warning",
    "color-status-danger",
    "color-focus-ring",
    "color-overlay",
];
const light = {
    "color-bg-canvas": "#f6f6f7",
    "color-bg-surface": "#ffffff",
    "color-bg-raised": "#ffffff",
    "color-bg-selected": "#dbe7fb",
    "color-bg-hover": "#eceef1",
    "color-text-primary": "#14161a",
    "color-text-secondary": "#53585f",
    "color-text-inverse": "#ffffff",
    "color-border-subtle": "#d7dade",
    "color-border-strong": "#767c84",
    "color-accent": "#1a4fa0",
    "color-accent-text": "#ffffff",
    "color-status-neutral": "#53585f",
    "color-status-running": "#1a4fa0",
    "color-status-waiting": "#7a4a00",
    "color-status-success": "#0f6b3f",
    "color-status-warning": "#8a4b00",
    "color-status-danger": "#a1121f",
    "color-focus-ring": "#0b57d0",
    "color-overlay": "rgba(16, 18, 20, 0.45)",
};
const dark = {
    "color-bg-canvas": "#17191c",
    "color-bg-surface": "#1e2125",
    "color-bg-raised": "#262a2f",
    "color-bg-selected": "#2b3b55",
    "color-bg-hover": "#2a2e34",
    "color-text-primary": "#f2f4f6",
    "color-text-secondary": "#b3bac2",
    "color-text-inverse": "#14161a",
    "color-border-subtle": "#3a3f45",
    "color-border-strong": "#6c747d",
    "color-accent": "#8fb4f2",
    "color-accent-text": "#101418",
    "color-status-neutral": "#b3bac2",
    "color-status-running": "#8fb4f2",
    "color-status-waiting": "#e3b667",
    "color-status-success": "#67c391",
    "color-status-warning": "#e8a35c",
    "color-status-danger": "#f08a92",
    "color-focus-ring": "#9ec4ff",
    "color-overlay": "rgba(0, 0, 0, 0.6)",
};
const highContrastLight = {
    ...light,
    "color-bg-canvas": "#ffffff",
    "color-bg-surface": "#ffffff",
    "color-bg-raised": "#ffffff",
    "color-bg-selected": "#cfe0ff",
    "color-bg-hover": "#e6e6e6",
    "color-text-primary": "#000000",
    "color-text-secondary": "#1c1c1c",
    "color-border-subtle": "#000000",
    "color-border-strong": "#000000",
    "color-accent": "#00349b",
    "color-status-neutral": "#1c1c1c",
    "color-status-running": "#00349b",
    "color-status-waiting": "#5c3100",
    "color-status-success": "#004d2b",
    "color-status-warning": "#6b2f00",
    "color-status-danger": "#8a0010",
    "color-focus-ring": "#00349b",
};
const highContrastDark = {
    ...dark,
    "color-bg-canvas": "#000000",
    "color-bg-surface": "#000000",
    "color-bg-raised": "#0d0d0d",
    "color-bg-selected": "#003c74",
    "color-bg-hover": "#1a1a1a",
    "color-text-primary": "#ffffff",
    "color-text-secondary": "#f0f0f0",
    "color-border-subtle": "#ffffff",
    "color-border-strong": "#ffffff",
    "color-accent": "#b8d4ff",
    "color-accent-text": "#000000",
    "color-status-neutral": "#f0f0f0",
    "color-status-running": "#b8d4ff",
    "color-status-waiting": "#ffd48a",
    "color-status-success": "#86e3ad",
    "color-status-warning": "#ffc078",
    "color-status-danger": "#ffb3b8",
    "color-focus-ring": "#ffffff",
};
const SCALES = {
    light,
    dark,
    "hc-light": highContrastLight,
    "hc-dark": highContrastDark,
};
/** Focus ring widens in high contrast; it is never removed (§5.3, WCAG 2.2 AA). */
const FOCUS = {
    light: { "focus-ring-width": "2px", "focus-ring-offset": "2px" },
    dark: { "focus-ring-width": "2px", "focus-ring-offset": "2px" },
    "hc-light": { "focus-ring-width": "3px", "focus-ring-offset": "2px" },
    "hc-dark": { "focus-ring-width": "3px", "focus-ring-offset": "2px" },
};
/** Minimum usable desktop width (§5.6): below this the window is clamped, we never stack mobile-style. */
export const MIN_DESKTOP_WIDTH = 880;
export const MIN_DESKTOP_HEIGHT = 560;
/** Below this the library sidebar collapses to an icon rail (still desktop, not mobile). */
export const COMPACT_LAYOUT_WIDTH = 1040;
/** WCAG 2.2 AA target size (minimum) for pointer targets. */
export const MIN_TARGET_SIZE = 24;
export function resolveTokens(theme) {
    return {
        ...SPACE,
        ...RADIUS,
        ...TYPOGRAPHY,
        ...MOTION,
        ...SCALES[theme],
        ...FOCUS[theme],
    };
}
export function applyTokens(root, theme) {
    const tokens = resolveTokens(theme);
    for (const [name, value] of Object.entries(tokens)) {
        root.style.setProperty(`--${name}`, value);
    }
    root.dataset.theme = theme;
}
