import { describe, expect, it } from "vitest";
import { screen, within } from "@testing-library/react";
import { FakeBackend } from "../../testing/fakeBackend";
import { renderDevices } from "../../testing/devicesHarness";
import { expectNoAxeViolations } from "../../testing/axe";

async function openWizard(backend = new FakeBackend()) {
  const rendered = renderDevices({ backend, devices: [{ peripheral_id: "d1" }] });
  await rendered.user.click(await screen.findByRole("button", { name: "Add PLAUD device" }));
  return { ...rendered, backend };
}

describe("SetupWizard", () => {
  it("offers exactly the three App. B.8 options", async () => {
    await openWizard();
    const group = screen.getByRole("group", { name: "How do you want to bring recordings in?" });
    const radios = within(group).getAllByRole("radio");
    expect(radios).toHaveLength(3);
    expect(radios.map((r) => r.closest("label")?.querySelector("strong")?.textContent)).toEqual([
      "Connect directly (offline after setup)",
      "Use PLAUD account",
      "Import files",
    ]);
    await expectNoAxeViolations();
  });

  it("Connect directly: explains the flow, scans only on request and never mentions falling back to the cloud silently", async () => {
    const { user, backend } = await openWizard();
    await user.click(screen.getByRole("radio", { name: /Connect directly/ }));
    const panel = screen.getByRole("group", { name: "Connect directly" });
    expect(panel).toHaveTextContent("Supported, Experimental or Unsupported");
    expect(panel).toHaveTextContent("not a complete recording history");
    expect(panel).toHaveTextContent("never switches to the cloud on its own");
    expect(backend.calls.some((c) => c.command === "list_nearby_devices")).toBe(false);
    await user.click(within(panel).getByRole("button", { name: "Scan for nearby devices" }));
    expect(await screen.findByRole("region", { name: /^Nearby/ })).toBeInTheDocument();
    expect(backend.calls.filter((c) => c.command === "list_nearby_devices")).toHaveLength(1);
  });

  it("Use PLAUD account: labels it PLAUD Cloud, not a direct connection, and goes to Settings", async () => {
    const { user } = await openWizard();
    await user.click(screen.getByRole("radio", { name: /Use PLAUD account/ }));
    const panel = screen.getByRole("group", { name: "Use PLAUD account" });
    expect(panel).toHaveTextContent("PLAUD Cloud, not a direct connection");
    expect(panel).not.toHaveTextContent("Cloud: not used");
    expect(within(panel).getByRole("button", { name: "Open account settings" })).toBeInTheDocument();
  });

  it("Import files: works without hardware, an account or a network and opens the picker", async () => {
    const { user } = await openWizard();
    await user.click(screen.getByRole("radio", { name: /Import files/ }));
    const panel = screen.getByRole("group", { name: "Import files" });
    expect(panel).toHaveTextContent("without hardware, an account or a network connection");
    expect(within(panel).getByRole("button", { name: "Choose audio files" })).toBeInTheDocument();
  });

  it("closes without choosing anything", async () => {
    const { user } = await openWizard();
    await user.click(screen.getByRole("button", { name: "Close setup" }));
    expect(screen.queryByRole("heading", { name: "Add a PLAUD device" })).not.toBeInTheDocument();
  });
});
