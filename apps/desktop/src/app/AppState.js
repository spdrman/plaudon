import { jsx as _jsx } from "react/jsx-runtime";
import { createContext, useCallback, useContext, useEffect, useMemo, useReducer, useRef, useState, } from "react";
import { CommandError, invokeCommand } from "../ipc/client";
import { EventBus } from "../ipc/eventBus";
import { applyThemeToDocument, initialThemeState, themeReducer, watchSystemAppearance, watchSystemContrast, } from "../design/theme";
import { DEFAULT_ROUTE, formatRoute, parseRoute } from "./router";
const AppStateContext = createContext(null);
export function useAppState() {
    const value = useContext(AppStateContext);
    if (!value)
        throw new Error("useAppState must be used inside <AppStateProvider>");
    return value;
}
export function AppStateProvider({ children, bus: providedBus, initialRoute }) {
    const busRef = useRef(providedBus ?? null);
    if (!busRef.current)
        busRef.current = new EventBus();
    const bus = busRef.current;
    const [capabilities, setCapabilities] = useState(null);
    const [capabilitiesError, setCapabilitiesError] = useState(null);
    const [route, setRoute] = useState(initialRoute ??
        (typeof window === "undefined" ? DEFAULT_ROUTE : parseRoute(window.location.hash)));
    const [toasts, setToasts] = useState([]);
    const [theme, dispatchTheme] = useReducer(themeReducer, initialThemeState);
    const listeners = useRef(new Set());
    const refreshCapabilities = useCallback(async () => {
        try {
            const next = await invokeCommand("get_capabilities", {});
            setCapabilities(next);
            setCapabilitiesError(null);
            dispatchTheme({ type: "set-appearance", appearance: next.preferences.appearance });
            dispatchTheme({ type: "set-contrast", contrast: next.preferences.contrast });
        }
        catch (error) {
            setCapabilitiesError(error instanceof CommandError ? error : null);
        }
    }, []);
    useEffect(() => {
        void refreshCapabilities();
    }, [refreshCapabilities]);
    useEffect(() => applyThemeToDocument(document, theme) && undefined, [theme]);
    useEffect(() => {
        if (typeof window === "undefined")
            return;
        const stopAppearance = watchSystemAppearance(window, (dark) => dispatchTheme({ type: "system-appearance-changed", dark }));
        const stopContrast = watchSystemContrast(window, (high) => {
            if (high)
                dispatchTheme({ type: "set-contrast", contrast: "high" });
        });
        return () => {
            stopAppearance();
            stopContrast();
        };
    }, []);
    useEffect(() => {
        if (typeof window === "undefined")
            return;
        const onHashChange = () => setRoute(parseRoute(window.location.hash));
        window.addEventListener("hashchange", onHashChange);
        return () => window.removeEventListener("hashchange", onHashChange);
    }, []);
    useEffect(() => {
        const notify = (event) => {
            for (const listener of [...listeners.current])
                listener(event);
        };
        const off = bus.subscribe({
            onEvent: (event) => notify(event),
            // A dropped event invalidates every view: subscribers refetch.
            onResync: (_reason) => notify(null),
        });
        void bus.start().catch(() => notify(null));
        return () => {
            off();
            bus.stop();
        };
    }, [bus]);
    const navigate = useCallback((next) => {
        setRoute(next);
        if (typeof window !== "undefined") {
            const hash = formatRoute(next);
            if (window.location.hash !== hash)
                window.location.hash = hash;
        }
    }, []);
    const pushToast = useCallback((toast) => {
        const id = `toast-${Math.random().toString(36).slice(2, 10)}`;
        setToasts((current) => [...current, { ...toast, id }]);
        return id;
    }, []);
    const dismissToast = useCallback((id) => {
        setToasts((current) => current.filter((t) => t.id !== id));
    }, []);
    const onLibraryChanged = useCallback((handler) => {
        listeners.current.add(handler);
        return () => listeners.current.delete(handler);
    }, []);
    const value = useMemo(() => ({
        capabilities,
        capabilitiesError,
        preferences: capabilities?.preferences ?? null,
        refreshCapabilities,
        applyPreferences: (prefs) => setCapabilities((current) => (current ? { ...current, preferences: prefs } : current)),
        route,
        navigate,
        bus,
        onLibraryChanged,
        toasts,
        pushToast,
        dismissToast,
        appearance: theme.appearance,
        contrast: theme.contrast,
        setAppearance: (appearance) => dispatchTheme({ type: "set-appearance", appearance }),
        setContrast: (contrast) => dispatchTheme({ type: "set-contrast", contrast }),
    }), [
        capabilities,
        capabilitiesError,
        refreshCapabilities,
        route,
        navigate,
        bus,
        onLibraryChanged,
        toasts,
        pushToast,
        dismissToast,
        theme.appearance,
        theme.contrast,
    ]);
    return _jsx(AppStateContext.Provider, { value: value, children: children });
}
