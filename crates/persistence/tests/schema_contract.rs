//! Schema-level contract tests for the spec 13.2 and 13.3A invariants.
//!
//! These assert the constraints the database itself enforces, so an
//! application bug cannot write a state the schema is supposed to forbid.

use plaudon_persistence::db::Database;
use plaudon_persistence::migrations::{Migration, MigrationRunner, MIGRATIONS};
use sqlx::Row;

/// Column names that would indicate secret material at rest. An opaque vault
/// *reference* is permitted and is named explicitly below.
const FORBIDDEN_COLUMN_FRAGMENTS: &[&str] = &[
    "token",
    "secret",
    "password",
    "passphrase",
    "api_key",
    "apikey",
    "bearer",
    "signed_url",
    "signature",
    "private_key",
    "session",
    "cookie",
    "credential",
];

/// Columns that name an opaque vault handle rather than secret material.
const ALLOWED_REFERENCE_COLUMNS: &[&str] = &[
    "vault_service",
    "vault_account_reference",
    "device_credential_refs",
];

async fn migrated() -> (tempfile::TempDir, Database) {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(dir.path().join("app.sqlite")).await.unwrap();
    MigrationRunner::new().run(&db).await.unwrap();
    (dir, db)
}

async fn table_names(db: &Database) -> Vec<String> {
    sqlx::query("SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name")
        .fetch_all(db.pool())
        .await
        .unwrap()
        .into_iter()
        .map(|row| row.get::<String, _>(0))
        .collect()
}

async fn column_names(db: &Database, table: &str) -> Vec<String> {
    sqlx::query(sqlx::AssertSqlSafe(format!("PRAGMA table_info({table})")))
        .fetch_all(db.pool())
        .await
        .unwrap()
        .into_iter()
        .map(|row| row.get::<String, _>("name"))
        .collect()
}

#[tokio::test]
async fn every_spec_13_2_core_table_exists() {
    let (_dir, db) = migrated().await;
    let tables = table_names(&db).await;
    for required in [
        "recordings",
        "sources",
        "source_items",
        "devices",
        "device_capabilities",
        "device_transfers",
        "media_assets",
        "recording_media",
        "media_revisions",
        "transcript_revisions",
        "transcript_segments",
        "speakers",
        "ai_outputs",
        "output_evidence",
        "jobs",
        "job_steps",
        "usage_events",
        "inference_attempts",
        "consent_grants",
        "app_preferences",
        "export_events",
        "schema_migrations",
        "import_journal",
        "tombstones",
        "device_credential_refs",
        "compatibility_evidence",
        // EPIC I: meeting intelligence and source grounding (migration 0500).
        "pipeline_stage_runs",
        "output_claims",
        "output_dependencies",
        "transcript_tools",
        "tool_runs",
    ] {
        assert!(
            tables.contains(&required.to_owned()),
            "missing table {required}"
        );
    }
}

#[tokio::test]
async fn no_table_stores_a_token_a_signed_url_or_key_material() {
    let (_dir, db) = migrated().await;
    for table in table_names(&db).await {
        for column in column_names(&db, &table).await {
            let lower = column.to_ascii_lowercase();
            if ALLOWED_REFERENCE_COLUMNS.contains(&lower.as_str()) {
                continue;
            }
            for needle in FORBIDDEN_COLUMN_FRAGMENTS {
                assert!(
                    !lower.contains(needle),
                    "{table}.{column} looks like secret material at rest (spec 13.1)"
                );
            }
        }
    }
}

#[tokio::test]
async fn duplicate_media_identities_are_rejected() {
    let (_dir, db) = migrated().await;
    let insert = |id: &str| {
        let sql = "INSERT INTO media_assets (id, sha256, byte_length, relative_path, quality_class, verification_state, created_at)
                   VALUES (?1, ?2, 10, ?3, 'user_supplied_file', 'verified', 0)";
        sqlx::query(sql)
            .bind(id.to_owned())
            .bind("a".repeat(64))
            .bind(format!("media/sha256/aa/{id}.wav"))
            .execute(db.pool())
    };
    insert("asset-1").await.unwrap();
    let error = insert("asset-2").await.unwrap_err();
    assert!(
        error.to_string().to_lowercase().contains("unique"),
        "{error}"
    );
}

