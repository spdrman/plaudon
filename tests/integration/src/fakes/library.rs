//! A seeded library on disk, for the export, archive and deletion suites.
//!
//! The fixture is deliberately rich enough to exercise every rule the suites
//! assert: two media candidates of different fidelity, three transcript
//! revisions with `native`, `derived` and `none` alignment, a generated
//! output with evidence anchors in all three verification states, an
//! inferred and a confirmed speaker, tags, a user annotation and a source
//! association. It also seeds credential-shaped values in the places a
//! credential could plausibly end up, so the secret-exclusion suite has
//! something real to miss.

use std::path::{Path, PathBuf};

use plaudon_domain::ids::{MediaAssetId, RecordingId, TranscriptRevisionId};
use plaudon_domain::media::{MediaIdentity, Sha256Digest};
use plaudon_persistence::archive::{relative_path, MediaArchive};
use plaudon_persistence::migrations::MigrationRunner;
use plaudon_persistence::Database;
use uuid::Uuid;

/// An OpenRouter-shaped key seeded into credential storage locations.
pub const SEEDED_OPENROUTER_KEY: &str =
    "sk-or-v1-0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcd";
/// A PLAUD-cloud-shaped session token seeded into a source cursor.
pub const SEEDED_PLAUD_TOKEN: &str =
    "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJwbGF1ZCIsImV4cCI6OTk5OTk5OTk5OX0.c2lnbmF0dXJlLXZhbHVlLWhlcmU";
/// A signed download URL seeded into a job configuration snapshot.
pub const SEEDED_SIGNED_URL: &str =
    "https://files.plaud.example/audio/abc.wav?X-Amz-Signature=deadbeefdeadbeefdeadbeefdeadbeefdeadbeef&X-Amz-Expires=86400";
/// A device secret seeded into a vault reference row.
pub const SEEDED_DEVICE_SECRET: &str =
    "fe0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcd";

/// A library seeded on disk.
#[derive(Debug)]
pub struct SeededLibrary {
    /// Library root containing `app.sqlite`, `media/` and `temp/`.
    pub root: PathBuf,
    /// Open database handle.
    pub db: Database,
    /// Media archive over `root`.
    pub archive: MediaArchive,
    /// The seeded recording.
    pub recording_id: RecordingId,
    /// Revision with engine-reported times.
    pub native_revision: TranscriptRevisionId,
    /// Revision produced by a labelled alignment pass.
    pub derived_revision: TranscriptRevisionId,
    /// Text-only revision.
    pub text_only_revision: TranscriptRevisionId,
    /// Original-fidelity media candidate.
    pub original_asset: MediaAssetId,
    /// Compressed device-transfer candidate.
    pub compressed_asset: MediaAssetId,
    /// Bytes of the original candidate.
    pub original_bytes: Vec<u8>,
    /// Identity of the original candidate.
    pub original_identity: MediaIdentity,
}

/// Open an empty, migrated library at `root`.
pub async fn empty_library(root: &Path) -> (Database, MediaArchive) {
    std::fs::create_dir_all(root).expect("library root");
    let db = Database::open(root.join("app.sqlite"))
        .await
        .expect("open library");
    MigrationRunner::new().run(&db).await.expect("migrate");
    let archive = MediaArchive::open(root).expect("open archive");
    (db, archive)
}

