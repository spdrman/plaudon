# Opt-in hardware tests

Everything under this directory requires physical hardware, or a real
platform runtime in a particular state, and is **never** run in CI
(spec 18.3A). A synthetic fixture cannot produce a `Supported` compatibility
cell, and a Linux `bluer` result is not accepted as proof of macOS or Windows
operation.

This is a cargo package (`plaudon-hardware-tests`). Every suite is gated on
an environment variable and prints the variable it wants when it declines, so
a CI log shows the suite was present rather than forgotten.

## Apple native AI (spec 11.5, task H2.4)

```sh
PLAUDON_APPLE_HARDWARE_TESTS=1 \
PLAUDON_APPLE_AUDIO_FIXTURE=/path/to/speech-16k.wav \
PLAUDON_TEST_RUN_ID=apple-$(date -u +%Y%m%dT%H%M%SZ) \
  cargo test -p plaudon-hardware-tests --features apple-bridge \
    --test apple_compatibility -- --nocapture
```

Each run classifies the host into one of the three Mac classes spec 11.5
names, measures what that class can show, and writes an evidence record into
`target/hardware-evidence/`. A machine belongs to exactly one class, so
filling the matrix means running the suite on three machines:

| Class | What it must show |
|---|---|
| `apple_silicon_ready` | Apple speech produced a finalized transcript with zero network requests |
| `apple_silicon_unready` | Apple AI unavailable, transcription completed through whisper.cpp |
| `intel_or_unsupported` | the same, on an ineligible Mac |

`CompatibilityMatrix::gate_failures` refuses a `Supported` label without a
real-hardware record, and refuses the release gate while either degradation
class lacks a record of the local fallback completing. Current results are in
`docs/benchmarks/apple-native-ai.md`.

## PLAUD device cells

To contribute a device compatibility cell:

1. Run the opt-in scanner and record its output verbatim:
   `cargo run -p plaudon-plaud-ble --features hardware-scan --bin plaud-ble-scan -- 8`
2. Record device model, observed firmware fingerprint, host OS name and build,
   transport, authentication/provisioning route, audio format, transferred
   byte count, terminal-marker observation and digest.
3. Add the row to `compatibility.yaml` with `evidence_level:
   observed_on_identified_hardware` and a `test_run_id` that resolves to the
   archived record. `CompatibilityCell::validate` rejects the row otherwise.
4. Repeat the whole setup on a clean machine before promoting a cell to
   `status: supported` (spec 7.9 hardware shipping gate).

The opt-in suite and evidence emitter live in this package
(`plaudon-hardware-tests`); runner instructions are in
`docs/hardware/macos-apple-silicon.md` and `docs/hardware/windows-11-x86-64.md`.
