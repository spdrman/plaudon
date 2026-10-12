import { jsx as _jsx, jsxs as _jsxs } from "react/jsx-runtime";
import "./settings.css";
import { useAppState } from "../../app/AppState";
import { Button } from "../../components/Button";
import { AppearanceSection } from "./AppearanceSection";
import { PrivacySection } from "./PrivacySection";
import { AiKeySection } from "./AiKeySection";
import { NotificationSettingsSection } from "./NotificationSettingsSection";
import { formatBytes } from "./bytes";
/** Settings destination (§5.1, §5.2): privacy, AI providers, notifications, storage. */
export function SettingsScreen() {
    const { capabilities, navigate } = useAppState();
    const storage = capabilities?.storage ?? null;
    return (_jsxs("div", { className: "settings", children: [_jsx("h2", { className: "settings__title", children: "Settings" }), _jsx(SettingsSection, { title: "Appearance", children: _jsx(AppearanceSection, {}) }), _jsx(SettingsSection, { title: "Privacy & network", children: _jsx(PrivacySection, {}) }), _jsx(SettingsSection, { title: "AI key", children: _jsx(AiKeySection, {}) }), _jsx(SettingsSection, { title: "Notifications", children: _jsx(NotificationSettingsSection, {}) }), _jsx(SettingsSection, { title: "Storage", children: storage ? (_jsxs("dl", { className: "settings-facts", children: [_jsx("dt", { children: "Library location" }), _jsx("dd", { children: storage.library_path }), _jsx("dt", { children: "Free space" }), _jsx("dd", { children: formatBytes(storage.free_bytes) }), _jsx("dt", { children: "Total space" }), _jsx("dd", { children: formatBytes(storage.total_bytes) })] })) : (_jsx("p", { className: "settings-section__note", children: "Storage facts are not available yet." })) }), _jsxs(SettingsSection, { title: "Activity history", children: [_jsx("p", { className: "settings-section__note", children: "Every import, transcription and attention notice is kept in the app, whether or not the operating system showed a notification." }), _jsx(Button, { onClick: () => navigate({ name: "settings.activity" }), children: "Open activity history" })] })] }));
}
function SettingsSection({ title, children }) {
    const id = `settings-${title.toLowerCase().replace(/[^a-z]+/g, "-")}`;
    return (_jsxs("section", { className: "settings-section", "aria-labelledby": id, children: [_jsx("h2", { className: "settings-section__title", id: id, children: title }), children] }));
}
