//! Behaviour of the roster and biometric-tier repositories
//! (`VOICE_PROFILES_SPEC` VP-010, VP-072, VP-080, VP-130, VP-213, VP-216).
//!
//! These exercise the repositories against a real migrated SQLite library: no
//! mocks, no in-memory doubles. What is asserted is what an operator would see
//! in the file afterwards.

use plaudon_domain::ids::{PersonId, RecordingId, SpeakerId};
use plaudon_domain::time::Timestamp;
use plaudon_persistence::db::Database;
use plaudon_persistence::error::PersistenceError;
use plaudon_persistence::migrations::MigrationRunner;
use plaudon_persistence::people::{PeopleRepository, PersonDraft, PersonStatus};
use plaudon_persistence::voice_profiles::{
    EnrollmentDraft, ExemplarInsert, ResealedExemplar, SuggestionInsert, VoiceEnrollmentRepository,
};
use sqlx::Row;
use uuid::Uuid;

struct Library {
    _dir: tempfile::TempDir,
    db: Database,
    people: PeopleRepository,
    voices: VoiceEnrollmentRepository,
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

    Library {
        _dir: dir,
        people: PeopleRepository::new(db.clone()),
        voices: VoiceEnrollmentRepository::new(db.clone()),
        db,
        recording,
        run_id,
    }
}

impl Library {
    async fn speaker(&self, cluster: &str, label: &str) -> SpeakerId {
        let id = SpeakerId::new();
        sqlx::query("INSERT INTO speakers (id, recording_id, label, provenance, user_confirmed, cluster_key, original_label) VALUES (?1, ?2, ?3, 'inferred', 0, ?4, ?3)")
            .bind(id.to_string())
            .bind(self.recording.to_string())
            .bind(label.to_owned())
            .bind(cluster.to_owned())
            .execute(self.db.pool())
            .await
            .unwrap();
        id
    }

    async fn speaker_row(&self, id: SpeakerId) -> (Option<String>, String, i64) {
        let row = sqlx::query("SELECT person_id, name, user_confirmed FROM speakers WHERE id = ?1")
            .bind(id.to_string())
            .fetch_one(self.db.pool())
            .await
            .unwrap();
        (
            row.get::<Option<String>, _>("person_id"),
            row.get::<Option<String>, _>("name").unwrap_or_default(),
            row.get::<i64, _>("user_confirmed"),
        )
    }

    async fn exemplar_count(&self, person: PersonId) -> i64 {
        sqlx::query("SELECT COUNT(*) FROM voice_exemplars WHERE person_id = ?1")
            .bind(person.to_string())
            .fetch_one(self.db.pool())
            .await
            .unwrap()
            .get(0)
    }

    fn exemplar(&self, person: PersonId) -> ExemplarInsert {
        ExemplarInsert {
            person_id: person,
            recording_id: Some(self.recording),
            diarization_run_id: Some(self.run_id),
            cluster_key: Some("c0".to_owned()),
            source_start_ms: Some(1_000),
            source_end_ms: Some(9_000),
            speech_ms: 6_000,
            model_id: "wespeaker-resnet34-lm".to_owned(),
            model_version: "1.0".to_owned(),
            model_sha256: "b".repeat(64),
            embedding_dim: 192,
            embedding_cipher: vec![3u8; 64],
            embedding_nonce: vec![4u8; 24],
        }
    }

    fn suggestion(&self, person: PersonId, cluster: &str, score: f64) -> SuggestionInsert {
        SuggestionInsert {
            recording_id: self.recording,
            diarization_run_id: self.run_id,
            cluster_key: cluster.to_owned(),
            person_id: person,
            score,
            band: "suggest".to_owned(),
            model_id: "wespeaker-resnet34-lm".to_owned(),
            model_version: "1.0".to_owned(),
        }
    }
}

fn at(millis: i64) -> Timestamp {
    Timestamp::from_millis(millis)
}

fn consent() -> EnrollmentDraft {
    EnrollmentDraft {
        consent_version: "voice-profiles-disclosure/1".to_owned(),
        consent_note: Some("asked at the stand-up".to_owned()),
        retention_months: 12,
    }
}

