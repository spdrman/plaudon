/**
 * In-memory stand-in for the `device_*` commands (spec §15.2, EPIC L).
 *
 * It mirrors the rules the Rust backend enforces so UI tests exercise the same
 * states: a credential does not authenticate a device, an experimental profile
 * may list but never transfer, `device_authorize` never echoes the token, and a
 * radio that cannot be used is reported rather than shown as an empty list.
 * It carries register-style strings only because the real DTOs do; the UI under
 * test must render them verbatim and never compose its own.
 */
import type {
  CloudRoute,
  DeviceAuthStatus,
  DeviceCandidate,
  DeviceDisconnect,
  DeviceImport,
  DeviceListing,
  DeviceProbe,
  DeviceRecording,
  DeviceScan,
  DeviceSection,
  DeviceSupportStatus,
  DeviceSyncPolicy,
  DeviceTransfer,
  DeviceTransferState,
  ErrorEnvelope,
  EventKind,
  RadioStatus,
  UiEvent,
} from "../ipc/contract";

type Emit = (kind: EventKind, partial?: Partial<UiEvent>) => UiEvent;
type Fail = (code: string, message: string, extra?: Partial<ErrorEnvelope>) => ErrorEnvelope;

export const SOURCE_BLE = "Device BLE · compressed ASR";

export const LEGACY_DISCLOSURES_FIXTURE = [
  "This device profile exchanges a static token over an unencrypted Bluetooth link. Someone within radio range could record the exchange.",
  "A copied token authenticates as your device, so profile authentication proves the protocol succeeded, not that the peer is cryptographically identified.",
  "Turning on Local-only Mode stops this application from using the internet. It does not encrypt the Bluetooth link.",
];

const STATUS_LABELS: Record<DeviceSupportStatus, string> = {
  not_evaluated: "Not evaluated",
  experimental: "Experimental",
  supported: "Supported",
  known_unsupported: "Known unsupported",
};

const STATE_LABELS: Record<DeviceTransferState, string> = {
  discovered: "On device",
  queued: "Queued",
  connecting: "Connecting",
  authenticating: "Authenticating",
  requesting: "Requesting",
  receiving: "Transferring",
  verifying: "Verifying",
  committing: "Transferred locally",
  imported: "Archived",
  interrupted: "Interrupted",
  waiting_for_device: "Waiting for device",
  paused: "Paused",
  cancelled: "Cancelled",
  unsupported: "Unsupported",
};

const COMPLETENESS_DELTA =
  "Unsynced recordings only. Recordings already synchronized to a phone may be missing, and direct transfer cannot retrieve files deleted from the device, so this is not a complete recording history.";

export interface FakeDeviceSpec {
  peripheral_id: string;
  advertised_name?: string | null;
  rssi_dbm?: number | null;
  /** Advertising right now. */
  nearby?: boolean;
  /** Linked before (a credential is stored). */
  authorized?: boolean;
  /** A probe has established model and firmware. */
  probed?: boolean;
  support_status?: DeviceSupportStatus;
  section?: DeviceSection;
  model?: string | null;
  firmware?: string | null;
  battery_percent?: number | null;
  known_limitations?: string[];
  disclosures?: string[];
  permits_transfer?: boolean;
  experimental_probe_available?: boolean;
  may_advertise_sync?: boolean;
  last_contact_at?: string | null;
  recordings?: Array<{ id: string; name: string; bytes?: number }>;
  truncated?: boolean;
  cloud_route?: CloudRoute;
  identity_authenticated?: boolean;
}

interface FakeDevice extends Required<Omit<FakeDeviceSpec, "recordings">> {
  recordings: Array<{ id: string; name: string; bytes: number }>;
  approved: boolean;
  syncEnabled: boolean;
  transfers: Map<string, DeviceTransfer>;
  listed: boolean;
}

export class FakeDevices {
  /** Secrets the UI handed over; tests assert they are never echoed back. */
  receivedTokens: string[] = [];
  radio: RadioStatus = { state: "scanned", code: null, message: "Scan finished. Only devices advertising right now appear as nearby." };
  /** When set, `list_nearby_devices` fails with this envelope. */
  scanFailure: ErrorEnvelope | null = null;
  readonly #devices = new Map<string, FakeDevice>();
  #transferCounter = 0;
  readonly #emit: Emit;
  readonly #fail: Fail;

  constructor(emit: Emit, fail: Fail) {
    this.#emit = emit;
    this.#fail = fail;
  }

