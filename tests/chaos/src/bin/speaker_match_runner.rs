//! Runs the real resumable speaker-matching pipeline against a real library,
//! so the harness can kill the process at a checkpoint boundary and re-run it
//! (`VOICE_PROFILES_SPEC` VP-102, VP-105).
//!
//! Usage: `speaker-match-runner <db-path>`
//!
//! Every identifier is fixed and every seeding write is idempotent, so running
//! the binary twice over the same file is exactly "the job was killed and
//! started again": the second run finds the first run's committed checkpoints
//! and resumes from them.
//!
//! Hermetic by construction: the deterministic diarization provider, the
//! scripted embedder and a fixed-key in-memory credential store. No model, no
//! audio, no corpus, no network.

use std::sync::Arc;

use plaudon_ai_contracts::job::JobContext;
use plaudon_ai_diarization::provider::RunPolicy;
use plaudon_ai_diarization::{FakeDiarizationProvider, SpeakerTurn};
use plaudon_app_core::voice_profiles::embedder::{ScriptedEmbedder, SpeakerEmbedder};
use plaudon_app_core::voice_profiles::pipeline::{
    MatchGate, SpeakerMatchPipeline, SpeakerMatchRequest, CONSENT_DISCLOSURE_VERSION,
};
use plaudon_app_core::voice_profiles::store::{EmbeddingCipher, ExemplarBinding};
use plaudon_domain::ids::{JobId, PersonId, RecordingId, TranscriptRevisionId};
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
use plaudon_security::secrets::{InMemorySecretStore, SecretScope, SecretStore, SecretString};

/// Fixed identities, so a re-run is the same job over the same library.
const RECORDING: &str = "11111111-1111-4111-8111-111111111111";
const REVISION: &str = "22222222-2222-4222-8222-222222222222";
const PERSON: &str = "33333333-3333-4333-8333-333333333333";

/// The voice-profile data key both runs derive, so a ciphertext sealed by the
/// killed process opens in the one that resumes. A fixed test key, never a
/// production path: the real key is generated locally and lives in the OS
/// vault (VP-071).
const FIXED_KEY_HEX: &str = "7f6e5d4c3b2a19087f6e5d4c3b2a19087f6e5d4c3b2a19087f6e5d4c3b2a1908";

const DIM: usize = 8;
const SEGMENT_MS: u64 = 6_000;
const SEGMENTS: u32 = 4;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    assert_eq!(args.len(), 2, "usage: speaker-match-runner <db>");
    let path = args[1].clone();

    JobStore::migrate(&path).await.expect("migrate");
    let db = Database::open(&path).await.expect("open library");
    let jobs = JobStore::open(
        plaudon_jobs::writer::Database::open(&path, 2)
            .await
            .expect("job database"),
    )
    .await
    .expect("job store");

    let recording: RecordingId = RECORDING.parse().expect("recording id");
    let revision_id: TranscriptRevisionId = REVISION.parse().expect("revision id");
    let person: PersonId = PERSON.parse().expect("person id");

    seed_library(&db, recording, revision_id).await;

    let secrets: Arc<dyn SecretStore> = Arc::new(InMemorySecretStore::new());
    secrets
        .store(
            &SecretScope::VoiceProfileKey,
            &SecretString::new(FIXED_KEY_HEX),
        )
        .expect("seed the data key");
    let cipher = EmbeddingCipher::new(Arc::clone(&secrets));
    let embedder = Arc::new(ScriptedEmbedder::new(DIM));
    seed_enrolment(&db, &cipher, embedder.as_ref(), person).await;

    // The key is fixed, so the second process deduplicates onto the first
    // process's job row: that is what makes this a resume rather than a new
    // job (§14.2, VP-101).
    let job_id = enqueue(&jobs, recording).await;

    let pipeline = SpeakerMatchPipeline::new(
        jobs,
        db,
        cipher,
        Arc::new(FakeDiarizationProvider::new(vec![
            SpeakerTurn::new("S1", 0, 12_000),
            SpeakerTurn::new("S2", 12_000, 24_000),
        ])),
        Arc::clone(&embedder) as Arc<dyn SpeakerEmbedder>,
    );
    let request = SpeakerMatchRequest {
        job_id,
        recording_id: recording,
        media_asset_id: "m1".to_owned(),
        media_path: std::path::PathBuf::from("media/sha256/ab/x.wav"),
        audio_sha256: "ab".repeat(32),
        duration_ms: Some(u64::from(SEGMENTS) * SEGMENT_MS),
        revision: revision(recording, revision_id),
        gate: MatchGate {
            matching_enabled: true,
            accepted_disclosure_version: Some(CONSENT_DISCLOSURE_VERSION.to_owned()),
            enrolled_profiles: 1,
            key_available: true,
        },
        policy: RunPolicy::default_posture(),
        speaker_count_hint: None,
    };

    let outcome = pipeline
        .run(&request, &JobContext::new(job_id))
        .await
        .expect("the pipeline runs");
    println!(
        "PLAUDON_MATCH_FINISHED suggestions={} embedded={} resumed={}",
        outcome.suggestions_written,
        outcome.embedded_clusters,
        outcome.resumed_stages.len()
    );
}

