//! The resumable speaker-matching pipeline against a real migrated library
//! (`VOICE_PROFILES_SPEC` §4, VP-020..VP-026, VP-100..VP-105; ADR-0082).
//!
//! Everything below the contract boundary is scripted — the deterministic
//! diarization provider of `plaudon-ai-diarization` and the scripted embedder
//! of `plaudon-app-core` — and everything above it is production code: real
//! checkpoints in a real `job_checkpoints` table, real AEAD sealing under a
//! real `SecretScope::VoiceProfileKey`, real suggestion rows.
//!
//! No audio, no model and no corpus is involved anywhere (§17.5).

use std::path::PathBuf;
use std::sync::Arc;

use plaudon_ai_contracts::job::{JobContext, RecordingProgressSink};
use plaudon_ai_diarization::fake::FakeDiarizationProvider;
use plaudon_ai_diarization::provider::RunPolicy;
use plaudon_ai_diarization::SpeakerTurn;
use plaudon_app_core::voice_profiles::embedder::{ScriptedEmbedder, SpeakerEmbedder};
use plaudon_app_core::voice_profiles::pipeline::{
    GateRefusal, MatchGate, SpeakerMatchError, SpeakerMatchPipeline, SpeakerMatchPlan,
    SpeakerMatchRequest, SpeakerMatchStage, CONSENT_DISCLOSURE_VERSION,
};
use plaudon_app_core::voice_profiles::store::{EmbeddingCipher, ExemplarBinding};
use plaudon_domain::ids::{JobId, PersonId, RecordingId};
use plaudon_domain::media::Sha256Digest;
use plaudon_domain::model::{EngineId, ModelIdentity};
use plaudon_domain::time::Timestamp;
use plaudon_domain::transcript::{
    AlignmentOrigin, RevisionOrigin, RevisionStatus, TranscriptRevision, TranscriptRevisionDraft,
    TranscriptSegment,
};
use plaudon_jobs::key::{JobKey, ProcessingJobKey, ProcessingStage};
use plaudon_jobs::scheduler::ResourceClass;
use plaudon_jobs::store::{EnqueueRequest, JobStore};
use plaudon_persistence::db::Database;
use plaudon_persistence::voice_profiles::{
    EnrollmentDraft, ExemplarInsert, VoiceEnrollmentRepository,
};
use plaudon_security::secrets::{InMemorySecretStore, SecretStore};
use tokio_util::sync::CancellationToken;

/// Dimension of the scripted checkpoint. Small: the arithmetic is the
/// matching core's business, exercised in `voice_matching.rs`.
const DIM: usize = 8;

/// Audio digest of the fixture recording.
fn audio_sha() -> String {
    "ab".repeat(32)
}

struct Library {
    _dir: tempfile::TempDir,
    db: Database,
    jobs: JobStore,
    cipher: EmbeddingCipher,
    recording: RecordingId,
    revision: TranscriptRevision,
    media_path: PathBuf,
}

/// Six segments of two seconds each: `S1` speaks the first three, `S2` the
/// next two, and the last is a span both are present in.
const SEGMENT_COUNT: u32 = 6;
const SEGMENT_MS: u64 = 2_000;

