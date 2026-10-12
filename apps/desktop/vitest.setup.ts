import "@testing-library/jest-dom/vitest";
import { afterEach } from "vitest";
import { cleanup } from "@testing-library/react";

// jsdom lacks matchMedia; the theme layer depends on it for OS appearance.
if (!window.matchMedia) {
  Object.defineProperty(window, "matchMedia", {
    writable: true,
    value: (query: string) => {
      const listeners = new Set<(e: MediaQueryListEvent) => void>();
      return {
        matches: false,
        media: query,
        onchange: null,
        addEventListener: (_: string, cb: (e: MediaQueryListEvent) => void) => listeners.add(cb),
        removeEventListener: (_: string, cb: (e: MediaQueryListEvent) => void) => listeners.delete(cb),
        dispatchEvent: () => false,
        addListener: () => undefined,
        removeListener: () => undefined,
      };
    },
  });
}

if (!window.HTMLElement.prototype.scrollTo) {
  window.HTMLElement.prototype.scrollTo = function scrollTo() {
    /* jsdom has no layout engine; virtualization tests drive scrollTop directly */
  } as HTMLElement["scrollTo"];
}

afterEach(() => {
  cleanup();
  // The router reads `location.hash`; a route set by one test must not leak.
  window.location.hash = "";
});