#[tokio::test]
async fn external_ids_are_unique_per_source_but_null_repeats_freely() {
    let (_dir, db) = migrated().await;
    sqlx::query("INSERT INTO sources (id, source_kind, account_scope, display_name, status, created_at) VALUES ('s1','plaud_cloud_cli','a','A','configured',0)")
        .execute(db.pool()).await.unwrap();
    for n in 1..=3 {
        sqlx::query("INSERT INTO recordings (id, title, created_at) VALUES (?1, 'r', 0)")
            .bind(format!("r{n}"))
            .execute(db.pool())
            .await
            .unwrap();
    }
    let insert = |id: &str, recording: &str, external: Option<&str>| {
        sqlx::query("INSERT INTO source_items (id, source_id, external_id, recording_id, import_status, created_at) VALUES (?1,'s1',?2,?3,'imported',0)")
            .bind(id.to_owned())
            .bind(external.map(str::to_owned))
            .bind(recording.to_owned())
            .execute(db.pool())
    };
    insert("i1", "r1", Some("ext")).await.unwrap();
    assert!(
        insert("i2", "r2", Some("ext")).await.is_err(),
        "duplicate external id must fail"
    );
    insert("i3", "r2", None).await.unwrap();
    insert("i4", "r3", None).await.unwrap();
}

#[tokio::test]
async fn deleting_a_source_never_cascades_into_media_or_transcripts() {
    let (_dir, db) = migrated().await;
    sqlx::query("INSERT INTO sources (id, source_kind, account_scope, display_name, status, created_at) VALUES ('s1','plaud_cloud_cli','a','A','configured',0)")
        .execute(db.pool()).await.unwrap();
    sqlx::query("INSERT INTO recordings (id, title, created_at) VALUES ('r1','r',0)")
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO source_items (id, source_id, external_id, recording_id, import_status, created_at) VALUES ('i1','s1','ext','r1','imported',0)")
        .execute(db.pool()).await.unwrap();
    sqlx::query("INSERT INTO media_assets (id, sha256, byte_length, relative_path, quality_class, verification_state, created_at) VALUES ('m1', ?1, 1, 'media/sha256/aa/x.wav','user_supplied_file','verified',0)")
        .bind("b".repeat(64)).execute(db.pool()).await.unwrap();
    sqlx::query("INSERT INTO transcript_revisions (id, recording_id, origin, input_sha256, engine, model_id, alignment_origin, status, normalization_profile, chunking_strategy, created_at) VALUES ('t1','r1','machine',?1,'whisper-cpp','base.en','none','complete','nfc','single',0)")
        .bind("b".repeat(64)).execute(db.pool()).await.unwrap();

    // A source still referenced by an item cannot simply disappear.
    assert!(sqlx::query("DELETE FROM sources WHERE id = 's1'")
        .execute(db.pool())
        .await
        .is_err());

    sqlx::query("DELETE FROM source_items WHERE id = 'i1'")
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("DELETE FROM sources WHERE id = 's1'")
        .execute(db.pool())
        .await
        .unwrap();

    let media: i64 = sqlx::query("SELECT COUNT(*) FROM media_assets")
        .fetch_one(db.pool())
        .await
        .unwrap()
        .get(0);
    let transcripts: i64 = sqlx::query("SELECT COUNT(*) FROM transcript_revisions")
        .fetch_one(db.pool())
        .await
        .unwrap()
        .get(0);
    assert_eq!(media, 1);
    assert_eq!(transcripts, 1);
}