async fn library() -> Library {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("app.sqlite");
    JobStore::migrate(&path).await.expect("migrate");

    let db = Database::open(&path).await.expect("library database");
    let jobs = JobStore::open(
        plaudon_jobs::writer::Database::open(&path, 2)
            .await
            .expect("job database"),
    )
    .await
    .expect("job store");

    let recording = RecordingId::new();
    sqlx::query("INSERT INTO recordings (id, title, created_at) VALUES (?1, 'Weekly', 0)")
        .bind(recording.to_string())
        .execute(db.pool())
        .await
        .expect("recording");
    sqlx::query(
        "INSERT INTO media_assets (id, sha256, byte_length, relative_path, quality_class,
             verification_state, created_at)
         VALUES ('m1', ?1, 1, 'media/sha256/ab/x.wav', 'user_supplied_file', 'verified', 0)",
    )
    .bind(audio_sha())
    .execute(db.pool())
    .await
    .expect("media asset");

    let segments: Vec<TranscriptSegment> = (0..SEGMENT_COUNT)
        .map(|index| {
            let start = u64::from(index) * SEGMENT_MS;
            TranscriptSegment::timed(index, start, start + SEGMENT_MS, format!("segment {index}"))
        })
        .collect();
    let revision = TranscriptRevision::try_new(TranscriptRevisionDraft {
        recording_id: recording,
        parent_id: None,
        origin: RevisionOrigin::Machine,
        input_sha256: Sha256Digest::of(b"fixture-audio"),
        engine: EngineId::new("scripted"),
        model: ModelIdentity::new(EngineId::new("scripted"), "small"),
        locale: None,
        alignment_origin: AlignmentOrigin::Native,
        status: RevisionStatus::Complete,
        normalization_profile: "none".to_owned(),
        chunking_strategy: "single".to_owned(),
        duration_ms: Some(u64::from(SEGMENT_COUNT) * SEGMENT_MS),
        created_at: Timestamp::from_millis(0),
        segments,
    })
    .expect("fixture revision");

    sqlx::query(
        "INSERT INTO transcript_revisions (id, recording_id, origin, input_sha256, engine,
             model_id, alignment_origin, status, normalization_profile, chunking_strategy,
             duration_ms, created_at)
         VALUES (?1, ?2, 'machine', ?3, 'scripted', 'small', 'native', 'complete', 'none',
                 'single', ?4, 0)",
    )
    .bind(revision.id.to_string())
    .bind(recording.to_string())
    .bind(revision.input_sha256.to_hex())
    .bind(i64::try_from(u64::from(SEGMENT_COUNT) * SEGMENT_MS).unwrap())
    .execute(db.pool())
    .await
    .expect("revision row");
    for segment in revision.segments() {
        sqlx::query(
            "INSERT INTO transcript_segments (id, transcript_revision_id, seq_index, start_ms,
                 end_ms, text, alignment_origin)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'native')",
        )
        .bind(uuid::Uuid::now_v7().to_string())
        .bind(revision.id.to_string())
        .bind(i64::from(segment.seq_index))
        .bind(i64::try_from(segment.start.expect("timed").get()).unwrap())
        .bind(i64::try_from(segment.end.expect("timed").get()).unwrap())
        .bind(&segment.text)
        .execute(db.pool())
        .await
        .expect("segment row");
    }

    let secrets: Arc<dyn SecretStore> = Arc::new(InMemorySecretStore::new());
    Library {
        _dir: dir,
        cipher: EmbeddingCipher::new(secrets),
        recording,
        revision,
        media_path: dir_media_path(),
        db,
        jobs,
    }
}

/// The scripted embedder never opens the file, so the path only has to be a
/// path: no fixture audio is created, because none would be used.
fn dir_media_path() -> PathBuf {
    PathBuf::from("media/sha256/ab/x.wav")
}

impl Library {
    /// Turns: `S1` over segments 0-2, `S2` over segments 3-4, and both over the
    /// last segment, which is therefore overlap and must never be embedded
    /// (VP-022).
    fn turns(&self) -> Vec<SpeakerTurn> {
        vec![
            SpeakerTurn::new("S1", 0, 6_000),
            SpeakerTurn::new("S2", 6_000, 10_000),
            SpeakerTurn::new("S1", 10_000, 12_000),
            SpeakerTurn::new("S2", 10_000, 12_000),
        ]
    }

    async fn enqueue(&self) -> JobId {
        let key = ProcessingJobKey::new(
            audio_sha(),
            ProcessingStage::SpeakerMatch,
            "fake-deterministic",
            "scripted-deterministic",
        );
        self.jobs
            .enqueue(
                EnqueueRequest::new("speaker_match", JobKey::Processing(key), ResourceClass::Cpu)
                    .with_recording(self.recording),
            )
            .await
            .expect("enqueue")
            .job_id()
    }

