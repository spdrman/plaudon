//! The encrypted exemplar store, end to end against a real migrated library
//! (`VOICE_PROFILES_SPEC` VP-071, VP-095; ADR-0014; threat-model row TM-045).
//!
//! The locked-store tests are the important ones: they prove that an
//! unavailable OS credential store locks the **biometric tier only**, and that
//! the name-only roster keeps working beside it (spec §12.2A).

use std::sync::Arc;

use plaudon_app_core::voice_profiles::{
    EmbeddingCipher, ExemplarBinding, SealedEmbedding, VoiceProfileError, EMBEDDING_NONCE_BYTES,
};
use plaudon_domain::ids::{PersonId, RecordingId, SpeakerId};
use plaudon_domain::time::Timestamp;
use plaudon_persistence::db::Database;
use plaudon_persistence::migrations::MigrationRunner;
use plaudon_persistence::people::{PeopleRepository, PersonDraft};
use plaudon_persistence::voice_profiles::{
    EnrollmentDraft, ExemplarInsert, VoiceEnrollmentRepository,
};
use plaudon_security::secrets::{
    InMemorySecretStore, SecretScope, SecretStore, UnsupportedSecretStore,
};

fn embedding(dim: usize) -> Vec<f32> {
    (0..dim)
        .map(|index| (index as f32).mul_add(0.001, -0.5))
        .collect()
}

fn binding(person: PersonId, dim: u32) -> ExemplarBinding {
    ExemplarBinding {
        person_id: person,
        model_id: "wespeaker-resnet34-lm".to_owned(),
        model_version: "1.0".to_owned(),
        embedding_dim: dim,
    }
}

fn in_memory() -> (Arc<InMemorySecretStore>, EmbeddingCipher) {
    let store = Arc::new(InMemorySecretStore::new());
    let cipher = EmbeddingCipher::new(store.clone() as Arc<dyn SecretStore>);
    (store, cipher)
}

#[test]
fn an_embedding_round_trips_through_the_sealed_form() {
    let (store, cipher) = in_memory();
    let person = PersonId::new();
    let vector = embedding(192);
    let binding = binding(person, 192);

    let sealed = cipher.seal(&vector, &binding).unwrap();
    assert_eq!(sealed.nonce.len(), EMBEDDING_NONCE_BYTES);
    assert_eq!(sealed.embedding_dim, 192);
    assert!(
        sealed.cipher.len() >= vector.len() * 4 + 16,
        "ciphertext carries the vector plus a Poly1305 tag"
    );

    let opened = cipher.open(&sealed, &binding).unwrap();
    assert_eq!(opened.as_slice(), vector.as_slice());

    // The key was created on first use, under its own scope, and nowhere else.
    assert_eq!(store.entry_count(), 1);
    assert!(store.status(&SecretScope::VoiceProfileKey).unwrap().present);
    assert!(cipher.check_available().unwrap());
}

#[test]
fn the_ciphertext_never_contains_the_plaintext_vector() {
    let (_store, cipher) = in_memory();
    let person = PersonId::new();
    let vector = embedding(192);
    let sealed = cipher.seal(&vector, &binding(person, 192)).unwrap();

    let plaintext: Vec<u8> = vector.iter().flat_map(|v| v.to_le_bytes()).collect();
    assert!(
        !sealed
            .cipher
            .windows(plaintext.len().min(sealed.cipher.len()))
            .any(|window| window == plaintext.as_slice()),
        "the stored blob must not contain the vector"
    );
    // Nor any single value of it, at any offset.
    for value in &vector {
        let bytes = value.to_le_bytes();
        assert!(
            !sealed.cipher.windows(4).any(|window| window == bytes),
            "a raw f32 of the embedding appears in the ciphertext"
        );
    }
}

#[test]
fn two_seals_of_the_same_vector_differ() {
    let (_store, cipher) = in_memory();
    let person = PersonId::new();
    let vector = embedding(64);
    let first = cipher.seal(&vector, &binding(person, 64)).unwrap();
    let second = cipher.seal(&vector, &binding(person, 64)).unwrap();
    assert_ne!(first.nonce, second.nonce, "a nonce is never reused");
    assert_ne!(first.cipher, second.cipher);
}

