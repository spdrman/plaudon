# Opt-in hardware tests

Everything under this directory requires physical hardware and is **never**
run in CI (spec 18.3A). A synthetic fixture cannot produce a `Supported`
compatibility cell, and a Linux `bluer` result is not accepted as proof of
macOS or Windows operation.

To contribute a compatibility cell:

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