    /// Enrol `person` with two exemplars of the scripted voice of `cluster`.
    async fn enrol(&self, embedder: &ScriptedEmbedder, cluster: &str) -> PersonId {
        let person = PersonId::new();
        sqlx::query(
            "INSERT INTO people (id, display_name, created_at, updated_at, status)
             VALUES (?1, 'Priya', 0, 0, 'active')",
        )
        .bind(person.to_string())
        .execute(self.db.pool())
        .await
        .expect("person");

        let voices = VoiceEnrollmentRepository::new(self.db.clone());
        voices
            .enroll(
                person,
                &EnrollmentDraft {
                    consent_version: CONSENT_DISCLOSURE_VERSION.to_owned(),
                    consent_note: None,
                    retention_months: 12,
                },
                Timestamp::from_millis(0),
            )
            .await
            .expect("enrolment");

        let checkpoint = embedder.checkpoint();
        let vector = embedder.voice_of(cluster);
        for index in 0..2 {
            let binding = ExemplarBinding {
                person_id: person,
                model_id: checkpoint.binding.model_id.clone(),
                model_version: checkpoint.binding.model_version.clone(),
                embedding_dim: u32::try_from(DIM).unwrap(),
            };
            let sealed = self.cipher.seal(&vector, &binding).expect("seal");
            voices
                .add_exemplar(
                    &ExemplarInsert {
                        person_id: person,
                        recording_id: None,
                        diarization_run_id: None,
                        cluster_key: None,
                        source_start_ms: None,
                        source_end_ms: None,
                        speech_ms: 6_000,
                        model_id: checkpoint.binding.model_id.clone(),
                        model_version: checkpoint.binding.model_version.clone(),
                        model_sha256: checkpoint.binding.model_sha256.to_hex(),
                        embedding_dim: sealed.embedding_dim,
                        embedding_cipher: sealed.cipher,
                        embedding_nonce: sealed.nonce,
                    },
                    Timestamp::from_millis(i64::from(index)),
                )
                .await
                .expect("exemplar");
        }
        person
    }

    fn pipeline(&self, embedder: Arc<ScriptedEmbedder>) -> SpeakerMatchPipeline {
        SpeakerMatchPipeline::new(
            self.jobs.clone(),
            self.db.clone(),
            self.cipher.clone(),
            Arc::new(FakeDiarizationProvider::new(self.turns())),
            embedder,
        )
    }

    fn request(&self, job_id: JobId, gate: MatchGate) -> SpeakerMatchRequest {
        SpeakerMatchRequest {
            job_id,
            recording_id: self.recording,
            media_asset_id: "m1".to_owned(),
            media_path: self.media_path.clone(),
            audio_sha256: audio_sha(),
            duration_ms: Some(u64::from(SEGMENT_COUNT) * SEGMENT_MS),
            revision: self.revision.clone(),
            gate,
            policy: RunPolicy::default_posture(),
            speaker_count_hint: None,
        }
    }

    async fn suggestion_rows(&self) -> Vec<(String, String, String, f64)> {
        sqlx::query_as(
            "SELECT cluster_key, person_id, state, score FROM speaker_suggestions
             ORDER BY cluster_key, person_id",
        )
        .fetch_all(self.db.pool())
        .await
        .expect("suggestions")
    }

    async fn count(&self, sql: &'static str) -> i64 {
        sqlx::query_scalar(sql)
            .fetch_one(self.db.pool())
            .await
            .expect("count")
    }
}

/// The gate a user who turned matching on and enrolled somebody is behind.
fn open_gate() -> MatchGate {
    MatchGate {
        matching_enabled: true,
        accepted_disclosure_version: Some(CONSENT_DISCLOSURE_VERSION.to_owned()),
        enrolled_profiles: 1,
        key_available: true,
    }
}

/// A context whose cancellation token the caller holds, so a test can stop a
/// run at an exact point: the scripted embedder cancels it after a known
/// number of windows, which is the in-process stand-in for a kill. The
/// out-of-process version of the same cases lives in `tests/chaos`.
fn cancellable_context(job_id: JobId, token: CancellationToken) -> JobContext {
    JobContext {
        job_id,
        cancel: token,
        progress: Arc::new(RecordingProgressSink::new()),
    }
}

fn plain_context(job_id: JobId) -> JobContext {
    JobContext::new(job_id)
}

