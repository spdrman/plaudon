import { jsx as _jsx, jsxs as _jsxs } from "react/jsx-runtime";
import { useCallback, useEffect, useState } from "react";
import "../activity/activity.css";
import { NOTIFICATION_CATEGORIES } from "../../ipc/contract";
import { useCommandMutation } from "../../ipc/useCommand";
import { useAppState } from "../../app/AppState";
import { CATEGORY_PRESENTATION } from "../activity/ActivityScreen";
const FALLBACK = {
    enabled: false,
    categories: {
        import_finished: false,
        transcription_finished: false,
        requires_attention: false,
        cloud_budget_exceeded: false,
    },
};
/** §5.5 opt-in notification preferences; the activity history is unaffected by them. */
export function NotificationSettingsSection() {
    const { capabilities, preferences, applyPreferences } = useAppState();
    const [settings, setSettings] = useState(preferences?.notifications ?? FALLBACK);
    const [permission, setPermission] = useState(null);
    const save = useCommandMutation("set_notification_settings");
    useEffect(() => {
        if (preferences)
            setSettings(preferences.notifications);
    }, [preferences]);
    const effectivePermission = permission ?? capabilities?.permissions.notifications ?? null;
    const persist = useCallback(async (next) => {
        setSettings(next);
        try {
            const response = await save.run({ settings: next });
            setPermission(response.os_permission);
            if (preferences)
                applyPreferences({ ...preferences, notifications: response.settings });
        }
        catch {
            // The envelope is rendered below; the toggle stays where the user put it.
        }
    }, [applyPreferences, preferences, save]);
    return (_jsxs("section", { className: "notification-settings", "aria-labelledby": "notification-settings-heading", children: [_jsx("h2", { id: "notification-settings-heading", children: "Desktop notifications" }), _jsx("p", { className: "notification-settings__note", children: "Notifications are opt-in and use privacy-safe text. Everything listed here is also recorded in the activity history, which stays complete whatever you choose." }), _jsxs("label", { className: "notification-settings__check", children: [_jsx("input", { type: "checkbox", role: "switch", checked: settings.enabled, onChange: (event) => void persist({ ...settings, enabled: event.target.checked }) }), "Show desktop notifications"] }), _jsxs("fieldset", { className: "notification-settings__group", disabled: !settings.enabled, children: [_jsx("legend", { className: "notification-settings__legend", children: "Notify me about" }), NOTIFICATION_CATEGORIES.map((category) => (_jsxs("label", { className: "notification-settings__check", children: [_jsx("input", { type: "checkbox", checked: settings.categories[category], onChange: (event) => void persist({
                                    ...settings,
                                    categories: { ...settings.categories, [category]: event.target.checked },
                                }) }), CATEGORY_PRESENTATION[category].label] }, category)))] }), save.error ? (_jsx("p", { className: "activity__error", role: "alert", children: save.error.envelope.message })) : null, _jsx("p", { className: "notification-settings__note", role: "status", children: permissionText(effectivePermission) })] }));
}
function permissionText(permission) {
    switch (permission) {
        case "granted":
            return "Operating system status: notifications are permitted.";
        case "denied":
            return "Operating system status: notifications are blocked for this app. The activity history still records every event. To change it, allow notifications for Plaudon in your operating system's notification settings.";
        case "unsupported":
            return "Operating system status: this system does not support desktop notifications. The activity history still records every event.";
        case "not_requested":
            return "Operating system status: permission has not been requested yet. It is requested the first time a notification is sent.";
        case null:
            return "Operating system status: unknown.";
    }
}