/// The loser's exemplars, re-sealed for the merge.
///
/// A ciphertext authenticates the person it belongs to (VP-071), so a merge
/// takes the moved exemplars sealed under the survivor's binding; the
/// re-sealing itself lives in `plaudon_app_core::voice_profiles::enroll`,
/// which owns the key. This suite stores opaque bytes rather than real
/// ciphertexts, so it re-presents them unchanged — what is under test here is
/// that the merge demands coverage of exactly the rows it moves.
async fn resealed(lib: &Library, loser: PersonId) -> Vec<ResealedExemplar> {
    lib.voices
        .exemplars(loser)
        .await
        .unwrap()
        .into_iter()
        .map(|row| ResealedExemplar {
            id: row.id,
            embedding_cipher: row.embedding_cipher,
            embedding_nonce: row.embedding_nonce,
        })
        .collect()
}

#[tokio::test]
async fn the_roster_works_with_no_enrolment_anywhere_in_the_library() {
    // VP-011 / §12.2A: manual mode is the whole product for a user who never
    // turns matching on.
    let lib = library().await;
    let priya = lib
        .people
        .create(
            &PersonDraft {
                display_name: "Priya".to_owned(),
                aliases: vec!["Priya R".to_owned()],
                email: Some("priya@example.test".to_owned()),
                organisation: Some("Acme".to_owned()),
            },
            at(10),
        )
        .await
        .unwrap();
    let sam = lib
        .people
        .create(&PersonDraft::named("Sam"), at(11))
        .await
        .unwrap();

    let listed = lib.people.list_active().await.unwrap();
    assert_eq!(
        listed
            .iter()
            .map(|p| p.display_name.as_str())
            .collect::<Vec<_>>(),
        vec!["Priya", "Sam"]
    );
    assert_eq!(listed[0].aliases, vec!["Priya R".to_owned()]);

    let speaker = lib.speaker("c0", "Speaker 1").await;
    lib.people
        .assign_speaker(speaker, priya.id, at(12))
        .await
        .unwrap();
    let (person_id, name, confirmed) = lib.speaker_row(speaker).await;
    assert_eq!(person_id, Some(priya.id.to_string()));
    assert_eq!(name, "Priya");
    assert_eq!(confirmed, 1);

    let enrolments: i64 = sqlx::query("SELECT COUNT(*) FROM person_voice_enrollments")
        .fetch_one(lib.db.pool())
        .await
        .unwrap()
        .get(0);
    assert_eq!(
        enrolments, 0,
        "no biometric row may appear from a manual pick"
    );

    lib.people.clear_assignment(speaker, at(13)).await.unwrap();
    let (person_id, name, _) = lib.speaker_row(speaker).await;
    assert_eq!(person_id, None);
    assert_eq!(name, "Priya", "clearing a link keeps the text snapshot");

    let kinds: Vec<String> = lib
        .people
        .events(priya.id)
        .await
        .unwrap()
        .into_iter()
        .map(|event| event.kind)
        .collect();
    assert_eq!(
        kinds,
        vec!["person_created", "assignment_set", "assignment_cleared"]
    );
    assert_eq!(lib.people.events(sam.id).await.unwrap().len(), 1);
}

#[tokio::test]
async fn renaming_journals_both_names_and_the_blast_radius() {
    // VP-213a: the user is told how many recordings show the name; the journal
    // keeps the sentence honest after the fact.
    let lib = library().await;
    let person = lib
        .people
        .create(&PersonDraft::named("Priya"), at(1))
        .await
        .unwrap();
    let speaker = lib.speaker("c0", "Speaker 1").await;
    lib.people
        .assign_speaker(speaker, person.id, at(2))
        .await
        .unwrap();
    assert_eq!(lib.people.linked_recordings(person.id).await.unwrap(), 1);

    let updated = lib
        .people
        .update(person.id, &PersonDraft::named("Priya Raman"), at(3))
        .await
        .unwrap();
    assert_eq!(updated.display_name, "Priya Raman");
    assert_eq!(updated.updated_at, at(3));

    let rename = lib
        .people
        .events(person.id)
        .await
        .unwrap()
        .into_iter()
        .find(|event| event.kind == "person_renamed")
        .expect("a rename is journalled");
    let detail: serde_json::Value = serde_json::from_str(&rename.detail_json).unwrap();
    assert_eq!(detail["from"], "Priya");
    assert_eq!(detail["to"], "Priya Raman");
    assert_eq!(detail["recordings_showing_the_name"], 1);

    let blank = lib
        .people
        .update(person.id, &PersonDraft::named("   "), at(4))
        .await;
    assert!(matches!(blank, Err(PersistenceError::Invalid(_))));
}