/// A minimal RIFF/WAVE file carrying a metadata chunk with a serial number.
#[must_use]
pub fn wav_with_serial(sample_count: usize, serial: &str) -> Vec<u8> {
    let fmt: [u8; 16] = [
        1, 0, 1, 0, 0x44, 0xAC, 0, 0, 0x88, 0x58, 0x01, 0, 2, 0, 16, 0,
    ];
    let data: Vec<u8> = (0..sample_count)
        .flat_map(|index| ((index % 4096) as u16).to_le_bytes())
        .collect();
    let mut info = Vec::new();
    info.extend_from_slice(b"INFOISRF");
    info.extend_from_slice(&u32::try_from(serial.len()).unwrap().to_le_bytes());
    info.extend_from_slice(serial.as_bytes());
    if info.len() % 2 == 1 {
        info.push(0);
    }

    let mut body = Vec::new();
    body.extend_from_slice(b"WAVE");
    body.extend_from_slice(b"fmt ");
    body.extend_from_slice(&u32::try_from(fmt.len()).unwrap().to_le_bytes());
    body.extend_from_slice(&fmt);
    body.extend_from_slice(b"LIST");
    body.extend_from_slice(&u32::try_from(info.len()).unwrap().to_le_bytes());
    body.extend_from_slice(&info);
    body.extend_from_slice(b"data");
    body.extend_from_slice(&u32::try_from(data.len()).unwrap().to_le_bytes());
    body.extend_from_slice(&data);

    let mut out = Vec::new();
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&u32::try_from(body.len()).unwrap().to_le_bytes());
    out.extend_from_slice(&body);
    out
}

async fn insert_media(
    db: &Database,
    archive: &MediaArchive,
    bytes: &[u8],
    extension: &str,
    quality_class: &str,
    created_at: i64,
) -> (MediaAssetId, MediaIdentity, String) {
    let identity = MediaIdentity::of_bytes(bytes, extension);
    let relative = relative_path(&identity, extension);
    let absolute = archive.absolute(&relative).expect("archive path");
    std::fs::create_dir_all(absolute.parent().expect("parent")).expect("create dir");
    std::fs::write(&absolute, bytes).expect("write blob");

    let id = MediaAssetId::new();
    sqlx::query(
        "INSERT INTO media_assets
             (id, sha256, byte_length, relative_path, mime, codec, sample_rate, channels,
              duration_ms, quality_class, verification_state, verified_at, created_at)
         VALUES (?1, ?2, ?3, ?4, 'audio/wav', 'pcm_s16le', 44100, 1, 1000, ?5, 'verified', ?6, ?6)",
    )
    .bind(id.to_string())
    .bind(identity.sha256.to_hex())
    .bind(i64::try_from(identity.byte_length).unwrap())
    .bind(&relative)
    .bind(quality_class)
    .bind(created_at)
    .execute(db.pool())
    .await
    .expect("insert media asset");
    (id, identity, relative)
}

