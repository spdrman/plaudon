import { describe, expect, it } from "vitest";
import { act, screen, waitFor, within } from "@testing-library/react";
import { FakeBackend } from "../../testing/fakeBackend";
import { LEGACY_DISCLOSURES_FIXTURE } from "../../testing/fakeDevices";
import { renderDevices, scanNow } from "../../testing/devicesHarness";
import type { RenderWithAppResult } from "../../testing/renderWithApp";
import { expectNoAxeViolations } from "../../testing/axe";

const TOKEN = "00112233445566778899aabbccddeeff";

async function openDetail(
  user: RenderWithAppResult["user"],
  name: string,
): Promise<void> {
  await scanNow(user);
  await user.click(screen.getByRole("button", { name: `Open details of ${name}` }));
  await screen.findByRole("heading", { name, level: 3 });
}

function fact(label: string): string {
  const term = screen.getByText(label, { selector: "dt" });
  return term.parentElement?.querySelector("dd")?.textContent ?? "";
}

describe("DeviceDetail: facts", () => {
  it("shows model, firmware, transport, name, signal, battery, identity and the register's status", async () => {
    const { user } = renderDevices({ devices: [{ peripheral_id: "d1", advertised_name: "Desk Note" }] });
    await openDetail(user, "Desk Note");
    await screen.findByText("Capabilities");

    expect(fact("Model")).toBe("Note");
    expect(fact("Firmware")).toBe("V0095");
    expect(fact("Transport")).toBe("Bluetooth LE (BLE)");
    expect(fact("Advertised name")).toBe("Desk Note");
    expect(fact("Signal strength")).toBe("-52 dBm");
    expect(fact("Battery")).toBe("81%");
    expect(fact("Identity")).toBe("Unverified identity");
    expect(fact("Last contact")).toBe("Never");
    expect(fact("Files on the device")).toBe("Unknown until the recordings are listed");
    expect(fact("Source")).toBe("Device BLE · compressed ASR");
    expect(fact("Cloud")).toBe("Cloud: not used");
    expect(screen.getAllByText("Not evaluated").length).toBeGreaterThan(0);
  });

  it("lists the register's known limitations verbatim", async () => {
    const { user } = renderDevices({ devices: [{ peripheral_id: "d1", advertised_name: "Desk Note" }] });
    await openDetail(user, "Desk Note");
    const limits = await screen.findByRole("region", { name: "Known limitations" });
    expect(within(limits).getByText(/delta of unsynchronized files/)).toBeInTheDocument();
    expect(within(limits).getByText(/compressed \.ASR Opus sidecar/)).toBeInTheDocument();
  });

  it("shows the legacy radio disclosures for the legacy profile and none otherwise", async () => {
    const { user } = renderDevices({
      devices: [
        { peripheral_id: "d1", advertised_name: "Desk Note" },
        { peripheral_id: "d2", advertised_name: "Newer Note", disclosures: [], section: "unsupported_firmware", support_status: "known_unsupported" },
      ],
    });
    await openDetail(user, "Desk Note");
    const disclosures = await screen.findByRole("region", { name: "Radio disclosures" });
    for (const text of LEGACY_DISCLOSURES_FIXTURE) {
      expect(within(disclosures).getByText(text)).toBeInTheDocument();
    }
    await user.click(screen.getByRole("button", { name: "All devices" }));
    await user.click(await screen.findByRole("button", { name: "Open details of Newer Note" }));
    await screen.findByRole("heading", { name: "Newer Note", level: 3 });
    await screen.findByText("Capabilities");
    expect(screen.queryByRole("region", { name: "Radio disclosures" })).not.toBeInTheDocument();
  });

  it("reports an unverifiable firmware, battery and signal as Unknown, never a guess", async () => {
    const { user } = renderDevices({
      devices: [{ peripheral_id: "d1", advertised_name: "Desk Note", firmware: null, battery_percent: null, rssi_dbm: null, section: "unsupported_firmware" }],
    });
    await openDetail(user, "Desk Note");
    await screen.findByText("Capabilities");
    expect(fact("Firmware")).toBe("Unknown");
    expect(fact("Battery")).toBe("Unknown");
    expect(fact("Signal strength")).toBe("Unknown");
    expect(fact("Files on the device")).not.toMatch(/\d/);
  });

  it("shows Cloud: not used only when no cloud route was involved", async () => {
    const { user } = renderDevices({
      devices: [{ peripheral_id: "d1", advertised_name: "Desk Note", cloud_route: "used" }],
    });
    await openDetail(user, "Desk Note");
    await screen.findByText("Capabilities");
    expect(fact("Cloud")).toBe("Cloud: used");
    expect(screen.queryByText("Cloud: not used")).not.toBeInTheDocument();
  });

  it("never fabricates a file count before the device was listed", async () => {
    const { user } = renderDevices({ devices: [{ peripheral_id: "d1", advertised_name: "Desk Note" }] });
    await openDetail(user, "Desk Note");
    await screen.findByText("Capabilities");
    expect(screen.queryByRole("note")).not.toBeInTheDocument();
    expect(fact("Files on the device")).toMatch(/Unknown/);
  });

  it("tells a previously linked device apart: it is not probed while out of range", async () => {
    const backend = new FakeBackend();
    const { user } = renderDevices({
      backend,
      devices: [{ peripheral_id: "d1", advertised_name: "Old Note", nearby: false, authorized: true }],
    });
    await openDetail(user, "Old Note");
    expect(screen.getByText(/not advertising right now/)).toBeInTheDocument();
    expect(backend.calls.some((call) => call.command === "device_probe")).toBe(false);
    expect(fact("Firmware")).toBe("V0095");
  });

  it("has no accessibility violations", async () => {
    const { user } = renderDevices({ devices: [{ peripheral_id: "d1", advertised_name: "Desk Note" }] });
    await openDetail(user, "Desk Note");
    await screen.findByText("Capabilities");
    await expectNoAxeViolations();
  });
});

