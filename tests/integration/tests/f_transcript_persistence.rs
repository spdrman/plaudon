//! Transcript revision persistence (spec §8.5, §8.6, §8.6A — task F2.4).
//!
//! These tests exercise the real SQLite schema through the real migration
//! runner: no in-memory substitute, because the invariants being checked are
//! schema constraints.

use plaudon_ai_contracts::stt::{TranscriptDraft, TranscriptSegmentDraft};
use plaudon_ai_contracts::transcript::{
    AnnotationKind, DraftWordAlignment, SourceTime, TimePrecision, TranscriptAnnotation,
};
use plaudon_ai_contracts::{AlignmentOrigin, ChunkingStrategy, NormalizationProfile};
use plaudon_domain::ids::RecordingId;
use plaudon_domain::media::Sha256Digest;
use plaudon_domain::model::{EngineId, ModelIdentity};
use plaudon_domain::time::Timestamp;
use plaudon_domain::transcript::{Confidence, ConfidenceScale, RevisionOrigin};
use plaudon_persistence::migrations::MigrationRunner;
use plaudon_persistence::transcript::{EncryptedProvenance, RevisionContext, TranscriptRepository};
use plaudon_persistence::Database;

const RATE: u32 = 16_000;

async fn open_library() -> (tempfile::TempDir, Database, RecordingId) {
    let dir = tempfile::tempdir().expect("tempdir");
    let database = Database::open(dir.path().join("app.sqlite"))
        .await
        .expect("open database");
    MigrationRunner::new()
        .run(&database)
        .await
        .expect("migrate");

    let recording_id = RecordingId::new();
    sqlx::query("INSERT INTO recordings (id, title, created_at) VALUES (?1, ?2, ?3)")
        .bind(recording_id.to_string())
        .bind("Weekly sync")
        .bind(0_i64)
        .execute(database.pool())
        .await
        .expect("insert recording");
    (dir, database, recording_id)
}

fn time(sample_offset: u64) -> SourceTime {
    SourceTime::new(sample_offset, RATE, TimePrecision::Segment)
}

fn engine() -> EngineId {
    EngineId::new("whisper.cpp")
}

fn native_draft() -> TranscriptDraft {
    let mut first = TranscriptSegmentDraft::timed(0, time(0), time(RATE.into()), "good morning");
    first.confidence = Some(Confidence::new(
        engine(),
        ConfidenceScale::AverageLogProbability,
        -0.21,
    ));
    first.words.push(DraftWordAlignment {
        text: "good".into(),
        start: SourceTime::new(0, RATE, TimePrecision::Word),
        end: SourceTime::new(8_000, RATE, TimePrecision::Word),
        confidence: Some(Confidence::new(
            engine(),
            ConfidenceScale::Probability,
            0.93,
        )),
    });
    first.words.push(DraftWordAlignment {
        text: "morning".into(),
        start: SourceTime::new(8_000, RATE, TimePrecision::Word),
        end: SourceTime::new(16_000, RATE, TimePrecision::Word),
        confidence: None,
    });
    let second = TranscriptSegmentDraft::timed(
        1,
        time(u64::from(RATE)),
        time(u64::from(RATE) * 2),
        "the budget is approved",
    );

    let mut draft = TranscriptDraft::native(
        engine(),
        ModelIdentity::new(engine(), "ggml-tiny.en").with_version("v1"),
        Sha256Digest::of(b"source bytes"),
        vec![first, second],
    );
    draft.duration_ms = Some(2_000);
    draft.backend = Some("metal".into());
    draft.chunking_strategy = ChunkingStrategy::Overlap {
        chunk_samples: 480_000,
        overlap_samples: 80_000,
    };
    draft.normalization_profile = Some(NormalizationProfile {
        sample_rate_hz: RATE,
        channel_policy: "downmix_mono".into(),
        sample_format: "f32le".into(),
        pcm_sha256: Sha256Digest::of(b"pcm"),
    });
    draft.engine_params.insert("threads".into(), "4".into());
    draft.engine_params.insert("beam_size".into(), "1".into());
    draft.annotations.push(TranscriptAnnotation {
        kind: AnnotationKind::SuspectedNonSpeech,
        reason: "text claimed over a region measured as 0% speech".into(),
        segment_index: 1,
        start: Some(time(u64::from(RATE))),
        end: Some(time(u64::from(RATE) * 2)),
    });
    draft
}