#[tokio::test]
async fn a_text_only_segment_cannot_carry_a_seek_time() {
    let (_dir, db) = migrated().await;
    sqlx::query("INSERT INTO recordings (id, title, created_at) VALUES ('r1','r',0)")
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO transcript_revisions (id, recording_id, origin, input_sha256, engine, model_id, alignment_origin, status, normalization_profile, chunking_strategy, created_at) VALUES ('t1','r1','machine',?1,'e','m','none','complete','nfc','single',0)")
        .bind("c".repeat(64)).execute(db.pool()).await.unwrap();

    let bad = sqlx::query("INSERT INTO transcript_segments (id, transcript_revision_id, seq_index, start_ms, end_ms, text, alignment_origin) VALUES ('s1','t1',0,0,100,'hello','none')")
        .execute(db.pool()).await;
    assert!(bad.is_err(), "alignment_origin none must forbid times");

    sqlx::query("INSERT INTO transcript_segments (id, transcript_revision_id, seq_index, text, alignment_origin) VALUES ('s1','t1',0,'hello','none')")
        .execute(db.pool()).await.unwrap();

    let inverted = sqlx::query("INSERT INTO transcript_segments (id, transcript_revision_id, seq_index, start_ms, end_ms, text, alignment_origin) VALUES ('s2','t1',1,500,100,'x','native')")
        .execute(db.pool()).await;
    assert!(inverted.is_err(), "an inverted range must be rejected");

    let duplicate_index = sqlx::query("INSERT INTO transcript_segments (id, transcript_revision_id, seq_index, text, alignment_origin) VALUES ('s3','t1',0,'again','none')")
        .execute(db.pool()).await;
    assert!(
        duplicate_index.is_err(),
        "segment indices are unique within a revision"
    );
}

#[tokio::test]
async fn an_unverified_transfer_cannot_be_marked_verified() {
    let (_dir, db) = migrated().await;
    sqlx::query("INSERT INTO devices (id, vendor, model, first_seen_at, last_seen_at) VALUES ('d1','PLAUD','Note',0,0)")
        .execute(db.pool()).await.unwrap();
    let bad = sqlx::query("INSERT INTO device_transfers (id, device_id, transport, received_bytes, transfer_status, verification_status, started_at) VALUES ('x','d1','ble',10,'interrupted','verified',0)")
        .execute(db.pool()).await;
    assert!(
        bad.is_err(),
        "an incomplete transfer must never be verified"
    );
}

#[tokio::test]
async fn an_unknown_cost_cannot_masquerade_as_an_amount() {
    let (_dir, db) = migrated().await;
    sqlx::query(
        "INSERT INTO jobs (id, job_type, state, created_at) VALUES ('j1','transcribe','queued',0)",
    )
    .execute(db.pool())
    .await
    .unwrap();
    let bad = sqlx::query("INSERT INTO usage_events (id, job_id, provider, model_id, estimated_cost, cost_confidence, created_at) VALUES ('u1','j1','openrouter','m','0.00','unknown',0)")
        .execute(db.pool()).await;
    assert!(bad.is_err(), "an unknown cost must not carry an amount");

    let also_bad = sqlx::query("INSERT INTO usage_events (id, job_id, provider, model_id, cost_confidence, created_at) VALUES ('u2','j1','openrouter','m','finalized',0)")
        .execute(db.pool()).await;
    assert!(also_bad.is_err(), "a finalized cost must carry an amount");
}

#[tokio::test]
async fn an_inferred_speaker_cannot_claim_to_be_confirmed() {
    let (_dir, db) = migrated().await;
    sqlx::query("INSERT INTO recordings (id, title, created_at) VALUES ('r1','r',0)")
        .execute(db.pool())
        .await
        .unwrap();
    let bad = sqlx::query("INSERT INTO speakers (id, recording_id, label, provenance, user_confirmed) VALUES ('sp1','r1','Speaker 1','inferred',1)")
        .execute(db.pool()).await;
    assert!(bad.is_err());
    sqlx::query("INSERT INTO speakers (id, recording_id, label, provenance, user_confirmed) VALUES ('sp1','r1','Speaker 1','inferred',0)")
        .execute(db.pool()).await.unwrap();
}

