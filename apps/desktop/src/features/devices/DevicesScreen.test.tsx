import { describe, expect, it } from "vitest";
import { screen, within } from "@testing-library/react";
import { FakeBackend, envelope } from "../../testing/fakeBackend";
import { ALL_SECTION_DEVICES, renderDevices, scanNow } from "../../testing/devicesHarness";
import { expectNoAxeViolations } from "../../testing/axe";
import { SECTION_LABELS, SECTION_ORDER } from "./devicePresentation";

function region(name: string): HTMLElement {
  return screen.getByRole("region", { name: new RegExp(`^${name}`) });
}

describe("DevicesScreen: before and around a scan", () => {
  it("scans only when asked and offers import as the way forward", async () => {
    const backend = new FakeBackend();
    backend.devices.addDevice({ peripheral_id: "dev-1" });
    const { user } = renderDevices({ backend });

    expect(await screen.findByRole("heading", { name: "No scan yet" })).toBeInTheDocument();
    const deviceCalls = () =>
      backend.calls.filter(
        (call) => call.command === "list_nearby_devices" || call.command.startsWith("device_"),
      );
    expect(deviceCalls()).toHaveLength(0);
    expect(screen.getByRole("button", { name: "Import audio files instead" })).toBeInTheDocument();

    await scanNow(user);
    expect(deviceCalls().map((call) => call.command)).toEqual(["list_nearby_devices"]);
  });

  it("renders all five App. B.8 sections in order, each as a labelled region", async () => {
    const { user } = renderDevices({ devices: ALL_SECTION_DEVICES });
    await scanNow(user);

    const headings = screen
      .getAllByRole("heading", { level: 3 })
      .map((heading) => heading.textContent?.replace(/\s*\(\d+\)$/, ""));
    expect(headings).toEqual(SECTION_ORDER.map((section) => SECTION_LABELS[section]));
    expect(SECTION_ORDER.map((section) => SECTION_LABELS[section])).toEqual([
      "Nearby",
      "Connected",
      "Needs authorization",
      "Unsupported firmware",
      "Previously linked",
    ]);
    for (const label of Object.values(SECTION_LABELS)) {
      expect(region(label)).toBeInTheDocument();
    }
    await expectNoAxeViolations();
  });

  it("puts a device that is only advertising under Nearby with unknown model and firmware", async () => {
    const { user } = renderDevices({ devices: ALL_SECTION_DEVICES });
    await scanNow(user);
    const nearby = within(region("Nearby"));
    expect(nearby.getByText("Nearby Note")).toBeInTheDocument();
    expect(nearby.getByText(/Model: Unknown · Firmware: Unknown/)).toBeInTheDocument();
    expect(nearby.getByText("Unverified identity")).toBeInTheDocument();
    expect(nearby.getByText("Not evaluated")).toBeInTheDocument();
  });

  it("shows a connected device as profile-authenticated, never as verified", async () => {
    const { user } = renderDevices({ devices: ALL_SECTION_DEVICES });
    await scanNow(user);
    const connected = within(region("Connected"));
    expect(connected.getByText("Connected Note")).toBeInTheDocument();
    expect(connected.getByText("Profile-authenticated")).toBeInTheDocument();
    expect(connected.getByText("Supported")).toBeInTheDocument();
    expect(screen.queryByText(/cryptographically/i)).not.toBeInTheDocument();
  });

  it("lists a probed device without a credential under Needs authorization", async () => {
    const { user } = renderDevices({ devices: ALL_SECTION_DEVICES });
    await scanNow(user);
    const needs = within(region("Needs authorization"));
    expect(needs.getByText("Needs Auth Note")).toBeInTheDocument();
    expect(needs.getByText("Unverified identity")).toBeInTheDocument();
    expect(needs.getByText(/Firmware: V0095/)).toBeInTheDocument();
  });

  it("lists unsupported firmware with the register's own label", async () => {
    const { user } = renderDevices({ devices: ALL_SECTION_DEVICES });
    await scanNow(user);
    const unsupported = within(region("Unsupported firmware"));
    expect(unsupported.getByText("Newer Note")).toBeInTheDocument();
    expect(unsupported.getByText("Known unsupported")).toBeInTheDocument();
    expect(unsupported.getByText(/Firmware: encrypted-mode/)).toBeInTheDocument();
  });

  it("lists a linked device that is not advertising under Previously linked, without a signal", async () => {
    const { user } = renderDevices({ devices: ALL_SECTION_DEVICES });
    await scanNow(user);
    const linked = within(region("Previously linked"));
    expect(linked.getByText("Old Note")).toBeInTheDocument();
    expect(linked.getByText(/Signal: Unknown/)).toBeInTheDocument();
    expect(linked.getByText(/Authorization stored/)).toBeInTheDocument();
  });

  it("states a status by text and glyph, not by colour alone", async () => {
    const { user } = renderDevices({ devices: ALL_SECTION_DEVICES });
    await scanNow(user);
    for (const badge of document.querySelectorAll("[data-tone]")) {
      expect((badge.textContent ?? "").trim().length).toBeGreaterThan(1);
      expect(badge.querySelector("[aria-hidden='true']")?.textContent).toBeTruthy();
    }
  });
});