#[tokio::test]
async fn a_revision_persists_every_spec_8_5_field() {
    let (_dir, database, recording_id) = open_library().await;
    let repository = TranscriptRepository::new(database);
    let draft = native_draft();

    let id = repository
        .save_draft(
            &draft,
            &RevisionContext::machine(recording_id, Timestamp::from_millis(10)),
        )
        .await
        .expect("save");

    let stored = repository.revision(id).await.expect("load");
    assert_eq!(stored.recording_id, recording_id);
    assert_eq!(stored.input_sha256, Sha256Digest::of(b"source bytes"));
    assert_eq!(stored.engine, "whisper.cpp");
    assert_eq!(stored.model_id, "ggml-tiny.en");
    assert_eq!(stored.model_version.as_deref(), Some("v1"));
    assert_eq!(stored.alignment_origin, AlignmentOrigin::Native);
    assert_eq!(stored.normalization_profile, "16000hz/downmix_mono/f32le");
    assert_eq!(stored.chunking_strategy, "overlap");
    assert_eq!(stored.engine_backend.as_deref(), Some("metal"));
    assert_eq!(stored.duration_ms, Some(2_000));

    let params: serde_json::Value =
        serde_json::from_str(&stored.engine_params_json.expect("params")).expect("json");
    assert_eq!(params["threads"], "4");
    assert_eq!(params["beam_size"], "1");

    // Detailed provenance is off by default (§8.5).
    assert!(!stored.has_provenance);
}

#[tokio::test]
async fn segments_round_trip_ordered_with_times_confidence_and_words() {
    let (_dir, database, recording_id) = open_library().await;
    let repository = TranscriptRepository::new(database);
    let draft = native_draft();
    let id = repository
        .save_draft(
            &draft,
            &RevisionContext::machine(recording_id, Timestamp::from_millis(10)),
        )
        .await
        .expect("save");

    let segments = repository.segments(id).await.expect("segments");
    assert_eq!(segments.len(), 2);
    assert_eq!(
        segments.iter().map(|s| s.seq_index).collect::<Vec<_>>(),
        vec![0, 1]
    );
    assert_eq!(segments[0].start_ms, Some(0));
    assert_eq!(segments[0].end_ms, Some(1_000));
    assert_eq!(segments[0].text, "good morning");
    assert_eq!(
        segments[0].confidence_scale.as_deref(),
        Some("average_log_probability"),
        "the scale must survive so nothing compares it across vendors"
    );
    assert!((segments[0].confidence.expect("confidence") + 0.21).abs() < 1e-9);

    assert_eq!(segments[0].words.len(), 2);
    assert_eq!(segments[0].words[0].text, "good");
    assert_eq!(segments[0].words[0].start_ms, 0);
    assert_eq!(segments[0].words[0].end_ms, 500);
    assert_eq!(segments[0].words[1].confidence, None);
    assert_eq!(segments[1].words.len(), 0);

    let annotations = repository.annotations(id).await.expect("annotations");
    assert_eq!(annotations.len(), 1);
    assert_eq!(annotations[0].kind, AnnotationKind::SuspectedNonSpeech);
    assert_eq!(annotations[0].segment_index, 1);
}

#[tokio::test]
async fn a_text_only_revision_stores_no_seek_times() {
    let (_dir, database, recording_id) = open_library().await;
    let repository = TranscriptRepository::new(database);
    let draft = TranscriptDraft::text_only(
        engine(),
        ModelIdentity::new(engine(), "text-only"),
        Sha256Digest::of(b"x"),
        vec![
            TranscriptSegmentDraft::text_only(0, "no timeline here"),
            TranscriptSegmentDraft::text_only(1, "still none"),
        ],
    );

    let id = repository
        .save_draft(
            &draft,
            &RevisionContext::machine(recording_id, Timestamp::from_millis(1)),
        )
        .await
        .expect("save");

    let stored = repository.revision(id).await.expect("load");
    assert_eq!(stored.alignment_origin, AlignmentOrigin::None);
    for segment in repository.segments(id).await.expect("segments") {
        assert_eq!(segment.start_ms, None);
        assert_eq!(segment.end_ms, None);
        assert_eq!(segment.words.len(), 0);
    }
}

#[tokio::test]
async fn the_schema_refuses_a_time_under_alignment_origin_none() {
    // The guard is not only in application code: §8.6A is a CHECK constraint,
    // so no future writer can bypass it.
    let (_dir, database, recording_id) = open_library().await;
    let repository = TranscriptRepository::new(database.clone());
    let draft = TranscriptDraft::text_only(
        engine(),
        ModelIdentity::new(engine(), "text-only"),
        Sha256Digest::of(b"x"),
        vec![TranscriptSegmentDraft::text_only(0, "text")],
    );
    let id = repository
        .save_draft(
            &draft,
            &RevisionContext::machine(recording_id, Timestamp::from_millis(1)),
        )
        .await
        .expect("save");

    let result = sqlx::query(
        "INSERT INTO transcript_segments
             (id, transcript_revision_id, seq_index, start_ms, end_ms, text, alignment_origin)
         VALUES ('forged', ?1, 99, 0, 1000, 'invented', 'none')",
    )
    .bind(id.to_string())
    .execute(database.pool())
    .await;
    assert!(
        result.is_err(),
        "the database accepted a seek time on a text-only revision"
    );
}