#[tokio::test]
async fn merging_repoints_assignments_enrolments_and_suggestions() {
    // VP-213c: the survivor ends up with everything; the loser stays as a
    // tombstone so the journal's ids keep resolving.
    let lib = library().await;
    let survivor = lib
        .people
        .create(&PersonDraft::named("Priya"), at(1))
        .await
        .unwrap();
    let duplicate = lib
        .people
        .create(&PersonDraft::named("priya r"), at(2))
        .await
        .unwrap();

    let speaker = lib.speaker("c0", "Speaker 1").await;
    lib.people
        .assign_speaker(speaker, duplicate.id, at(3))
        .await
        .unwrap();
    lib.voices
        .enroll(duplicate.id, &consent(), at(4))
        .await
        .unwrap();
    lib.voices
        .add_exemplar(&lib.exemplar(duplicate.id), at(5))
        .await
        .unwrap();
    lib.voices
        .record_suggestions(&[lib.suggestion(duplicate.id, "c1", 0.81)], at(6))
        .await
        .unwrap();

    let preview = lib
        .people
        .merge_preview(duplicate.id, survivor.id)
        .await
        .unwrap();
    assert_eq!(preview.speaker_links, 1);
    assert_eq!(preview.recordings, 1);
    assert_eq!(preview.exemplars, 1);
    assert_eq!(preview.pending_suggestions, 1);
    assert!(preview.enrollment_moved);

    let outcome = lib
        .people
        .merge(
            duplicate.id,
            survivor.id,
            &resealed(&lib, duplicate.id).await,
            at(7),
        )
        .await
        .unwrap();
    assert_eq!(outcome, preview, "the preview is what the merge executes");

    let (person_id, name, _) = lib.speaker_row(speaker).await;
    assert_eq!(person_id, Some(survivor.id.to_string()));
    assert_eq!(name, "Priya");
    assert_eq!(lib.exemplar_count(survivor.id).await, 1);
    assert_eq!(lib.exemplar_count(duplicate.id).await, 0);
    assert!(lib.voices.enrollment(survivor.id).await.unwrap().is_some());
    assert!(lib.voices.enrollment(duplicate.id).await.unwrap().is_none());
    assert_eq!(
        lib.voices
            .pending_for_recording(lib.recording)
            .await
            .unwrap()
            .into_iter()
            .map(|row| row.person_id)
            .collect::<Vec<_>>(),
        vec![survivor.id]
    );

    let tombstone = lib.people.get(duplicate.id).await.unwrap().unwrap();
    assert_eq!(tombstone.status, PersonStatus::Merged);
    assert!(
        lib.people
            .list_active()
            .await
            .unwrap()
            .iter()
            .all(|row| row.id != duplicate.id),
        "a merged entry is not pickable"
    );

    // Both ids carry the merge event, so the journal resolves from either side.
    for id in [survivor.id, duplicate.id] {
        let event = lib
            .people
            .events(id)
            .await
            .unwrap()
            .into_iter()
            .find(|event| event.kind == "person_merged")
            .expect("merge is journalled for both people");
        let detail: serde_json::Value = serde_json::from_str(&event.detail_json).unwrap();
        assert_eq!(detail["recordings_affected"], 1);
        assert_eq!(detail["exemplars_moved"], 1);
        assert_eq!(detail["surviving_person_id"], survivor.id.to_string());
    }

    assert!(matches!(
        lib.people.merge(survivor.id, survivor.id, &[], at(8)).await,
        Err(PersistenceError::Invalid(_))
    ));
    // A tombstone cannot become a merge target.
    assert!(matches!(
        lib.people
            .merge(survivor.id, duplicate.id, &[], at(9))
            .await,
        Err(PersistenceError::Invalid(_))
    ));
}

