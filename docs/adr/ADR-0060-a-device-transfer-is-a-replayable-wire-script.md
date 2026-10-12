# ADR-0060: A device transfer is a replayable wire script, and a derivative is never an original

- **Status:** Accepted
- **Date:** 2026-10-11
- **Spec:** §7.7, §7.9, §17.3A, §18.2A, §18.3A, App. B.3, App. B.4, App. B.6

## Context

We have no PLAUD hardware attached to this build, and the only source for the legacy BLE protocol is a Linux-first reverse-engineering project. Two problems follow. First, every hostile case the specification requires us to handle — a missing terminal marker, a forged length, a conflicting duplicate, a mid-transfer disconnect, an encrypted-mode offer — has to be exercised without a radio, repeatedly, on three operating systems. Second, the bytes a legacy device yields are a compressed `.ASR` Opus sidecar, and this build bundles no Opus decoder (spike A1.4), so the obvious "decode to WAV" step is not available to us.

## Decision

A device session is driven by a single-owner actor over a `GattTransport` whose only write method takes a `ControlFrame`, which in turn can only be built from the three-variant `ReadOnlyOpcode` allowlist. The simulator is a second implementation of that same transport: it replays a versioned wire script (`tests/fixtures/device/<profile>/*.json`) keyed by protocol profile, device model and firmware. Hostile cases are scripts, not code paths, so each one is a committed artefact that a reader can inspect and a golden test keeps honest.

Acquired bytes are stored exactly as received, with `quality_class = compressed_device_transfer`. The playable artefact we produce is an Ogg Opus **remux**: the coded packets are copied unchanged into a standard container, which is a container change rather than a re-encode. Decoded PCM is reported as `Unavailable { UnsupportedCodec }` with an explanation, not synthesized. Verification requires the terminal marker, a contiguous range, a device-declared length that agrees with the byte count, and a sidecar whose own Opus framing parses; our SHA-256 is recorded but is never the thing that makes a transfer complete.

## Rejected alternatives

- **Mock the device at the `DirectDevice` trait level instead of the wire.** It would exercise our orchestration and skip the codec, the framing, the bounds and the reassembly, which is where every §18.6A hostile case actually lives.
- **Generate hostile cases only in code.** A script committed as a fixture is reviewable and is versioned by firmware; a generated case is invisible once the test passes. We kept a builder for the families that need one case per chunk boundary and committed the rest.
- **Write a WAV derivative by resampling silence or by shelling out to an external decoder.** The first fabricates audio; the second adds an unvetted runtime dependency and a packaging problem the spec deliberately defers.
- **Call the remuxed Ogg Opus file the original recording.** Spec §7.9 and App. B.9 forbid it, and the fidelity classes exist precisely so the interface cannot.
- **Buffer a recording in memory and hash it at the end.** App. B.4 requires bounded buffers; the streaming assembler holds only out-of-order chunks, up to a capped window.

## Consequences

- Every §18.6A BLE authentication and transfer case runs in CI in under a second, with no radio and no network.
- A fixture is evidence of our behaviour, never of a device's: `EvidenceSource::Simulator` cannot promote a compatibility cell, and the suite asserts that.
- A user on a legacy profile gets the exact transferred bytes plus a playable conversion, and is told plainly that no decoded WAV is available in this build.
- If a real device's listing layout differs from the bounded layout we implemented, the fixtures and the decoder change together, and the affected compatibility cells stay `not_evaluated` until hardware says otherwise.
