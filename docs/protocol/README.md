# Protocol documentation

- `plaud-legacy-ble.md` — the observed legacy BLE frame layout, GATT
  identifiers and the read-only opcode allowlist implemented in
  `crates/plaud-protocol`.
- `apple-bridge.md` — the Swift/Rust bridge: envelopes, versioning,
  ownership, threading, cancellation, the error vocabulary and the five
  operations. The normative statement of the ABI itself is
  `native/apple-bridge/include/plaudon_apple_bridge.h`; the document shapes
  are constrained by `schemas/apple-bridge-envelope.v1.schema.json`, and
  `crates/ai-apple/tests/abi_snapshot.rs` fails the build when the three
  disagree.
