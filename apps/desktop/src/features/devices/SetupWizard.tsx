import { useId, useState } from "react";
import { Button } from "../../components/Button";
import { useAppState } from "../../app/AppState";
import { useImport } from "../import/useImport";

export type SetupOption = "direct" | "account" | "files";

/** The three App. B.8 options, in the spec's words. */
export const SETUP_OPTIONS: ReadonlyArray<{
  value: SetupOption;
  title: string;
  summary: string;
}> = [
  {
    value: "direct",
    title: "Connect directly (offline after setup)",
    summary:
      "Discover, check support, authorize by an approved method, preview the recordings on the device, import and choose local processing.",
  },
  {
    value: "account",
    title: "Use PLAUD account",
    summary: "The official cloud command-line tool, for historical and account-side recordings.",
  },
  {
    value: "files",
    title: "Import files",
    summary: "Works without hardware, an account or a network connection.",
  },
];

export interface SetupWizardProps {
  onScan: () => void;
  scanning: boolean;
  onClose: () => void;
}

/**
 * Setup wizard (App. B.8, §4.2A). Exactly three options; each branch states
 * what it involves, and none of them silently becomes another. In particular a
 * slow or unsupported direct connection never switches to the cloud on its own.
 */
export function SetupWizard({ onScan, scanning, onClose }: SetupWizardProps) {
  const [option, setOption] = useState<SetupOption | null>(null);
  const { navigate } = useAppState();
  const { pickFiles, pending } = useImport();
  const legendId = useId();

  return (
    <section className="device-wizard" aria-labelledby="device-wizard-heading">
      <div className="device-wizard__head">
        <h3 id="device-wizard-heading" className="device-wizard__heading">
          Add a PLAUD device
        </h3>
        <Button variant="quiet" onClick={onClose}>
          Close setup
        </Button>
      </div>
      <fieldset className="device-wizard__options" aria-labelledby={legendId}>
        <legend id={legendId} className="device-wizard__legend">
          How do you want to bring recordings in?
        </legend>
        {SETUP_OPTIONS.map((entry) => (
          <label key={entry.value} className="device-wizard__option">
            <input
              type="radio"
              name="device-setup-option"
              value={entry.value}
              checked={option === entry.value}
              onChange={() => setOption(entry.value)}
            />
            <span>
              <strong>{entry.title}</strong>
              <span className="device-wizard__summary">{entry.summary}</span>
            </span>
          </label>
        ))}
      </fieldset>

      {option === "direct" ? (
        <div className="device-wizard__panel" role="group" aria-label="Connect directly">
          <ol className="device-wizard__steps">
            <li>Scan for nearby devices. Bluetooth permission is requested only when you scan.</li>
            <li>
              Check support. Each device is shown as Supported, Experimental or Unsupported from the
              compatibility register, and anything not yet verified is opt-in.
            </li>
            <li>Authorize with a token you already hold for your own device.</li>
            <li>Preview the recordings on the device, then import the ones you choose.</li>
          </ol>
          <p>
            A scan lists only what the device holds and advertises right now. The phone app may
            already have removed recordings from the device, so a direct scan is not a complete
            recording history.
          </p>
          <p>
            A slow Bluetooth transfer is shown as it is. It never switches to the cloud on its own.
          </p>
          <Button variant="primary" onClick={onScan} aria-busy={scanning} disabled={scanning}>
            Scan for nearby devices
          </Button>
        </div>
      ) : null}

      {option === "account" ? (
        <div className="device-wizard__panel" role="group" aria-label="Use PLAUD account">
          <p>
            This route uses the PLAUD cloud through the official command-line tool. It is PLAUD
            Cloud, not a direct connection to a device, and it needs an internet connection.
          </p>
          <p>Account sign-in and the tool&apos;s status are managed in Settings.</p>
          <Button variant="primary" onClick={() => navigate({ name: "settings" })}>
            Open account settings
          </Button>
        </div>
      ) : null}

      {option === "files" ? (
        <div className="device-wizard__panel" role="group" aria-label="Import files">
          <p>
            Choose audio files from this computer. This works without hardware, an account or a
            network connection.
          </p>
          <Button variant="primary" onClick={() => void pickFiles()} aria-busy={pending}>
            Choose audio files
          </Button>
        </div>
      ) : null}
    </section>
  );
}