#[tokio::test]
async fn a_committed_journal_row_must_point_at_an_asset() {
    let (_dir, db) = migrated().await;
    let bad = sqlx::query("INSERT INTO import_journal (id, state, staging_path, target_relative_path, expected_sha256, expected_byte_length, quality_class, created_at, updated_at) VALUES ('ij1','committed','/tmp/x.part','media/sha256/aa/x.wav', ?1, 1, 'user_supplied_file', 0, 0)")
        .bind("d".repeat(64))
        .execute(db.pool()).await;
    assert!(bad.is_err(), "a committed import must reference its asset");
}

#[tokio::test]
async fn job_keys_are_unique_when_present() {
    let (_dir, db) = migrated().await;
    let insert = |id: &str, key: Option<&str>| {
        sqlx::query("INSERT INTO jobs (id, job_type, state, job_key, created_at) VALUES (?1,'transcribe','queued',?2,0)")
            .bind(id.to_owned())
            .bind(key.map(str::to_owned))
            .execute(db.pool())
    };
    insert("j1", Some("k")).await.unwrap();
    assert!(insert("j2", Some("k")).await.is_err());
    insert("j3", None).await.unwrap();
    insert("j4", None).await.unwrap();
}

#[tokio::test]
async fn foreign_keys_are_enforced_on_the_pooled_connections_the_tests_use() {
    let (_dir, db) = migrated().await;
    assert!(db.foreign_keys_enabled().await.unwrap());
    assert_eq!(db.journal_mode().await.unwrap().to_ascii_lowercase(), "wal");
    let orphan = sqlx::query("INSERT INTO source_items (id, source_id, recording_id, import_status, created_at) VALUES ('x','missing','missing','imported',0)")
        .execute(db.pool()).await;
    assert!(orphan.is_err(), "a dangling foreign key must be rejected");
}

#[tokio::test]
async fn the_required_indices_exist_verbatim() {
    let (_dir, db) = migrated().await;
    let indices: Vec<String> = sqlx::query("SELECT name FROM sqlite_master WHERE type = 'index'")
        .fetch_all(db.pool())
        .await
        .unwrap()
        .into_iter()
        .map(|row| row.get::<String, _>(0))
        .collect();
    for required in [
        "ux_media_sha",
        "ux_source_external",
        "ix_segments_revision_time",
        "ux_inference_attempt",
    ] {
        assert!(
            indices.contains(&required.to_owned()),
            "missing index {required}"
        );
    }
}

// ---------------------------------------------------------------------------
// Voice profiles (migration 0900): the roster and its biometric tier
// (VOICE_PROFILES_SPEC VP-070..VP-080, ADR-0080, ADR-0082).
// ---------------------------------------------------------------------------

/// A recording, a media asset, a diarization run and one speaker row — the
/// pre-existing shape every voice-profile row hangs off.
async fn recording_with_a_speaker(db: &Database) {
    sqlx::query("INSERT INTO recordings (id, title, created_at) VALUES ('r1','Weekly',0)")
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO media_assets (id, sha256, byte_length, relative_path, quality_class, verification_state, created_at) VALUES ('m1', ?1, 1, 'media/sha256/aa/x.wav','user_supplied_file','verified',0)")
        .bind("e".repeat(64))
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO diarization_runs (id, recording_id, media_asset_id, provider_id, privacy_class, status, input_sha256, created_at) VALUES ('run1','r1','m1','local','local_only','succeeded',?1,0)")
        .bind("e".repeat(64))
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO speakers (id, recording_id, label, name, provenance, user_confirmed, cluster_key, original_label) VALUES ('sp1','r1','Priya','Priya','user_confirmed',1,'c0','Speaker 1')")
        .execute(db.pool())
        .await
        .unwrap();
}

async fn person(db: &Database, id: &str, name: &str) {
    sqlx::query("INSERT INTO people (id, display_name, created_at, updated_at, status) VALUES (?1, ?2, 0, 0, 'active')")
        .bind(id.to_owned())
        .bind(name.to_owned())
        .execute(db.pool())
        .await
        .unwrap();
}

