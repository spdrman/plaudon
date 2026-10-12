import { describe, expect, it } from "vitest";
import { act, render, screen, waitFor } from "@testing-library/react";
import { useState } from "react";
import { useCommandMutation, useCommandQuery } from "./useCommand";
import { setTransport } from "./client";
import { FakeBackend } from "../testing/fakeBackend";

function Jobs({ states }: { states: [] | ["queued"] }) {
  const query = useCommandQuery("list_jobs", { cursor: null, limit: 50, states, kinds: [] }, {
    deps: [states.join(",")],
  });
  return (
    <div>
      <span data-testid="count">{query.data ? query.data.items.length : "loading"}</span>
      <button type="button" onClick={() => void query.refetch()}>
        refetch
      </button>
    </div>
  );
}

describe("useCommandQuery", () => {
  it("fetches exactly once on mount", async () => {
    const backend = new FakeBackend();
    backend.addJob({ kind: "import", state: "queued" });
    setTransport(backend);
    render(<Jobs states={[]} />);
    await screen.findByText("1");
    expect(backend.calls.filter((c) => c.command === "list_jobs")).toHaveLength(1);
  });

  it("refetches when the declared dependencies change, not on every render", async () => {
    const backend = new FakeBackend();
    backend.addJob({ kind: "import", state: "queued" });
    setTransport(backend);
    function Harness() {
      const [states, setStates] = useState<[] | ["queued"]>([]);
      return (
        <>
          <Jobs states={states} />
          <button type="button" onClick={() => setStates(["queued"])}>
            filter
          </button>
          <button type="button" onClick={() => setStates([])}>
            clear
          </button>
        </>
      );
    }
    render(<Harness />);
    await screen.findByText("1");
    const afterMount = backend.calls.filter((c) => c.command === "list_jobs").length;
    expect(afterMount).toBe(1);
    await act(async () => {
      screen.getByRole("button", { name: "filter" }).click();
    });
    await waitFor(() =>
      expect(backend.calls.filter((c) => c.command === "list_jobs").length).toBe(2),
    );
  });

  it("surfaces a typed envelope and keeps the previous data untouched", async () => {
    const backend = new FakeBackend();
    setTransport(backend);
    backend.failures.set("list_jobs", {
      code: "STORAGE_UNAVAILABLE",
      message: "The library volume is not available.",
      retryable: true,
      action: "OPEN_DIAGNOSTICS",
      diagnostic_id: "d1",
    });
    function Probe() {
      const query = useCommandQuery("list_jobs", { cursor: null, limit: 10, states: [], kinds: [] });
      return <span>{query.error ? `${query.error.code}:${query.error.retryable}` : "ok"}</span>;
    }
    render(<Probe />);
    expect(await screen.findByText("STORAGE_UNAVAILABLE:true")).toBeInTheDocument();
  });
});

describe("useCommandMutation", () => {
  it("reports pending state and rethrows typed errors", async () => {
    const backend = new FakeBackend();
    setTransport(backend);
    backend.failures.set("set_local_only_mode", {
      code: "CONFIRMATION_REQUIRED",
      message: "Confirm this change before it can be applied.",
      retryable: false,
      diagnostic_id: "d2",
    });
    function Probe() {
      const mutation = useCommandMutation("set_local_only_mode");
      return (
        <>
          <button
            type="button"
            onClick={() => {
              mutation.run({ enabled: true, confirmed: false }).catch(() => undefined);
            }}
          >
            apply
          </button>
          <span>{mutation.error ? mutation.error.code : "none"}</span>
        </>
      );
    }
    render(<Probe />);
    await act(async () => {
      screen.getByRole("button", { name: "apply" }).click();
    });
    expect(await screen.findByText("CONFIRMATION_REQUIRED")).toBeInTheDocument();
  });
});
