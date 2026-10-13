//! Wholesale erasure and retention expiry, against a real migrated library
//! (`VOICE_PROFILES_SPEC` VP-095, VP-130; ADR-0014; threat-model row TM-046).
//!
//! No mocks: the secret store is the in-memory implementation the security
//! crate ships for tests, and everything else is SQLite on disk. What is
//! asserted is what an operator would find in the file afterwards.

use std::sync::Arc;

use plaudon_app_core::voice_profiles::{VoiceProfileEraser, VoiceRetentionService, TOMBSTONE_DAYS};
use plaudon_app_core::EmbeddingCipher;
use plaudon_domain::ids::{PersonId, RecordingId};
use plaudon_domain::time::Timestamp;
use plaudon_persistence::db::Database;
use plaudon_persistence::migrations::MigrationRunner;
use plaudon_persistence::people::{PeopleRepository, PersonDraft};
use plaudon_persistence::voice_profiles::{
    EnrollmentDraft, ExemplarInsert, SuggestionInsert, VoiceEnrollmentRepository,
    VoiceProfileGateRepository,
};
use plaudon_security::secrets::{InMemorySecretStore, SecretScope, SecretStore};
use sqlx::Row;
use uuid::Uuid;

const NOW: Timestamp = Timestamp::from_millis(1_800_000_000_000);
const DAY_MS: i64 = 24 * 60 * 60 * 1000;

struct Library {
    _dir: tempfile::TempDir,
    db: Database,
    people: PeopleRepository,
    voices: VoiceEnrollmentRepository,
    gate: VoiceProfileGateRepository,
    secrets: Arc<InMemorySecretStore>,
    cipher: EmbeddingCipher,
    recording: RecordingId,
    run_id: Uuid,
}

async fn library() -> Library {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(dir.path().join("app.sqlite")).await.unwrap();
    MigrationRunner::new().run(&db).await.unwrap();

    let recording = RecordingId::new();
    let run_id = Uuid::now_v7();
    sqlx::query("INSERT INTO recordings (id, title, created_at) VALUES (?1,'Weekly',0)")
        .bind(recording.to_string())
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO media_assets (id, sha256, byte_length, relative_path, quality_class, verification_state, created_at) VALUES ('m1', ?1, 1, 'media/sha256/aa/x.wav','user_supplied_file','verified',0)")
        .bind("a".repeat(64))
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO diarization_runs (id, recording_id, media_asset_id, provider_id, privacy_class, status, input_sha256, created_at) VALUES (?1, ?2, 'm1','local','local_only','succeeded',?3,0)")
        .bind(run_id.to_string())
        .bind(recording.to_string())
        .bind("a".repeat(64))
        .execute(db.pool())
        .await
        .unwrap();

    let secrets = Arc::new(InMemorySecretStore::new());
    let cipher = EmbeddingCipher::new(secrets.clone() as Arc<dyn SecretStore>);
    Library {
        _dir: dir,
        people: PeopleRepository::new(db.clone()),
        voices: VoiceEnrollmentRepository::new(db.clone()),
        gate: VoiceProfileGateRepository::new(db.clone()),
        secrets,
        cipher,
        db,
        recording,
        run_id,
    }
}

impl Library {
    async fn person(&self, name: &str) -> PersonId {
        self.people
            .create(&PersonDraft::named(name), NOW)
            .await
            .unwrap()
            .id
    }

    /// Enrol `person` and seal one real exemplar under the library key.
    async fn enrol(&self, person: PersonId, at: Timestamp) {
        self.voices
            .enroll(
                person,
                &EnrollmentDraft {
                    consent_version: "voice-consent/1".to_owned(),
                    consent_note: None,
                    retention_months: 12,
                },
                at,
            )
            .await
            .unwrap();
        let vector: Vec<f32> = (0..192).map(|i| i as f32 * 0.01).collect();
        let sealed = self
            .cipher
            .seal(
                &vector,
                &plaudon_app_core::ExemplarBinding {
                    person_id: person,
                    model_id: "wespeaker-resnet34-lm".to_owned(),
                    model_version: "1.0".to_owned(),
                    embedding_dim: 192,
                },
            )
            .unwrap();
        self.voices
            .add_exemplar(
                &ExemplarInsert {
                    person_id: person,
                    recording_id: Some(self.recording),
                    diarization_run_id: Some(self.run_id),
                    cluster_key: Some("c0".to_owned()),
                    source_start_ms: Some(0),
                    source_end_ms: Some(12_000),
                    speech_ms: 12_000,
                    model_id: "wespeaker-resnet34-lm".to_owned(),
                    model_version: "1.0".to_owned(),
                    model_sha256: "b".repeat(64),
                    embedding_dim: sealed.embedding_dim,
                    embedding_cipher: sealed.cipher.clone(),
                    embedding_nonce: sealed.nonce.clone(),
                },
                at,
            )
            .await
            .unwrap();
    }

