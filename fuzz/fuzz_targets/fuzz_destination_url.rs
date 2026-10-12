#![no_main]
// D2.4 fuzz target — outbound destination parsing (§7.3, §16.2 SSRF row): scheme, userinfo, IP literals, loopback/private hosts, label redaction.
//
// The body of this target lives in
// `../../crates/security/tests/fuzz_corpus_replay.rs` (module `checks`) so that
// the libFuzzer driver and the stable-toolchain corpus-replay test
// (`cargo test -p plaudon-security --test fuzz_corpus_replay`) execute byte-for-byte
// identical code and assert identical invariants. The `#[cfg(test)]` replay
// harness inside that file is stripped when it is compiled as a module here.
//
// Corpus: `tests/fixtures/fuzz-corpus/destination_url/`.

#[path = "../../crates/security/tests/fuzz_corpus_replay.rs"]
mod replay;

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    replay::checks::check_destination_url(data);
});