#[tokio::test]
async fn merging_two_enrolled_people_keeps_the_shorter_retention() {
    // VP-130 allows shortening, never lengthening — including by merge.
    let lib = library().await;
    let survivor = lib
        .people
        .create(&PersonDraft::named("Priya"), at(1))
        .await
        .unwrap();
    let duplicate = lib
        .people
        .create(&PersonDraft::named("P. Raman"), at(2))
        .await
        .unwrap();
    lib.voices
        .enroll(
            survivor.id,
            &EnrollmentDraft {
                retention_months: 6,
                ..consent()
            },
            at(3),
        )
        .await
        .unwrap();
    lib.voices
        .enroll(duplicate.id, &consent(), at(4))
        .await
        .unwrap();
    lib.voices
        .add_exemplar(&lib.exemplar(duplicate.id), at(5))
        .await
        .unwrap();
    lib.voices
        .mark_matched(duplicate.id, at(600))
        .await
        .unwrap();

    let outcome = lib
        .people
        .merge(
            duplicate.id,
            survivor.id,
            &resealed(&lib, duplicate.id).await,
            at(7),
        )
        .await
        .unwrap();
    assert!(!outcome.enrollment_moved);

    let kept = lib.voices.enrollment(survivor.id).await.unwrap().unwrap();
    assert_eq!(kept.retention_months, 6);
    assert_eq!(kept.last_matched_at, Some(at(600)));
    assert_eq!(lib.exemplar_count(survivor.id).await, 1);
    assert!(lib.voices.enrollment(duplicate.id).await.unwrap().is_none());
}

#[tokio::test]
async fn deleting_a_person_reports_and_journals_exactly_what_it_destroyed() {
    // VP-080: counts in the disclosure are the counts that happened, and the
    // journal outlives the row.
    let lib = library().await;
    let person = lib
        .people
        .create(&PersonDraft::named("Priya"), at(1))
        .await
        .unwrap();
    let speaker = lib.speaker("c0", "Speaker 1").await;
    lib.people
        .assign_speaker(speaker, person.id, at(2))
        .await
        .unwrap();
    lib.voices
        .enroll(person.id, &consent(), at(3))
        .await
        .unwrap();
    lib.voices
        .add_exemplar(&lib.exemplar(person.id), at(4))
        .await
        .unwrap();
    lib.voices
        .record_suggestions(&[lib.suggestion(person.id, "c1", 0.9)], at(5))
        .await
        .unwrap();

    let outcome = lib.people.delete(person.id, at(6)).await.unwrap();
    assert_eq!(outcome.severed_speaker_links, 1);
    assert_eq!(outcome.recordings, 1);
    assert_eq!(outcome.exemplars, 1);
    assert_eq!(outcome.suggestions, 1);

    assert!(lib.people.get(person.id).await.unwrap().is_none());
    let (person_id, name, _) = lib.speaker_row(speaker).await;
    assert_eq!(person_id, None);
    assert_eq!(name, "Priya", "the recording keeps the label it displayed");

    let deleted = lib
        .people
        .events(person.id)
        .await
        .unwrap()
        .into_iter()
        .find(|event| event.kind == "person_deleted")
        .expect("the journal survives the person (VP-080)");
    let detail: serde_json::Value = serde_json::from_str(&deleted.detail_json).unwrap();
    assert_eq!(detail["speaker_links_severed"], 1);
    assert_eq!(detail["exemplars_destroyed"], 1);
    assert_eq!(detail["recordings_keeping_a_name_snapshot"], 1);

    assert!(matches!(
        lib.people.delete(PersonId::new(), at(7)).await,
        Err(PersistenceError::RecordNotFound)
    ));
}

#[tokio::test]
async fn deleting_an_enrolment_leaves_a_name_only_roster_entry() {
    let lib = library().await;
    let person = lib
        .people
        .create(&PersonDraft::named("Priya"), at(1))
        .await
        .unwrap();
    let speaker = lib.speaker("c0", "Speaker 1").await;
    lib.people
        .assign_speaker(speaker, person.id, at(2))
        .await
        .unwrap();
    lib.voices
        .enroll(person.id, &consent(), at(3))
        .await
        .unwrap();
    lib.voices
        .add_exemplar(&lib.exemplar(person.id), at(4))
        .await
        .unwrap();
    lib.voices
        .record_suggestions(&[lib.suggestion(person.id, "c1", 0.9)], at(5))
        .await
        .unwrap();

    let outcome = lib
        .voices
        .delete_enrollment(person.id, at(6))
        .await
        .unwrap();
    assert_eq!(outcome.exemplars, 1);
    assert_eq!(outcome.suggestions, 1);

    assert!(lib.voices.enrollment(person.id).await.unwrap().is_none());
    assert_eq!(lib.exemplar_count(person.id).await, 0);
    assert_eq!(
        lib.voices
            .pending_for_recording(lib.recording)
            .await
            .unwrap()
            .len(),
        0
    );

    let still_there = lib.people.get(person.id).await.unwrap().unwrap();
    assert_eq!(still_there.status, PersonStatus::Active);
    let (person_id, name, _) = lib.speaker_row(speaker).await;
    assert_eq!(person_id, Some(person.id.to_string()));
    assert_eq!(name, "Priya");

    // VP-080: the journal survives losing the enrolment, exactly as it
    // survives losing the person.
    let kinds: Vec<String> = lib
        .people
        .events(person.id)
        .await
        .unwrap()
        .into_iter()
        .map(|event| event.kind)
        .collect();
    assert_eq!(
        kinds,
        vec![
            "person_created",
            "assignment_set",
            "enrolled",
            "exemplar_added",
            "enrollment_deleted",
        ]
    );

    assert!(matches!(
        lib.voices.delete_enrollment(person.id, at(7)).await,
        Err(PersistenceError::RecordNotFound)
    ));
}