async fn enrollment(db: &Database, person_id: &str) {
    sqlx::query("INSERT INTO person_voice_enrollments (person_id, consent_version, retention_months, status, created_at) VALUES (?1,'v1',12,'active',0)")
        .bind(person_id.to_owned())
        .execute(db.pool())
        .await
        .unwrap();
}

async fn exemplar(db: &Database, id: &str, person_id: &str) {
    sqlx::query("INSERT INTO voice_exemplars (id, person_id, recording_id, diarization_run_id, cluster_key, speech_ms, model_id, model_version, model_sha256, embedding_dim, embedding_cipher, embedding_nonce, created_at) VALUES (?1, ?2, 'r1', 'run1', 'c0', 4000, 'wespeaker-resnet34-lm', '1.0', ?3, 192, ?4, ?5, 0)")
        .bind(id.to_owned())
        .bind(person_id.to_owned())
        .bind("f".repeat(64))
        .bind(vec![7u8; 32])
        .bind(vec![9u8; 24])
        .execute(db.pool())
        .await
        .unwrap();
}

async fn suggestion(db: &Database, id: &str, person_id: &str, cluster: &str) {
    sqlx::query("INSERT INTO speaker_suggestions (id, recording_id, diarization_run_id, cluster_key, person_id, score, score_kind, band, state, model_id, model_version, created_at) VALUES (?1,'r1','run1',?2,?3,0.87,'cosine_similarity_asnorm','suggest','pending','wespeaker-resnet34-lm','1.0',0)")
        .bind(id.to_owned())
        .bind(cluster.to_owned())
        .bind(person_id.to_owned())
        .execute(db.pool())
        .await
        .unwrap();
}

async fn count(db: &Database, sql: &str) -> i64 {
    sqlx::query(sqlx::AssertSqlSafe(sql.to_owned()))
        .fetch_one(db.pool())
        .await
        .unwrap()
        .get(0)
}

#[tokio::test]
async fn the_voice_profile_tables_and_the_speaker_link_exist() {
    let (_dir, db) = migrated().await;
    let tables = table_names(&db).await;
    for required in [
        "people",
        "person_voice_enrollments",
        "voice_exemplars",
        "speaker_suggestions",
        "voice_profile_events",
    ] {
        assert!(
            tables.contains(&required.to_owned()),
            "missing voice-profile table {required}"
        );
    }
    assert!(
        column_names(&db, "speakers")
            .await
            .contains(&"person_id".to_owned()),
        "speakers must carry the roster link (VP-072)"
    );

    let indices: Vec<String> = sqlx::query("SELECT name FROM sqlite_master WHERE type = 'index'")
        .fetch_all(db.pool())
        .await
        .unwrap()
        .into_iter()
        .map(|row| row.get::<String, _>(0))
        .collect();
    for required in [
        "ix_people_status_name",
        "ix_voice_exemplars_person",
        "ix_speaker_suggestions_person",
        "ix_voice_profile_events_person",
        "ix_speakers_person",
    ] {
        assert!(
            indices.contains(&required.to_owned()),
            "missing index {required}"
        );
    }
}

#[tokio::test]
async fn the_roster_tier_needs_nothing_from_the_biometric_tier() {
    // VP-011 / §12.2A: a name-only roster entry is complete on its own, and a
    // speaker row can be assigned to it without any enrolment existing.
    let (_dir, db) = migrated().await;
    recording_with_a_speaker(&db).await;
    person(&db, "p1", "Priya").await;
    sqlx::query("UPDATE speakers SET person_id = 'p1' WHERE id = 'sp1'")
        .execute(db.pool())
        .await
        .unwrap();

    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM person_voice_enrollments").await,
        0
    );
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM speakers WHERE person_id = 'p1' AND name = 'Priya'"
        )
        .await,
        1
    );
}