describe("DevicesScreen: empty, denied and unavailable radios (§5.4)", () => {
  it("explains a scan that heard nothing with the corrective actions, not an empty page", async () => {
    const { user } = renderDevices();
    await scanNow(user);
    expect(screen.getByRole("heading", { name: "No PLAUD device found" })).toBeInTheDocument();
    expect(screen.getByText(/Bluetooth permission is allowed/)).toBeInTheDocument();
    expect(screen.getByText(/powered on and charged/)).toBeInTheDocument();
    expect(screen.getByText(/Close the PLAUD phone app/)).toBeInTheDocument();
    expect(screen.getByText(/model is one this application can check/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Scan again" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Import audio files instead" })).toBeInTheDocument();
    expect(screen.getByText(/No PLAUD devices were heard in this scan\./)).toBeInTheDocument();
  });

  it("reports a denied radio as denied, never as no devices nearby", async () => {
    const backend = new FakeBackend();
    backend.devices.setRadio("permission_denied");
    backend.devices.addDevice({ peripheral_id: "dev-1" });
    backend.devices.addDevice({ peripheral_id: "dev-linked", nearby: false, authorized: true, advertised_name: "Old Note" });
    const { user } = renderDevices({ backend });
    await scanNow(user);

    const alert = screen.getByRole("alert");
    expect(within(alert).getByRole("heading", { name: "Bluetooth access is not permitted" })).toBeInTheDocument();
    expect(alert).toHaveTextContent("Allow Bluetooth for Plaudon in your system settings");
    expect(alert).toHaveAttribute("data-radio-state", "permission_denied");
    expect(screen.queryByText("No PLAUD device found")).not.toBeInTheDocument();
    expect(screen.queryByText(/No PLAUD devices were heard/)).not.toBeInTheDocument();
    expect(within(region("Nearby")).getByText(/Not checked: the radio could not be used/)).toBeInTheDocument();
    // What is known without a radio is still shown.
    expect(within(region("Previously linked")).getByText("Old Note")).toBeInTheDocument();
    expect(within(alert).getByRole("button", { name: "Import audio files instead" })).toBeInTheDocument();
  });

  it("reports a build without a radio as such, and says it is not the same as no devices", async () => {
    const backend = new FakeBackend();
    backend.devices.setRadio("unavailable_in_build");
    const { user } = renderDevices({ backend });
    await scanNow(user);
    const alert = screen.getByRole("alert");
    expect(within(alert).getByRole("heading", { name: "This build cannot scan for Bluetooth devices" })).toBeInTheDocument();
    expect(alert).toHaveTextContent("not the same as no devices being nearby");
    expect(screen.queryByText(/No PLAUD devices were heard/)).not.toBeInTheDocument();
    expect(screen.queryByText("No PLAUD device found")).not.toBeInTheDocument();
  });

  it("renders a failed scan as the error envelope with a retry", async () => {
    const backend = new FakeBackend();
    backend.devices.scanFailure = envelope("DEVICE_BUSY", "The device is busy. Close other applications connected to it and try again.", {
      retryable: true,
      action: "WAIT_AND_RETRY",
    });
    const { user } = renderDevices({ backend });
    await scanNow(user);
    expect(screen.getByRole("alert")).toHaveTextContent("The device is busy");
    expect(screen.getByRole("button", { name: "Retry" })).toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: "No PLAUD device found" })).not.toBeInTheDocument();
  });

  it("opens the detail of a device from the list", async () => {
    const { user } = renderDevices({ devices: ALL_SECTION_DEVICES });
    await scanNow(user);
    await user.click(screen.getByRole("button", { name: "Open details of Needs Auth Note" }));
    expect(await screen.findByRole("heading", { name: "Needs Auth Note", level: 3 })).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "All devices" }));
    expect(await screen.findByRole("region", { name: /^Nearby/ })).toBeInTheDocument();
  });

  it("tells the user when a deep-linked device was not in the latest scan", async () => {
    const { user } = renderDevices({ devices: ALL_SECTION_DEVICES, selectedId: "gone" });
    await scanNow(user);
    expect(screen.getByText(/not in the latest scan/)).toBeInTheDocument();
  });
});
