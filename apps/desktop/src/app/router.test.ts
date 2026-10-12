import { describe, expect, it } from "vitest";
import {
  DESTINATIONS,
  destinationOf,
  formatRoute,
  parseRoute,
  routeTitle,
  type Route,
} from "./router";

const ROUTES: Route[] = [
  { name: "library" },
  { name: "library.recording", recordingId: "rec-1" },
  { name: "inbox" },
  { name: "jobs" },
  { name: "jobs.job", jobId: "job-9" },
  { name: "templates" },
  { name: "settings" },
  { name: "settings.activity" },
  { name: "settings.sources" },
  { name: "devices" },
  { name: "devices.device", peripheralId: "hci0/dev_AA:BB" },
];

describe("router", () => {
  it("round-trips every route through the hash", () => {
    for (const route of ROUTES) {
      expect(parseRoute(formatRoute(route))).toEqual(route);
    }
  });

  it("keeps recording detail inside the Library destination", () => {
    expect(destinationOf({ name: "library.recording", recordingId: "r" })).toBe("library");
    expect(DESTINATIONS).not.toContain("recording");
  });

  it("falls back to Library for unknown or empty hashes", () => {
    expect(parseRoute("")).toEqual({ name: "library" });
    expect(parseRoute("#/nope")).toEqual({ name: "library" });
    expect(parseRoute("#/")).toEqual({ name: "library" });
  });

  it("escapes ids that contain separators", () => {
    const route: Route = { name: "library.recording", recordingId: "a/b c" };
    expect(formatRoute(route)).toBe("#/library/a%2Fb%20c");
    expect(parseRoute(formatRoute(route))).toEqual(route);
  });

  it("names every route for the window header", () => {
    for (const route of ROUTES) expect(routeTitle(route)).toBeTruthy();
  });
});