#[test]
fn a_tampered_ciphertext_is_refused_rather_than_decoded() {
    let (_store, cipher) = in_memory();
    let person = PersonId::new();
    let binding = binding(person, 64);
    let sealed = cipher.seal(&embedding(64), &binding).unwrap();

    let mut flipped = sealed.clone();
    flipped.cipher[3] ^= 0b0000_0001;
    assert!(matches!(
        cipher.open(&flipped, &binding),
        Err(VoiceProfileError::Tampered)
    ));

    let mut wrong_nonce = sealed.clone();
    wrong_nonce.nonce[0] ^= 0xff;
    assert!(matches!(
        cipher.open(&wrong_nonce, &binding),
        Err(VoiceProfileError::Tampered)
    ));

    let truncated = SealedEmbedding {
        cipher: sealed.cipher[..sealed.cipher.len() - 1].to_vec(),
        ..sealed.clone()
    };
    assert!(matches!(
        cipher.open(&truncated, &binding),
        Err(VoiceProfileError::Tampered)
    ));
}

#[test]
fn a_ciphertext_cannot_be_moved_to_another_person_or_model() {
    // The binding is authenticated data: editing the row around the blob
    // destroys it instead of relabelling somebody's voice (VP-075, ADR-0082).
    let (_store, cipher) = in_memory();
    let priya = PersonId::new();
    let sam = PersonId::new();
    let sealed = cipher.seal(&embedding(64), &binding(priya, 64)).unwrap();

    assert!(matches!(
        cipher.open(&sealed, &binding(sam, 64)),
        Err(VoiceProfileError::Tampered)
    ));

    let other_version = ExemplarBinding {
        model_version: "2.0".to_owned(),
        ..binding(priya, 64)
    };
    assert!(matches!(
        cipher.open(&sealed, &other_version),
        Err(VoiceProfileError::Tampered)
    ));

    let other_model = ExemplarBinding {
        model_id: "titanet-large".to_owned(),
        ..binding(priya, 64)
    };
    assert!(matches!(
        cipher.open(&sealed, &other_model),
        Err(VoiceProfileError::Tampered)
    ));

    let mut relabelled_dim = sealed.clone();
    relabelled_dim.embedding_dim = 32;
    assert!(matches!(
        cipher.open(&relabelled_dim, &binding(priya, 32)),
        Err(VoiceProfileError::Tampered)
    ));
}

#[test]
fn destroying_the_key_makes_every_remaining_ciphertext_undecryptable() {
    // VP-095 "forget everyone": the erasure that reaches bytes we no longer
    // control, e.g. inside an encrypted backup already written.
    let (store, cipher) = in_memory();
    let person = PersonId::new();
    let binding = binding(person, 64);
    let sealed = cipher.seal(&embedding(64), &binding).unwrap();

    cipher.destroy_key().unwrap();
    assert_eq!(store.entry_count(), 0);
    assert!(matches!(
        cipher.open(&sealed, &binding),
        Err(VoiceProfileError::Tampered)
    ));

    // Idempotent: a user who runs it twice got what they asked for twice.
    cipher.destroy_key().unwrap();

    // A fresh key does not resurrect the old ciphertext.
    let reborn = cipher.seal(&embedding(64), &binding).unwrap();
    assert!(cipher.open(&reborn, &binding).is_ok());
    assert!(matches!(
        cipher.open(&sealed, &binding),
        Err(VoiceProfileError::Tampered)
    ));
}

#[test]
fn a_malformed_vector_or_key_is_refused() {
    let (store, cipher) = in_memory();
    let person = PersonId::new();
    assert!(matches!(
        cipher.seal(&[], &binding(person, 0)),
        Err(VoiceProfileError::Malformed(_))
    ));
    assert!(
        matches!(
            cipher.seal(&embedding(64), &binding(person, 65)),
            Err(VoiceProfileError::Malformed(_))
        ),
        "a binding that disagrees with the vector is a caller bug, not a guess"
    );
    let sealed = cipher.seal(&embedding(64), &binding(person, 64)).unwrap();
    let short_nonce = SealedEmbedding {
        nonce: vec![0u8; 8],
        ..sealed.clone()
    };
    assert!(matches!(
        cipher.open(&short_nonce, &binding(person, 64)),
        Err(VoiceProfileError::Malformed(_))
    ));

    // A vault entry that is not a 32-byte key is refused, never padded.
    store
        .store(
            &SecretScope::VoiceProfileKey,
            &plaudon_security::secrets::SecretString::new("deadbeef"),
        )
        .unwrap();
    assert!(matches!(
        cipher.open(&sealed, &binding(person, 64)),
        Err(VoiceProfileError::MalformedKey)
    ));
}

