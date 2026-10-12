# Hardware test runner: Windows 11 x86-64

Opt-in real-hardware suite for direct PLAUD device import (spec §18.3A,
§18.6A, §7.9 "Hardware shipping gate", App. B.9). Implemented in
`tests/hardware` (package `plaudon-hardware-tests`). The suite **never runs in
default CI**: every hardware test is `#[ignore]` and also checks an explicit
environment gate.

## What counts as evidence

- A run on this page is evidence for **Windows 11 x86-64 only**. Linux `bluer` peripheral
  bootstrap results are **not accepted as proof of Mac or Windows pairing**
  (§18.3A); a result from another host never promotes a cell for this one.
- A simulator result is never hardware evidence. A record whose `source` is
  `simulator` cannot promote a `compatibility.yaml` cell.
- A cell becomes `supported` only through `EvidenceRecord::promoted`: real
  hardware, every required check `passed`, a verified transfer, a read-only
  opcode trace, an observed non-destructive scan and a network-denied run. A
  missing or failed check keeps the cell at its previous, lower status. Repeat
  the whole setup on a clean machine before changing the register (§7.9).

## Prerequisites

- An x86-64 PC on Windows 11 with a Bluetooth LE adapter, the version named
  in the cell's `os_version_range`, a PLAUD device on the firmware named in
  `firmware_range`, and a Rust toolchain matching `rust-toolchain.toml`
  (MSVC target `x86_64-pc-windows-msvc`).
- **Bluetooth permission.** Settings → Privacy & security → Bluetooth must be
  on, and for a packaged app, *Let desktop apps access Bluetooth* must be
  on. Run `bluetooth_permission` with it off and confirm a clear in-app error
  rather than a silent empty scan.
- Close the PLAUD mobile app and turn off phone Bluetooth, except while
  deliberately running `host_ble_exclusivity_with_mobile_app` and
  `listing_delta_before_after_phone_sync`. Windows allows only one BLE client
  per GATT service in some stacks; record what you observe.
- A scratch volume small enough to fill for `full_disk`, and Windows Defender
  Firewall outbound rules able to block PLAUD and OpenRouter hosts for
  `network_denied`.
- Sleep/wake needs Modern Standby or S3 enabled in the power plan.

## Opting in

The suite is off unless **all** of these are set; only the exact value `1` opts in.

| Variable | Meaning |
| --- | --- |
| `PLAUDON_HARDWARE_TESTS=1` | Explicit opt-in. |
| `PLAUDON_HARDWARE_PROFILE` | Named device profile `<source_type>:<model>:<auth_mode>`, for example `plaud_device_ble:note:legacy_static_token`. It must select a cell of `compatibility.yaml` for this host; an unknown name grants nothing. |
| `PLAUDON_HARDWARE_OBSERVATIONS` | Path of the observations file (below). |
| `PLAUDON_HARDWARE_EVIDENCE_OUT` | Where `emit_evidence_record` writes the evidence JSON. |

```
set PLAUDON_HARDWARE_TESTS=1
set PLAUDON_HARDWARE_PROFILE=plaud_device_ble:note:legacy_static_token
set PLAUDON_HARDWARE_OBSERVATIONS=%USERPROFILE%\plaud-run\observations.json
set PLAUDON_HARDWARE_EVIDENCE_OUT=%USERPROFILE%\plaud-run\evidence.json
cargo test -p plaudon-hardware-tests --test hardware -- --ignored --test-threads=1
```

Without the gate, `cargo test -p plaudon-hardware-tests` lists the hardware tests
as `ignored`; with `-- --ignored` each prints `SKIPPED` and nothing is touched.

## The observations file

The suite does not invent results. You follow the App. B.9 procedure with the
app under test and write down what happened; the suite validates it, derives
what can be derived, and refuses unsafe content.

