//! Schema-level contract tests for the spec 13.2 and 13.3A invariants.
//!
//! These assert the constraints the database itself enforces, so an
//! application bug cannot write a state the schema is supposed to forbid.

use plaudon_persistence::db::Database;
use plaudon_persistence::migrations::MigrationRunner;
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