#[tokio::test]
async fn an_enrolled_voice_becomes_a_suggestion_and_nothing_is_assigned() {
    let library = library().await;
    let embedder = Arc::new(ScriptedEmbedder::new(DIM));
    let person = library.enrol(&embedder, "S1").await;
    let job_id = library.enqueue().await;
    let pipeline = library.pipeline(Arc::clone(&embedder));

    let outcome = pipeline
        .run(
            &library.request(job_id, open_gate()),
            &plain_context(job_id),
        )
        .await
        .expect("the pipeline runs");

    assert_eq!(outcome.clusters, 2, "two clusters were diarized");
    assert_eq!(outcome.embedded_clusters, 2);
    assert_eq!(outcome.profiles_scored, 1);
    assert!(outcome.refusal.is_none());
    assert!(!outcome.resumed());

    let rows = library.suggestion_rows().await;
    assert_eq!(rows.len(), 1, "only the enrolled voice scored: {rows:?}");
    let (cluster, person_id, state, score) = &rows[0];
    assert_eq!(cluster, "S1", "the suggestion names the matching cluster");
    assert_eq!(person_id, &person.to_string());
    assert_eq!(state, "pending", "a match is a suggestion, never a label");
    assert!(score.is_finite());

    // ADR-0082: no score at any value writes a name.
    assert_eq!(
        library
            .count("SELECT COUNT(*) FROM speakers WHERE name IS NOT NULL")
            .await,
        0,
        "no speaker row was named by the job"
    );
    assert_eq!(
        library
            .count("SELECT COUNT(*) FROM speakers WHERE user_confirmed = 1")
            .await,
        0
    );

    // The biometric scratch does not outlive the job that needed it.
    assert_eq!(
        library
            .count("SELECT COUNT(*) FROM speaker_match_cluster_embeddings")
            .await,
        0
    );
    assert_eq!(
        library
            .count("SELECT COUNT(*) FROM speaker_match_reports")
            .await,
        0
    );
}

#[tokio::test]
async fn overlapping_speech_never_reaches_the_embedder() {
    let library = library().await;
    let embedder = Arc::new(ScriptedEmbedder::new(DIM));
    library.enrol(&embedder, "S1").await;
    let job_id = library.enqueue().await;

    library
        .pipeline(Arc::clone(&embedder))
        .run(
            &library.request(job_id, open_gate()),
            &plain_context(job_id),
        )
        .await
        .expect("the pipeline runs");

    // The last segment (10_000..12_000) is attributed to both clusters, so no
    // window may start inside it (VP-022).
    for window in embedder.windows() {
        assert!(
            window.end_ms <= 10_000,
            "a window overlapped the two-speaker span: {window:?}"
        );
    }
}

#[tokio::test]
async fn a_kill_between_embed_and_match_resumes_at_match() {
    let library = library().await;
    // The fixture produces five windows; cancelling on the fifth stops the run
    // after the embed stage committed and before the match stage starts —
    // exactly where a kill between the two leaves it.
    let token = CancellationToken::new();
    let embedder = Arc::new(ScriptedEmbedder::new(DIM).cancelling_after(5, token.clone()));
    library.enrol(&embedder, "S1").await;
    let job_id = library.enqueue().await;
    let pipeline = library.pipeline(Arc::clone(&embedder));

    let interrupted = pipeline
        .run(
            &library.request(job_id, open_gate()),
            &cancellable_context(job_id, token),
        )
        .await;
    assert!(
        matches!(interrupted, Err(SpeakerMatchError::Cancelled)),
        "expected the interrupted run to stop: {interrupted:?}"
    );
    let embedded_before = embedder.call_count();
    assert!(embedded_before > 0, "the embed stage did run");
    assert_eq!(
        library
            .count("SELECT COUNT(*) FROM speaker_match_cluster_embeddings")
            .await,
        2,
        "the embed stage's output is durable"
    );
    assert_eq!(library.suggestion_rows().await.len(), 0);

    // The same job, resumed.
    let outcome = library
        .pipeline(Arc::clone(&embedder))
        .run(
            &library.request(job_id, open_gate()),
            &plain_context(job_id),
        )
        .await
        .expect("the resumed run finishes");

    assert_eq!(
        embedder.call_count(),
        embedded_before,
        "resuming re-ran the embedder instead of reusing its committed output"
    );
    assert_eq!(
        outcome.resumed_stages,
        vec![
            SpeakerMatchStage::Diarize,
            SpeakerMatchStage::Reconcile,
            SpeakerMatchStage::Embed
        ],
        "the first three stages were skipped, the rest ran"
    );
    assert_eq!(library.suggestion_rows().await.len(), 1);
    assert_eq!(
        library.count("SELECT COUNT(*) FROM diarization_runs").await,
        1,
        "the resumed job reused its diarization run"
    );
}

