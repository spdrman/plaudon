<!-- GENERATED FILE: run `plaudon-release compat-matrix` to regenerate. -->
<!-- Source of truth: compatibility.yaml (spec 2.5B). Do not edit by hand. -->

# Compatibility matrix

Register schema version 1, last updated 2026-10-12.

Every cell uses the spec 1.6 evidence vocabulary: `Documented`, `Observed in upstream code`, `Validated`, `Supported`, `Experimental`, `Unsupported`. A `Supported` cell exists only when an archived evidence record on identified hardware backs it; otherwise the honest term is used instead. A route we have not evaluated on identified hardware is published as `Unsupported`: the product fails closed rather than inviting a user to rely on an unproven path.

## PLAUD Note / plaud_device_ble

| Host OS | OS versions | Firmware | Auth mode | Transport | Media | Claim | Evidence |
|---|---|---|---|---|---|---|---|
| macos | 26.0, 27.0 | V0095 | legacy_static_token | ble | opus (compressed_device_transfer) | **Unsupported** | none archived |
| macos | 26.0, 27.0 | encrypted-mode | encrypted_handshake | ble | opus (compressed_device_transfer) | **Unsupported** | none archived |
| windows | 11 | V0095 | legacy_static_token | ble | opus (compressed_device_transfer) | **Unsupported** | none archived |

Known limitations:

- Frame codec and read-only opcode allowlist implemented and unit tested; never exercised against real hardware by this build.
- Upstream plaude-cli evidence is Linux/BlueZ-first; macOS operation with real hardware is not confirmed (spec 2.5A).
- The BLE listing is a delta of unsynchronized files; recordings already synced to a phone may be absent.
- Legacy mode exchanges a static token over an unencrypted link.
- Yields a compressed .ASR Opus sidecar, not the original stereo WAV.
- The per-entry listing and data-request payload layouts are this build's bounded definition, not a published or observed vendor layout; a real device may differ (docs/protocol/plaud-legacy-ble.md).
- The playable artefact is an Ogg Opus remux of the same coded packets, a derived conversion; this build bundles no Opus decoder, so no decoded WAV is produced (A1.4).
- Offset reads are undemonstrated, so an interrupted transfer always restarts into fresh staging rather than resuming.
- Automatic transfer when nearby is unavailable until a non-destructive audit passes for this profile.
- RSA/ChaCha20-Poly1305 device mode is identified in reverse-engineering notes but implemented by no codec we have (spec 2.5A); we fail closed rather than downgrading.
- Never exercised against real hardware by this build.
- A cross-platform BLE crate proves central discovery exists; it does not prove PLAUD protocol success (spec 2.5B).
- The per-entry listing and data-request payload layouts are this build's bounded definition, not a published or observed vendor layout.
- The playable artefact is an Ogg Opus remux, a derived conversion; this build bundles no Opus decoder, so no decoded WAV is produced (A1.4).

## PLAUD Note / plaud_device_usb

| Host OS | OS versions | Firmware | Auth mode | Transport | Media | Claim | Evidence |
|---|---|---|---|---|---|---|---|
| macos | 26.0, 27.0 | V0095 | none | usb_mass_storage | pcm_s16le (original_device_recording) | **Unsupported** | none archived |

Known limitations:

- Requires the device's own 'Access via USB' setting; we never toggle it.
- macOS mounts removable volumes read-write by default and writes .Spotlight-V100/.fseventsd/.DS_Store, so a zero-physical-writes claim is not available (spec 7.8).
- Our code issues no writes; host writes are the operating system's.

## PLAUD Note / plaud_device_wifi

| Host OS | OS versions | Firmware | Auth mode | Transport | Media | Claim | Evidence |
|---|---|---|---|---|---|---|---|
| macos | 26.0, 27.0 | any | unknown | wifi | unknown (unknown) | **Unsupported** | none archived |

Known limitations:

- The upstream Wi-Fi transport is a stub; advertisement, session authorization, framing and integrity are all unproven (spec 7.8).

## PLAUD Note Pro / plaud_device_usb

| Host OS | OS versions | Firmware | Auth mode | Transport | Media | Claim | Evidence |
|---|---|---|---|---|---|---|---|
| macos | 26.0, 27.0 | any | none | usb_mass_storage | unknown (unknown) | **Unsupported** | none archived |

Known limitations:

- Vendor documents USB as charging only for Note Pro; the application must not instruct a user to mount it (spec 7.8).

## PLAUD NotePin S / plaud_device_usb

| Host OS | OS versions | Firmware | Auth mode | Transport | Media | Claim | Evidence |
|---|---|---|---|---|---|---|---|
| macos | 26.0, 27.0 | any | none | usb_mass_storage | unknown (unknown) | **Unsupported** | none archived |

Known limitations:

- Vendor documents no USB recording access for NotePin S (spec App. B.2).

## local_file

| Host OS | OS versions | Firmware | Auth mode | Transport | Media | Claim | Evidence |
|---|---|---|---|---|---|---|---|
| macos | 26.0, 27.0 | n/a | none | filesystem | multi (user_supplied_file) | **Supported** | run-local-import-001 |

Known limitations:

- Opus-in-Ogg and raw .ASR sidecars have no decoder in the current Symphonia feature set; they import as bytes and report UNSUPPORTED_CODEC on decode (A1.4).

## plaud_cloud_cli

| Host OS | OS versions | Firmware | Auth mode | Transport | Media | Claim | Evidence |
|---|---|---|---|---|---|---|---|
| macos | 27.0 | n/a | browser_login | https | unknown (vendor_cloud_export) | **Experimental** | none archived |

Known limitations:

- Exactly one release has been exercised: @plaud-ai/cli 0.3.15 on macOS 27.0 (arm64). The committed table crates/source-plaud-cli/cli-compatibility.yaml holds that single row and matches versions exactly, so every other release fails closed as CLI_UNSUPPORTED (ADR-0050).
- The tool offers no JSON or other machine-readable mode, so the listing is scraped from its human-readable table under a version lock, and the scraping compatibility warning stays on.
- Listing dates are day-precision with no timezone, so no capture time is recorded for an imported recording; the listing also carries no byte length and no transcript column, and names are truncated by the vendor at 35 characters.
- Vendor session revocation (plaud logout) has never been exercised against a real account by this build.
- Vendor transcripts and summaries are unverified: this build fetches and imports no vendor artifacts.
- Windows and Linux hosts are unverified; only macOS 27.0 was exercised.
- Resume of an interrupted download is not implemented; an interrupted transfer restarts from zero, even though a Range request was observed to answer 206 with a strong validator.
- Rate-limit and session-expiry behaviour was never provoked, so those vendor error codes are unverified.
- The npm package listing declares no license; we neither bundle nor redistribute it.
- Whether a Google or Apple PLAUD login works with the CLI is unverified; no token-paste workaround will be offered.

