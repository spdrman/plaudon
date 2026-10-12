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
import { RecordingDetailScreen } from "../features/recording/RecordingDetailScreen";
import { SearchScreen } from "../features/search/SearchScreen";
import { InboxScreen } from "../features/library/InboxScreen";
import { JobsScreen } from "../features/jobs/JobsScreen";
import { TemplatesScreen } from "../features/templates/TemplatesScreen";
import { SettingsScreen } from "../features/settings/SettingsScreen";
import { ActivityScreen } from "../features/activity/ActivityScreen";
import { SourcesScreen } from "../features/sources/SourcesScreen";
import { DevicesScreen } from "../features/devices/DevicesScreen";
import { ImportDropTarget } from "../features/import/ImportDropTarget";
import { ImportButton } from "../features/import/ImportButton";
import { ActivityBell } from "../features/activity/ActivityBell";
import { EmptyState } from "../components/EmptyState";
import { Button } from "../components/Button";
import { ShortcutsDialog } from "./ShortcutsDialog";
import { destinationForEvent, isTextEntryTarget } from "./shortcuts";

export function App() {
  const {
    route,
    capabilities,
    capabilitiesError,
    refreshCapabilities,
    applyPreferences,
    navigate,
    onLibraryChanged,
  } = useAppState();
  const width = useWindowWidth();
  const [shortcutsOpen, setShortcutsOpen] = useState(false);

  const onKeyDown = useCallback(
    (event: KeyboardEvent) => {
      if (isTextEntryTarget(event.target)) return;
      const destination = destinationForEvent(event);
      if (destination) {
        event.preventDefault();
        navigate({ name: destination } as never);
        return;
      }
      if (event.key === "?" && !event.metaKey && !event.ctrlKey) {
        event.preventDefault();
        setShortcutsOpen(true);
      }
    },
    [navigate],
  );

  useEffect(() => {
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [onKeyDown]);

  // Clicking an OS notification activates the window and asks the UI to open
  // the affected recording or job (§5.5, ADR-0020). Delivery is a hint: we
  // navigate, then the destination refetches its own authoritative state.
  useEffect(
    () =>
      onLibraryChanged((event) => {
        if (!event) return;
        const target = activationTargetOf(event);
        if (!target) return;
        if (target.kind === "recording") navigate({ name: "library.recording", recordingId: target.id });
        else if (target.kind === "job") navigate({ name: "jobs.job", jobId: target.id });
        else navigate({ name: "settings.activity" });
      }),
    [onLibraryChanged, navigate],
  );

  if (capabilitiesError && !capabilities) {
    return (
      <main className="app-content">
        <EmptyState
          title="Plaudon could not start"
          body={capabilitiesError.message}
          actions={
            <Button variant="primary" onClick={() => void refreshCapabilities()}>
              Try again
            </Button>
          }
        />
      </main>
    );
  }

  if (!capabilities) {
    return (
      <main className="app-content" aria-busy="true">
        <p>Loading your library…</p>
      </main>
    );
  }

  if (!capabilities.preferences.onboarding_completed) {
    return (
      <OnboardingFlow
        capabilities={capabilities}
        onCompleted={(preferences) => {
          applyPreferences(preferences);
          navigate({ name: "library" });
        }}
      />
    );
  }

  return (
    <ImportDropTarget>
      <div className="app-shell" data-compact={width < COMPACT_LAYOUT_WIDTH}>
        <a className="skip-link" href="#main-region">
          Skip to main content
        </a>
        <AppNav />
        <div className="app-main">
          <header className="app-header">
            <h1 className="app-header__title">{routeTitle(route)}</h1>
            <div className="app-header__actions">
              <ActivityBell />
              <ImportButton />
            </div>
          </header>
          {width < MIN_DESKTOP_WIDTH ? (
            <p className="screen-width-notice" role="status">
              This window is narrower than the {MIN_DESKTOP_WIDTH}px minimum. Columns are scrollable;
              resize the window for the full layout.
            </p>
          ) : null}
          <main
            className={
              route.name === "library" || route.name === "library.recording"
                ? "app-content app-content--flush"
                : "app-content"
            }
          >
            {/* `main` cannot carry the tabpanel role, so the panel is an inner
                box that the navigation's `aria-controls` points at. */}
            <div
              className="app-content__panel"
              id="main-region"
              role="tabpanel"
              aria-labelledby={`nav-${route.name.split(".")[0]}`}
              tabIndex={-1}
            >
              <RouteView />
            </div>
          </main>
        </div>
        <ToastRegion />
        <ShortcutsDialog open={shortcutsOpen} onClose={() => setShortcutsOpen(false)} />
      </div>
    </ImportDropTarget>
  );
}

function RouteView() {
  const { route, navigate } = useAppState();
  switch (route.name) {
    case "library":
      return (
        <LibraryScreen
          onOpenRecording={(recordingId) => navigate({ name: "library.recording", recordingId })}
        />
      );
    case "library.search":
      return (
        <SearchScreen
          onNavigate={(recordingId, _revisionId, _seqIndex) =>
            navigate({ name: "library.recording", recordingId })
          }
        />
      );
    case "library.recording":
      return (
        <RecordingDetailScreen
          recordingId={route.recordingId}
          onBack={() => navigate({ name: "library" })}
        />
      );
    case "inbox":
      return (
        <InboxScreen
          onOpenRecording={(recordingId) => navigate({ name: "library.recording", recordingId })}
        />
      );
    case "jobs":
    case "jobs.job":
      return (
        <JobsScreen
          focusJobId={route.name === "jobs.job" ? route.jobId : null}
          onOpenTarget={(target) =>
            navigate(
              target.kind === "recording"
                ? { name: "library.recording", recordingId: target.id }
                : { name: "jobs.job", jobId: target.id },
            )
          }
        />
      );
    case "templates":
      return <TemplatesScreen />;
    case "devices":
    case "devices.device":
      return (
        <DevicesScreen
          selectedId={route.name === "devices.device" ? route.peripheralId : null}
          onSelect={(peripheralId) =>
            navigate(
              peripheralId ? { name: "devices.device", peripheralId } : { name: "devices" },
            )
          }
        />
      );
    case "settings":
      return <SettingsScreen />;
    case "settings.activity":
      return (
        <ActivityScreen
          onOpenTarget={(target) =>
            navigate(
              target.kind === "recording"
                ? { name: "library.recording", recordingId: target.id }
                : target.kind === "job"
                  ? { name: "jobs.job", jobId: target.id }
                  : { name: "settings" },
            )
          }
        />
      );
    case "settings.sources":
      return <SourcesScreen />;
    default: {
      const never: never = route;
      throw new Error(`unhandled route ${JSON.stringify(never)}`);
    }
  }
}

function useWindowWidth(): number {
  const [width, setWidth] = useState(() =>
    typeof window === "undefined" ? 1280 : window.innerWidth,
  );
  useEffect(() => {
    if (typeof window === "undefined") return;
    const onResize = () => setWidth(window.innerWidth);
    window.addEventListener("resize", onResize);
    return () => window.removeEventListener("resize", onResize);
  }, []);
  return width;
}
