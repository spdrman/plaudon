import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useReducer,
  useRef,
  useState,
  type ReactNode,
} from "react";
import type { Capabilities, Preferences, UiEvent } from "../ipc/contract";
import { CommandError, invokeCommand } from "../ipc/client";
import { EventBus, type ResyncReason } from "../ipc/eventBus";
import {
  applyThemeToDocument,
  initialThemeState,
  themeReducer,
  watchSystemAppearance,
  watchSystemContrast,
  type AppearancePreference,
  type ContrastPreference,
} from "../design/theme";
import { DEFAULT_ROUTE, formatRoute, parseRoute, type Route } from "./router";

export interface Toast {
  id: string;
  title: string;
  body?: string;
  tone: "info" | "success" | "warning" | "danger";
}

export interface AppStateValue {
  capabilities: Capabilities | null;
  capabilitiesError: CommandError | null;
  preferences: Preferences | null;
  refreshCapabilities: () => Promise<void>;
  applyPreferences: (prefs: Preferences) => void;
  route: Route;
  navigate: (route: Route) => void;
  bus: EventBus;
  /** Subscribe to committed-state change hints; the callback must refetch. */
  onLibraryChanged: (handler: (event: UiEvent | null) => void) => () => void;
  toasts: Toast[];
  pushToast: (toast: Omit<Toast, "id">) => string;
  dismissToast: (id: string) => void;
  appearance: AppearancePreference;
  contrast: ContrastPreference;
  setAppearance: (value: AppearancePreference) => void;
  setContrast: (value: ContrastPreference) => void;
}

const AppStateContext = createContext<AppStateValue | null>(null);

export function useAppState(): AppStateValue {
  const value = useContext(AppStateContext);
  if (!value) throw new Error("useAppState must be used inside <AppStateProvider>");
  return value;
}

export interface AppStateProviderProps {
  children: ReactNode;
  bus?: EventBus;
  initialRoute?: Route;
}

export function AppStateProvider({ children, bus: providedBus, initialRoute }: AppStateProviderProps) {
  const busRef = useRef<EventBus | null>(providedBus ?? null);
  if (!busRef.current) busRef.current = new EventBus();
  const bus = busRef.current;

  const [capabilities, setCapabilities] = useState<Capabilities | null>(null);
  const [capabilitiesError, setCapabilitiesError] = useState<CommandError | null>(null);
  const [route, setRoute] = useState<Route>(
    initialRoute ??
      (typeof window === "undefined" ? DEFAULT_ROUTE : parseRoute(window.location.hash)),
  );
  const [toasts, setToasts] = useState<Toast[]>([]);
  const [theme, dispatchTheme] = useReducer(themeReducer, initialThemeState);
  const listeners = useRef(new Set<(event: UiEvent | null) => void>());

  const refreshCapabilities = useCallback(async () => {
    try {
      const next = await invokeCommand("get_capabilities", {});
      setCapabilities(next);
      setCapabilitiesError(null);
      dispatchTheme({ type: "set-appearance", appearance: next.preferences.appearance });
      dispatchTheme({ type: "set-contrast", contrast: next.preferences.contrast });
    } catch (error) {
      setCapabilitiesError(error instanceof CommandError ? error : null);
    }
  }, []);

  useEffect(() => {
    void refreshCapabilities();
  }, [refreshCapabilities]);

  useEffect(() => applyThemeToDocument(document, theme) && undefined, [theme]);

  useEffect(() => {
    if (typeof window === "undefined") return;
    const stopAppearance = watchSystemAppearance(window, (dark) =>
      dispatchTheme({ type: "system-appearance-changed", dark }),
    );
    const stopContrast = watchSystemContrast(window, (high) => {
      if (high) dispatchTheme({ type: "set-contrast", contrast: "high" });
    });
    return () => {
      stopAppearance();
      stopContrast();
    };
  }, []);

  useEffect(() => {
    if (typeof window === "undefined") return;
    const onHashChange = () => setRoute(parseRoute(window.location.hash));
    window.addEventListener("hashchange", onHashChange);
    return () => window.removeEventListener("hashchange", onHashChange);
  }, []);

  useEffect(() => {
    const notify = (event: UiEvent | null) => {
      for (const listener of [...listeners.current]) listener(event);
    };
    const off = bus.subscribe({
      onEvent: (event) => notify(event),
      // A dropped event invalidates every view: subscribers refetch.
      onResync: (_reason: ResyncReason) => notify(null),
    });
    void bus.start().catch(() => notify(null));
    return () => {
      off();
      bus.stop();
    };
  }, [bus]);

  const navigate = useCallback((next: Route) => {
    setRoute(next);
    if (typeof window !== "undefined") {
      const hash = formatRoute(next);
      if (window.location.hash !== hash) window.location.hash = hash;
    }
  }, []);

  const pushToast = useCallback((toast: Omit<Toast, "id">) => {
    const id = `toast-${Math.random().toString(36).slice(2, 10)}`;
    setToasts((current) => [...current, { ...toast, id }]);
    return id;
  }, []);

  const dismissToast = useCallback((id: string) => {
    setToasts((current) => current.filter((t) => t.id !== id));
  }, []);

  const onLibraryChanged = useCallback((handler: (event: UiEvent | null) => void) => {
    listeners.current.add(handler);
    return () => listeners.current.delete(handler);
  }, []);

  const value = useMemo<AppStateValue>(
    () => ({
      capabilities,
      capabilitiesError,
      preferences: capabilities?.preferences ?? null,
      refreshCapabilities,
      applyPreferences: (prefs: Preferences) =>
        setCapabilities((current) => (current ? { ...current, preferences: prefs } : current)),
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
    }),
    [
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
    ],
  );

  return <AppStateContext.Provider value={value}>{children}</AppStateContext.Provider>;
}
