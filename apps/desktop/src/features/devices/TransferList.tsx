import type { DeviceRecording } from "../../ipc/contract";
import { Button } from "../../components/Button";
import { Progress } from "../../components/Progress";
import { StatusBadge } from "../../components/StatusBadge";
import { formatBytes } from "../settings/bytes";
import { formatTimestamp } from "../activity/ActivityScreen";
import {
  CANCELLABLE_STATES,
  TRANSFER_PRESENTATION,
  formatBandwidth,
  formatEta,
} from "./devicePresentation";

export interface TransferListProps {
  items: DeviceRecording[];
  /** Whether rows can be chosen for import. */
  selectable: boolean;
  selected: ReadonlySet<string>;
  onToggle: (recordingId: string) => void;
  /** Cancelling disconnects from the device; nothing partial is imported. */
  onCancel: () => void;
  cancelPending: boolean;
}

/**
 * The recordings a device lists, each with its own transfer state (§4.2A.8).
 * Labels are the DTO's `state_label`, which Rust takes from
 * `TransferState::ui_label`; bandwidth is observed, the ETA is an estimate and
 * is labelled as one.
 */
export function TransferList({
  items,
  selectable,
  selected,
  onToggle,
  onCancel,
  cancelPending,
}: TransferListProps) {
  return (
    <ul className="transfer-list" aria-label="Recordings on the device">
      {items.map((item) => {
        const { transfer } = item;
        const presentation = TRANSFER_PRESENTATION[transfer.state];
        const inFlight = CANCELLABLE_STATES.includes(transfer.state);
        const choosable = selectable && transfer.state === "discovered";
        const checkboxId = `device-recording-${item.recording_id}`;
        const received = transfer.bytes_received;
        const fraction =
          received !== null && item.declared_byte_length > 0
            ? received / item.declared_byte_length
            : null;
        return (
          <li
            key={item.recording_id}
            className="transfer-row"
            data-transfer-state={transfer.state}
          >
            <div className="transfer-row__head">
              {choosable ? (
                <>
                  <input
                    id={checkboxId}
                    type="checkbox"
                    checked={selected.has(item.recording_id)}
                    onChange={() => onToggle(item.recording_id)}
                  />
                  <label className="transfer-row__name" htmlFor={checkboxId}>
                    {item.display_name}
                  </label>
                </>
              ) : (
                <span className="transfer-row__name">{item.display_name}</span>
              )}
              <StatusBadge
                tone={presentation.tone}
                glyph={presentation.glyph}
                label={transfer.state_label}
              />
            </div>
            <p className="transfer-row__meta">
              Declared by the device: {formatBytes(item.declared_byte_length)}
              {item.captured_at ? ` · Captured ${formatTimestamp(item.captured_at)}` : ""}
            </p>
            <p className="transfer-row__meta">
              Source: {transfer.source_label} ·{" "}
              {transfer.cloud_route === "not_used" ? "Cloud: not used" : "Cloud: used"}
            </p>
            {transfer.verified ? (
              <p className="transfer-row__meta">Bytes verified before they were kept.</p>
            ) : null}
            {transfer.state === "receiving" ? (
              <div className="transfer-row__progress">
                <Progress
                  label={`Transfer of ${item.display_name}`}
                  value={fraction}
                  detail="of the size the device declared"
                />
                <p className="transfer-row__meta">
                  Observed bandwidth: {formatBandwidth(transfer.observed_bytes_per_sec)}
                </p>
                <p className="transfer-row__meta">
                  Estimated time remaining (an estimate, not a promise):{" "}
                  {formatEta(transfer.eta_seconds_estimate)}
                </p>
              </div>
            ) : null}
            {inFlight ? (
              <Button
                variant="quiet"
                glyph="×"
                disabled={cancelPending}
                aria-label={`Cancel transfer of ${item.display_name}`}
                onClick={onCancel}
              >
                Cancel transfer
              </Button>
            ) : null}
          </li>
        );
      })}
    </ul>
  );
}
