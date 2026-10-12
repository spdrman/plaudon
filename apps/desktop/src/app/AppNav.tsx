import { useRef } from "react";
import { DESTINATIONS, DESTINATION_LABELS, destinationOf, type Destination } from "./router";
import { useAppState } from "./AppState";

const GLYPHS: Record<Destination, string> = {
  library: "L",
  inbox: "I",
  jobs: "J",
  templates: "T",
  settings: "S",
  devices: "D",
};

/**
 * Top-level navigation (§5.1) as a roving-tabindex vertical tablist: one tab
 * stop, arrow keys move between destinations, Enter/Space activates.
 */
export function AppNav() {
  const { route, navigate } = useAppState();
  const active = destinationOf(route);
  const refs = useRef(new Map<Destination, HTMLButtonElement | null>());

  const move = (delta: number) => {
    const index = DESTINATIONS.indexOf(active);
    const next = DESTINATIONS[(index + delta + DESTINATIONS.length) % DESTINATIONS.length]!;
    navigate({ name: next } as never);
    requestAnimationFrame(() => refs.current.get(next)?.focus());
  };

  return (
    <nav className="app-nav" aria-label="Primary">
      <ul role="tablist" aria-orientation="vertical" className="app-nav__list">
        {DESTINATIONS.map((destination) => {
          const selected = destination === active;
          return (
            <li key={destination} role="none">
              <button
                type="button"
                role="tab"
                id={`nav-${destination}`}
                aria-selected={selected}
                aria-controls="main-region"
                tabIndex={selected ? 0 : -1}
                ref={(el) => {
                  refs.current.set(destination, el);
                }}
                className={`app-nav__item${selected ? " app-nav__item--active" : ""}`}
                onClick={() => navigate({ name: destination } as never)}
                onKeyDown={(event) => {
                  if (event.key === "ArrowDown" || event.key === "ArrowRight") {
                    event.preventDefault();
                    move(1);
                  } else if (event.key === "ArrowUp" || event.key === "ArrowLeft") {
                    event.preventDefault();
                    move(-1);
                  } else if (event.key === "Home") {
                    event.preventDefault();
                    navigate({ name: DESTINATIONS[0] } as never);
                  } else if (event.key === "End") {
                    event.preventDefault();
                    navigate({ name: DESTINATIONS[DESTINATIONS.length - 1]! } as never);
                  }
                }}
              >
                <span aria-hidden="true" className="app-nav__glyph">
                  {GLYPHS[destination]}
                </span>
                <span className="app-nav__label">{DESTINATION_LABELS[destination]}</span>
              </button>
            </li>
          );
        })}
      </ul>
    </nav>
  );
}
