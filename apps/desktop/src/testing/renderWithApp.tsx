import { render, type RenderResult } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { ReactElement } from "react";
import { AppStateProvider } from "../app/AppState";
import type { Route } from "../app/router";
import { EventBus } from "../ipc/eventBus";
import { setTransport } from "../ipc/client";
import { FakeBackend, type FakeBackendOptions } from "./fakeBackend";

export interface RenderWithAppResult extends RenderResult {
  backend: FakeBackend;
  user: ReturnType<typeof userEvent.setup>;
}

export interface RenderWithAppOptions extends FakeBackendOptions {
  backend?: FakeBackend;
  route?: Route;
}

/** Renders a tree inside the app providers with the in-memory backend wired up. */
export function renderWithApp(
  ui: ReactElement,
  options: RenderWithAppOptions = {},
): RenderWithAppResult {
  const { backend: provided, route, ...backendOptions } = options;
  const backend = provided ?? new FakeBackend(backendOptions);
  setTransport(backend);
  const bus = new EventBus(backend);
  const user = userEvent.setup();
  const result = render(
    <AppStateProvider bus={bus} initialRoute={route}>
      {ui}
    </AppStateProvider>,
  );
  return { ...result, backend, user };
}
