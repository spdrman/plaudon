import { useState } from "react";
import { screen, waitFor } from "@testing-library/react";
import { DevicesScreen } from "../features/devices/DevicesScreen";
import { FakeBackend } from "./fakeBackend";
import { renderWithApp, type RenderWithAppResult } from "./renderWithApp";
import type { FakeDeviceSpec } from "./fakeDevices";

function ControlledDevices({ initial }: { initial: string | null }) {
  const [selected, setSelected] = useState<string | null>(initial);
  return <DevicesScreen selectedId={selected} onSelect={setSelected} />;
}

/** One device per App. B.8 section, each in the state the Rust backend would report. */
export const ALL_SECTION_DEVICES: FakeDeviceSpec[] = [
  { peripheral_id: "dev-nearby", advertised_name: "Nearby Note" },
  {
    peripheral_id: "dev-connected",
    advertised_name: "Connected Note",
    probed: true,
    authorized: true,
    identity_authenticated: true,
    support_status: "supported",
    section: "connected",
    permits_transfer: true,
  },
  {
    peripheral_id: "dev-needs-auth",
    advertised_name: "Needs Auth Note",
    probed: true,
    section: "needs_authorization",
  },
  {
    peripheral_id: "dev-unsupported",
    advertised_name: "Newer Note",
    probed: true,
    section: "unsupported_firmware",
    support_status: "known_unsupported",
    firmware: "encrypted-mode",
    known_limitations: [
      "Encrypted handshake firmware is reported as unsupported; it is never downgraded.",
    ],
    disclosures: [],
  },
  {
    peripheral_id: "dev-linked",
    advertised_name: "Old Note",
    nearby: false,
    authorized: true,
    last_contact_at: "2026-10-01T09:00:00Z",
  },
];

export interface RenderDevicesOptions {
  devices?: FakeDeviceSpec[];
  selectedId?: string | null;
  backend?: FakeBackend;
}

export function renderDevices(options: RenderDevicesOptions = {}): RenderWithAppResult {
  const backend = options.backend ?? new FakeBackend();
  for (const spec of options.devices ?? []) backend.devices.addDevice(spec);
  return renderWithApp(<ControlledDevices initial={options.selectedId ?? null} />, { backend });
}

/** Presses Scan and waits for the result to render. */
export async function scanNow(user: RenderWithAppResult["user"]): Promise<void> {
  await user.click(screen.getAllByRole("button", { name: "Scan for nearby devices" })[0]!);
  await waitFor(() => expect(screen.queryByText("Scanning for devices…")).not.toBeInTheDocument());
}
