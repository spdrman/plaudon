/**
 * Navigation model (§5.1). Five top-level destinations; recording detail stays
 * inside Library and never becomes a top-level destination.
 */
export const DESTINATIONS = ["library", "inbox", "jobs", "templates", "settings"];
export const DEFAULT_ROUTE = { name: "library" };
export const DESTINATION_LABELS = {
    library: "Library",
    inbox: "Inbox",
    jobs: "Jobs",
    templates: "Templates",
    settings: "Settings",
};
/** The top-level destination that owns a route, for nav highlighting. */
export function destinationOf(route) {
    switch (route.name) {
        case "library":
        case "library.recording":
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
        default: {
            const never = route;
            throw new Error(`unhandled route ${JSON.stringify(never)}`);
        }
    }
}
export function formatRoute(route) {
    switch (route.name) {
        case "library":
            return "#/library";
        case "library.recording":
            return `#/library/${encodeURIComponent(route.recordingId)}`;
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
        default: {
            const never = route;
            throw new Error(`unhandled route ${JSON.stringify(never)}`);
        }
    }
}
export function parseRoute(hash) {
    const path = hash.replace(/^#?\/?/, "").split("?")[0] ?? "";
    const parts = path.split("/").filter(Boolean).map(decodeURIComponent);
    const [head, tail] = parts;
    switch (head) {
        case undefined:
        case "":
        case "library":
            return tail ? { name: "library.recording", recordingId: tail } : { name: "library" };
        case "inbox":
            return { name: "inbox" };
        case "jobs":
            return tail ? { name: "jobs.job", jobId: tail } : { name: "jobs" };
        case "templates":
            return { name: "templates" };
        case "settings":
            if (tail === "activity")
                return { name: "settings.activity" };
            if (tail === "sources")
                return { name: "settings.sources" };
            return { name: "settings" };
        default:
            return DEFAULT_ROUTE;
    }
}
/** Title shown in the window/header for a route. */
export function routeTitle(route) {
    if (route.name === "library.recording")
        return "Recording";
    if (route.name === "settings.activity")
        return "Activity history";
    if (route.name === "settings.sources")
        return "PLAUD and sources";
    return DESTINATION_LABELS[destinationOf(route)];
}