  /** Radio helpers for the empty and denied states (spec §5.4). */
  setRadio(state: RadioStatus["state"]): void {
    this.radio =
      state === "permission_denied"
        ? {
            state,
            code: "RADIO_PERMISSION_DENIED",
            message:
              "Bluetooth access is not permitted for this application, so no scan ran. Allow Bluetooth for Plaudon in your system settings, then scan again.",
          }
        : state === "unavailable_in_build"
          ? {
              state,
              code: "SOURCE_UNAVAILABLE",
              message:
                "This build of Plaudon cannot scan for Bluetooth devices, so no scan ran. This is not the same as no devices being nearby. Import files works without a device.",
            }
          : { state, code: null, message: "Scan finished. Only devices advertising right now appear as nearby." };
  }

  addDevice(spec: FakeDeviceSpec): void {
    const status = spec.support_status ?? "not_evaluated";
    this.#devices.set(spec.peripheral_id, {
      peripheral_id: spec.peripheral_id,
      advertised_name: spec.advertised_name === undefined ? "PLAUD_NOTE" : spec.advertised_name,
      rssi_dbm: spec.rssi_dbm === undefined ? -52 : spec.rssi_dbm,
      nearby: spec.nearby ?? true,
      authorized: spec.authorized ?? false,
      probed: spec.probed ?? false,
      support_status: status,
      section: spec.section ?? "needs_authorization",
      model: spec.model === undefined ? "Note" : spec.model,
      firmware: spec.firmware === undefined ? "V0095" : spec.firmware,
      battery_percent: spec.battery_percent === undefined ? 81 : spec.battery_percent,
      known_limitations: spec.known_limitations ?? [
        "The BLE listing is a delta of unsynchronized files; recordings already synced to a phone may be absent.",
        "Yields a compressed .ASR Opus sidecar, not the original stereo WAV.",
      ],
      disclosures: spec.disclosures ?? LEGACY_DISCLOSURES_FIXTURE,
      permits_transfer: spec.permits_transfer ?? status === "supported",
      experimental_probe_available: spec.experimental_probe_available ?? status === "not_evaluated",
      may_advertise_sync: spec.may_advertise_sync ?? false,
      last_contact_at: spec.last_contact_at === undefined ? null : spec.last_contact_at,
      recordings: (spec.recordings ?? [
        { id: "rec-1", name: "Standup" },
        { id: "rec-2", name: "Customer call" },
      ]).map((r) => ({ ...r, bytes: r.bytes ?? 1_000_000 })),
      truncated: spec.truncated ?? false,
      cloud_route: spec.cloud_route ?? "not_used",
      identity_authenticated: spec.identity_authenticated ?? false,
      approved: false,
      syncEnabled: false,
      transfers: new Map(),
      listed: false,
    });
  }

  /** Test hook: put one recording into a transfer state and publish progress. */
  setTransfer(
    peripheralId: string,
    recordingId: string,
    state: DeviceTransferState,
    patch: Partial<DeviceTransfer> = {},
  ): void {
    const device = this.#require(peripheralId);
    const previous = device.transfers.get(recordingId);
    device.transfers.set(recordingId, {
      bytes_received: null,
      observed_bytes_per_sec: null,
      eta_seconds_estimate: null,
      source_label: SOURCE_BLE,
      cloud_route: device.cloud_route,
      ...previous,
      transfer_id: previous?.transfer_id ?? `transfer-${(this.#transferCounter += 1)}`,
      state,
      state_label: STATE_LABELS[state],
      verified: state === "committing" || state === "imported",
      ...patch,
    });
    this.#emit("device.transfer_progress", {
      payload: { peripheral_id: peripheralId, recording_id: recordingId, state },
    });
    if (state === "committing") {
      this.#emit("device.transfer_verified", {
        payload: { peripheral_id: peripheralId, recording_id: recordingId, state },
      });
    }
  }

  invoke(command: string, request: Record<string, any>): unknown {
    switch (command) {
      case "list_nearby_devices":
        return this.#scan();
      case "device_probe":
        return this.#probe(request.peripheral_id, request.experimental_probe_approved === true);
      case "device_authorize":
        return this.#authorize(request.peripheral_id, request.token_hex);
      case "device_list_recordings":
        return this.#list(request.peripheral_id);
      case "device_import":
        return this.#import(request.peripheral_id, request.recording_ids ?? [], request.local_only === true);
      case "device_disconnect":
        return this.#disconnect(request.peripheral_id, request.forget_authorization === true);
      case "device_sync_policy":
        return this.#sync(request.peripheral_id, request.enable_when_nearby === true);
      default:
        throw new Error(`FakeDevices cannot handle ${command}`);
    }
  }

  #require(id: string): FakeDevice {
    const device = this.#devices.get(id);
    if (!device) throw this.#fail("INVALID_ARGUMENT", "The device was not seen in the latest scan; scan again first.");
    return device;
  }

  #sectionOf(device: FakeDevice): DeviceSection {
    if (!device.nearby) return "previously_linked";
    if (!device.probed) return "nearby";
    if (device.identity_authenticated && device.section !== "unsupported_firmware") return "connected";
    if (device.section === "needs_authorization" && device.authorized) return "nearby";
    return device.section;
  }

  #identity(device: FakeDevice) {
    return device.identity_authenticated
      ? ({ identity: "profile_authenticated", identity_label: "Profile-authenticated" } as const)
      : ({ identity: "unverified", identity_label: "Unverified identity" } as const);
  }

  #candidate(device: FakeDevice): DeviceCandidate {
    const status = device.probed ? device.support_status : "not_evaluated";
    return {
      peripheral_id: device.peripheral_id,
      section: this.#sectionOf(device),
      advertised_name: device.advertised_name,
      rssi_dbm: device.nearby ? device.rssi_dbm : null,
      transport: "ble",
      model: device.probed || !device.nearby ? device.model : null,
      firmware: device.probed || !device.nearby ? device.firmware : null,
      support_status: status,
      status_label: STATUS_LABELS[status],
      ...this.#identity(device),
      authorized: device.authorized,
      last_contact_at: device.last_contact_at,
    };
  }

  #scan(): DeviceScan {
    if (this.scanFailure) throw this.scanFailure;
    const scanned = this.radio.state === "scanned";
    const devices = [...this.#devices.values()]
      .filter((d) => (scanned && d.nearby) || d.authorized)
      .map((d) => {
        if (!scanned) d.nearby = false;
        return this.#candidate(d);
      });
    if (scanned) {
      for (const d of this.#devices.values()) if (d.nearby) this.#emit("device.discovered", { payload: { peripheral_id: d.peripheral_id } });
    }
    return { radio: this.radio, devices, scanned_at: scanned ? "2026-10-11T09:00:00Z" : null };
  }

  #syncDto(device: FakeDevice): DeviceSyncPolicy {
    const may = device.may_advertise_sync && device.permits_transfer;
    return {
      peripheral_id: device.peripheral_id,
      may_advertise: may,
      enabled: may && device.syncEnabled,
      default_on: false,
      audit_code: may ? "UNCHANGED" : "NOT_RUN",
      explanation: may
        ? "Repeated read-only passes left the device's own file list unchanged."
        : device.permits_transfer
          ? "Automatic transfer when nearby is off: this device profile has not passed a non-destructive audit yet."
          : "Automatic transfer when nearby is unavailable: this device profile is not Supported in the compatibility register.",
    };
  }

  #probe(id: string, approved: boolean): DeviceProbe {
    const device = this.#require(id);
    device.probed = true;
    device.approved = approved && device.experimental_probe_available;
    const listingOk = device.permits_transfer || device.approved;
    const sync = this.#syncDto(device);
    this.#emit("device.compatibility_changed", { payload: { peripheral_id: id } });
    return {
      peripheral_id: id,
      advertised_name: device.advertised_name,
      transport: "ble",
      model: device.model,
      firmware: device.firmware,
      auth_mode: device.firmware ? "legacy_static_token" : null,
      battery_percent: device.battery_percent,
      rssi_dbm: device.rssi_dbm,
      section: this.#sectionOf(device),
      support_status: device.support_status,
      status_label: STATUS_LABELS[device.support_status],
      known_limitations: device.known_limitations,
      gate: {
        permits_transfer: device.permits_transfer,
        experimental_probe_available: device.experimental_probe_available,
        experimental_probe_approved: device.approved,
        refusal_code: device.permits_transfer ? null : device.experimental_probe_available ? "NOT_EVALUATED" : "KNOWN_UNSUPPORTED",
      },
      ...this.#identity(device),
      capabilities: [
        { capability: "read_only_listing", available: listingOk, reason: listingOk ? null : `Compatibility status is ${STATUS_LABELS[device.support_status]}; a read-only experimental probe must be approved first.` },
        { capability: "read_only_transfer", available: device.permits_transfer, reason: device.permits_transfer ? null : `Compatibility status is ${STATUS_LABELS[device.support_status]}; only a Supported profile may transfer.` },
        { capability: "automatic_sync", available: sync.may_advertise, reason: sync.may_advertise ? null : sync.explanation },
      ],
      source_label: SOURCE_BLE,
      cloud_route: device.cloud_route,
      disclosures: device.disclosures,
      provisioning_note:
        "Import an authorization token you already hold for your own device. It is stored in this computer's credential vault and never leaves it.",
      authorized: device.authorized,
      last_contact_at: device.last_contact_at,
      sync,
    };
  }

  #authorize(id: string, token: unknown): DeviceAuthStatus {
    const device = this.#require(id);
    if (typeof token !== "string" || !/^[0-9a-fA-F]+$/.test(token) || token.length % 2 !== 0) {
      throw this.#fail("INVALID_ARGUMENT", "The request was not valid.");
    }
    if (token.length !== 32 && token.length !== 64) {
      throw this.#fail("INVALID_ARGUMENT", "The request was not valid.");
    }
    this.receivedTokens.push(token);
    device.authorized = true;
    return { peripheral_id: id, credential_stored: true, identity: "unverified", identity_label: "Unverified identity" };
  }

  #list(id: string): DeviceListing {
    const device = this.#require(id);
    if (!device.authorized) {
      throw this.#fail("AUTH_BOOTSTRAP_REQUIRED", "This device requires a provisioning step this application does not perform.");
    }
    if (!device.permits_transfer && !device.approved) {
      throw this.#fail("CAPABILITY_UNVERIFIED", "This capability is not verified for your configuration and was not used.");
    }
    const newlyConnected = !device.identity_authenticated;
    device.identity_authenticated = true;
    device.listed = true;
    device.last_contact_at = "2026-10-11T09:01:00Z";
    const items: DeviceRecording[] = device.recordings.map((r) => ({
      recording_id: r.id,
      display_name: r.name,
      declared_byte_length: r.bytes,
      captured_at: "2026-10-10T08:00:00Z",
      transfer: device.transfers.get(r.id) ?? {
        transfer_id: null,
        state: "discovered",
        state_label: STATE_LABELS.discovered,
        bytes_received: null,
        observed_bytes_per_sec: null,
        eta_seconds_estimate: null,
        verified: false,
        source_label: SOURCE_BLE,
        cloud_route: device.cloud_route,
      },
    }));
    if (newlyConnected) this.#emit("device.connected", { payload: { peripheral_id: id } });
    return {
      peripheral_id: id,
      items,
      file_count: items.length,
      completeness: device.truncated ? "unsynced_delta_truncated" : "unsynced_delta",
      completeness_label: COMPLETENESS_DELTA,
      ...this.#identity(device),
    };
  }

  #import(id: string, ids: string[], _localOnly: boolean): DeviceImport {
    const device = this.#require(id);
    if (!device.permits_transfer) {
      throw this.#fail("CAPABILITY_UNVERIFIED", "This capability is not verified for your configuration and was not used.");
    }
    if (!device.listed) throw this.#fail("INVALID_ARGUMENT", "The request was not valid.");
    const transferIds: string[] = [];
    for (const recordingId of ids) {
      if (!device.recordings.some((r) => r.id === recordingId)) {
        throw this.#fail("NOT_PRESENT_ON_DEVICE", "The device does not list this recording. It may already have been synchronized elsewhere.");
      }
    }
    for (const recordingId of ids) {
      this.setTransfer(id, recordingId, "queued");
      transferIds.push(device.transfers.get(recordingId)!.transfer_id!);
    }
    return { transfer_ids: transferIds };
  }

  #disconnect(id: string, forget: boolean): DeviceDisconnect {
    const device = this.#devices.get(id);
    if (device) {
      for (const [recordingId, transfer] of device.transfers) {
        if (!["imported", "cancelled", "discovered"].includes(transfer.state)) {
          this.setTransfer(id, recordingId, "cancelled", { eta_seconds_estimate: null });
        }
      }
      if (device.identity_authenticated) this.#emit("device.disconnected", { payload: { peripheral_id: id } });
      device.identity_authenticated = false;
      device.listed = false;
      if (forget) device.authorized = false;
    }
    return { peripheral_id: id, local_archive_retained: true, authorization_revoked: forget };
  }

  #sync(id: string, enable: boolean): DeviceSyncPolicy {
    const device = this.#require(id);
    const policy = this.#syncDto(device);
    if (enable && !policy.may_advertise) {
      throw this.#fail("CAPABILITY_UNVERIFIED", "This capability is not verified for your configuration and was not used.");
    }
    device.syncEnabled = enable;
    return this.#syncDto(device);
  }
}