#[tokio::test]
async fn an_exemplar_cannot_exist_without_an_enrolment() {
    let (_dir, db) = migrated().await;
    recording_with_a_speaker(&db).await;
    person(&db, "p1", "Priya").await;

    let orphan = sqlx::query("INSERT INTO voice_exemplars (id, person_id, speech_ms, model_id, model_version, model_sha256, embedding_dim, embedding_cipher, embedding_nonce, created_at) VALUES ('x1','p1',4000,'m','1',?1,192,?2,?3,0)")
        .bind("f".repeat(64))
        .bind(vec![7u8; 32])
        .bind(vec![9u8; 24])
        .execute(db.pool())
        .await;
    assert!(
        orphan.is_err(),
        "an embedding without a consented enrolment must be unrepresentable"
    );
}

#[tokio::test]
async fn the_biometric_tier_refuses_rows_that_break_its_own_rules() {
    let (_dir, db) = migrated().await;
    recording_with_a_speaker(&db).await;
    person(&db, "p1", "Priya").await;
    enrollment(&db, "p1").await;

    // Retention is bounded at 12 months and cannot be zero (VP-130).
    person(&db, "p2", "Sam").await;
    let too_long = sqlx::query("INSERT INTO person_voice_enrollments (person_id, consent_version, retention_months, status, created_at) VALUES ('p2','v1',24,'active',0)")
        .execute(db.pool())
        .await;
    assert!(too_long.is_err(), "retention beyond 12 months is forbidden");

    // An exemplar below the enrolment minimum is unrepresentable (VP-064).
    let too_short = sqlx::query("INSERT INTO voice_exemplars (id, person_id, speech_ms, model_id, model_version, model_sha256, embedding_dim, embedding_cipher, embedding_nonce, created_at) VALUES ('x1','p1',400,'m','1',?1,192,?2,?3,0)")
        .bind("f".repeat(64))
        .bind(vec![7u8; 32])
        .bind(vec![9u8; 24])
        .execute(db.pool())
        .await;
    assert!(
        too_short.is_err(),
        "speech_ms below one second is forbidden"
    );

    // A nonce that is not an XChaCha20-Poly1305 nonce is unrepresentable.
    let bad_nonce = sqlx::query("INSERT INTO voice_exemplars (id, person_id, speech_ms, model_id, model_version, model_sha256, embedding_dim, embedding_cipher, embedding_nonce, created_at) VALUES ('x2','p1',4000,'m','1',?1,192,?2,?3,0)")
        .bind("f".repeat(64))
        .bind(vec![7u8; 32])
        .bind(vec![9u8; 12])
        .execute(db.pool())
        .await;
    assert!(bad_nonce.is_err(), "a 12-byte nonce must be rejected");
}

#[tokio::test]
async fn a_suggestion_is_a_score_with_an_honest_decision_state() {
    let (_dir, db) = migrated().await;
    recording_with_a_speaker(&db).await;
    person(&db, "p1", "Priya").await;
    person(&db, "p2", "Sam").await;
    enrollment(&db, "p1").await;
    suggestion(&db, "s1", "p1", "c0").await;

    // The same (run, cluster, person) cannot be suggested twice (VP-102).
    let duplicate = sqlx::query("INSERT INTO speaker_suggestions (id, recording_id, diarization_run_id, cluster_key, person_id, score, score_kind, band, state, model_id, model_version, created_at) VALUES ('s2','r1','run1','c0','p1',0.9,'cosine_similarity_asnorm','suggest','pending','m','1',0)")
        .execute(db.pool())
        .await;
    assert!(duplicate.is_err(), "suggestions are idempotent per cluster");

    // A pending row cannot claim it was decided, and a settled one must say when.
    let pending_decided =
        sqlx::query("UPDATE speaker_suggestions SET decided_at = 5 WHERE id = 's1'")
            .execute(db.pool())
            .await;
    assert!(pending_decided.is_err());
    let settled_without_time =
        sqlx::query("UPDATE speaker_suggestions SET state = 'confirmed' WHERE id = 's1'")
            .execute(db.pool())
            .await;
    assert!(settled_without_time.is_err());

    // A correction names somebody else, and only a correction may (VP-216).
    let correction_without_person = sqlx::query(
        "UPDATE speaker_suggestions SET state = 'corrected', decided_at = 5 WHERE id = 's1'",
    )
    .execute(db.pool())
    .await;
    assert!(correction_without_person.is_err());
    let self_correction = sqlx::query("UPDATE speaker_suggestions SET state = 'corrected', corrected_person_id = 'p1', decided_at = 5 WHERE id = 's1'")
        .execute(db.pool())
        .await;
    assert!(self_correction.is_err());
    sqlx::query("UPDATE speaker_suggestions SET state = 'corrected', corrected_person_id = 'p2', decided_at = 5 WHERE id = 's1'")
        .execute(db.pool())
        .await
        .unwrap();
}