#[tokio::test]
async fn a_locked_store_locks_the_biometric_tier_and_nothing_else() {
    // ADR-0014 + spec §12.2A: no plaintext fallback for embeddings, and no
    // collateral damage to the name-only roster.
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(dir.path().join("app.sqlite")).await.unwrap();
    MigrationRunner::new().run(&db).await.unwrap();
    let people = PeopleRepository::new(db.clone());
    let voices = VoiceEnrollmentRepository::new(db.clone());

    let recording = RecordingId::new();
    sqlx::query("INSERT INTO recordings (id, title, created_at) VALUES (?1,'Weekly',0)")
        .bind(recording.to_string())
        .execute(db.pool())
        .await
        .unwrap();
    let speaker = SpeakerId::new();
    sqlx::query("INSERT INTO speakers (id, recording_id, label, provenance, user_confirmed, cluster_key, original_label) VALUES (?1, ?2, 'Speaker 1','inferred',0,'c0','Speaker 1')")
        .bind(speaker.to_string())
        .bind(recording.to_string())
        .execute(db.pool())
        .await
        .unwrap();

    let locked = Arc::new(InMemorySecretStore::new());
    locked.set_locked(true);
    let cipher = EmbeddingCipher::new(locked.clone() as Arc<dyn SecretStore>);

    // Biometric tier: refused, with the actionable locked state.
    let person_id = PersonId::new();
    let error = cipher
        .seal(&embedding(64), &binding(person_id, 64))
        .unwrap_err();
    assert!(error.is_store_locked(), "unexpected error: {error}");
    assert!(cipher.check_available().unwrap_err().is_store_locked());
    assert_eq!(locked.entry_count(), 0, "no key was written anywhere");

    // Unsupported platform: same answer, no plaintext fallback.
    let unsupported = EmbeddingCipher::new(Arc::new(UnsupportedSecretStore::new()));
    assert!(unsupported
        .seal(&embedding(64), &binding(person_id, 64))
        .unwrap_err()
        .is_store_locked());

    // Roster tier: entirely unaffected.
    let priya = people
        .create(&PersonDraft::named("Priya"), Timestamp::from_millis(1))
        .await
        .unwrap();
    people
        .assign_speaker(speaker, priya.id, Timestamp::from_millis(2))
        .await
        .unwrap();
    assert_eq!(people.list_active().await.unwrap().len(), 1);
    assert_eq!(
        people.assignments_for_recording(recording).await.unwrap(),
        vec![(speaker, priya.id)]
    );
    assert_eq!(
        people.linked_recordings(priya.id).await.unwrap(),
        1,
        "manual assignment works with the credential store locked"
    );

    // And once the store unlocks, enrolment works without any migration or
    // repair step.
    locked.set_locked(false);
    voices
        .enroll(
            priya.id,
            &EnrollmentDraft {
                consent_version: "voice-profiles-disclosure/1".to_owned(),
                consent_note: None,
                retention_months: 12,
            },
            Timestamp::from_millis(3),
        )
        .await
        .unwrap();
    let sealed = cipher
        .seal(&embedding(192), &binding(priya.id, 192))
        .unwrap();
    voices
        .add_exemplar(
            &ExemplarInsert {
                person_id: priya.id,
                recording_id: Some(recording),
                diarization_run_id: None,
                cluster_key: Some("c0".to_owned()),
                source_start_ms: Some(0),
                source_end_ms: Some(8_000),
                speech_ms: 8_000,
                model_id: "wespeaker-resnet34-lm".to_owned(),
                model_version: "1.0".to_owned(),
                model_sha256: "c".repeat(64),
                embedding_dim: sealed.embedding_dim,
                embedding_cipher: sealed.cipher.clone(),
                embedding_nonce: sealed.nonce.clone(),
            },
            Timestamp::from_millis(4),
        )
        .await
        .unwrap();

    // What came back out of SQLite decrypts to what went in.
    let stored = voices.exemplars(priya.id).await.unwrap();
    assert_eq!(stored.len(), 1);
    let reread = SealedEmbedding {
        cipher: stored[0].embedding_cipher.clone(),
        nonce: stored[0].embedding_nonce.clone(),
        embedding_dim: stored[0].embedding_dim,
    };
    let opened = cipher.open(&reread, &binding(priya.id, 192)).unwrap();
    assert_eq!(opened.as_slice(), embedding(192).as_slice());

    // Locking the store again stops decryption without touching the roster.
    locked.set_unavailable(true);
    assert!(cipher
        .open(&reread, &binding(priya.id, 192))
        .unwrap_err()
        .is_store_locked());
    assert_eq!(people.list_active().await.unwrap().len(), 1);
    db.close().await;
}

