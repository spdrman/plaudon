#![no_main]
// O1.6 fuzz target — Opus packet decoding (§16.2 "Malicious audio decoder input", §18.5, ADR-0015): forged framing, truncated frames, hostile channel counts.
//
// The body of this target lives in
// `../../crates/security/tests/fuzz_corpus_replay.rs` (module `checks`) so that
// the libFuzzer driver and the stable-toolchain corpus-replay test
// (`cargo test -p plaudon-security --test fuzz_corpus_replay`) execute byte-for-byte
// identical code and assert identical invariants. The `#[cfg(test)]` replay
// harness inside that file is stripped when it is compiled as a module here.
//
// Corpus: `tests/fixtures/fuzz-corpus/opus_packet/`.

#[path = "../../crates/security/tests/fuzz_corpus_replay.rs"]
mod replay;

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    replay::checks::check_opus_packet(data);
});