describe("DeviceDetail: identity and authorization", () => {
  it("stays Unverified identity after the token is stored and becomes Profile-authenticated only after a listing", async () => {
    const backend = new FakeBackend();
    const { user } = renderDevices({
      backend,
      devices: [{ peripheral_id: "d1", advertised_name: "Desk Note", support_status: "experimental", experimental_probe_available: true }],
    });
    await openDetail(user, "Desk Note");
    await screen.findByRole("region", { name: "Authorize this device" });
    expect(fact("Identity")).toBe("Unverified identity");

    await user.click(
      screen.getByRole("checkbox", { name: /Allow a read-only experimental probe/ }),
    );
    await user.type(screen.getByLabelText("Authorization token (hexadecimal)"), TOKEN);
    await user.click(screen.getByRole("button", { name: "Store in system keychain" }));
    await waitFor(() => expect(backend.devices.receivedTokens).toEqual([TOKEN]));
    await screen.findByRole("button", { name: "List recordings on the device" });
    expect(fact("Identity")).toBe("Unverified identity");

    await user.click(screen.getByRole("button", { name: "List recordings on the device" }));
    await screen.findByRole("list", { name: "Recordings on the device" });
    expect(fact("Identity")).toBe("Profile-authenticated");
  });

  it("hands the token over once, clears it, and never renders or re-sends it", async () => {
    const backend = new FakeBackend();
    const { user, container } = renderDevices({
      backend,
      devices: [{ peripheral_id: "d1", advertised_name: "Desk Note" }],
    });
    await openDetail(user, "Desk Note");
    const field = await screen.findByLabelText("Authorization token (hexadecimal)");
    expect(field).toHaveAttribute("type", "password");
    expect(field).toHaveAttribute("autocomplete", "off");
    await user.type(field, TOKEN);
    await user.click(screen.getByRole("button", { name: "Store in system keychain" }));
    await waitFor(() => expect(backend.devices.receivedTokens).toHaveLength(1));

    expect(container.textContent).not.toContain(TOKEN);
    expect((container.querySelector("input[type=password]") as HTMLInputElement | null)?.value ?? "").toBe("");
    const authorizeCalls = backend.calls.filter((call) => call.command === "device_authorize");
    expect(authorizeCalls).toHaveLength(1);
    const others = backend.calls.filter((call) => call.command !== "device_authorize");
    expect(JSON.stringify(others)).not.toContain(TOKEN);
  });

  it("shows the envelope message for a refused token without echoing it", async () => {
    const backend = new FakeBackend();
    const { user, container } = renderDevices({
      backend,
      devices: [{ peripheral_id: "d1", advertised_name: "Desk Note" }],
    });
    await openDetail(user, "Desk Note");
    await user.type(await screen.findByLabelText("Authorization token (hexadecimal)"), "00112233");
    await user.click(screen.getByRole("button", { name: "Store in system keychain" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("The request was not valid.");
    expect(container.textContent).not.toContain("00112233");
  });

  it("offers no listing for a profile that is not evaluated until the user opts in to a read-only probe", async () => {
    const { user } = renderDevices({
      devices: [{ peripheral_id: "d1", advertised_name: "Desk Note", authorized: true, probed: true, section: "nearby" }],
    });
    await openDetail(user, "Desk Note");
    const list = await screen.findByRole("button", { name: "List recordings on the device" });
    expect(list).toBeDisabled();
    expect(screen.getByText(/Listing is not available for this device yet/)).toBeInTheDocument();
    expect(screen.getByText(/a read-only experimental probe must be approved first/)).toBeInTheDocument();
  });

});

describe("DeviceDetail: listing and the three transfer states", () => {
  async function supportedListing(backend = new FakeBackend()) {
    const rendered = renderDevices({
      backend,
      devices: [{ peripheral_id: "d1", advertised_name: "Desk Note", authorized: true, probed: true, section: "nearby", support_status: "supported", permits_transfer: true }],
    });
    await openDetail(rendered.user, "Desk Note");
    await rendered.user.click(await screen.findByRole("button", { name: "List recordings on the device" }));
    await screen.findByRole("list", { name: "Recordings on the device" });
    return { ...rendered, backend };
  }

  it("states the file count with the delta-listing completeness qualifier", async () => {
    await supportedListing();
    expect(fact("Files on the device")).toBe("2");
    const note = screen.getByRole("note");
    expect(note).toHaveTextContent("Unsynced recordings only");
    expect(note).toHaveTextContent("not a complete recording history");
    expect(note).toHaveTextContent("cannot retrieve files deleted from the device");
  });

  it("says nothing was listed, and still does not claim a complete archive, for an empty listing", async () => {
    const backend = new FakeBackend();
    backend.devices.addDevice({ peripheral_id: "d1", advertised_name: "Desk Note", authorized: true, probed: true, section: "nearby", support_status: "supported", permits_transfer: true, recordings: [] });
    const { user } = renderDevices({ backend });
    await openDetail(user, "Desk Note");
    await user.click(await screen.findByRole("button", { name: "List recordings on the device" }));
    expect(await screen.findByText("The device lists no recordings right now.")).toBeInTheDocument();
    expect(screen.getByRole("note")).toHaveTextContent("not a complete recording history");
    expect(fact("Files on the device")).toBe("0");
  });

  it("renders On device, Transferred locally and Archived as three distinct states", async () => {
    const { backend, user } = await supportedListing();
    await user.click(screen.getByRole("checkbox", { name: "Standup" }));
    await user.click(screen.getByRole("checkbox", { name: "Customer call" }));
    await user.click(screen.getByRole("button", { name: "Import selected for local processing" }));
    expect(backend.calls.find((call) => call.command === "device_import")?.args).toEqual({
      peripheral_id: "d1",
      recording_ids: ["rec-1", "rec-2"],
      local_only: true,
    });

    // Rec 1 is moved off the device, rec 2 stays on it.
    await act(async () => {
      backend.devices.setTransfer("d1", "rec-1", "committing");
    });
    // rec-2 never started: back to "On device" in the authoritative store.
    const rows = await screen.findAllByRole("listitem");
    expect(rows.length).toBeGreaterThanOrEqual(2);
    await waitFor(() => {
      const row = document.querySelector('[data-transfer-state="committing"]');
      expect(row).not.toBeNull();
    });
    const committing = document.querySelector('[data-transfer-state="committing"]') as HTMLElement;
    expect(within(committing).getByText("Transferred locally")).toBeInTheDocument();
    expect(within(committing).getByText(/Bytes verified/)).toBeInTheDocument();

    await act(async () => {
      backend.devices.setTransfer("d1", "rec-1", "imported");
    });
    await waitFor(() =>
      expect(document.querySelector('[data-transfer-state="imported"]')).not.toBeNull(),
    );
    const archived = document.querySelector('[data-transfer-state="imported"]') as HTMLElement;
    expect(within(archived).getByText("Archived")).toBeInTheDocument();

    const labels = new Set<string>();
    const glyphs = new Set<string>();
    const tones = new Set<string>();
    for (const row of document.querySelectorAll("[data-transfer-state]")) {
      const badge = row.querySelector("[data-tone]") as HTMLElement;
      labels.add(badge.textContent ?? "");
      glyphs.add(badge.querySelector("[aria-hidden='true']")?.textContent ?? "");
      tones.add(badge.getAttribute("data-tone") ?? "");
    }
    expect(labels.size).toBeGreaterThanOrEqual(2);
    expect(glyphs.size).toBeGreaterThanOrEqual(2);
    expect(tones.size).toBeGreaterThanOrEqual(2);
  });

  it("gives On device, Transferred locally and Archived different labels, glyphs and tones", async () => {
    const { backend, user } = await supportedListing();
    const read = (state: string) => {
      const badge = document.querySelector(`[data-transfer-state="${state}"] [data-tone]`) as HTMLElement;
      return {
        label: badge.textContent?.replace(/\s+/g, " ").trim() ?? "",
        glyph: badge.querySelector("[aria-hidden='true']")?.textContent ?? "",
        tone: badge.getAttribute("data-tone"),
      };
    };
    const onDevice = read("discovered");

    await user.click(screen.getByRole("checkbox", { name: "Standup" }));
    await user.click(screen.getByRole("button", { name: "Import selected for local processing" }));
    await act(async () => backend.devices.setTransfer("d1", "rec-1", "committing"));
    await waitFor(() => expect(document.querySelector('[data-transfer-state="committing"]')).not.toBeNull());
    await act(async () => backend.devices.setTransfer("d1", "rec-2", "imported"));
    await waitFor(() => expect(document.querySelector('[data-transfer-state="imported"]')).not.toBeNull());

    const states = [onDevice, read("committing"), read("imported")];
    expect(new Set(states.map((s) => s.glyph)).size).toBe(3);
    expect(new Set(states.map((s) => s.tone)).size).toBe(3);
    expect(states.map((s) => s.label)).toEqual([
      expect.stringContaining("On device"),
      expect.stringContaining("Transferred locally"),
      expect.stringContaining("Archived"),
    ]);
  });

  it("shows observed bandwidth and an ETA that is labelled as an estimate while receiving", async () => {
    const { backend, user } = await supportedListing();
    await user.click(screen.getByRole("checkbox", { name: "Standup" }));
    await user.click(screen.getByRole("button", { name: "Import selected for local processing" }));
    await act(async () =>
      backend.devices.setTransfer("d1", "rec-1", "receiving", {
        bytes_received: 100_000,
        observed_bytes_per_sec: 50_000,
        eta_seconds_estimate: 18,
      }),
    );
    const row = await waitFor(() => {
      const found = document.querySelector('[data-transfer-state="receiving"]') as HTMLElement | null;
      expect(found).not.toBeNull();
      return found!;
    });
    expect(within(row).getByText(/Observed bandwidth: 49 kB\/s/)).toBeInTheDocument();
    expect(within(row).getByText(/Estimated time remaining \(an estimate, not a promise\): about 18 s/)).toBeInTheDocument();
    expect(within(row).getByRole("progressbar")).toBeInTheDocument();
    expect(within(row).getByText(/of the size the device declared/)).toBeInTheDocument();
    expect(row).toHaveTextContent("Source: Device BLE · compressed ASR · Cloud: not used");
    expect(within(row).getByText("Transferring")).toBeInTheDocument();
  });

  it("shows no ETA when none can be estimated, instead of inventing one", async () => {
    const { backend, user } = await supportedListing();
    await user.click(screen.getByRole("checkbox", { name: "Standup" }));
    await user.click(screen.getByRole("button", { name: "Import selected for local processing" }));
    await act(async () => backend.devices.setTransfer("d1", "rec-1", "receiving", { bytes_received: null }));
    const row = await waitFor(() => document.querySelector('[data-transfer-state="receiving"]') as HTMLElement);
    expect(within(row).getByText(/Cannot be estimated yet/)).toBeInTheDocument();
    expect(within(row).getByText(/Not measured yet/)).toBeInTheDocument();
  });

  it("cancels a running transfer by disconnecting, without removing archived recordings", async () => {
    const { backend, user } = await supportedListing();
    await user.click(screen.getByRole("checkbox", { name: "Standup" }));
    await user.click(screen.getByRole("button", { name: "Import selected for local processing" }));
    await act(async () => backend.devices.setTransfer("d1", "rec-1", "receiving", { bytes_received: 10 }));
    const cancel = await screen.findByRole("button", { name: "Cancel transfer of Standup" });
    await user.click(cancel);

    const call = backend.calls.find((c) => c.command === "device_disconnect");
    expect(call?.args).toEqual({ peripheral_id: "d1", forget_authorization: false });
    expect(await screen.findByText(/Recordings already in your library were kept/)).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /Cancel transfer/ })).not.toBeInTheDocument();
    expect(fact("Identity")).toBe("Unverified identity");
  });

  it("refetches the listing when a transfer event arrives and does not rescan", async () => {
    const { backend } = await supportedListing();
    const before = backend.calls.length;
    await act(async () => backend.devices.setTransfer("d1", "rec-1", "committing"));
    await waitFor(() =>
      expect(backend.calls.slice(before).map((c) => c.command)).toContain("device_list_recordings"),
    );
    expect(backend.calls.slice(before).map((c) => c.command)).not.toContain("list_nearby_devices");
  });

});

