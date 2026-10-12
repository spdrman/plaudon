import { jsx as _jsx } from "react/jsx-runtime";
import { render } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { AppStateProvider } from "../app/AppState";
import { EventBus } from "../ipc/eventBus";
import { setTransport } from "../ipc/client";
import { FakeBackend } from "./fakeBackend";
/** Renders a tree inside the app providers with the in-memory backend wired up. */
export function renderWithApp(ui, options = {}) {
    const { backend: provided, route, ...backendOptions } = options;
    const backend = provided ?? new FakeBackend(backendOptions);
    setTransport(backend);
    const bus = new EventBus(backend);
    const user = userEvent.setup();
    const result = render(_jsx(AppStateProvider, { bus: bus, initialRoute: route, children: ui }));
    return { ...result, backend, user };
}