#[tokio::test]
async fn deleting_an_enrolment_destroys_the_voiceprint_and_keeps_the_person() {
    // VP-080: "deleting an enrolment destroys its exemplars and leaves the
    // person as a name-only roster entry".
    let (_dir, db) = migrated().await;
    recording_with_a_speaker(&db).await;
    person(&db, "p1", "Priya").await;
    enrollment(&db, "p1").await;
    exemplar(&db, "x1", "p1").await;
    sqlx::query("UPDATE speakers SET person_id = 'p1' WHERE id = 'sp1'")
        .execute(db.pool())
        .await
        .unwrap();

    sqlx::query("DELETE FROM person_voice_enrollments WHERE person_id = 'p1'")
        .execute(db.pool())
        .await
        .unwrap();

    assert_eq!(count(&db, "SELECT COUNT(*) FROM voice_exemplars").await, 0);
    assert_eq!(count(&db, "SELECT COUNT(*) FROM people").await, 1);
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM speakers WHERE person_id = 'p1' AND name = 'Priya'"
        )
        .await,
        1,
        "the roster link and the name snapshot survive losing the voiceprint"
    );
}

#[tokio::test]
async fn deleting_a_person_severs_links_and_leaves_every_name_snapshot() {
    // VP-080 / VP-213d: the biometric rows go, the transcripts keep their labels.
    let (_dir, db) = migrated().await;
    recording_with_a_speaker(&db).await;
    person(&db, "p1", "Priya").await;
    enrollment(&db, "p1").await;
    exemplar(&db, "x1", "p1").await;
    suggestion(&db, "s1", "p1", "c0").await;
    sqlx::query("UPDATE speakers SET person_id = 'p1' WHERE id = 'sp1'")
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO voice_profile_events (id, person_id, kind, detail_json, created_at) VALUES ('e1','p1','enrolled','{\"consent_version\":\"v1\"}',0)")
        .execute(db.pool())
        .await
        .unwrap();

    sqlx::query("DELETE FROM people WHERE id = 'p1'")
        .execute(db.pool())
        .await
        .unwrap();

    assert_eq!(count(&db, "SELECT COUNT(*) FROM voice_exemplars").await, 0);
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM person_voice_enrollments").await,
        0
    );
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM speaker_suggestions").await,
        0
    );
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM speakers WHERE id = 'sp1' AND person_id IS NULL AND name = 'Priya'"
        )
        .await,
        1,
        "the name snapshot must outlive the roster entry"
    );
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM voice_profile_events").await,
        1,
        "the consent journal must outlive the person it describes (VP-080)"
    );
}

