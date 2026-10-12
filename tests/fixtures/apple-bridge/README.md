# Apple bridge fuzz seed corpus

Inputs for two suites that must agree on what "hostile" means:

- `crates/ai-apple/tests/envelope_fuzz.rs` feeds every file to the Rust
  decoder and asserts that none of them produces a capability claim.
- `native/apple-bridge/tests/abi_stress.c` feeds every file to all five C
  entry points under AddressSanitizer and UndefinedBehaviorSanitizer, and
  asserts that each one answers with a document rather than crashing.

Add a file here whenever a new shape breaks something. Names are descriptive
so a failure says what it was.