#[tokio::test]
async fn re_transcription_adds_a_revision_and_leaves_the_prior_one_intact() {
    let (_dir, database, recording_id) = open_library().await;
    let repository = TranscriptRepository::new(database);

    let first = repository
        .save_draft(
            &native_draft(),
            &RevisionContext::machine(recording_id, Timestamp::from_millis(10)),
        )
        .await
        .expect("first");

    // A human edit of the first revision.
    let mut edited = native_draft();
    edited.segments[0].text = "Good morning, everyone.".into();
    let human = repository
        .save_draft(
            &edited,
            &RevisionContext {
                parent_id: Some(first),
                origin: RevisionOrigin::HumanEdit,
                ..RevisionContext::machine(recording_id, Timestamp::from_millis(20))
            },
        )
        .await
        .expect("human edit");

    // Re-transcribing with a different model must not disturb either.
    let mut other_model = native_draft();
    other_model.model = ModelIdentity::new(engine(), "ggml-base.en").with_version("v1");
    let reprocessed = repository
        .save_draft(
            &other_model,
            &RevisionContext::machine(recording_id, Timestamp::from_millis(30)),
        )
        .await
        .expect("reprocess");

    let all = repository.revisions_for(recording_id).await.expect("list");
    assert_eq!(all, vec![first, human, reprocessed]);

    assert_eq!(
        repository.segments(first).await.expect("first")[0].text,
        "good morning"
    );
    assert_eq!(
        repository.segments(human).await.expect("human")[0].text,
        "Good morning, everyone.",
        "the human edit must survive reprocessing"
    );
    assert_eq!(
        repository
            .revision(reprocessed)
            .await
            .expect("new")
            .model_id,
        "ggml-base.en"
    );
}

#[tokio::test]
async fn opt_in_provenance_is_stored_as_ciphertext_only() {
    let (_dir, database, recording_id) = open_library().await;
    let repository = TranscriptRepository::new(database.clone());

    let id = repository
        .save_draft(
            &native_draft(),
            &RevisionContext {
                provenance: Some(EncryptedProvenance {
                    ciphertext: b"\x9f\x1c\x00opaque".to_vec(),
                    nonce: vec![1, 2, 3, 4],
                    scheme: "xchacha20poly1305".into(),
                }),
                ..RevisionContext::machine(recording_id, Timestamp::from_millis(10))
            },
        )
        .await
        .expect("save");

    assert!(repository.revision(id).await.expect("load").has_provenance);

    // Nothing credential-shaped reaches the file: the column is a BLOB of
    // ciphertext and the scheme label, never a response body.
    let row: (Vec<u8>, String) = sqlx::query_as(
        "SELECT raw_provenance_cipher, raw_provenance_scheme
         FROM transcript_revisions WHERE id = ?1",
    )
    .bind(id.to_string())
    .fetch_one(database.pool())
    .await
    .expect("row");
    assert_eq!(row.0, b"\x9f\x1c\x00opaque");
    assert_eq!(row.1, "xchacha20poly1305");

    let text = String::from_utf8_lossy(&row.0);
    for secret in ["Bearer", "sk-", "Authorization", "api_key"] {
        assert!(!text.contains(secret), "provenance leaked `{secret}`");
    }
}

#[tokio::test]
async fn deleting_a_recording_leaves_no_dangling_transcript_rows() {
    let (_dir, database, recording_id) = open_library().await;
    let repository = TranscriptRepository::new(database.clone());
    repository
        .save_draft(
            &native_draft(),
            &RevisionContext::machine(recording_id, Timestamp::from_millis(10)),
        )
        .await
        .expect("save");

    let removed = repository
        .delete_revisions_for(recording_id)
        .await
        .expect("delete");
    assert_eq!(removed, 1);

    for (table, query) in [
        (
            "transcript_revisions",
            "SELECT count(*) FROM transcript_revisions",
        ),
        (
            "transcript_segments",
            "SELECT count(*) FROM transcript_segments",
        ),
        ("transcript_words", "SELECT count(*) FROM transcript_words"),
        (
            "transcript_annotations",
            "SELECT count(*) FROM transcript_annotations",
        ),
    ] {
        let count: i64 = sqlx::query_scalar(query)
            .fetch_one(database.pool())
            .await
            .expect("count");
        assert_eq!(count, 0, "{table} still has rows after deletion");
    }
}

#[tokio::test]
async fn saving_is_atomic() {
    // A draft whose annotation points past the end of the segment list is
    // rejected by the CHECK on `segment_index`? No — it is a valid index, so
    // instead force a constraint failure with a duplicate seq_index and prove
    // the revision row did not survive it.
    let (_dir, database, recording_id) = open_library().await;
    let repository = TranscriptRepository::new(database.clone());

    let mut draft = native_draft();
    draft.segments[1].seq_index = 0; // duplicate index violates UNIQUE
    let result = repository
        .save_draft(
            &draft,
            &RevisionContext::machine(recording_id, Timestamp::from_millis(10)),
        )
        .await;
    assert!(
        result.is_err(),
        "a duplicate segment index must be rejected"
    );

    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM transcript_revisions")
        .fetch_one(database.pool())
        .await
        .expect("count");
    assert_eq!(count, 0, "a failed save left a revision behind");
}