#[tokio::test]
async fn expiry_destroys_the_ciphertexts_and_keeps_the_tombstone() {
    // VP-130: an expired profile stops matching and says so, rather than
    // disappearing without explanation.
    let lib = library().await;
    let person = lib
        .people
        .create(&PersonDraft::named("Priya"), at(1))
        .await
        .unwrap();
    lib.voices
        .enroll(person.id, &consent(), at(2))
        .await
        .unwrap();
    lib.voices
        .add_exemplar(&lib.exemplar(person.id), at(3))
        .await
        .unwrap();

    let outcome = lib.voices.expire(person.id, at(4)).await.unwrap();
    assert_eq!(outcome.exemplars, 1);
    assert_eq!(lib.exemplar_count(person.id).await, 0);
    let enrolment = lib.voices.enrollment(person.id).await.unwrap().unwrap();
    assert_eq!(enrolment.status, "expired");
    assert!(lib
        .people
        .events(person.id)
        .await
        .unwrap()
        .iter()
        .any(|event| event.kind == "enrollment_expired"));
}

#[tokio::test]
async fn retention_may_be_shortened_and_never_lengthened() {
    let lib = library().await;
    let person = lib
        .people
        .create(&PersonDraft::named("Priya"), at(1))
        .await
        .unwrap();
    lib.voices
        .enroll(person.id, &consent(), at(2))
        .await
        .unwrap();

    lib.voices.shorten_retention(person.id, 3).await.unwrap();
    assert_eq!(
        lib.voices
            .enrollment(person.id)
            .await
            .unwrap()
            .unwrap()
            .retention_months,
        3
    );
    assert!(matches!(
        lib.voices.shorten_retention(person.id, 12).await,
        Err(PersistenceError::Invalid(_))
    ));
    assert!(matches!(
        lib.voices.shorten_retention(person.id, 0).await,
        Err(PersistenceError::Invalid(_))
    ));
    assert!(matches!(
        lib.voices
            .enroll(
                person.id,
                &EnrollmentDraft {
                    retention_months: 24,
                    ..consent()
                },
                at(3)
            )
            .await,
        Err(PersistenceError::Invalid(_))
    ));
}

#[tokio::test]
async fn an_exemplar_must_arrive_already_sealed() {
    // The repository has no plaintext-vector path at all; what it does check is
    // that what it is handed looks like AEAD output bound to a real model.
    let lib = library().await;
    let person = lib
        .people
        .create(&PersonDraft::named("Priya"), at(1))
        .await
        .unwrap();
    lib.voices
        .enroll(person.id, &consent(), at(2))
        .await
        .unwrap();

    let short_nonce = ExemplarInsert {
        embedding_nonce: vec![0u8; 12],
        ..lib.exemplar(person.id)
    };
    assert!(matches!(
        lib.voices.add_exemplar(&short_nonce, at(3)).await,
        Err(PersistenceError::Invalid(_))
    ));

    let bad_digest = ExemplarInsert {
        model_sha256: "not-a-digest".to_owned(),
        ..lib.exemplar(person.id)
    };
    assert!(matches!(
        lib.voices.add_exemplar(&bad_digest, at(3)).await,
        Err(PersistenceError::Invalid(_))
    ));

    let unenrolled = lib
        .people
        .create(&PersonDraft::named("Sam"), at(3))
        .await
        .unwrap();
    assert!(matches!(
        lib.voices
            .add_exemplar(&lib.exemplar(unenrolled.id), at(4))
            .await,
        Err(PersistenceError::Conflict)
    ));

    let id = lib
        .voices
        .add_exemplar(&lib.exemplar(person.id), at(5))
        .await
        .unwrap();
    let stored = lib.voices.exemplars(person.id).await.unwrap();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].id, id);
    assert_eq!(stored[0].embedding_cipher, vec![3u8; 64]);
    assert_eq!(stored[0].embedding_dim, 192);
    // The journal records that an exemplar was added, with the model binding
    // and the duration — and nothing that came out of the cipher.
    let added = lib
        .people
        .events(person.id)
        .await
        .unwrap()
        .into_iter()
        .find(|event| event.kind == "exemplar_added")
        .expect("adding an exemplar is journalled");
    let detail: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(&added.detail_json).unwrap();
    let mut keys: Vec<&str> = detail.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(keys, vec!["model_id", "model_version", "speech_ms"]);
}

