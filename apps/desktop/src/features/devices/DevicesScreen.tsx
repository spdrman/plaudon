import { useCallback, useState } from "react";
import "./devices.css";
import type { DeviceScan } from "../../ipc/contract";
import { useCommandMutation } from "../../ipc/useCommand";
import { Button } from "../../components/Button";
import { EmptyState } from "../../components/EmptyState";
import { CommandErrorNotice } from "../recording/errorNotice";
import { useImport } from "../import/useImport";
import { DeviceDetail } from "./DeviceDetail";
import { DeviceList } from "./DeviceList";
import { SetupWizard } from "./SetupWizard";

/** Scan length. The scan is user-triggered (§15.2); this only bounds it. */
const SCAN_TIMEOUT_MS = 5_000;

/** §5.4 corrective actions for a device that was not found. */
const NOT_FOUND_ACTIONS = [
  "Check that Bluetooth permission is allowed for Plaudon in your system settings.",
  "Check that the device is powered on and charged.",
  "Close the PLAUD phone app: a phone connected to the device can keep other hosts out.",
  "Check that the model is one this application can check. Unsupported models are listed with their status once they are found.",
];

export interface DevicesScreenProps {
  /** Device whose detail is open, from a deep link. */
  selectedId: string | null;
  onSelect: (peripheralId: string | null) => void;
}

/**
 * The Devices workspace (App. B.8). A scan, a probe and a listing each run only
 * as the result of the user's own action (Scan, opening a device, List); no
 * event and no timer starts one, so nothing scans, connects or reads from a
 * device on its own.
 */
export function DevicesScreen({ selectedId, onSelect }: DevicesScreenProps) {
  const [scan, setScan] = useState<DeviceScan | null>(null);
  const [wizardOpen, setWizardOpen] = useState(false);
  const scanM = useCommandMutation("list_nearby_devices");
  const { pickFiles } = useImport();

  const runScan = useCallback(async () => {
    try {
      setScan(await scanM.run({ timeout_ms: SCAN_TIMEOUT_MS }));
    } catch {
      // Rendered from `scanM.error`.
    }
    // `run` is stable per command.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const selected = selectedId
    ? (scan?.devices.find((device) => device.peripheral_id === selectedId) ?? null)
    : null;

  return (
    <section className="devices" aria-labelledby="devices-heading">
      <div className="devices__header">
        <h2 id="devices-heading" className="devices__heading">
          PLAUD devices
        </h2>
        <div className="devices__actions">
          <Button
            variant="primary"
            glyph="⌖"
            aria-busy={scanM.pending}
            disabled={scanM.pending}
            onClick={() => void runScan()}
          >
            Scan for nearby devices
          </Button>
          <Button onClick={() => setWizardOpen((open) => !open)} aria-expanded={wizardOpen}>
            Add PLAUD device
          </Button>
        </div>
      </div>
      <p className="devices__intro">
        Each device is shown with the compatibility status recorded for its exact model and
        firmware. Scanning asks for Bluetooth permission only when you start a scan.
      </p>

      {wizardOpen ? (
        <SetupWizard
          onScan={() => void runScan()}
          scanning={scanM.pending}
          onClose={() => setWizardOpen(false)}
        />
      ) : null}

      <p className="devices__status" role="status">
        {scanM.pending ? "Scanning for devices…" : ""}
      </p>
      {scanM.error ? (
        <CommandErrorNotice error={scanM.error} onRetry={() => void runScan()} />
      ) : null}

      {scan && scan.radio.state !== "scanned" ? (
        <div
          className="devices__radio"
          role="alert"
          data-radio-state={scan.radio.state}
        >
          <h3 className="devices__radio-title">
            {scan.radio.state === "permission_denied"
              ? "Bluetooth access is not permitted"
              : "This build cannot scan for Bluetooth devices"}
          </h3>
          <p>{scan.radio.message}</p>
          <div className="devices__actions">
            <Button onClick={() => void runScan()}>Scan again</Button>
            <Button variant="primary" onClick={() => void pickFiles()}>
              Import audio files instead
            </Button>
          </div>
        </div>
      ) : null}

      {scan && scan.radio.state === "scanned" && scan.devices.every((d) => d.section === "previously_linked") ? (
        <EmptyState
          title="No PLAUD device found"
          body={
            <>
              The scan ran and heard no PLAUD device. This is not the same as having no device.
              <ul className="devices__checklist">
                {NOT_FOUND_ACTIONS.map((text) => (
                  <li key={text}>{text}</li>
                ))}
              </ul>
            </>
          }
          actions={
            <>
              <Button onClick={() => void runScan()}>Scan again</Button>
              <Button variant="primary" onClick={() => void pickFiles()}>
                Import audio files instead
              </Button>
            </>
          }
        />
      ) : null}

      {scan === null && !scanM.pending && !scanM.error ? (
        <EmptyState
          title="No scan yet"
          body="Scan to see which PLAUD devices are nearby and which you linked before. Nothing connects to a device until you open it."
          actions={
            <>
              <Button variant="primary" onClick={() => void runScan()}>
                Scan for nearby devices
              </Button>
              <Button onClick={() => void pickFiles()}>Import audio files instead</Button>
            </>
          }
        />
      ) : null}

      {scan && selectedId ? (
        selected ? (
          <DeviceDetail
            key={selected.peripheral_id}
            device={selected}
            onBack={() => onSelect(null)}
            onChanged={() => void runScan()}
          />
        ) : (
          <div role="status" className="devices__status">
            <p>This device was not in the latest scan. Scan again with it in range.</p>
            <Button onClick={() => onSelect(null)}>All devices</Button>
          </div>
        )
      ) : null}

      {scan && !selectedId ? (
        <DeviceList
          devices={scan.devices}
          scanRan={scan.radio.state === "scanned"}
          onOpen={(id) => onSelect(id)}
        />
      ) : null}
    </section>
  );
}
