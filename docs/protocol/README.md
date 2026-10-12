# Protocol documentation

- `plaud-legacy-ble.md` — the observed legacy BLE frame layout, GATT
  identifiers and the read-only opcode allowlist implemented in
  `crates/plaud-protocol`.
- The Swift/Rust bridge ABI is specified in
  `native/apple-bridge/include/plaudon_apple_bridge.h`; its ownership,
  threading and cancellation rules are normative.
