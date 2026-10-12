/**
 * Navigation model (§5.1). Six top-level destinations; recording detail stays
 * inside Library and never becomes a top-level destination. Devices (App. B.8)
 * is last so the Mod+1…Mod+5 shortcuts keep their meaning.
 */
export const DESTINATIONS = [
  "library",
  "inbox",
  "jobs",
  "templates",
  "settings",
  "devices",
] as const;
export type Destination = (typeof DESTINATIONS)[number];

export type Route =
  | { name: "library" }
  | { name: "library.recording"; recordingId: string }
  | { name: "library.search" }
  | { name: "inbox" }
  | { name: "jobs" }
  | { name: "jobs.job"; jobId: string }
  | { name: "templates" }
  | { name: "settings" }
  | { name: "settings.activity" }
  | { name: "settings.sources" }
  | { name: "devices" }
  | { name: "devices.device"; peripheralId: string };

export const DEFAULT_ROUTE: Route = { name: "library" };

export const DESTINATION_LABELS: Record<Destination, string> = {
  library: "Library",
  inbox: "Inbox",
  jobs: "Jobs",
  templates: "Templates",
  settings: "Settings",
  devices: "Devices",
};

/** The top-level destination that owns a route, for nav highlighting. */
export function destinationOf(route: Route): Destination {
  switch (route.name) {
    case "library":
    case "library.recording":
    case "library.search":
      return "library";
    case "inbox":
      return "inbox";
    case "jobs":
    case "jobs.job":
      return "jobs";
    case "templates":
      return "templates";
    case "settings":
    case "settings.activity":
    case "settings.sources":
      return "settings";
    case "devices":
    case "devices.device":
      return "devices";
    default: {
      const never: never = route;
      throw new Error(`unhandled route ${JSON.stringify(never)}`);
    }
  }
}

export function formatRoute(route: Route): string {
  switch (route.name) {
    case "library":
      return "#/library";
    case "library.recording":
      return `#/library/${encodeURIComponent(route.recordingId)}`;
    case "library.search":
      return "#/library/search";
    case "inbox":
      return "#/inbox";
    case "jobs":
      return "#/jobs";
    case "jobs.job":
      return `#/jobs/${encodeURIComponent(route.jobId)}`;
    case "templates":
      return "#/templates";
    case "settings":
      return "#/settings";
    case "settings.activity":
      return "#/settings/activity";
    case "settings.sources":
      return "#/settings/sources";
    case "devices":
      return "#/devices";
    case "devices.device":
      return `#/devices/${encodeURIComponent(route.peripheralId)}`;
    default: {
      const never: never = route;
      throw new Error(`unhandled route ${JSON.stringify(never)}`);
    }
  }
}

export function parseRoute(hash: string): Route {
  const path = hash.replace(/^#?\/?/, "").split("?")[0] ?? "";
  const parts = path.split("/").filter(Boolean).map(decodeURIComponent);
  const [head, tail] = parts;
  switch (head) {
    case undefined:
    case "":
    case "library":
      if (tail === "search") return { name: "library.search" };
      return tail ? { name: "library.recording", recordingId: tail } : { name: "library" };
    case "inbox":
      return { name: "inbox" };
    case "jobs":
      return tail ? { name: "jobs.job", jobId: tail } : { name: "jobs" };
    case "templates":
      return { name: "templates" };
    case "settings":
      if (tail === "activity") return { name: "settings.activity" };
      if (tail === "sources") return { name: "settings.sources" };
      return { name: "settings" };
    case "devices":
      return tail ? { name: "devices.device", peripheralId: tail } : { name: "devices" };
    default:
      return DEFAULT_ROUTE;
  }
}

/** Title shown in the window/header for a route. */
export function routeTitle(route: Route): string {
  if (route.name === "library.recording") return "Recording";
  if (route.name === "settings.activity") return "Activity history";
  if (route.name === "settings.sources") return "PLAUD and sources";
  if (route.name === "devices.device") return "Device";
  return DESTINATION_LABELS[destinationOf(route)];
}
