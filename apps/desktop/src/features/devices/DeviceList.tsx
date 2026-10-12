import type { DeviceCandidate, DeviceSection } from "../../ipc/contract";
import { Button } from "../../components/Button";
import { StatusBadge } from "../../components/StatusBadge";
import {
  SECTION_EMPTY,
  SECTION_LABELS,
  SECTION_ORDER,
  SUPPORT_PRESENTATION,
  deviceName,
  formatSignal,
} from "./devicePresentation";

export interface DeviceListProps {
  devices: DeviceCandidate[];
  /** False when the radio could not be used, so "empty" would be a false claim. */
  scanRan: boolean;
  onOpen: (peripheralId: string) => void;
}

/**
 * The five App. B.8 sections. A section that is empty because the radio could
 * not be used says so; it never reads as "nothing nearby".
 */
export function DeviceList({ devices, scanRan, onOpen }: DeviceListProps) {
  const bySection = (section: DeviceSection) => devices.filter((d) => d.section === section);
  return (
    <div className="device-sections">
      {SECTION_ORDER.map((section) => {
        const rows = bySection(section);
        const headingId = `devices-section-${section}`;
        const listensToRadio = section !== "previously_linked";
        return (
          <section
            key={section}
            className="device-section"
            data-section={section}
            aria-labelledby={headingId}
          >
            <h3 id={headingId} className="device-section__heading">
              {SECTION_LABELS[section]}
              <span className="device-section__count"> ({rows.length})</span>
            </h3>
            {rows.length > 0 ? (
              <ul className="device-list">
                {rows.map((device) => (
                  <DeviceRow key={device.peripheral_id} device={device} onOpen={onOpen} />
                ))}
              </ul>
            ) : (
              <p className="device-section__empty">
                {listensToRadio && !scanRan
                  ? "Not checked: the radio could not be used for this scan."
                  : SECTION_EMPTY[section]}
              </p>
            )}
          </section>
        );
      })}
    </div>
  );
}

function DeviceRow({
  device,
  onOpen,
}: {
  device: DeviceCandidate;
  onOpen: (peripheralId: string) => void;
}) {
  const name = deviceName(device.advertised_name);
  const support = SUPPORT_PRESENTATION[device.support_status];
  return (
    <li className="device-row">
      <div className="device-row__main">
        <p className="device-row__name">{name}</p>
        <p className="device-row__meta">
          Model: {device.model ?? "Unknown"} · Firmware: {device.firmware ?? "Unknown"} · Signal:{" "}
          {formatSignal(device.rssi_dbm)}
        </p>
        <p className="device-row__meta">
          <StatusBadge tone={support.tone} glyph={support.glyph} label={device.status_label} />{" "}
          <span className="device-row__identity">{device.identity_label}</span>
          {device.authorized ? " · Authorization stored" : null}
        </p>
      </div>
      <Button onClick={() => onOpen(device.peripheral_id)} aria-label={`Open details of ${name}`}>
        Details
      </Button>
    </li>
  );
}
