/**
 * Presentation tables for the Devices workspace (spec §5.2, App. B.8).
 *
 * Only structure lives here: section names, glyphs, tones and number
 * formatting. Every compatibility statement (status label, limitations,
 * disclosures, media label, completeness qualifier, transfer-state label) comes
 * from the DTO, which the Rust side derives from `compatibility.yaml` and the
 * `*_ui_label()` helpers. Nothing in this file is allowed to describe what a
 * BLE source yields or how trustworthy a connection is.
 */
import type {
  DeviceCapabilityName,
  DeviceSection,
  DeviceSupportStatus,
  DeviceTransferState,
} from "../../ipc/contract";
import type { StatusTone } from "../../components/StatusBadge";
import { formatBytes } from "../settings/bytes";

/** The five App. B.8 sections, in display order, with their exact names. */
export const SECTION_ORDER: readonly DeviceSection[] = [
  "nearby",
  "connected",
  "needs_authorization",
  "unsupported_firmware",
  "previously_linked",
];

export const SECTION_LABELS: Record<DeviceSection, string> = {
  nearby: "Nearby",
  connected: "Connected",
  needs_authorization: "Needs authorization",
  unsupported_firmware: "Unsupported firmware",
  previously_linked: "Previously linked",
};

/** Shown for a section with no rows after a scan that actually ran. */
export const SECTION_EMPTY: Record<DeviceSection, string> = {
  nearby: "No PLAUD devices were heard in this scan.",
  connected: "No device is connected.",
  needs_authorization: "No scanned device is waiting for authorization.",
  unsupported_firmware: "No scanned device reported unsupported firmware.",
  previously_linked: "No device has been linked on this computer.",
};

export interface Presentation {
  tone: StatusTone;
  glyph: string;
}

export const SUPPORT_PRESENTATION: Record<DeviceSupportStatus, Presentation> = {
  supported: { tone: "success", glyph: "✓" },
  experimental: { tone: "warning", glyph: "!" },
  not_evaluated: { tone: "neutral", glyph: "?" },
  known_unsupported: { tone: "danger", glyph: "×" },
};

/**
 * `On device`, `Transferred locally` and `Archived` (§4.2A.8) differ in tone,
 * glyph and label so no state is told apart by colour alone.
 */
export const TRANSFER_PRESENTATION: Record<DeviceTransferState, Presentation> = {
  discovered: { tone: "neutral", glyph: "○" },
  queued: { tone: "waiting", glyph: "…" },
  connecting: { tone: "running", glyph: "↻" },
  authenticating: { tone: "running", glyph: "↻" },
  requesting: { tone: "running", glyph: "↻" },
  receiving: { tone: "running", glyph: "↓" },
  verifying: { tone: "running", glyph: "≟" },
  committing: { tone: "waiting", glyph: "◐" },
  imported: { tone: "success", glyph: "●" },
  interrupted: { tone: "warning", glyph: "!" },
  waiting_for_device: { tone: "waiting", glyph: "…" },
  paused: { tone: "waiting", glyph: "‖" },
  cancelled: { tone: "neutral", glyph: "×" },
  unsupported: { tone: "danger", glyph: "×" },
};

/** States in which a transfer can still be cancelled. */
export const CANCELLABLE_STATES: readonly DeviceTransferState[] = [
  "queued",
  "connecting",
  "authenticating",
  "requesting",
  "receiving",
  "verifying",
  "committing",
  "waiting_for_device",
  "paused",
];

export const CAPABILITY_LABELS: Record<DeviceCapabilityName, string> = {
  read_only_listing: "Read-only listing of recordings",
  read_only_transfer: "Read-only transfer",
  automatic_sync: "Automatic transfer when nearby",
};

export const TRANSPORT_LABELS: Record<string, string> = {
  ble: "Bluetooth LE (BLE)",
};

export function transportLabel(transport: string): string {
  return TRANSPORT_LABELS[transport] ?? transport;
}

/** An unknown stays unknown: never `0 dBm`, never a guess. */
export function formatSignal(dbm: number | null): string {
  return dbm === null ? "Unknown" : `${dbm} dBm`;
}

export function formatBattery(percent: number | null): string {
  return percent === null ? "Unknown" : `${percent}%`;
}

export function formatBandwidth(bytesPerSecond: number | null): string {
  return bytesPerSecond === null ? "Not measured yet" : `${formatBytes(bytesPerSecond)}/s`;
}

/** Whole-unit, rounded-up estimate text; callers label it as an estimate. */
export function formatEta(seconds: number | null): string {
  if (seconds === null) return "Cannot be estimated yet";
  if (seconds < 60) return `about ${Math.max(1, Math.round(seconds))} s`;
  if (seconds < 3600) return `about ${Math.round(seconds / 60)} min`;
  return `about ${Math.round(seconds / 360) / 10} h`;
}

export function deviceName(advertisedName: string | null): string {
  return advertisedName && advertisedName.trim().length > 0 ? advertisedName : "Unnamed device";
}
