import { useCallback, useEffect, useRef, useState, type FormEvent } from "react";
import type { DeviceCandidate, DeviceListing, DeviceProbe } from "../../ipc/contract";
import { useCommandMutation } from "../../ipc/useCommand";
import { useAppState } from "../../app/AppState";
import { Button } from "../../components/Button";
import { Dialog } from "../../components/Dialog";
import { TextField } from "../../components/Field";
import { StatusBadge } from "../../components/StatusBadge";
import { CommandErrorNotice } from "../recording/errorNotice";
import { formatTimestamp } from "../activity/ActivityScreen";
import { TransferList } from "./TransferList";
import {
  CAPABILITY_LABELS,
  SUPPORT_PRESENTATION,
  deviceName,
  formatBattery,
  formatSignal,
  transportLabel,
} from "./devicePresentation";

export interface DeviceDetailProps {
  device: DeviceCandidate;
  onBack: () => void;
  /** Called after a change that can move the device between list sections. */
  onChanged: () => void;
}

/**
 * Detail page of one device (App. B.8): model, firmware (only when a probe
 * verified it), transport, advertised name, radio strength, battery,
 * capabilities, last contact, the number of files on the device with the
 * completeness qualifier, identity assurance, the register's status and
 * limitations, and whether automatic sync may be advertised.
 *
 * Every state shown here is fetched; events only trigger a refetch (§15.3).
 */
