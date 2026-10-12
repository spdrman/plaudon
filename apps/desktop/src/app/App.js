import { jsx as _jsx, jsxs as _jsxs } from "react/jsx-runtime";
import { useCallback, useEffect, useState } from "react";
import "../design/global.css";
import "../components/components.css";
import "./app.css";
import { useAppState } from "./AppState";
import { AppNav } from "./AppNav";
import { ToastRegion } from "./ToastRegion";
import { routeTitle } from "./router";
import { activationTargetOf } from "../ipc/contract";
import { COMPACT_LAYOUT_WIDTH, MIN_DESKTOP_WIDTH } from "../design/tokens";
import { OnboardingFlow } from "../features/onboarding/OnboardingFlow";
import { LibraryScreen } from "../features/library/LibraryScreen";
import { InboxScreen } from "../features/library/InboxScreen";
import { RecordingDetailScreen } from "../features/recording/RecordingDetailScreen";
import { JobsScreen } from "../features/jobs/JobsScreen";
import { TemplatesScreen } from "../features/templates/TemplatesScreen";
import { SettingsScreen } from "../features/settings/SettingsScreen";
import { ActivityScreen } from "../features/activity/ActivityScreen";
import { SourcesScreen } from "../features/sources/SourcesScreen";
import { ImportDropTarget } from "../features/import/ImportDropTarget";
import { ImportButton } from "../features/import/ImportButton";
import { ActivityBell } from "../features/activity/ActivityBell";
import { EmptyState } from "../components/EmptyState";
import { Button } from "../components/Button";
import { ShortcutsDialog } from "./ShortcutsDialog";
import { destinationForEvent, isTextEntryTarget } from "./shortcuts";
export function App() {
    const { route, capabilities, capabilitiesError, refreshCapabilities, applyPreferences, navigate, onLibraryChanged, } = useAppState();
    const width = useWindowWidth();
    const [shortcutsOpen, setShortcutsOpen] = useState(false);
    const onKeyDown = useCallback((event) => {
        if (isTextEntryTarget(event.target))
            return;
        const destination = destinationForEvent(event);
        if (destination) {
            event.preventDefault();
            navigate({ name: destination });
            return;
        }
        if (event.key === "?" && !event.metaKey && !event.ctrlKey) {
            event.preventDefault();
            setShortcutsOpen(true);
        }
    }, [navigate]);
    useEffect(() => {
        window.addEventListener("keydown", onKeyDown);
        return () => window.removeEventListener("keydown", onKeyDown);
    }, [onKeyDown]);
    // Clicking an OS notification activates the window and asks the UI to open
    // the affected recording or job (§5.5, ADR-0020). Delivery is a hint: we
    // navigate, then the destination refetches its own authoritative state.
    useEffect(() => onLibraryChanged((event) => {
        if (!event)
            return;
        const target = activationTargetOf(event);
        if (!target)
            return;
        if (target.kind === "recording")
            navigate({ name: "library.recording", recordingId: target.id });
        else if (target.kind === "job")
            navigate({ name: "jobs.job", jobId: target.id });
        else
            navigate({ name: "settings.activity" });
    }), [onLibraryChanged, navigate]);
    if (capabilitiesError && !capabilities) {
        return (_jsx("main", { className: "app-content", children: _jsx(EmptyState, { title: "Plaudon could not start", body: capabilitiesError.message, actions: _jsx(Button, { variant: "primary", onClick: () => void refreshCapabilities(), children: "Try again" }) }) }));
    }
    if (!capabilities) {
        return (_jsx("main", { className: "app-content", "aria-busy": "true", children: _jsx("p", { children: "Loading your library\u2026" }) }));
    }
    if (!capabilities.preferences.onboarding_completed) {
        return (_jsx(OnboardingFlow, { capabilities: capabilities, onCompleted: (preferences) => {
                applyPreferences(preferences);
                navigate({ name: "library" });
            } }));
    }
    return (_jsx(ImportDropTarget, { children: _jsxs("div", { className: "app-shell", "data-compact": width < COMPACT_LAYOUT_WIDTH, children: [_jsx("a", { className: "skip-link", href: "#main-region", children: "Skip to main content" }), _jsx(AppNav, {}), _jsxs("div", { className: "app-main", children: [_jsxs("header", { className: "app-header", children: [_jsx("h1", { className: "app-header__title", children: routeTitle(route) }), _jsxs("div", { className: "app-header__actions", children: [_jsx(ActivityBell, {}), _jsx(ImportButton, {})] })] }), width < MIN_DESKTOP_WIDTH ? (_jsxs("p", { className: "screen-width-notice", role: "status", children: ["This window is narrower than the ", MIN_DESKTOP_WIDTH, "px minimum. Columns are scrollable; resize the window for the full layout."] })) : null, _jsx("main", { className: route.name === "library" || route.name === "library.recording"
                                ? "app-content app-content--flush"
                                : "app-content", children: _jsx("div", { className: "app-content__panel", id: "main-region", role: "tabpanel", "aria-labelledby": `nav-${route.name.split(".")[0]}`, tabIndex: -1, children: _jsx(RouteView, {}) }) })] }), _jsx(ToastRegion, {}), _jsx(ShortcutsDialog, { open: shortcutsOpen, onClose: () => setShortcutsOpen(false) })] }) }));
}
function RouteView() {
    const { route, navigate } = useAppState();
    switch (route.name) {
        case "library":
            return (_jsx(LibraryScreen, { onOpenRecording: (recordingId) => navigate({ name: "library.recording", recordingId }) }));
        case "library.recording":
            return (_jsx(RecordingDetailScreen, { recordingId: route.recordingId, onBack: () => navigate({ name: "library" }) }));
        case "inbox":
            return (_jsx(InboxScreen, { onOpenRecording: (recordingId) => navigate({ name: "library.recording", recordingId }) }));
        case "jobs":
        case "jobs.job":
            return (_jsx(JobsScreen, { focusJobId: route.name === "jobs.job" ? route.jobId : null, onOpenTarget: (target) => navigate(target.kind === "recording"
                    ? { name: "library.recording", recordingId: target.id }
                    : { name: "jobs.job", jobId: target.id }) }));
        case "templates":
            return _jsx(TemplatesScreen, {});
        case "settings":
            return _jsx(SettingsScreen, {});
        case "settings.activity":
            return (_jsx(ActivityScreen, { onOpenTarget: (target) => navigate(target.kind === "recording"
                    ? { name: "library.recording", recordingId: target.id }
                    : target.kind === "job"
                        ? { name: "jobs.job", jobId: target.id }
                        : { name: "settings" }) }));
        case "settings.sources":
            return _jsx(SourcesScreen, {});
        default: {
            const never = route;
            throw new Error(`unhandled route ${JSON.stringify(never)}`);
        }
    }
}
function useWindowWidth() {
    const [width, setWidth] = useState(() => typeof window === "undefined" ? 1280 : window.innerWidth);
    useEffect(() => {
        if (typeof window === "undefined")
            return;
        const onResize = () => setWidth(window.innerWidth);
        window.addEventListener("resize", onResize);
        return () => window.removeEventListener("resize", onResize);
    }, []);
    return width;
}