/// The in-memory revision the pipeline reconciles against; its rows are seeded
/// with the same identities.
fn revision(recording: RecordingId, revision_id: TranscriptRevisionId) -> TranscriptRevision {
    let segments = (0..SEGMENTS)
        .map(|index| {
            let start = u64::from(index) * SEGMENT_MS;
            TranscriptSegment::timed(index, start, start + SEGMENT_MS, format!("segment {index}"))
        })
        .collect();
    let mut revision = TranscriptRevision::try_new(TranscriptRevisionDraft {
        recording_id: recording,
        parent_id: None,
        origin: RevisionOrigin::Machine,
        input_sha256: Sha256Digest::of(b"chaos-audio"),
        engine: EngineId::new("scripted"),
        model: ModelIdentity::new(EngineId::new("scripted"), "small"),
        locale: None,
        alignment_origin: AlignmentOrigin::Native,
        status: RevisionStatus::Complete,
        normalization_profile: "none".to_owned(),
        chunking_strategy: "single".to_owned(),
        duration_ms: Some(u64::from(SEGMENTS) * SEGMENT_MS),
        created_at: Timestamp::from_millis(0),
        segments,
    })
    .expect("fixture revision");
    revision.id = revision_id;
    revision
}

async fn seed_library(db: &Database, recording: RecordingId, revision_id: TranscriptRevisionId) {
    sqlx::query("INSERT OR IGNORE INTO recordings (id, title, created_at) VALUES (?1, 'Chaos', 0)")
        .bind(recording.to_string())
        .execute(db.pool())
        .await
        .expect("recording");
    sqlx::query(
        "INSERT OR IGNORE INTO media_assets (id, sha256, byte_length, relative_path,
             quality_class, verification_state, created_at)
         VALUES ('m1', ?1, 1, 'media/sha256/ab/x.wav', 'user_supplied_file', 'verified', 0)",
    )
    .bind("ab".repeat(32))
    .execute(db.pool())
    .await
    .expect("media asset");
    sqlx::query(
        "INSERT OR IGNORE INTO transcript_revisions (id, recording_id, origin, input_sha256,
             engine, model_id, alignment_origin, status, normalization_profile,
             chunking_strategy, duration_ms, created_at)
         VALUES (?1, ?2, 'machine', ?3, 'scripted', 'small', 'native', 'complete', 'none',
                 'single', ?4, 0)",
    )
    .bind(revision_id.to_string())
    .bind(recording.to_string())
    .bind(Sha256Digest::of(b"chaos-audio").to_hex())
    .bind(i64::try_from(u64::from(SEGMENTS) * SEGMENT_MS).unwrap())
    .execute(db.pool())
    .await
    .expect("revision");
    for index in 0..SEGMENTS {
        let start = u64::from(index) * SEGMENT_MS;
        sqlx::query(
            "INSERT OR IGNORE INTO transcript_segments (id, transcript_revision_id, seq_index,
                 start_ms, end_ms, text, alignment_origin)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'native')",
        )
        .bind(format!("segment-{index}"))
        .bind(revision_id.to_string())
        .bind(i64::from(index))
        .bind(i64::try_from(start).unwrap())
        .bind(i64::try_from(start + SEGMENT_MS).unwrap())
        .bind(format!("segment {index}"))
        .execute(db.pool())
        .await
        .expect("segment");
    }
}

async fn seed_enrolment(
    db: &Database,
    cipher: &EmbeddingCipher,
    embedder: &ScriptedEmbedder,
    person: PersonId,
) {
    let existing: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM person_voice_enrollments WHERE person_id = ?1")
            .bind(person.to_string())
            .fetch_one(db.pool())
            .await
            .expect("count");
    if existing > 0 {
        return;
    }
    sqlx::query(
        "INSERT OR IGNORE INTO people (id, display_name, created_at, updated_at, status)
         VALUES (?1, 'Priya', 0, 0, 'active')",
    )
    .bind(person.to_string())
    .execute(db.pool())
    .await
    .expect("person");

    let voices = VoiceEnrollmentRepository::new(db.clone());
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
    let vector = embedder.voice_of("S1");
    for index in 0..2u8 {
        let binding = ExemplarBinding {
            person_id: person,
            model_id: checkpoint.binding.model_id.clone(),
            model_version: checkpoint.binding.model_version.clone(),
            embedding_dim: u32::try_from(DIM).unwrap(),
        };
        let sealed = cipher.seal(&vector, &binding).expect("seal");
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
}

async fn enqueue(jobs: &JobStore, recording: RecordingId) -> JobId {
    let key = ProcessingJobKey::new(
        "ab".repeat(32),
        ProcessingStage::SpeakerMatch,
        "fake-deterministic",
        "scripted-deterministic",
    );
    jobs.enqueue(
        EnqueueRequest::new("speaker_match", JobKey::Processing(key), ResourceClass::Cpu)
            .with_recording(recording),
    )
    .await
    .expect("enqueue")
    .job_id()
}
