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