#[tokio::test]
async fn deleting_a_recording_keeps_the_exemplar_and_forgets_where_it_came_from() {
    // VP-080: an exemplar belongs to the enrolment, not to the recording;
    // suggestions about that recording go with it.
    let (_dir, db) = migrated().await;
    recording_with_a_speaker(&db).await;
    person(&db, "p1", "Priya").await;
    enrollment(&db, "p1").await;
    exemplar(&db, "x1", "p1").await;
    suggestion(&db, "s1", "p1", "c0").await;

    // The real deletion path (`DeletionService`) removes a recording's own
    // rows first; what matters here is what that does to the voice tier.
    sqlx::query("DELETE FROM speakers WHERE recording_id = 'r1'")
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("DELETE FROM diarization_runs WHERE recording_id = 'r1'")
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("DELETE FROM recording_media WHERE recording_id = 'r1'")
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("DELETE FROM recordings WHERE id = 'r1'")
        .execute(db.pool())
        .await
        .unwrap();

    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM voice_exemplars WHERE id = 'x1' AND recording_id IS NULL AND diarization_run_id IS NULL"
        )
        .await,
        1
    );
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM speaker_suggestions").await,
        0
    );
}

#[tokio::test]
async fn the_journal_accepts_only_known_kinds_and_json_objects() {
    let (_dir, db) = migrated().await;
    let bad_kind = sqlx::query("INSERT INTO voice_profile_events (id, person_id, kind, detail_json, created_at) VALUES ('e1','p1','voiceprint_sold','{}',0)")
        .execute(db.pool())
        .await;
    assert!(bad_kind.is_err());
    let bad_detail = sqlx::query("INSERT INTO voice_profile_events (id, person_id, kind, detail_json, created_at) VALUES ('e2','p1','person_created','not json',0)")
        .execute(db.pool())
        .await;
    assert!(bad_detail.is_err());
    sqlx::query("INSERT INTO voice_profile_events (id, person_id, kind, detail_json, created_at) VALUES ('e3','p1','person_created','{\"display_name\":\"Priya\"}',0)")
        .execute(db.pool())
        .await
        .unwrap();
}

#[tokio::test]
async fn migrating_a_populated_0800_library_backs_up_first_and_keeps_its_data() {
    // VP-074: the 0900 upgrade behaves like every other upgrade — a verified
    // pre-migration backup, then an additive schema change that touches no
    // existing row.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("app.sqlite");
    let db = Database::open(&path).await.unwrap();

    let through_0800: Vec<Migration> = MIGRATIONS
        .iter()
        .copied()
        .filter(|migration| migration.version <= 800)
        .collect();
    assert_eq!(
        through_0800.last().map(|m| m.version),
        Some(800),
        "this test models the library shipped before voice profiles"
    );
    MigrationRunner::with_migrations(through_0800.clone())
        .run(&db)
        .await
        .unwrap();
    recording_with_a_speaker(&db).await;

    let report = MigrationRunner::new().run(&db).await.unwrap();
    // 0900 brings the tier in; 0901 the match job's scratch; 0902 the
    // retention clock and the wholesale-erasure journal kind (VP-095, VP-130).
    assert_eq!(report.applied, vec![900, 901, 902]);
    let backup = report
        .backup_path
        .expect("an upgrade of a populated library must back up first");
    assert!(backup.exists(), "{} must exist", backup.display());

    // The backup is the previous schema, untouched by 0900.
    let restored = Database::open(&backup).await.unwrap();
    let before: Vec<String> = sqlx::query("PRAGMA table_info(speakers)")
        .fetch_all(restored.pool())
        .await
        .unwrap()
        .into_iter()
        .map(|row| row.get::<String, _>("name"))
        .collect();
    assert!(!before.contains(&"person_id".to_owned()));
    restored.close().await;

    // The upgraded library kept every row and gained the link column, NULL.
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM speakers WHERE id = 'sp1' AND name = 'Priya' AND person_id IS NULL"
        )
        .await,
        1
    );
    assert_eq!(count(&db, "SELECT COUNT(*) FROM recordings").await, 1);
    assert_eq!(count(&db, "SELECT COUNT(*) FROM diarization_runs").await, 1);
    assert_eq!(count(&db, "SELECT COUNT(*) FROM people").await, 0);

    // And a person can now be assigned in that upgraded library.
    person(&db, "p1", "Priya").await;
    sqlx::query("UPDATE speakers SET person_id = 'p1' WHERE id = 'sp1'")
        .execute(db.pool())
        .await
        .unwrap();
    db.close().await;
}
