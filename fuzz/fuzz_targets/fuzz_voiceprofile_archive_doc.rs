#![no_main]
// VP-185 fuzz target — the `archive-voiceprofile/1` document against the
// restorer's parser and validator (VOICE_PROFILES_SPEC §17.3, §7.4).
//
// The body of this target lives in
// `../../crates/export/tests/voiceprofile_corpus_replay.rs` (module `checks`)
// so that the libFuzzer driver and the stable-toolchain corpus-replay test
// (`cargo test -p plaudon-export --test voiceprofile_corpus_replay`) execute
// byte-for-byte identical code and assert identical invariants. The
// `#[cfg(test)]` replay harness inside that file is stripped when it is
// compiled as a module here.
//
// Corpus: `tests/fixtures/fuzz-corpus/voiceprofile_archive_doc/`.

#[path = "../../crates/export/tests/voiceprofile_corpus_replay.rs"]
mod replay;

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    replay::checks::check_voiceprofile_archive_doc(data);
});
