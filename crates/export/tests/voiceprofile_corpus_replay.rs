//! VP-185 — `archive-voiceprofile/1` parsing invariants, plus the stable
//! corpus replay that enforces them on every PR.
//!
//! # How this file is used twice
//!
//! It follows the house pattern established by
//! `crates/security/tests/fuzz_corpus_replay.rs`, with the dependency pointed
//! where it belongs: the document type lives in `plaudon-export`, so the shared
//! checks live beside that crate's tests.
//!
//! 1. `cargo test -p plaudon-export --test voiceprofile_corpus_replay` — the
//!    `#[cfg(test)] mod replay` harness feeds every file in
//!    `tests/fixtures/fuzz-corpus/voiceprofile_archive_doc/` through
//!    [`checks::check_voiceprofile_archive_doc`] and asserts the invariants.
//!    Runs on stable, without cargo-fuzz.
//! 2. `fuzz/fuzz_targets/fuzz_voiceprofile_archive_doc.rs` pulls this file in
//!    with `#[path = …] mod replay;` and calls the same function, so the
//!    fuzzer and the replay test execute byte-for-byte identical code.
//!
//! Consequences of the arrangement, respected throughout: [`checks`] uses only
//! `plaudon-export`'s public API, `serde_json` and `std`, and this file carries
//! no inner attribute other than doc comments.
//!
//! # The invariants
//!
//! The parser is the restore path's first contact with an untrusted archive, so
//! what it must guarantee is: never panic, never accept an unknown field
//! (`deny_unknown_fields` is how a newer build's extra column fails loudly
//! instead of being silently dropped), and never let a parsed document claim a
//! namespace or retention term the schema forbids. A violated invariant is a
//! `panic!`, which is the signal both drivers look for.

/// Invariant checks, shared verbatim between the libFuzzer target and the
/// corpus replay test.
#[allow(dead_code)]
pub mod checks {
    use plaudon_export::archive::voiceprofiles::{
        embedding_shape, in_namespace, EmbeddingShape, VoiceProfileDocument, BLOB_RUN_CHARS,
        NAMESPACE, VOICEPROFILE_SCHEMA,
    };

    /// Upper bound on the input fed to the parser.
    pub const MAX_INPUT_BYTES: usize = 1 << 20;

    /// Parse one candidate `archive-voiceprofile/1` document.
    pub fn check_voiceprofile_archive_doc(data: &[u8]) {
        if data.len() > MAX_INPUT_BYTES {
            return;
        }
        let Ok(document) = serde_json::from_slice::<VoiceProfileDocument>(data) else {
            // A refusal is a correct outcome for arbitrary bytes. What matters
            // is that it is a refusal and not a panic.
            return;
        };
        if document.validate().is_err() {
            // Shape-valid, value-invalid: the restorer refuses it, and so do we.
            return;
        }

        // A validated document is one this build would restore, so every field
        // it carries has to satisfy what the schema promises.
        assert_eq!(
            document.schema_version, VOICEPROFILE_SCHEMA,
            "a validated document names this schema"
        );
        assert!(
            !document.person_id.is_empty(),
            "an enrolment without a person is not restorable"
        );
        let path = VoiceProfileDocument::entry_path(&document.person_id);
        assert!(
            in_namespace(&path),
            "a voice document's path is inside {NAMESPACE}: {path}"
        );

        let enrollment = &document.enrollment;
        assert!(
            (1..=12).contains(&enrollment.retention_months),
            "retention is 1..=12 months (VP-130), got {}",
            enrollment.retention_months
        );
        assert!(
            enrollment.status == "active" || enrollment.status == "expired",
            "unknown enrolment status {}",
            enrollment.status
        );

        for exemplar in &document.exemplars {
            assert!(!exemplar.id.is_empty(), "an exemplar carries an identity");
            assert!(
                exemplar.embedding_dim >= 1 && exemplar.embedding_dim <= 8192,
                "dimensionality outside what the schema accepts: {}",
                exemplar.embedding_dim
            );
            if let (Some(start), Some(end)) = (exemplar.source_start_ms, exemplar.source_end_ms) {
                assert!(end >= start, "a source span cannot end before it starts");
            }
            // Whatever the ciphertext looks like, a blob of that length must be
            // recognised as biometric-shaped: that rule is what keeps such a
            // value out of every other namespace.
            if exemplar.embedding_cipher_b64.len() >= BLOB_RUN_CHARS
                && exemplar.embedding_cipher_b64.bytes().all(|byte| {
                    byte.is_ascii_alphanumeric() || byte == b'+' || byte == b'/' || byte == b'='
                })
            {
                assert_eq!(
                    embedding_shape(Some("note"), &exemplar.embedding_cipher_b64),
                    Some(EmbeddingShape::BinaryBlob),
                    "a sealed blob must be recognised as biometric-shaped"
                );
            }
        }

        // Re-serializing a parsed document and parsing it again is stable: the
        // restore path and the backup path agree about the wire form.
        let round_trip = serde_json::to_vec(&document).expect("a parsed document re-serializes");
        let again: VoiceProfileDocument =
            serde_json::from_slice(&round_trip).expect("a written document parses");
        assert_eq!(again, document, "round trip changed the document");
    }
}

#[cfg(test)]
mod replay {
    use std::path::PathBuf;

    fn corpus_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .expect("repository root")
            .join("tests/fixtures/fuzz-corpus/voiceprofile_archive_doc")
    }

    #[test]
    fn every_committed_corpus_entry_satisfies_the_parser_invariants() {
        let dir = corpus_dir();
        let entries: Vec<PathBuf> = std::fs::read_dir(&dir)
            .unwrap_or_else(|error| panic!("read {}: {error}", dir.display()))
            .map(|entry| entry.expect("corpus entry").path())
            .filter(|path| path.is_file())
            .collect();
        assert!(
            entries.len() >= 8,
            "the committed corpus is the regression suite; it must not shrink"
        );
        for path in entries {
            let bytes = std::fs::read(&path).expect("read corpus entry");
            super::checks::check_voiceprofile_archive_doc(&bytes);
        }
    }

    #[test]
    fn a_valid_document_parses_and_an_unknown_field_does_not() {
        let valid = std::fs::read(corpus_dir().join("valid_minimal.json")).unwrap();
        assert!(
            serde_json::from_slice::<plaudon_export::archive::voiceprofiles::VoiceProfileDocument>(
                &valid
            )
            .is_ok(),
            "the corpus must contain at least one document this build accepts"
        );
        let unknown = std::fs::read(corpus_dir().join("unknown_field.json")).unwrap();
        assert!(
            serde_json::from_slice::<plaudon_export::archive::voiceprofiles::VoiceProfileDocument>(
                &unknown
            )
            .is_err(),
            "deny_unknown_fields is what stops a newer build's column travelling silently"
        );
    }
}