/// Seed the full fixture library at `root`.
///
/// `with_secrets` additionally writes credential-shaped values into the rows
/// an archive must never carry.
pub async fn seed(root: &Path, with_secrets: bool) -> SeededLibrary {
    let (db, archive) = empty_library(root).await;
    let recording_id = RecordingId::new();

    let original_bytes = wav_with_serial(512, "PLAUD-SERIAL-A1B2C3");
    let compressed_bytes = wav_with_serial(64, "PLAUD-SERIAL-A1B2C3");
    let (original_asset, original_identity, _) = insert_media(
        &db,
        &archive,
        &original_bytes,
        "wav",
        "original_device_recording",
        1_700_000_000_000,
    )
    .await;
    let (compressed_asset, _, _) = insert_media(
        &db,
        &archive,
        &compressed_bytes,
        "asr",
        "compressed_device_transfer",
        1_700_000_001_000,
    )
    .await;

    sqlx::query(
        "INSERT INTO recordings
             (id, title, captured_at, duration_ms, preferred_media_asset_id, annotation,
              created_at, deleted_at)
         VALUES (?1, ?2, 1700000000000, 12000, ?3, ?4, 1700000000000, NULL)",
    )
    .bind(recording_id.to_string())
    .bind("Weekly sync: Q3 \"plan\" / review")
    .bind(original_asset.to_string())
    .bind("Remember to send the deck. Budget was not specified.")
    .execute(db.pool())
    .await
    .expect("insert recording");

    for tag in ["project-apollo", "weekly"] {
        sqlx::query("INSERT INTO recording_tags (recording_id, tag) VALUES (?1, ?2)")
            .bind(recording_id.to_string())
            .bind(tag)
            .execute(db.pool())
            .await
            .expect("insert tag");
    }

    for (asset, quality, obtained) in [
        (
            original_asset,
            "original_device_recording",
            1_700_000_000_000i64,
        ),
        (
            compressed_asset,
            "compressed_device_transfer",
            1_700_000_001_000,
        ),
    ] {
        sqlx::query(
            "INSERT INTO recording_media (recording_id, media_asset_id, quality_class, obtained_at)
             VALUES (?1, ?2, ?3, ?4)",
        )
        .bind(recording_id.to_string())
        .bind(asset.to_string())
        .bind(quality)
        .bind(obtained)
        .execute(db.pool())
        .await
        .expect("attach media");
    }

    // A source association, so provenance survives a round trip.
    let source_id = Uuid::now_v7().to_string();
    sqlx::query(
        "INSERT INTO sources
             (id, source_kind, account_scope, display_name, status, capabilities_json,
              last_verified_at, sync_cursor, sync_policy, created_at)
         VALUES (?1, 'local_file', 'local', 'Imported files', 'ready', '{}', NULL, ?2, 'manual', 1700000000000)",
    )
    .bind(&source_id)
    .bind(if with_secrets {
        Some(SEEDED_PLAUD_TOKEN)
    } else {
        None
    })
    .execute(db.pool())
    .await
    .expect("insert source");

    sqlx::query(
        "INSERT INTO source_items
             (id, source_id, external_id, recording_id, import_status, last_seen_at,
              remote_version, remote_state, tombstone_reason, created_at)
         VALUES (?1, ?2, 'external-7', ?3, 'imported', 1700000000000, 'v1', 'present', NULL, 1700000000000)",
    )
    .bind(Uuid::now_v7().to_string())
    .bind(&source_id)
    .bind(recording_id.to_string())
    .execute(db.pool())
    .await
    .expect("insert source item");

    // Speakers: one inferred cluster, one user-confirmed name.
    let inferred_speaker = Uuid::now_v7().to_string();
    let confirmed_speaker = Uuid::now_v7().to_string();
    sqlx::query(
        "INSERT INTO speakers (id, recording_id, label, name, provenance, user_confirmed)
         VALUES (?1, ?2, 'Speaker 1', NULL, 'inferred', 0)",
    )
    .bind(&inferred_speaker)
    .bind(recording_id.to_string())
    .execute(db.pool())
    .await
    .expect("insert speaker");
    sqlx::query(
        "INSERT INTO speakers (id, recording_id, label, name, provenance, user_confirmed)
         VALUES (?1, ?2, 'Speaker 2', 'Dana Hill', 'user_confirmed', 1)",
    )
    .bind(&confirmed_speaker)
    .bind(recording_id.to_string())
    .execute(db.pool())
    .await
    .expect("insert speaker");

    let native_revision = seed_revision(
        &db,
        recording_id,
        &original_identity,
        "native",
        "machine",
        None,
        &[
            (0, Some((0, 2_500)), "Welcome to the weekly sync."),
            (
                1,
                Some((2_500, 6_000)),
                "Dana will send the deck on Friday.",
            ),
            (2, Some((6_000, 12_000)), "No budget was agreed today."),
        ],
        Some(&inferred_speaker),
        1_700_000_010_000,
    )
    .await;

    let text_only_revision = seed_revision(
        &db,
        recording_id,
        &original_identity,
        "none",
        "machine",
        None,
        &[
            (0, None, "Welcome to the weekly sync."),
            (1, None, "Dana will send the deck on Friday."),
        ],
        None,
        1_700_000_020_000,
    )
    .await;

    let derived_revision = seed_revision(
        &db,
        recording_id,
        &original_identity,
        "derived",
        "derived_alignment",
        Some(text_only_revision),
        &[
            (0, Some((0, 2_400)), "Welcome to the weekly sync."),
            (
                1,
                Some((2_400, 5_900)),
                "Dana will send the deck on Friday.",
            ),
        ],
        Some(&confirmed_speaker),
        1_700_000_030_000,
    )
    .await;

    // A generated output with anchors in all three verification states.
    let segment_ids: Vec<String> = sqlx::query_scalar::<_, String>(
        "SELECT id FROM transcript_segments WHERE transcript_revision_id = ?1 ORDER BY seq_index",
    )
    .bind(native_revision.to_string())
    .fetch_all(db.pool())
    .await
    .expect("segments");

    let output_id = Uuid::now_v7().to_string();
    let body = serde_json::json!({
        "summary": "The team reviewed the Q3 plan.",
        "action_items": [
            {"verb": "send", "object": "the deck", "owner": "Dana Hill",
             "due_date": "not_specified", "status": "open"}
        ],
        "decisions": [
            {"statement": "No budget agreed", "participants": "unknown",
             "time_reference": "not_specified"}
        ]
    });
    sqlx::query(
        "INSERT INTO ai_outputs
             (id, recording_id, output_type, transcript_revision_id, template_version, model_id,
              prompt_version, schema_version, body_json, status, created_at)
         VALUES (?1, ?2, 'meeting_summary', ?3, 'general-meeting/1', 'fixture-model',
                 'prompt/1', 'meeting-summary/1', ?4, 'needs_review', 1700000040000)",
    )
    .bind(&output_id)
    .bind(recording_id.to_string())
    .bind(native_revision.to_string())
    .bind(body.to_string())
    .execute(db.pool())
    .await
    .expect("insert output");

    for (index, (item_path, status)) in [
        ("/action_items/0/verb", "source_linked"),
        ("/decisions/0/statement", "human_confirmed"),
        ("/summary", "unverified"),
    ]
    .into_iter()
    .enumerate()
    {
        sqlx::query(
            "INSERT INTO output_evidence
                 (id, output_id, item_path, segment_id, quote, verification_status)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )
        .bind(Uuid::now_v7().to_string())
        .bind(&output_id)
        .bind(item_path)
        .bind(&segment_ids[index.min(segment_ids.len() - 1)])
        .bind("Dana will send the deck on Friday.")
        .bind(status)
        .execute(db.pool())
        .await
        .expect("insert evidence");
    }

    if with_secrets {
        seed_secrets(&db).await;
    }

    SeededLibrary {
        root: root.to_path_buf(),
        db,
        archive,
        recording_id,
        native_revision,
        derived_revision,
        text_only_revision,
        original_asset,
        compressed_asset,
        original_bytes,
        original_identity,
    }
}