#[tokio::test]
async fn cancelling_during_embedding_stops_at_a_window_boundary_and_commits_nothing() {
    let library = library().await;
    // Cancel after one window: the loop stops at the next window boundary.
    let token = CancellationToken::new();
    let embedder = Arc::new(ScriptedEmbedder::new(DIM).cancelling_after(1, token.clone()));
    library.enrol(&embedder, "S1").await;
    let job_id = library.enqueue().await;
    let pipeline = library.pipeline(Arc::clone(&embedder));
    let ctx = cancellable_context(job_id, token);
    let outcome = pipeline
        .run(&library.request(job_id, open_gate()), &ctx)
        .await;

    assert!(
        matches!(outcome, Err(SpeakerMatchError::Cancelled)),
        "expected cancellation, got {outcome:?}"
    );
    assert_eq!(
        embedder.call_count(),
        1,
        "cancellation waited for more than the window it was raised in"
    );
    assert_eq!(
        library
            .count("SELECT COUNT(*) FROM speaker_match_cluster_embeddings")
            .await,
        0,
        "an uncommitted embed stage parked nothing"
    );
    assert_eq!(library.suggestion_rows().await.len(), 0);

    pipeline
        .discard_scratch(job_id)
        .await
        .expect("a cancelled job's scratch can be discarded");
}

#[tokio::test]
async fn a_crash_before_the_suggest_commit_mark_reinserts_nothing_and_keeps_a_decision() {
    let library = library().await;
    let embedder = Arc::new(ScriptedEmbedder::new(DIM));
    library.enrol(&embedder, "S1").await;
    let job_id = library.enqueue().await;

    library
        .pipeline(Arc::clone(&embedder))
        .run(
            &library.request(job_id, open_gate()),
            &plain_context(job_id),
        )
        .await
        .expect("the first run finishes");
    let rows = library.suggestion_rows().await;
    assert_eq!(rows.len(), 1);

    // The user settles the suggestion the job produced.
    let suggestion_id: String = sqlx::query_scalar("SELECT id FROM speaker_suggestions LIMIT 1")
        .fetch_one(library.db.pool())
        .await
        .expect("suggestion id");
    VoiceEnrollmentRepository::new(library.db.clone())
        .confirm_suggestion(
            suggestion_id.parse().expect("id"),
            Timestamp::from_millis(9),
        )
        .await
        .expect("confirm");

    // The state a crash between the insert and the commit mark leaves: rows
    // written, no commit mark, so the resumed job runs the stage again.
    sqlx::query("DELETE FROM job_checkpoints WHERE job_id = ?1 AND stage = 'suggest'")
        .bind(job_id.to_string())
        .execute(library.db.pool())
        .await
        .expect("drop the commit mark");

    library
        .pipeline(Arc::clone(&embedder))
        .run(
            &library.request(job_id, open_gate()),
            &plain_context(job_id),
        )
        .await
        .expect("the resumed run finishes");

    let rows = library.suggestion_rows().await;
    assert_eq!(
        rows.len(),
        1,
        "the unique key made the re-insert idempotent"
    );
    assert_eq!(
        rows[0].2, "confirmed",
        "a resumed job never resurrects a decision the user already made"
    );
}

