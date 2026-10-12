import { jsx as _jsx, jsxs as _jsxs } from "react/jsx-runtime";
import "./settings.css";
import { useAppState } from "../../app/AppState";
const APPEARANCE_LABELS = {
    system: "Follow the system",
    light: "Light",
    dark: "Dark",
};
const CONTRAST_LABELS = {
    standard: "Standard",
    high: "High contrast",
};
/** Appearance and contrast (§5.6): light/dark system integration, optional high contrast. */
export function AppearanceSection() {
    const { appearance, contrast, setAppearance, setContrast } = useAppState();
    return (_jsxs("div", { className: "settings-section__row", children: [_jsxs("fieldset", { className: "settings-fieldset", children: [_jsx("legend", { children: "Appearance" }), Object.keys(APPEARANCE_LABELS).map((value) => (_jsxs("label", { className: "settings-choice", children: [_jsx("input", { type: "radio", name: "appearance", value: value, checked: appearance === value, onChange: () => setAppearance(value) }), _jsx("span", { children: APPEARANCE_LABELS[value] })] }, value)))] }), _jsxs("fieldset", { className: "settings-fieldset", children: [_jsx("legend", { children: "Contrast" }), Object.keys(CONTRAST_LABELS).map((value) => (_jsxs("label", { className: "settings-choice", children: [_jsx("input", { type: "radio", name: "contrast", value: value, checked: contrast === value, onChange: () => setContrast(value) }), _jsx("span", { children: CONTRAST_LABELS[value] })] }, value)))] })] }));
}
