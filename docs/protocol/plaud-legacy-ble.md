# PLAUD legacy BLE profile

Scope: original PLAUD Note, firmware `V0095`-like legacy BLE. Source of the
protocol facts below: spec App. B.3, derived from the MIT-licensed
`Sumatoshi-tech/plaude-cli` repository. We implement decode and **read-only**
encode; see ADR-0009 for the provenance decision.

## GATT

| Identifier | Role |
|---|---|
| `0x1910` | Vendor service |
| `0x2BB1` | Control characteristic, host writes here |
| `0x2BB0` | Notification characteristic, device notifies here |
| `0x2A19` | Standard battery level |

Characteristics are always located by UUID. A fixed ATT handle is never used:
handles are per connection and differ between hosts, so caching one risks
writing to the wrong characteristic. `gatt::is_writable` returns true for
exactly one characteristic.

## Framing

```text
0x01 control:  | 0x01 | opcode u16 BE | length u16 BE | payload |
0x02 bulk:     | 0x02 | offset u32 BE | length u16 BE | payload |
```

`offset == 0xFFFFFFFF` is the end-of-transfer sentinel and carries no payload.

A declared length is a **claim**. The decoder caps a frame payload at 4 KiB and
refuses any frame whose declared length exceeds the bytes actually present, so
a forged length cannot drive allocation. Property tests decode arbitrary byte
strings and assert the decoder never panics and never allocates more than its
input.

## The acquisition allowlist

`ReadOnlyOpcode` is a closed enum with three variants and no `Other(u16)`:

| Opcode | Operation |
|---|---|
| `0x0001` | Authenticate |
| `0x001A` | Request the unsynchronized-file delta list |
| `0x001C` | Request a byte range of one recording |

A test enumerates the entire `u16` space and asserts that exactly those three
values decode. Every destructive opcode named in upstream notes —
`deleteFile`, `clearAllFile`, recording control, settings write, device
privacy toggle, OTA begin and data, sync acknowledgement, factory reset — is
rejected by `ForbiddenOpcode`, and none can be **constructed** at all, because
the enum is the only way to build a control frame (spec §7.9).

## Transfer completeness

Our own SHA-256 is not evidence of completeness. A transfer is `Complete` only
when all three hold:

1. the end-of-transfer sentinel was observed;
2. the received byte range is contiguous from zero;
3. a **device-declared total length** matches the bytes received.

Without a declared length the outcome is `CompleteLengthUnverified`, and
`finish()` returns nothing, so the bytes cannot be committed as an asset. A
truncated tail is otherwise indistinguishable from a complete file.

The assembler also tolerates what a radio link really does — out-of-order and
duplicate chunks — while refusing what it should not: a conflicting duplicate
at the same offset, a chunk after the terminal marker, a byte budget overrun,
an offset that overflows the address space, and a run of 64 frames that adds no
new bytes.

## Payload layouts: whose definition these are

Spec App. B.3 publishes the framing, the GATT identifiers and the three
opcodes. It does **not** publish a per-entry listing layout, and it warns that
upstream carries "fallback heuristics around response layouts" we must replace
with "a bounded, explicitly tested strategy".

The layouts in `payload.rs` are therefore **this build's** definition for the
named profile:

```text
authenticate  request : | token bytes (16 or 32, profile-constrained) |
authenticate  response: | status u8 | detail u8 |
list delta    request : | (empty) |
list delta    response: | count u16 BE | entry * count | truncated u8 |
  entry               : | id_len u8 | id | name_len u8 | name
                        | size u32 BE | captured_at u32 BE | codec u8 |
request data  request : | id_len u8 | id | offset u32 BE | length u32 BE |
```

They are exercised against our own fixtures and simulator, not against
hardware, which is exactly why every `compatibility.yaml` cell that would use
them is `not_evaluated`. What they do provide is a total parser: a declared
entry count is checked against the profile bound before anything is reserved,
a payload must end exactly after its last field, an undefined enumerant is an
error rather than a default, and no field is length-prefixed without a cap.

The listing itself is carried as bulk frames terminated by the sentinel, so
one bounded reassembly path serves both listings and transfers.

## Named profiles