/// Two libraries, one voice: nothing links the rows (TM-048).
///
/// The cross-library correlation threat is not "could someone decrypt the
/// store" — that is TM-045 — but "could two stores be joined". Until now the
/// answer was argued from the schema's shape; this is the argument as a test.
/// Enrol the *same* vector for the *same* person name in two independent
/// libraries and nothing comes out the same: not the person id, not the
/// enrolment id, not the nonce, not the ciphertext. There is no content-derived
/// identifier to join on, and the key that would make the ciphertexts
/// comparable never leaves its own library.
#[tokio::test]
async fn the_same_voice_in_two_libraries_shares_no_identifier_or_ciphertext() {
    async fn library(dir: &std::path::Path, name: &str) -> (Database, PersonId, SealedEmbedding) {
        let db = Database::open(dir.join(format!("{name}.sqlite")))
            .await
            .unwrap();
        MigrationRunner::new().run(&db).await.unwrap();
        let people = PeopleRepository::new(db.clone());
        let voices = VoiceEnrollmentRepository::new(db.clone());

        // Same display name in both libraries: the obvious join key a user
        // would hand an adversary, and it is a note, not an identifier.
        let person = people
            .create(&PersonDraft::named("Priya"), Timestamp::from_millis(1))
            .await
            .unwrap();
        voices
            .enroll(
                person.id,
                &EnrollmentDraft {
                    consent_version: "voice-profiles-disclosure/1".to_owned(),
                    consent_note: None,
                    retention_months: 12,
                },
                Timestamp::from_millis(2),
            )
            .await
            .unwrap();

        // A fresh secret store per library: this is what a second machine is.
        let (_store, cipher) = in_memory();
        let sealed = cipher
            .seal(&embedding(192), &binding(person.id, 192))
            .unwrap();
        voices
            .add_exemplar(
                &ExemplarInsert {
                    person_id: person.id,
                    recording_id: None,
                    diarization_run_id: None,
                    cluster_key: None,
                    source_start_ms: None,
                    source_end_ms: None,
                    speech_ms: 8_000,
                    model_id: "wespeaker-resnet34-lm".to_owned(),
                    model_version: "1.0".to_owned(),
                    model_sha256: "c".repeat(64),
                    embedding_dim: sealed.embedding_dim,
                    embedding_cipher: sealed.cipher.clone(),
                    embedding_nonce: sealed.nonce.clone(),
                },
                Timestamp::from_millis(3),
            )
            .await
            .unwrap();
        (db, person.id, sealed)
    }

    let dir = tempfile::tempdir().unwrap();
    let (first_db, first_person, first_sealed) = library(dir.path(), "first").await;
    let (second_db, second_person, second_sealed) = library(dir.path(), "second").await;

    assert_ne!(
        first_person, second_person,
        "the person id is random per library, not derived from the name or the voice"
    );
    assert_ne!(
        first_sealed.nonce, second_sealed.nonce,
        "a repeated nonce would make two ciphertexts comparable"
    );
    assert_ne!(
        first_sealed.cipher, second_sealed.cipher,
        "the same vector under two library keys must not produce the same bytes"
    );
    assert_eq!(
        first_sealed.embedding_dim, second_sealed.embedding_dim,
        "dimensionality is a property of the model, and it is the only thing the two rows share"
    );

    // Nothing persisted is a function of the vector: the only columns that
    // could join two libraries are the model identity, which is the same for
    // every user of the checkpoint and identifies nobody.
    let joinable: Vec<(String, String)> =
        sqlx::query_as("SELECT person_id, model_sha256 FROM voice_exemplars")
            .fetch_all(first_db.pool())
            .await
            .unwrap();
    let other: Vec<(String, String)> =
        sqlx::query_as("SELECT person_id, model_sha256 FROM voice_exemplars")
            .fetch_all(second_db.pool())
            .await
            .unwrap();
    assert_eq!(joinable.len(), 1);
    assert_eq!(other.len(), 1);
    assert_ne!(joinable[0].0, other[0].0);
    assert_eq!(
        joinable[0].1, other[0].1,
        "the model digest is shared on purpose: it identifies the checkpoint, not a person"
    );

    first_db.close().await;
    second_db.close().await;
}