    async fn suggest(&self, person: PersonId, cluster: &str) {
        self.voices
            .record_suggestions(
                &[SuggestionInsert {
                    recording_id: self.recording,
                    diarization_run_id: self.run_id,
                    cluster_key: cluster.to_owned(),
                    person_id: person,
                    score: 0.81,
                    band: "suggest".to_owned(),
                    model_id: "wespeaker-resnet34-lm".to_owned(),
                    model_version: "1.0".to_owned(),
                }],
                NOW,
            )
            .await
            .unwrap();
    }

    async fn count(&self, sql: &str) -> i64 {
        sqlx::query(sqlx::AssertSqlSafe(sql.to_owned()))
            .fetch_one(self.db.pool())
            .await
            .unwrap()
            .get::<i64, _>(0)
    }

    async fn journal_kinds(&self) -> Vec<String> {
        sqlx::query("SELECT kind FROM voice_profile_events ORDER BY created_at, id")
            .fetch_all(self.db.pool())
            .await
            .unwrap()
            .iter()
            .map(|row| row.get::<String, _>("kind"))
            .collect()
    }

    /// Move an enrolment's whole history into the past, so the retention clock
    /// has elapsed without the test waiting a year.
    async fn age(&self, person: PersonId, days: i64) {
        let moment = NOW.as_millis() - days * DAY_MS;
        sqlx::query("UPDATE person_voice_enrollments SET created_at = ?2 WHERE person_id = ?1")
            .bind(person.to_string())
            .bind(moment)
            .execute(self.db.pool())
            .await
            .unwrap();
        sqlx::query("UPDATE voice_exemplars SET created_at = ?2 WHERE person_id = ?1")
            .bind(person.to_string())
            .bind(moment)
            .execute(self.db.pool())
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn forget_everyone_destroys_the_tier_the_key_and_the_toggle_but_keeps_the_roster() {
    let library = library().await;
    let priya = library.person("Priya").await;
    let sam = library.person("Sam").await;
    library.enrol(priya, NOW).await;
    library.enrol(sam, NOW).await;
    library.suggest(priya, "c0").await;
    library.gate.activate("voice-consent/1", NOW).await.unwrap();

    let eraser = VoiceProfileEraser::new(library.db.clone(), library.cipher.clone());
    let disclosure = eraser.disclose().await.unwrap();
    assert_eq!(disclosure.enrollments, 2);
    assert_eq!(disclosure.exemplars, 2);
    assert_eq!(disclosure.suggestions, 1);
    assert_eq!(disclosure.people_kept, 2);
    assert!(disclosure.key_present, "sealing created the library key");
    assert!(
        disclosure
            .limits
            .iter()
            .any(|line| line.contains("not recall")),
        "the disclosure states what erasure cannot reach: {:?}",
        disclosure.limits
    );

    let receipt = eraser.forget_all(NOW).await.unwrap();
    assert_eq!(receipt.enrollments_destroyed, disclosure.enrollments);
    assert_eq!(receipt.exemplars_destroyed, disclosure.exemplars);
    assert_eq!(receipt.suggestions_destroyed, disclosure.suggestions);
    assert_eq!(receipt.people_kept, disclosure.people_kept);
    assert!(receipt.key_destroyed && receipt.feature_disabled);

    assert_eq!(
        library
            .count("SELECT COUNT(*) FROM person_voice_enrollments")
            .await,
        0
    );
    assert_eq!(
        library.count("SELECT COUNT(*) FROM voice_exemplars").await,
        0
    );
    assert_eq!(
        library
            .count("SELECT COUNT(*) FROM speaker_suggestions")
            .await,
        0
    );
    // The roster is names, not voiceprints: it survives untouched.
    assert_eq!(library.count("SELECT COUNT(*) FROM people").await, 2);
    assert_eq!(library.people.list_active().await.unwrap().len(), 2);

    // Cryptographic erasure: the key is gone from the vault, so any ciphertext
    // in a backup written earlier is permanently undecryptable.
    assert!(library.secrets.load(&SecretScope::VoiceProfileKey).is_err());
    let gate = library.gate.load().await.unwrap();
    assert!(!gate.enabled && !gate.is_active());

    let kinds = library.journal_kinds().await;
    assert_eq!(
        kinds.iter().filter(|kind| *kind == "all_forgotten").count(),
        1,
        "VP-095 writes exactly one journal event: {kinds:?}"
    );
}

#[tokio::test]
async fn forgetting_everyone_twice_is_not_an_error() {
    let library = library().await;
    let priya = library.person("Priya").await;
    library.enrol(priya, NOW).await;
    let eraser = VoiceProfileEraser::new(library.db.clone(), library.cipher.clone());

    eraser.forget_all(NOW).await.unwrap();
    let second = eraser.forget_all(NOW).await.unwrap();
    assert_eq!(second.enrollments_destroyed, 0);
    assert_eq!(second.exemplars_destroyed, 0);
    assert!(second.key_destroyed);
}

#[tokio::test]
async fn an_unused_profile_expires_and_keeps_a_named_tombstone() {
    let library = library().await;
    let priya = library.person("Priya").await;
    library.enrol(priya, NOW).await;
    library.age(priya, 400).await;

    let report = VoiceRetentionService::new(library.db.clone())
        .sweep(NOW)
        .await
        .unwrap();
    assert_eq!(report.expired, 1);
    assert_eq!(report.purged, 0);
    assert!(!report.resumed);

    // Ciphertexts gone, the row kept as a tombstone, the person untouched.
    assert_eq!(
        library.count("SELECT COUNT(*) FROM voice_exemplars").await,
        0
    );
    assert_eq!(
        library
            .count("SELECT COUNT(*) FROM person_voice_enrollments WHERE status = 'expired'")
            .await,
        1
    );
    assert_eq!(library.count("SELECT COUNT(*) FROM people").await, 1);
    assert!(library
        .journal_kinds()
        .await
        .contains(&"enrollment_expired".to_owned()));
}

#[tokio::test]
async fn a_profile_in_use_is_left_alone() {
    let library = library().await;
    let priya = library.person("Priya").await;
    library.enrol(priya, NOW).await;
    library.age(priya, 400).await;
    // A match today is use, whatever the enrolment's age.
    library.voices.mark_matched(priya, NOW).await.unwrap();

    let report = VoiceRetentionService::new(library.db.clone())
        .sweep(NOW)
        .await
        .unwrap();
    assert_eq!(report.expired, 0);
    assert_eq!(
        library.count("SELECT COUNT(*) FROM voice_exemplars").await,
        1
    );
}

#[tokio::test]
async fn a_shortened_retention_term_is_honoured() {
    let library = library().await;
    let priya = library.person("Priya").await;
    library.enrol(priya, NOW).await;
    library.voices.shorten_retention(priya, 1).await.unwrap();
    // Older than one month, far younger than twelve.
    library.age(priya, 45).await;

    let report = VoiceRetentionService::new(library.db.clone())
        .sweep(NOW)
        .await
        .unwrap();
    assert_eq!(report.expired, 1);
}

#[tokio::test]
async fn the_sweep_is_idempotent_across_runs() {
    let library = library().await;
    let priya = library.person("Priya").await;
    library.enrol(priya, NOW).await;
    library.age(priya, 400).await;

    let retention = VoiceRetentionService::new(library.db.clone());
    retention.sweep(NOW).await.unwrap();
    let second = retention.sweep(NOW).await.unwrap();
    assert_eq!(second.expired, 0, "a second pass destroys nothing again");
    assert_eq!(
        library
            .journal_kinds()
            .await
            .iter()
            .filter(|kind| *kind == "enrollment_expired")
            .count(),
        1,
        "and journals no second deletion"
    );
}

#[tokio::test]
async fn an_interrupted_sweep_is_resumed_rather_than_restarted() {
    let library = library().await;
    let priya = library.person("Priya").await;
    library.enrol(priya, NOW).await;
    library.age(priya, 400).await;

    // A pass that opened its journal row and then died.
    let planned_at = Timestamp::from_millis(NOW.as_millis() - DAY_MS);
    let crashed = library
        .voices
        .open_retention_sweep(planned_at)
        .await
        .unwrap();
    assert!(crashed.completed_at.is_none());

    let report = VoiceRetentionService::new(library.db.clone())
        .sweep(NOW)
        .await
        .unwrap();
    assert!(report.resumed, "the open pass was adopted");
    assert_eq!(report.sweep_id, crashed.id);
    assert_eq!(
        report.planned_at, planned_at,
        "a resumed pass evaluates the clock where it was planned, not where it resumed"
    );
    assert_eq!(report.expired, 1);
    assert!(library
        .voices
        .unfinished_retention_sweep()
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn an_expired_tombstone_is_removed_after_the_visibility_window() {
    let library = library().await;
    let priya = library.person("Priya").await;
    library.enrol(priya, NOW).await;
    library.age(priya, 400).await;

    let retention = VoiceRetentionService::new(library.db.clone());
    retention.sweep(NOW).await.unwrap();

    // Still inside the window: the named tombstone explains why matching
    // stopped.
    let inside = Timestamp::from_millis(NOW.as_millis() + (TOMBSTONE_DAYS as i64 - 1) * DAY_MS);
    assert_eq!(retention.sweep(inside).await.unwrap().purged, 0);
    assert_eq!(
        library
            .count("SELECT COUNT(*) FROM person_voice_enrollments")
            .await,
        1
    );

    let after = Timestamp::from_millis(NOW.as_millis() + (TOMBSTONE_DAYS as i64 + 1) * DAY_MS);
    let report = retention.sweep(after).await.unwrap();
    assert_eq!(report.purged, 1);
    assert_eq!(
        library
            .count("SELECT COUNT(*) FROM person_voice_enrollments")
            .await,
        0
    );
    // The person stays, name-only.
    assert_eq!(library.people.list_active().await.unwrap().len(), 1);
}