export function DeviceDetail({ device, onBack, onChanged }: DeviceDetailProps) {
  const { onLibraryChanged } = useAppState();
  const peripheralId = device.peripheral_id;
  const [probe, setProbe] = useState<DeviceProbe | null>(null);
  const [listing, setListing] = useState<DeviceListing | null>(null);
  const [selected, setSelected] = useState<ReadonlySet<string>>(new Set());
  const [token, setToken] = useState("");
  const [forgetOpen, setForgetOpen] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);

  const probeM = useCommandMutation("device_probe");
  const authorizeM = useCommandMutation("device_authorize");
  const listM = useCommandMutation("device_list_recordings");
  const importM = useCommandMutation("device_import");
  const disconnectM = useCommandMutation("device_disconnect");
  const syncM = useCommandMutation("device_sync_policy");

  // A disconnect must not be undone by the refetch its own events trigger.
  const quiet = useRef(false);
  const listed = useRef(false);

  const runProbe = useCallback(
    async (approved: boolean) => {
      try {
        setProbe(await probeM.run({ peripheral_id: peripheralId, experimental_probe_approved: approved }));
      } catch {
        // Rendered from `probeM.error`.
      }
    },
    // `run` is stable per command.
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [peripheralId],
  );

  const loadListing = useCallback(async () => {
    try {
      const next = await listM.run({ peripheral_id: peripheralId });
      quiet.current = false;
      listed.current = true;
      setListing(next);
    } catch {
      // Rendered from `listM.error`.
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [peripheralId]);

  useEffect(() => {
    // Opening a device is the user's explicit request to check it; a device
    // that is not in range cannot be probed.
    if (device.section !== "previously_linked") void runProbe(false);
    // Only on first open of this device.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [peripheralId]);

  useEffect(
    () =>
      onLibraryChanged((event) => {
        if (!listed.current || quiet.current) return;
        const relevant =
          event === null ||
          ((event.kind === "device.transfer_progress" || event.kind === "device.transfer_verified") &&
            event.payload.peripheral_id === peripheralId);
        if (relevant) void loadListing();
      }),
    [onLibraryChanged, loadListing, peripheralId],
  );

  const onAuthorize = async (event: FormEvent) => {
    event.preventDefault();
    if (token.trim().length === 0) return;
    try {
      await authorizeM.run({ peripheral_id: peripheralId, token_hex: token.trim() });
      // The value leaves React state as soon as it has been handed over.
      setToken("");
      await runProbe(probe?.gate.experimental_probe_approved ?? false);
      onChanged();
    } catch {
      // Rendered from `authorizeM.error`.
    }
  };

  const onImport = async () => {
    try {
      await importM.run({
        peripheral_id: peripheralId,
        recording_ids: [...selected],
        local_only: true,
      });
      setSelected(new Set());
      await loadListing();
    } catch {
      // Rendered from `importM.error`.
    }
  };

  const disconnect = async (forget: boolean) => {
    quiet.current = true;
    try {
      await disconnectM.run({ peripheral_id: peripheralId, forget_authorization: forget });
      listed.current = false;
      setListing(null);
      setSelected(new Set());
      setForgetOpen(false);
      setNotice(
        forget
          ? "The stored authorization was removed from the system keychain. Recordings already in your library were kept."
          : "Disconnected from the device. Anything not yet archived was not added to your library. Recordings already in your library were kept.",
      );
      onChanged();
      if (forget) onBack();
    } catch {
      quiet.current = false;
    }
  };

  const onSync = async (enable: boolean) => {
    try {
      const policy = await syncM.run({ peripheral_id: peripheralId, enable_when_nearby: enable });
      setProbe((current) => (current ? { ...current, sync: policy } : current));
    } catch {
      // Rendered from `syncM.error`.
    }
  };

  const name = deviceName(probe?.advertised_name ?? device.advertised_name);
  const statusLabel = probe?.status_label ?? device.status_label;
  const support = SUPPORT_PRESENTATION[probe?.support_status ?? device.support_status];
  const identityLabel = listing?.identity_label ?? probe?.identity_label ?? device.identity_label;
  const model = probe?.model ?? device.model;
  const firmware = probe?.firmware ?? device.firmware;
  const authorized = probe?.authorized ?? device.authorized;
  const capabilityOf = (key: "read_only_listing" | "read_only_transfer") =>
    probe?.capabilities.find((c) => c.capability === key)?.available ?? false;
  const canList = authorized && capabilityOf("read_only_listing");
  const canTransfer = capabilityOf("read_only_transfer");
  const needsCredential =
    probe !== null &&
    !probe.authorized &&
    (probe.section === "needs_authorization" || probe.section === "nearby");
  const lastContact = probe?.last_contact_at ?? device.last_contact_at;
  const inFlight = listing?.items.some((item) =>
    ["queued", "connecting", "authenticating", "requesting", "receiving", "verifying"].includes(
      item.transfer.state,
    ),
  );

  return (
    <article className="device-detail" aria-labelledby="device-detail-heading">
      <div className="device-detail__bar">
        <Button variant="quiet" glyph="←" onClick={onBack}>
          All devices
        </Button>
      </div>
      <h3 id="device-detail-heading" className="device-detail__heading">
        {name}
      </h3>
      <div className="device-detail__badges">
        <StatusBadge tone={support.tone} glyph={support.glyph} label={statusLabel} />
        <span className="device-detail__identity">
          <span aria-hidden="true">{identityLabel === "Unverified identity" ? "? " : "✓ "}</span>
          {identityLabel}
        </span>
      </div>

      {probeM.pending ? (
        <p role="status" className="device-detail__status">
          Checking the device…
        </p>
      ) : null}
      {device.section === "previously_linked" && probe === null ? (
        <p role="status" className="device-detail__status">
          This device is not advertising right now. Bring it within range and scan again to check it.
        </p>
      ) : null}
      {probeM.error ? (
        <CommandErrorNotice error={probeM.error} onRetry={() => void runProbe(false)} />
      ) : null}
      {notice ? (
        <p role="status" className="device-detail__status">
          {notice}
        </p>
      ) : null}

      <section aria-labelledby="device-facts-heading" className="device-detail__section">
        <h4 id="device-facts-heading" className="device-detail__subheading">
          Device facts
        </h4>
        <dl className="device-facts">
          <div>
            <dt>Model</dt>
            <dd>{model ?? "Unknown"}</dd>
          </div>
          <div>
            <dt>Firmware</dt>
            <dd>{firmware ?? "Unknown"}</dd>
          </div>
          <div>
            <dt>Transport</dt>
            <dd>{transportLabel(probe?.transport ?? device.transport)}</dd>
          </div>
          <div>
            <dt>Advertised name</dt>
            <dd>{probe?.advertised_name ?? device.advertised_name ?? "Not advertised"}</dd>
          </div>
          <div>
            <dt>Signal strength</dt>
            <dd>{formatSignal(probe?.rssi_dbm ?? device.rssi_dbm)}</dd>
          </div>
          <div>
            <dt>Battery</dt>
            <dd>{formatBattery(probe?.battery_percent ?? null)}</dd>
          </div>
          <div>
            <dt>Identity</dt>
            <dd>{identityLabel}</dd>
          </div>
          <div>
            <dt>Last contact</dt>
            <dd>{lastContact ? formatTimestamp(lastContact) : "Never"}</dd>
          </div>
          <div>
            <dt>Files on the device</dt>
            <dd>
              {listing ? listing.file_count : "Unknown until the recordings are listed"}
            </dd>
          </div>
          {probe ? (
            <div>
              <dt>Source</dt>
              <dd>{probe.source_label}</dd>
            </div>
          ) : null}
          {probe ? (
            <div>
              <dt>Cloud</dt>
              <dd>{probe.cloud_route === "not_used" ? "Cloud: not used" : "Cloud: used"}</dd>
            </div>
          ) : null}
        </dl>
        {identityLabel === "Unverified identity" ? (
          <p className="device-detail__note">
            An advertisement can be copied, so a name or a signal does not identify this device. It
            stays unverified until the device accepts its authorization.
          </p>
        ) : null}
      </section>

      {probe && probe.disclosures.length > 0 ? (
        <section
          aria-labelledby="device-disclosures-heading"
          className="device-detail__section device-detail__section--note"
        >
          <h4 id="device-disclosures-heading" className="device-detail__subheading">
            Radio disclosures
          </h4>
          <ul>
            {probe.disclosures.map((text) => (
              <li key={text}>{text}</li>
            ))}
          </ul>
        </section>
      ) : null}

      {probe ? (
        <section aria-labelledby="device-capabilities-heading" className="device-detail__section">
          <h4 id="device-capabilities-heading" className="device-detail__subheading">
            Capabilities
          </h4>
          <ul className="device-capabilities">
            {probe.capabilities.map((capability) => (
              <li key={capability.capability}>
                <span className="device-capabilities__name">
                  {CAPABILITY_LABELS[capability.capability]}
                </span>
                {": "}
                <strong>{capability.available ? "Available" : "Not available"}</strong>
                {capability.reason ? <span> — {capability.reason}</span> : null}
              </li>
            ))}
          </ul>
        </section>
      ) : null}

      {probe && probe.known_limitations.length > 0 ? (
        <section aria-labelledby="device-limits-heading" className="device-detail__section">
          <h4 id="device-limits-heading" className="device-detail__subheading">
            Known limitations
          </h4>
          <ul>
            {probe.known_limitations.map((text) => (
              <li key={text}>{text}</li>
            ))}
          </ul>
        </section>
      ) : null}

      {probe ? (
        <section aria-labelledby="device-sync-heading" className="device-detail__section">
          <h4 id="device-sync-heading" className="device-detail__subheading">
            Automatic sync
          </h4>
          {probe.sync.may_advertise ? (
            <label className="device-detail__check">
              <input
                type="checkbox"
                checked={probe.sync.enabled}
                disabled={syncM.pending}
                onChange={(event) => void onSync(event.target.checked)}
              />
              Transfer recordings automatically when this device is nearby (read-only)
            </label>
          ) : (
            <p>Not offered. {probe.sync.explanation}</p>
          )}
          {syncM.error ? <CommandErrorNotice error={syncM.error} /> : null}
        </section>
      ) : null}

      {probe && probe.gate.experimental_probe_available ? (
        <section aria-labelledby="device-experimental-heading" className="device-detail__section">
          <h4 id="device-experimental-heading" className="device-detail__subheading">
            Experimental probe
          </h4>
          {probe.gate.experimental_probe_available ? (
            <label className="device-detail__check">
              <input
                type="checkbox"
                checked={probe.gate.experimental_probe_approved}
                disabled={probeM.pending}
                onChange={(event) => void runProbe(event.target.checked)}
              />
              Allow a read-only experimental probe of this device (listing only, no transfer)
            </label>
          ) : null}
        </section>
      ) : null}

      {needsCredential && probe ? (
        <section aria-labelledby="device-authorize-heading" className="device-detail__section">
          <h4 id="device-authorize-heading" className="device-detail__subheading">
            Authorize this device
          </h4>
          {probe.provisioning_note ? <p>{probe.provisioning_note}</p> : null}
          <form className="device-detail__form" onSubmit={(event) => void onAuthorize(event)}>
            <TextField
              label="Authorization token (hexadecimal)"
              type="password"
              autoComplete="off"
              spellCheck={false}
              value={token}
              onChange={(event) => setToken(event.target.value)}
              hint="Stored in the system keychain for this device only. It is never shown again and never leaves this computer."
            />
            <Button
              type="submit"
              variant="primary"
              disabled={authorizeM.pending || token.trim().length === 0}
            >
              Store in system keychain
            </Button>
          </form>
          {authorizeM.error ? <CommandErrorNotice error={authorizeM.error} /> : null}
        </section>
      ) : null}

      {probe && authorized ? (
        <section aria-labelledby="device-recordings-heading" className="device-detail__section">
          <h4 id="device-recordings-heading" className="device-detail__subheading">
            Recordings on the device
          </h4>
          <div className="device-detail__actions">
            <Button
              variant={listing ? "default" : "primary"}
              disabled={!canList || listM.pending}
              aria-busy={listM.pending}
              onClick={() => void loadListing()}
            >
              {listing ? "Refresh the list" : "List recordings on the device"}
            </Button>
            {!canList ? (
              <span className="device-detail__hint">
                Listing is not available for this device yet; see the capabilities above.
              </span>
            ) : null}
          </div>
          {listM.error ? <CommandErrorNotice error={listM.error} onRetry={() => void loadListing()} /> : null}
          {listing ? (
            <>
              <p className="device-detail__completeness" role="note">
                {listing.completeness_label}
              </p>
              {listing.items.length === 0 ? (
                <p>The device lists no recordings right now.</p>
              ) : (
                <TransferList
                  items={listing.items}
                  selectable={canTransfer}
                  selected={selected}
                  onToggle={(id) =>
                    setSelected((current) => {
                      const next = new Set(current);
                      if (next.has(id)) next.delete(id);
                      else next.add(id);
                      return next;
                    })
                  }
                  onCancel={() => void disconnect(false)}
                  cancelPending={disconnectM.pending}
                />
              )}
              {canTransfer ? (
                <div className="device-detail__actions">
                  <Button
                    variant="primary"
                    disabled={selected.size === 0 || importM.pending || Boolean(inFlight)}
                    onClick={() => void onImport()}
                  >
                    Import selected for local processing
                  </Button>
                </div>
              ) : (
                <p className="device-detail__hint">
                  Transfer is not enabled for this device profile ({statusLabel}).
                </p>
              )}
            </>
          ) : null}
          {importM.error ? <CommandErrorNotice error={importM.error} /> : null}
        </section>
      ) : null}

      <div className="device-detail__actions">
        {listing || identityLabel !== "Unverified identity" ? (
          <Button onClick={() => void disconnect(false)} disabled={disconnectM.pending}>
            Disconnect
          </Button>
        ) : null}
        {authorized ? (
          <Button variant="danger" onClick={() => setForgetOpen(true)}>
            Forget authorization
          </Button>
        ) : null}
      </div>
      {disconnectM.error ? <CommandErrorNotice error={disconnectM.error} /> : null}

      <Dialog
        open={forgetOpen}
        title="Forget this device's authorization?"
        description="The stored token is removed from the system keychain. Recordings already in your library are kept. You will need to authorize the device again to transfer from it."
        destructive
        onClose={() => setForgetOpen(false)}
        footer={
          <>
            <Button onClick={() => setForgetOpen(false)}>Keep authorization</Button>
            <Button variant="danger" onClick={() => void disconnect(true)}>
              Forget authorization
            </Button>
          </>
        }
      />
    </article>
  );
}
