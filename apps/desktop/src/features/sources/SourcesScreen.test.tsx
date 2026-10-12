import { describe, expect, it } from "vitest";
import { screen, waitFor, within } from "@testing-library/react";
import { FakeBackend, envelope, fakePlaudListing, fakePlaudSnapshot } from "../../testing/fakeBackend";
import { renderWithApp } from "../../testing/renderWithApp";
import { expectNoAxeViolations } from "../../testing/axe";
import { SourcesScreen } from "./SourcesScreen";

function readyBackend(count = 2): FakeBackend {
  const backend = new FakeBackend();
  backend.plaud = fakePlaudSnapshot({
    state: "authenticated",
    headline: "Connected to PLAUD",
    next_action: { kind: "none" },
  });
  backend.plaudListing = fakePlaudListing(count);
  return backend;
}

describe("SourcesScreen", () => {
  it("renders the diagnostics panel from plaud_probe alone", async () => {
    const backend = new FakeBackend();
    backend.plaud = fakePlaudSnapshot({
      state: "unconfigured",
      headline: "PLAUD command line tool not found",
      detail: "Install it from PLAUD's official documentation, then connect again.",
      next_action: { kind: "install_cli" },
      compatibility: "unknown",
      detected_version: null,
      program_origin: null,
    });
    renderWithApp(<SourcesScreen />, { backend });

    expect(
      await screen.findByRole("heading", { name: "PLAUD command line tool not found" }),
    ).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "Open PLAUD's installation guide" })).toBeInTheDocument();
    const plaudCalls = backend.calls.map((call) => call.command).filter((n) => n.startsWith("plaud_"));
    expect(new Set(plaudCalls)).toEqual(new Set(["plaud_probe"]));
    await expectNoAxeViolations();
  });

  it("signs in, lists, selects and imports, then shows what was imported", async () => {
    const backend = readyBackend();
    backend.plaud = fakePlaudSnapshot({
      state: "cli_detected",
      next_action: { kind: "sign_in" },
      headline: "PLAUD tool found",
    });
    const { user } = renderWithApp(<SourcesScreen />, { backend });

    const signIn = await screen.findByRole("button", { name: "Sign in to PLAUD" });
    backend.plaud = fakePlaudSnapshot({ state: "authenticated", next_action: { kind: "none" } });
    await user.click(signIn);
    expect(backend.calls.find((call) => call.command === "plaud_connect")?.args).toEqual({
      user_intent: "connect_plaud_account",
    });

    await user.click(await screen.findByRole("button", { name: "List my PLAUD recordings" }));
    await screen.findByText("Recording 1");
    expect(screen.getByText("1:30")).toBeInTheDocument();

    await user.click(screen.getByRole("checkbox", { name: "Recording 2" }));
    await user.click(screen.getByRole("button", { name: "Import selected (1)" }));
    await waitFor(() =>
      expect(backend.calls.find((call) => call.command === "plaud_import")?.args).toEqual({
        external_ids: ["rec_2"],
      }),
    );
    expect(await screen.findByText("Imported")).toBeInTheDocument();
    await expectNoAxeViolations();
  });

  it("select all skips recordings already in the library", async () => {
    const backend = readyBackend(3);
    backend.plaudListing.items[0]!.imported_recording_id = "rec-existing";
    const { user } = renderWithApp(<SourcesScreen />, { backend });
    await user.click(await screen.findByRole("button", { name: "List my PLAUD recordings" }));
    await screen.findByText("Recording 1");
    expect(screen.getByRole("checkbox", { name: "Recording 1" })).toBeDisabled();
    expect(screen.getByText("In library")).toBeInTheDocument();
    await user.click(screen.getByRole("checkbox", { name: "Select all" }));
    expect(screen.getByRole("button", { name: "Import selected (2)" })).toBeEnabled();
  });

  it("shows the empty-account advisory as advice, not an error", async () => {
    const backend = readyBackend(0);
    backend.plaudListing = {
      ...fakePlaudListing(0),
      empty_account_advisory: "ADVISORY: confirm you are signed in to the right regional account.",
    };
    const { user } = renderWithApp(<SourcesScreen />, { backend });
    await user.click(await screen.findByRole("button", { name: "List my PLAUD recordings" }));

    const group = await screen.findByRole("group", { name: "No recordings found" });
    expect(
      within(group).getByText("ADVISORY: confirm you are signed in to the right regional account."),
    ).toBeInTheDocument();
    expect(within(group).getByRole("button", { name: "Import audio" })).toBeInTheDocument();
    expect(screen.queryByRole("alert")).toBeNull();
    await expectNoAxeViolations();
  });

  it("discloses a possibly incomplete listing", async () => {
    const backend = readyBackend();
    backend.plaudListing = { ...fakePlaudListing(), completeness: "possibly_incomplete" };
    backend.plaud = fakePlaudSnapshot({
      state: "ready",
      warnings: ["WARNING FROM RUST: paging has not been tested."],
    });
    const { user } = renderWithApp(<SourcesScreen />, { backend });
    await user.click(await screen.findByRole("button", { name: "List my PLAUD recordings" }));
    expect(await screen.findByText("May be incomplete")).toBeInTheDocument();
    expect(screen.getByText("WARNING FROM RUST: paging has not been tested.")).toBeInTheDocument();
  });

  it("renders a command failure's own envelope message and nothing else", async () => {
    const backend = readyBackend();
    backend.failures.set(
      "plaud_list",
      envelope("SOURCE_UNAVAILABLE", "ENVELOPE MESSAGE FROM RUST", {
        retryable: true,
        action: "WAIT_AND_RETRY",
      }),
    );
    const { user } = renderWithApp(<SourcesScreen />, { backend });
    await user.click(await screen.findByRole("button", { name: "List my PLAUD recordings" }));
    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("ENVELOPE MESSAGE FROM RUST");
    expect(alert).toHaveTextContent("diag-source_unavailable");
    await expectNoAxeViolations();
  });

  it("shows a failed import's message per recording", async () => {
    const backend = readyBackend();
    backend.plaudImportOutcome = (id) => ({
      outcome: "failed",
      external_id: id,
      job_id: "job-x",
      error: envelope("LINK_EXPIRED", "LINK EXPIRED MESSAGE FROM RUST"),
    });
    const { user } = renderWithApp(<SourcesScreen />, { backend });
    await user.click(await screen.findByRole("button", { name: "List my PLAUD recordings" }));
    await user.click(await screen.findByRole("checkbox", { name: "Recording 1" }));
    await user.click(screen.getByRole("button", { name: "Import selected (1)" }));
    expect(await screen.findByText("LINK EXPIRED MESSAGE FROM RUST")).toBeInTheDocument();
    expect(screen.getByText("Failed")).toBeInTheDocument();
  });

  it("disconnect clears the listing and shows the snapshot Rust returned", async () => {
    const backend = readyBackend();
    const { user } = renderWithApp(<SourcesScreen />, { backend });
    await user.click(await screen.findByRole("button", { name: "List my PLAUD recordings" }));
    await screen.findByText("Recording 1");

    backend.plaud = fakePlaudSnapshot({
      state: "unconfigured",
      headline: "PLAUD account not connected",
      next_action: { kind: "install_cli" },
    });
    await user.click(screen.getByRole("button", { name: "Disconnect from this app" }));
    expect(await screen.findByRole("heading", { name: "PLAUD account not connected" })).toBeInTheDocument();
    expect(screen.queryByText("Recording 1")).toBeNull();
  });

  it("refetches the diagnostics when a source event arrives", async () => {
    const backend = readyBackend();
    renderWithApp(<SourcesScreen />, { backend });
    await screen.findByRole("heading", { name: "Connected to PLAUD" });

    backend.plaud = fakePlaudSnapshot({
      state: "expired_auth",
      headline: "Your PLAUD session expired",
      next_action: { kind: "sign_in" },
    });
    backend.emit("source.auth_required");
    expect(await screen.findByRole("heading", { name: "Your PLAUD session expired" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Sign in to PLAUD" })).toBeInTheDocument();
  });
});
