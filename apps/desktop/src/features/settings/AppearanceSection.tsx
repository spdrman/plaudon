import "./settings.css";
import { useAppState } from "../../app/AppState";
import type { AppearancePreference, ContrastPreference } from "../../design/theme";

const APPEARANCE_LABELS: Record<AppearancePreference, string> = {
  system: "Follow the system",
  light: "Light",
  dark: "Dark",
};

const CONTRAST_LABELS: Record<ContrastPreference, string> = {
  standard: "Standard",
  high: "High contrast",
};

/** Appearance and contrast (§5.6): light/dark system integration, optional high contrast. */
export function AppearanceSection() {
  const { appearance, contrast, setAppearance, setContrast } = useAppState();
  return (
    <div className="settings-section__row">
      <fieldset className="settings-fieldset">
        <legend>Appearance</legend>
        {(Object.keys(APPEARANCE_LABELS) as AppearancePreference[]).map((value) => (
          <label className="settings-choice" key={value}>
            <input
              type="radio"
              name="appearance"
              value={value}
              checked={appearance === value}
              onChange={() => setAppearance(value)}
            />
            <span>{APPEARANCE_LABELS[value]}</span>
          </label>
        ))}
      </fieldset>
      <fieldset className="settings-fieldset">
        <legend>Contrast</legend>
        {(Object.keys(CONTRAST_LABELS) as ContrastPreference[]).map((value) => (
          <label className="settings-choice" key={value}>
            <input
              type="radio"
              name="contrast"
              value={value}
              checked={contrast === value}
              onChange={() => setContrast(value)}
            />
            <span>{CONTRAST_LABELS[value]}</span>
          </label>
        ))}
      </fieldset>
    </div>
  );
}
