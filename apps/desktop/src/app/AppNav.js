import { jsx as _jsx, jsxs as _jsxs } from "react/jsx-runtime";
import { useRef } from "react";
import { DESTINATIONS, DESTINATION_LABELS, destinationOf } from "./router";
import { useAppState } from "./AppState";
const GLYPHS = {
    library: "L",
    inbox: "I",
    jobs: "J",
    templates: "T",
    settings: "S",
};
/**
 * Top-level navigation (§5.1) as a roving-tabindex vertical tablist: one tab
 * stop, arrow keys move between destinations, Enter/Space activates.
 */
export function AppNav() {
    const { route, navigate } = useAppState();
    const active = destinationOf(route);
    const refs = useRef(new Map());
    const move = (delta) => {
        const index = DESTINATIONS.indexOf(active);
        const next = DESTINATIONS[(index + delta + DESTINATIONS.length) % DESTINATIONS.length];
        navigate({ name: next });
        requestAnimationFrame(() => refs.current.get(next)?.focus());
    };
    return (_jsx("nav", { className: "app-nav", "aria-label": "Primary", children: _jsx("ul", { role: "tablist", "aria-orientation": "vertical", className: "app-nav__list", children: DESTINATIONS.map((destination) => {
                const selected = destination === active;
                return (_jsx("li", { role: "none", children: _jsxs("button", { type: "button", role: "tab", id: `nav-${destination}`, "aria-selected": selected, "aria-controls": "main-region", tabIndex: selected ? 0 : -1, ref: (el) => {
                            refs.current.set(destination, el);
                        }, className: `app-nav__item${selected ? " app-nav__item--active" : ""}`, onClick: () => navigate({ name: destination }), onKeyDown: (event) => {
                            if (event.key === "ArrowDown" || event.key === "ArrowRight") {
                                event.preventDefault();
                                move(1);
                            }
                            else if (event.key === "ArrowUp" || event.key === "ArrowLeft") {
                                event.preventDefault();
                                move(-1);
                            }
                            else if (event.key === "Home") {
                                event.preventDefault();
                                navigate({ name: DESTINATIONS[0] });
                            }
                            else if (event.key === "End") {
                                event.preventDefault();
                                navigate({ name: DESTINATIONS[DESTINATIONS.length - 1] });
                            }
                        }, children: [_jsx("span", { "aria-hidden": "true", className: "app-nav__glyph", children: GLYPHS[destination] }), _jsx("span", { className: "app-nav__label", children: DESTINATION_LABELS[destination] })] }) }, destination));
            }) }) }));
}