```json
{
  "observed_firmware": "V0095",
  "emitted_opcodes": ["authenticate", "listDelta", "requestData"],
  "non_destructive_scan_observed": true,
  "network_denied_run": true,
  "transfer": { "device_recording_id": "...", "acquired_mime": "audio/opus",
    "quality_class": "compressed_device_transfer", "received_bytes": 4096,
    "device_declared_byte_length": 4096, "terminal_marker_observed": true,
    "sha256_hex": "<64 hex>", "measured_duration_ms": 15000 },
  "checks": { "discover_only": { "outcome": "passed", "detail": "one device found, nothing connected" } }
}
```

- `observed_firmware` is read from the device, never assumed.
- A check you did not run is simply absent and is recorded `skipped`, which
  blocks promotion. Unknown check ids are rejected so a typo cannot hide a gap.
- `no_device_write_opcodes`, `transfer_verified` and
  `disconnect_during_transfer` are **derived**: from the opcode trace (only the
  read-only allowlist passes; anything else fails closed), from the transfer
  proof (terminal marker, declared length equal to received bytes, SHA-256) and
  from the six `disconnect_at_*` stages. You cannot assert them.
- `imported_bytes_verified` and `network_denied` are downgraded to `failed`
  when no verified transfer or no network-denied run backs them.
- `detail` must contain no credential and no recording content. Anything that
  looks like a secret assignment, a bearer credential, a long opaque token or
  transcript-length text is replaced by a fixed message and the check fails.

## Checks

`bluetooth_permission`, `discover_only`, `authorization_success`,
`authorization_failure`, `device_name_collision`, `missing_secret`,
`encrypted_handshake_detected`, `host_ble_exclusivity_with_mobile_app`,
`two_devices_in_range`, `listing_delta_before_after_phone_sync`,
`transfer_pull_15s`, `transfer_pull_5min`, `transfer_pull_30min`,
`transfer_pull_multi_hour` (where storage permits), `transfer_verified`,
`opus_decode_measurement`, `timestamp_verified_against_trusted_source`,
`disconnect_at_connecting`, `disconnect_at_authenticating`,
`disconnect_at_listing`, `disconnect_at_requesting`,
`disconnect_at_streaming`, `disconnect_at_finalizing`,
`disconnect_during_transfer`, `sleep_wake`, `resume_or_restart`,
`low_battery`, `full_disk`, `imported_bytes_verified`,
`no_device_write_opcodes`, `usb_mount_and_eject`, `network_denied`,
`recovery_after_process_kill`.

The ones needed for promotion are `EvidenceRecord::REQUIRED_CHECKS`; the rest
are archived with the record.

## What the suite does and does not touch

- It reads your observations file and the committed `compatibility.yaml`, and
  writes one evidence JSON at the path you chose.
- It never opens a radio, a USB volume or the vault itself, never sends a
  device opcode, and never deletes, clears, controls or writes to the device.
  The device-side procedure is read-only by construction: the opcode allowlist
  has no write, delete, clear, settings, OTA or sync-acknowledge operation.
- It never reads or stores a device secret. Device secrets stay in the OS
  credential vault.

## Privacy

**No secrets and no recording media leave the test machine by default.** The
evidence record holds metadata, a digest and counts only. Nothing is uploaded;
attaching a record to a release is a deliberate, manual act. Review the JSON
before sharing it.

## Evidence output

`PLAUDON_HARDWARE_EVIDENCE_OUT` receives a pretty-printed JSON document that
satisfies `schemas/device-evidence.v1.schema.json`: `test_run_id`,
`recorded_at`, `source`, `source_type`, `vendor`, `device_model`,
`observed_firmware`, `host_os`, `host_os_build`, `transport`, `auth_mode`,
`provisioning_route`, `transfer`, `non_destructive_scan_observed`,
`emitted_opcodes`, `network_denied_run` and `checks` (`id`, `outcome`
`passed|failed|skipped`, `detail`). `test_run_id` and `recorded_at` are what
`compatibility.yaml` cites as `test_run_id` and `last_verified_at`.
