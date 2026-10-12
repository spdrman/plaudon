import { describe, expect, it } from "vitest";
import {
  SHORTCUTS,
  destinationForEvent,
  formatShortcut,
  isMacPlatform,
  isTextEntryTarget,
} from "./shortcuts";

const event = (over: Partial<Parameters<typeof destinationForEvent>[0]>) => ({
  key: "1",
  metaKey: false,
  ctrlKey: false,
  altKey: false,
  shiftKey: false,
  ...over,
});

describe("shortcuts", () => {
  it("maps Mod+1..6 to the six destinations", () => {
    expect(destinationForEvent(event({ metaKey: true, key: "1" }))).toBe("library");
    expect(destinationForEvent(event({ ctrlKey: true, key: "3" }))).toBe("jobs");
    expect(destinationForEvent(event({ metaKey: true, key: "5" }))).toBe("settings");
    expect(destinationForEvent(event({ metaKey: true, key: "6" }))).toBe("devices");
  });

  it("ignores unmodified, out-of-range and chorded keys", () => {
    expect(destinationForEvent(event({ key: "1" }))).toBeNull();
    expect(destinationForEvent(event({ metaKey: true, key: "7" }))).toBeNull();
    expect(destinationForEvent(event({ metaKey: true, key: "1", shiftKey: true }))).toBeNull();
    expect(destinationForEvent(event({ metaKey: true, key: "x" }))).toBeNull();
  });

  it("renders the platform modifier", () => {
    expect(formatShortcut("Mod+A", true)).toBe("Cmd+A");
    expect(formatShortcut("Mod+A", false)).toBe("Ctrl+A");
    expect(isMacPlatform({ platform: "MacIntel" })).toBe(true);
    expect(isMacPlatform({ userAgent: "Windows NT 10.0" })).toBe(false);
  });

  it("detects text entry targets so typing is never swallowed", () => {
    const input = document.createElement("input");
    input.type = "text";
    const checkbox = document.createElement("input");
    checkbox.type = "checkbox";
    const textarea = document.createElement("textarea");
    const div = document.createElement("div");
    expect(isTextEntryTarget(input)).toBe(true);
    expect(isTextEntryTarget(textarea)).toBe(true);
    expect(isTextEntryTarget(checkbox)).toBe(false);
    expect(isTextEntryTarget(div)).toBe(false);
    expect(isTextEntryTarget(null)).toBe(false);
  });

  it("documents every group it claims to cover", () => {
    const groups = new Set(SHORTCUTS.map((s) => s.group));
    expect([...groups].sort()).toEqual([
      "General",
      "Library",
      "Navigation",
      "Playback",
      "Transcript",
    ]);
    for (const shortcut of SHORTCUTS) {
      expect(shortcut.keys).toBeTruthy();
      expect(shortcut.description.length).toBeGreaterThan(4);
    }
  });
});