#[tokio::test]
async fn re_running_a_match_updates_pending_rows_and_never_reopens_a_decision() {
    // VP-102 idempotence, VP-216 correction, ADR-0082: a score never becomes a
    // label on its own.
    let lib = library().await;
    let priya = lib
        .people
        .create(&PersonDraft::named("Priya"), at(1))
        .await
        .unwrap();
    let sam = lib
        .people
        .create(&PersonDraft::named("Sam"), at(2))
        .await
        .unwrap();
    lib.voices
        .enroll(priya.id, &consent(), at(3))
        .await
        .unwrap();

    lib.voices
        .record_suggestions(&[lib.suggestion(priya.id, "c0", 0.72)], at(4))
        .await
        .unwrap();
    lib.voices
        .record_suggestions(&[lib.suggestion(priya.id, "c0", 0.88)], at(5))
        .await
        .unwrap();
    let pending = lib
        .voices
        .pending_for_recording(lib.recording)
        .await
        .unwrap();
    assert_eq!(pending.len(), 1, "the unique key keeps one row per cluster");
    assert!((pending[0].score - 0.88).abs() < f64::EPSILON);

    // Nothing was assigned by any of that.
    let speaker = lib.speaker("c0", "Speaker 1").await;
    let (person_id, _, confirmed) = lib.speaker_row(speaker).await;
    assert_eq!(person_id, None);
    assert_eq!(confirmed, 0);

    let corrected = lib
        .voices
        .correct_suggestion(pending[0].id, sam.id, at(6))
        .await
        .unwrap();
    assert_eq!(corrected.state, "corrected");
    assert_eq!(corrected.corrected_person_id, Some(sam.id));
    assert_eq!(corrected.decided_at, Some(at(6)));

    // A settled row is not re-settled, and a re-run does not resurrect it.
    assert!(matches!(
        lib.voices.confirm_suggestion(pending[0].id, at(7)).await,
        Err(PersistenceError::Conflict)
    ));
    lib.voices
        .record_suggestions(&[lib.suggestion(priya.id, "c0", 0.95)], at(8))
        .await
        .unwrap();
    assert_eq!(
        lib.voices
            .pending_for_recording(lib.recording)
            .await
            .unwrap()
            .len(),
        0
    );

    // The rejected score survives as negative evidence; no embedding does.
    let row = sqlx::query("SELECT score, state FROM speaker_suggestions WHERE person_id = ?1")
        .bind(priya.id.to_string())
        .fetch_one(lib.db.pool())
        .await
        .unwrap();
    assert_eq!(row.get::<String, _>("state"), "corrected");
    assert!((row.get::<f64, _>("score") - 0.88).abs() < f64::EPSILON);

    let journalled = lib
        .people
        .events(priya.id)
        .await
        .unwrap()
        .into_iter()
        .filter(|event| event.kind == "suggestion_corrected")
        .count();
    assert_eq!(journalled, 1);
}

#[tokio::test]
async fn assignment_refuses_an_unknown_person_or_speaker() {
    let lib = library().await;
    let speaker = lib.speaker("c0", "Speaker 1").await;
    assert!(matches!(
        lib.people
            .assign_speaker(speaker, PersonId::new(), at(1))
            .await,
        Err(PersistenceError::Conflict)
    ));
    let person = lib
        .people
        .create(&PersonDraft::named("Priya"), at(2))
        .await
        .unwrap();
    assert!(matches!(
        lib.people
            .assign_speaker(SpeakerId::new(), person.id, at(3))
            .await,
        Err(PersistenceError::RecordNotFound)
    ));
    // Clearing a link that is not there is not a silent success.
    assert!(matches!(
        lib.people.clear_assignment(speaker, at(4)).await,
        Err(PersistenceError::RecordNotFound)
    ));
}