/// `(index, optional (start, end) offsets, text)` for one seeded segment.
type SeedSegment<'a> = (u32, Option<(u64, u64)>, &'a str);

#[allow(clippy::too_many_arguments)]
async fn seed_revision(
    db: &Database,
    recording_id: RecordingId,
    input: &MediaIdentity,
    alignment_origin: &str,
    origin: &str,
    parent: Option<TranscriptRevisionId>,
    segments: &[SeedSegment<'_>],
    speaker: Option<&str>,
    created_at: i64,
) -> TranscriptRevisionId {
    let revision = TranscriptRevisionId::new();
    sqlx::query(
        "INSERT INTO transcript_revisions
             (id, recording_id, parent_id, origin, input_sha256, engine, model_id, model_version,
              locale, alignment_origin, status, normalization_profile, chunking_strategy,
              duration_ms, created_at, engine_backend, engine_params_json)
         VALUES (?1, ?2, ?3, ?4, ?5, 'whisper.cpp', 'base.en', '1.5.4', 'en-US', ?6, 'complete',
                 'default', 'none', 12000, ?7, 'cpu', '{\"beam_size\":5}')",
    )
    .bind(revision.to_string())
    .bind(recording_id.to_string())
    .bind(parent.map(|id| id.to_string()))
    .bind(origin)
    .bind(input.sha256.to_hex())
    .bind(alignment_origin)
    .bind(created_at)
    .execute(db.pool())
    .await
    .expect("insert revision");

    for (index, times, text) in segments {
        let segment_id = Uuid::now_v7().to_string();
        sqlx::query(
            "INSERT INTO transcript_segments
                 (id, transcript_revision_id, seq_index, start_ms, end_ms, speaker_id, text,
                  confidence, confidence_scale, alignment_origin)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        )
        .bind(&segment_id)
        .bind(revision.to_string())
        .bind(i64::from(*index))
        .bind(times.map(|(start, _)| i64::try_from(start).unwrap()))
        .bind(times.map(|(_, end)| i64::try_from(end).unwrap()))
        .bind(if *index == 1 { speaker } else { None })
        .bind(*text)
        .bind(times.map(|_| -0.25f64))
        .bind(times.map(|_| "average_log_probability"))
        .bind(alignment_origin)
        .execute(db.pool())
        .await
        .expect("insert segment");

        if let Some((start, end)) = times {
            for (word_index, word) in text.split_whitespace().enumerate() {
                let span = (end - start) / u64::try_from(text.split_whitespace().count()).unwrap();
                let word_start = start + span * u64::try_from(word_index).unwrap();
                sqlx::query(
                    "INSERT INTO transcript_words
                         (id, segment_id, seq_index, start_ms, end_ms, text, confidence,
                          confidence_scale)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, NULL)",
                )
                .bind(Uuid::now_v7().to_string())
                .bind(&segment_id)
                .bind(i64::try_from(word_index).unwrap())
                .bind(i64::try_from(word_start).unwrap())
                .bind(i64::try_from(word_start + span).unwrap())
                .bind(word)
                .execute(db.pool())
                .await
                .expect("insert word");
            }
        }
    }

    sqlx::query(
        "INSERT INTO transcript_annotations
             (id, transcript_revision_id, segment_index, kind, reason, start_ms, end_ms)
         VALUES (?1, ?2, 0, 'low_confidence', 'fixture flag', NULL, NULL)",
    )
    .bind(Uuid::now_v7().to_string())
    .bind(revision.to_string())
    .execute(db.pool())
    .await
    .expect("insert annotation");

    revision
}

