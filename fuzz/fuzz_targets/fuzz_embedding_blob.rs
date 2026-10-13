#![no_main]
// VP-112 / VP-185 fuzz target — no record handed to the redactor comes out
// still carrying a speaker embedding, and the §6.3A response guard refuses an
// embedding-bearing document (VOICE_PROFILES_SPEC VP-112, threat-model TM-044).
//
// The body of this target lives in
// `../../crates/security/tests/fuzz_corpus_replay.rs` (module `checks`) so that
// the libFuzzer driver and the stable-toolchain corpus-replay test
// (`cargo test -p plaudon-security --test fuzz_corpus_replay`) execute byte-for-byte
// identical code and assert identical invariants. The `#[cfg(test)]` replay
// harness inside that file is stripped when it is compiled as a module here.
//
// Corpus: `tests/fixtures/fuzz-corpus/embedding_blob/`.

#[path = "../../crates/security/tests/fuzz_corpus_replay.rs"]
mod replay;

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    replay::checks::check_embedding_blob(data);
});