Nothing reads a bare constant at a call site. `ProtocolProfile` holds the GATT
identifiers, the 80-byte application chunk, the terminal sentinel, the token
lengths, the transfer ceiling, the listing bound, whether offset reads were
demonstrated on hardware (they were not) and whether the listing is a delta.
`profile_for(model, firmware)` matches both components exactly; an unlisted
firmware resolves to `None`, which is how a constant observed on one device
cannot reach another.

## Streaming to disk

`StreamingAssembler` holds only what the radio forces it to: chunks that
arrived ahead of the write cursor, capped at a 256 KiB reorder window.
Anything contiguous is handed back immediately, written to staging, hashed and
then forgotten, so a multi-hour recording never sits in memory. A device that
never sends the missing chunk exhausts the window and fails, rather than
growing without bound.

## The `.ASR` sidecar

`asr.rs` validates every Opus packet's table-of-contents byte and frame
packing and measures the encoded duration from the frame descriptors. That is
a measurement of the bitstream, not a decode.

`remux_to_ogg_opus` rewraps the same coded packets in a standard Ogg Opus
container. A remux is a **derived conversion**: bit-exact in the coded domain,
recorded as `QualityClass::LocalDerivative`, and never described as an
original WAV. This build bundles no Opus decoder (spike A1.4), so a decoded
WAV derivative reports `Unavailable { UnsupportedCodec }` with an explanation
instead of being fabricated.

## The device actor

`plaud-ble` owns one `GattTransport` per device inside a single-owner actor.
Callers hold a `DeviceHandle`; the command channel has depth one and the actor
sets a busy flag, so a second concurrent command is refused with
`DEVICE_BUSY` rather than interleaved onto the same GATT connection. Frames
are handed to a `FrameSink` as they arrive — `StagingAcquisition` implements
it — so bytes go from radio to staging without an intermediate buffer.
Progress carries verified received bytes, and an ETA appears only when a
measured rate and a device-declared length both exist.

## Fixtures and the simulator

`tests/fixtures/device/plaud-note-legacy-v0095/*.json` are versioned wire
scripts: the exact notification frames the device sends for each allowlisted
request, plus what it does afterwards (continue, disconnect, report busy,
report low power, fall silent). `FakeDevice` replays them; there is no
randomness and no clock dependency, so a scenario produces identical bytes on
every host. Each fixture names its profile, its model and its firmware, and
declares itself synthetic with a provenance line — spec §18.2A prohibits
shipping captured credentials or audio as test assets, and a test enforces it.

A simulator result is never compatibility evidence. `EvidenceSource::Simulator`
cannot promote a cell, and the suite asserts that too.

## Session lifecycle

```text
Discovered → Probing → NeedsProvisioning → Authenticated → Enumerating →
Available → Transferring → Verifying → DurableCommit → Imported
      ↘ UnsupportedProfile / UnsupportedAuth / PermissionDenied
      ↘ Unavailable / Busy / Disconnected / Interrupted / IntegrityFailed
```

`UnsupportedProfile`, `UnsupportedAuth`, `PermissionDenied` and
`IntegrityFailed` absorb every later event except a fresh discovery, so a
device that failed a gate cannot be talked into a transfer. Reads are
permitted only in `Available` and `Transferring`.

## What is not implemented, deliberately

- **Token bootstrap.** Upstream emulates a BLE peripheral through Linux
  BlueZ. That is not a macOS or Windows onboarding flow and we do not ship it.
  An existing token may be imported where lawfully obtained.
- **Encrypted-mode firmware.** RSA/ChaCha20-Poly1305 device mode is identified
  in reverse-engineering notes and implemented by no codec we have. We fail
  closed with `UnsupportedAuth`; we never downgrade to the legacy handshake.
- **Resume.** Offset reads are used only after hardware demonstrates correct
  repeated reads and offset semantics.
- **Automatic nearby sync.** Disabled unless repeated on-device audits prove
  file lists and cursors are unchanged.

## Security notes

Legacy mode exchanges a static token over an **unencrypted** radio link. We
warn near untrusted receivers and do not claim that offline ingestion is
encrypted. Raw GATT payloads are never logged in production because they can
contain credentials or audio.