/// Write credential-shaped values into every row an archive must not carry.
async fn seed_secrets(db: &Database) {
    let device_id = Uuid::now_v7().to_string();
    sqlx::query(
        "INSERT INTO devices (id, vendor, model, serial_fingerprint, discovered_name,
                              observed_firmware, first_seen_at, last_seen_at, trust_state)
         VALUES (?1, 'plaud', 'note', 'fp', 'PLAUD_NOTE', 'V0095', 1, 1, 'unverified')",
    )
    .bind(&device_id)
    .execute(db.pool())
    .await
    .expect("insert device");

    sqlx::query(
        "INSERT INTO device_credential_refs
             (device_id, vault_service, vault_account_reference, auth_mode, created_at,
              last_verified_at)
         VALUES (?1, 'ai.plaudon.plaudon', ?2, 'legacy_static_token', 1, 1)",
    )
    .bind(&device_id)
    .bind(SEEDED_DEVICE_SECRET)
    .execute(db.pool())
    .await
    .expect("insert credential ref");

    sqlx::query(
        "INSERT INTO jobs (id, job_type, state, priority, retry_count, job_key,
                           config_snapshot_json, input_hash, created_at)
         VALUES (?1, 'import', 'completed', 0, 0, NULL, ?2, NULL, 1)",
    )
    .bind(Uuid::now_v7().to_string())
    .bind(format!(
        "{{\"download_url\":\"{SEEDED_SIGNED_URL}\",\"api_key\":\"{SEEDED_OPENROUTER_KEY}\"}}"
    ))
    .execute(db.pool())
    .await
    .expect("insert job");

    sqlx::query(
        "INSERT INTO app_preferences (key, version, value_json) VALUES ('openrouter', 1, ?1)",
    )
    .bind(format!("{{\"key\":\"{SEEDED_OPENROUTER_KEY}\"}}"))
    .execute(db.pool())
    .await
    .expect("insert preference");
}

/// Digest of a file on disk.
#[must_use]
pub fn digest_of(path: &Path) -> Sha256Digest {
    Sha256Digest::of(&std::fs::read(path).expect("read file"))
}
