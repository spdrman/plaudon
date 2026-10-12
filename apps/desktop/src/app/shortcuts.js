import { DESTINATIONS } from "./router";
/**
 * The shortcut inventory shown by the help dialog (§5.3). Feature-local
 * shortcuts are declared here too so the help stays complete; each feature
 * implements the handler next to its own component.
 */
export const SHORTCUTS = [
    { keys: "Mod+1 … Mod+5", description: "Go to Library, Inbox, Jobs, Templates, Settings", group: "Navigation" },
    { keys: "Tab / Shift+Tab", description: "Move between controls", group: "Navigation" },
    { keys: "Arrow keys", description: "Move within the navigation, lists and transcript", group: "Navigation" },
    { keys: "?", description: "Show this keyboard reference", group: "General" },
    { keys: "Esc", description: "Close the dialog or cancel the current edit", group: "General" },
    { keys: "Enter", description: "Open the focused recording or job", group: "Library" },
    { keys: "Space", description: "Toggle selection of the focused row", group: "Library" },
    { keys: "Mod+A", description: "Select all loaded rows", group: "Library" },
    { keys: "Space", description: "Play or pause (when not editing text)", group: "Playback" },
    { keys: "J / L", description: "Jump back or forward 10 seconds", group: "Playback" },
    { keys: "K", description: "Pause", group: "Playback" },
    { keys: ", / .", description: "Decrease or increase playback speed", group: "Playback" },
    { keys: "Mod+Z / Shift+Mod+Z", description: "Undo or redo a transcript edit", group: "Transcript" },
    { keys: "Mod+F", description: "Search within the transcript", group: "Transcript" },
    { keys: "Mod+C", description: "Copy the selected passage with timestamps", group: "Transcript" },
];
export function isMacPlatform(nav) {
    const text = `${nav?.platform ?? ""} ${nav?.userAgent ?? ""}`;
    return /mac/i.test(text);
}
export function formatShortcut(keys, mac) {
    return keys.replace(/Mod/g, mac ? "Cmd" : "Ctrl");
}
/** Maps a keydown to a destination, or null when the event is not a nav shortcut. */
export function destinationForEvent(event) {
    if (!(event.metaKey || event.ctrlKey) || event.altKey || event.shiftKey)
        return null;
    const index = Number.parseInt(event.key, 10);
    if (!Number.isInteger(index) || index < 1 || index > DESTINATIONS.length)
        return null;
    return DESTINATIONS[index - 1] ?? null;
}
/** True when the event originates from a text entry control, where typing wins. */
export function isTextEntryTarget(target) {
    if (!(target instanceof HTMLElement))
        return false;
    if (target.isContentEditable)
        return true;
    const tag = target.tagName.toLowerCase();
    if (tag === "textarea")
        return true;
    if (tag !== "input")
        return false;
    const type = target.type;
    return !["checkbox", "radio", "button", "submit", "reset", "range", "file"].includes(type);
}