describe("DeviceDetail: automatic sync, disconnect and forgetting", () => {
  it("does not offer automatic sync until the audit allows it, and explains why", async () => {
    const { user } = renderDevices({ devices: [{ peripheral_id: "d1", advertised_name: "Desk Note", support_status: "supported", permits_transfer: true }] });
    await openDetail(user, "Desk Note");
    const region = await screen.findByRole("region", { name: "Automatic sync" });
    expect(within(region).getByText(/Not offered/)).toBeInTheDocument();
    expect(within(region).getByText(/has not passed a non-destructive audit yet/)).toBeInTheDocument();
    expect(within(region).queryByRole("checkbox")).not.toBeInTheDocument();
  });

  it("lets the user opt in once the audit allows it, defaulting to off", async () => {
    const backend = new FakeBackend();
    const { user } = renderDevices({
      backend,
      devices: [{ peripheral_id: "d1", advertised_name: "Desk Note", support_status: "supported", permits_transfer: true, may_advertise_sync: true }],
    });
    await openDetail(user, "Desk Note");
    const box = await screen.findByRole("checkbox", { name: /Transfer recordings automatically/ });
    expect(box).not.toBeChecked();
    await user.click(box);
    await waitFor(() => expect(box).toBeChecked());
    expect(backend.calls.find((c) => c.command === "device_sync_policy")?.args).toEqual({
      peripheral_id: "d1",
      enable_when_nearby: true,
    });
  });

  it("forgets the authorization only after an explicit confirmation", async () => {
    const backend = new FakeBackend();
    const { user } = renderDevices({
      backend,
      devices: [{ peripheral_id: "d1", advertised_name: "Desk Note", authorized: true, probed: true, section: "nearby" }],
    });
    await openDetail(user, "Desk Note");
    await user.click(await screen.findByRole("button", { name: "Forget authorization" }));
    const dialog = await screen.findByRole("alertdialog");
    expect(dialog).toHaveTextContent("Recordings already in your library are kept");
    expect(backend.calls.some((c) => c.command === "device_disconnect")).toBe(false);
    await user.click(within(dialog).getByRole("button", { name: "Forget authorization" }));
    await waitFor(() =>
      expect(backend.calls.find((c) => c.command === "device_disconnect")?.args).toEqual({
        peripheral_id: "d1",
        forget_authorization: true,
      }),
    );
  });
});

describe("DeviceDetail: experimental profiles", () => {
  it("lists after an explicit approval but never offers import", async () => {
    const backend = new FakeBackend();
    const { user } = renderDevices({
      backend,
      devices: [{ peripheral_id: "d1", advertised_name: "Desk Note", authorized: true, probed: true, section: "nearby", support_status: "experimental", experimental_probe_available: true }],
    });
    await openDetail(user, "Desk Note");
    expect(await screen.findByRole("button", { name: "List recordings on the device" })).toBeDisabled();
    await user.click(screen.getByRole("checkbox", { name: /Allow a read-only experimental probe/ }));
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "List recordings on the device" })).toBeEnabled(),
    );
    await user.click(screen.getByRole("button", { name: "List recordings on the device" }));
    await screen.findByRole("list", { name: "Recordings on the device" });
    expect(screen.queryByRole("button", { name: /Import selected/ })).not.toBeInTheDocument();
    expect(screen.queryByRole("checkbox", { name: "Standup" })).not.toBeInTheDocument();
    expect(screen.getByText(/Transfer is not enabled for this device profile \(Experimental\)/)).toBeInTheDocument();
    expect(backend.calls.some((c) => c.command === "device_import")).toBe(false);
  });
});