#[tokio::test]
async fn with_the_gate_closed_nothing_is_embedded_and_the_plan_has_no_biometric_stages() {
    let library = library().await;
    let embedder = Arc::new(ScriptedEmbedder::new(DIM));
    library.enrol(&embedder, "S1").await;
    let job_id = library.enqueue().await;

    let gate = MatchGate {
        matching_enabled: false,
        ..open_gate()
    };
    let plan = SpeakerMatchPlan::for_gate(&gate);
    assert_eq!(plan.stages(), SpeakerMatchStage::WITHOUT_MATCHING);
    assert!(!plan.contains(SpeakerMatchStage::Embed));
    assert!(!plan.contains(SpeakerMatchStage::Match));
    assert_eq!(plan.refusal(), Some(&GateRefusal::MatchingOff));

    let outcome = library
        .pipeline(Arc::clone(&embedder))
        .run(&library.request(job_id, gate), &plain_context(job_id))
        .await
        .expect("the pipeline runs the non-biometric stages");

    assert_eq!(outcome.refusal, Some(GateRefusal::MatchingOff));
    assert_eq!(outcome.embedded_clusters, 0);
    assert_eq!(
        embedder.call_count(),
        0,
        "no embedding was computed, not even transiently (VP-003)"
    );
    assert_eq!(library.suggestion_rows().await.len(), 0);
    // Diarization and reconciliation are ordinary work and still happened.
    assert_eq!(
        library.count("SELECT COUNT(*) FROM diarization_runs").await,
        1
    );
    assert!(
        library
            .count("SELECT COUNT(*) FROM segment_speaker_assignments")
            .await
            > 0
    );
}

#[tokio::test]
async fn with_nobody_enrolled_the_biometric_stages_are_not_planned() {
    let library = library().await;
    let embedder = Arc::new(ScriptedEmbedder::new(DIM));
    let job_id = library.enqueue().await;

    let gate = MatchGate {
        enrolled_profiles: 0,
        ..open_gate()
    };
    assert_eq!(
        SpeakerMatchPlan::for_gate(&gate).refusal(),
        Some(&GateRefusal::NoEnrolledProfiles)
    );

    let outcome = library
        .pipeline(Arc::clone(&embedder))
        .run(&library.request(job_id, gate), &plain_context(job_id))
        .await
        .expect("the pipeline runs");
    assert_eq!(outcome.refusal, Some(GateRefusal::NoEnrolledProfiles));
    assert_eq!(embedder.call_count(), 0);
}

#[tokio::test]
async fn a_profile_from_another_checkpoint_is_not_scored() {
    let library = library().await;
    // Enrol under version 1.0, match under version 2.0: two checkpoints, never
    // comparable (VP-026, VP-075).
    let enrolled_with = Arc::new(ScriptedEmbedder::new(DIM));
    library.enrol(&enrolled_with, "S1").await;
    let matching_with = Arc::new(ScriptedEmbedder::versioned(DIM, "2.0"));
    let job_id = library.enqueue().await;

    let outcome = library
        .pipeline(Arc::clone(&matching_with))
        .run(
            &library.request(job_id, open_gate()),
            &plain_context(job_id),
        )
        .await
        .expect("the pipeline runs");

    assert_eq!(outcome.profiles_scored, 0);
    assert_eq!(outcome.profiles_skipped_wrong_checkpoint, 1);
    assert_eq!(
        library.suggestion_rows().await.len(),
        0,
        "a profile from another checkpoint produced no score"
    );
}

#[tokio::test]
async fn a_cluster_without_enough_clean_speech_is_reported_not_embedded() {
    let library = library().await;
    let embedder = Arc::new(ScriptedEmbedder::new(DIM));
    library.enrol(&embedder, "S1").await;
    let job_id = library.enqueue().await;

    // `S3` speaks for two seconds, below the three-second identification
    // minimum of VP-065.
    let pipeline = SpeakerMatchPipeline::new(
        library.jobs.clone(),
        library.db.clone(),
        library.cipher.clone(),
        Arc::new(FakeDiarizationProvider::new(vec![
            SpeakerTurn::new("S1", 0, 6_000),
            SpeakerTurn::new("S3", 6_000, 8_000),
        ])),
        Arc::clone(&embedder) as Arc<dyn SpeakerEmbedder>,
    );
    let outcome = pipeline
        .run(
            &library.request(job_id, open_gate()),
            &plain_context(job_id),
        )
        .await
        .expect("the pipeline runs");

    assert_eq!(outcome.embedded_clusters, 1);
    assert_eq!(outcome.skipped_clusters.len(), 1);
    let skipped = &outcome.skipped_clusters[0];
    assert_eq!(skipped.cluster_key, "S3");
    assert_eq!(skipped.required_ms, 3_000);
    assert!(skipped.clean_speech_ms < 3_000);
    for window in embedder.windows() {
        assert_ne!(
            window.cluster_key, "S3",
            "a cluster below the minimum was embedded anyway"
        );
    }
}
