//! Diarization reconciliation and speaker labelling against a real library
//! (M1.5, M2.3; spec 8.7, 17.6).

use plaudon_ai_contracts::stt::{TranscriptDraft, TranscriptSegmentDraft};
use plaudon_ai_contracts::transcript::SourceTime;
use plaudon_ai_diarization::contract::SpeakerTurn;
use plaudon_app_core::diarization::{reconcile, result_from_turns, SegmentAssignment};
use plaudon_domain::ids::{RecordingId, TranscriptRevisionId};
use plaudon_domain::media::{MediaAsset, MediaIdentity, QualityClass, Sha256Digest};
use plaudon_domain::model::{EngineId, ModelIdentity};
use plaudon_domain::recording::Recording;
use plaudon_domain::time::Timestamp;
use plaudon_domain::transcript::{AlignmentOrigin, TranscriptRevision};
use plaudon_persistence::migrations::MigrationRunner;
use plaudon_persistence::repo::{MediaAssetRepository, RecordingRepository};
use plaudon_persistence::speakers::{
    AssignmentInsert, DiarizationRunInsert, SpeakerRepository, SpeakerTurnInsert,
};
use plaudon_persistence::transcript::{RevisionContext, TranscriptRepository};
use plaudon_persistence::Database;
use sqlx::Row;
use uuid::Uuid;

const SEGMENT_TEXTS: &[&str] = &[
    "We will ship the migration on Friday.",
    "I will own the rollback plan.",
    "Both of us will be on call.",
    "Nothing further was raised.",
];

struct Fixture {
    _dir: tempfile::TempDir,
    db: Database,
    recording_id: RecordingId,
    revision_id: TranscriptRevisionId,
    media_asset_id: String,
}

async fn fixture() -> Fixture {
    let dir = tempfile::tempdir().expect("temp dir");
    let db = Database::open(dir.path().join("library.sqlite"))
        .await
        .expect("open");
    MigrationRunner::new().run(&db).await.expect("migrate");

    let identity = MediaIdentity::of_bytes(b"fixture-media", "wav");
    let asset = MediaAsset {
        id: plaudon_domain::ids::MediaAssetId::new(),
        relative_path: format!("media/sha256/ab/{}.wav", identity.sha256.to_hex()),
        identity,
        quality_class: QualityClass::UserSuppliedFile,
        verification_state: plaudon_domain::media::VerificationState::Verified,
        verified_at: Some(Timestamp::from_millis(1)),
    };
    let media_asset_id = asset.id.to_string();
    MediaAssetRepository::new(db.clone())
        .insert(&asset, Timestamp::from_millis(1))
        .await
        .expect("insert asset");

    let recording_id = RecordingId::new();
    RecordingRepository::new(db.clone())
        .insert(&Recording::new(
            recording_id,
            "Platform sync",
            Timestamp::from_millis(1),
        ))
        .await
        .expect("insert recording");

    let revision_id = TranscriptRepository::new(db.clone())
        .save_draft(
            &TranscriptDraft {
                engine_id: EngineId::new("whisper.cpp"),
                model: ModelIdentity::new(EngineId::new("whisper.cpp"), "base.en"),
                input_sha256: Sha256Digest::of(b"fixture-media"),
                locale: None,
                alignment_origin: AlignmentOrigin::Native,
                segments: SEGMENT_TEXTS
                    .iter()
                    .enumerate()
                    .map(|(index, text)| {
                        let seq = u32::try_from(index).unwrap();
                        TranscriptSegmentDraft {
                            seq_index: seq,
                            start: Some(SourceTime::from_millis(u64::from(seq) * 2_000, 16_000)),
                            end: Some(SourceTime::from_millis(
                                u64::from(seq) * 2_000 + 2_000,
                                16_000,
                            )),
                            text: (*text).to_string(),
                            confidence: None,
                            speaker_id: None,
                            words: Vec::new(),
                        }
                    })
                    .collect(),
                duration_ms: Some(8_000),
                engine_params: std::collections::BTreeMap::new(),
                chunking_strategy: plaudon_ai_contracts::transcript::ChunkingStrategy::default(),
                normalization_profile: None,
                annotations: Vec::new(),
                backend: None,
            },
            &RevisionContext::machine(recording_id, Timestamp::from_millis(2)),
        )
        .await
        .expect("save draft");

    Fixture {
        _dir: dir,
        db,
        recording_id,
        revision_id,
        media_asset_id,
    }
}

async fn stored_revision(fixture: &Fixture) -> TranscriptRevision {
    // Rebuild the domain revision from the stored rows so reconciliation runs
    // against exactly what was persisted.
    let repo = TranscriptRepository::new(fixture.db.clone());
    let stored = repo.revision(fixture.revision_id).await.expect("revision");
    let segments = repo.segments(fixture.revision_id).await.expect("segments");
    TranscriptRevision::try_new(plaudon_domain::transcript::TranscriptRevisionDraft {
        recording_id: stored.recording_id,
        parent_id: None,
        origin: plaudon_domain::transcript::RevisionOrigin::Machine,
        input_sha256: stored.input_sha256,
        engine: EngineId::new(stored.engine.clone()),
        model: ModelIdentity::new(EngineId::new(stored.engine), stored.model_id),
        locale: None,
        alignment_origin: stored.alignment_origin,
        status: plaudon_domain::transcript::RevisionStatus::Complete,
        normalization_profile: stored.normalization_profile,
        chunking_strategy: stored.chunking_strategy,
        duration_ms: stored.duration_ms,
        created_at: Timestamp::from_millis(2),
        segments: segments
            .into_iter()
            .map(|segment| {
                plaudon_domain::transcript::TranscriptSegment::timed(
                    segment.seq_index,
                    segment.start_ms.unwrap_or(0),
                    segment.end_ms.unwrap_or(0),
                    segment.text,
                )
            })
            .collect(),
    })
    .expect("rebuild revision")
    // The rebuilt value mints a fresh id; carry the stored one so anchors and
    // assignments keep referring to the persisted revision.
    .with_id(fixture.revision_id)
}

/// The persisted revision id, which the domain constructor does not take.
trait WithId {
    fn with_id(self, id: TranscriptRevisionId) -> Self;
}

impl WithId for TranscriptRevision {
    fn with_id(mut self, id: TranscriptRevisionId) -> Self {
        self.id = id;
        self
    }
}

fn turns() -> Vec<SpeakerTurn> {
    vec![
        SpeakerTurn::new("cluster-a", 0, 2_000),
        SpeakerTurn::new("cluster-b", 2_000, 4_000),
        // Both speak over the third segment.
        SpeakerTurn::new("cluster-a", 4_000, 6_000),
        SpeakerTurn::new("cluster-b", 4_000, 6_000),
        // The fourth segment has no turn at all.
    ]
}

async fn persist_reconciliation(
    fixture: &Fixture,
) -> Vec<plaudon_persistence::speakers::StoredSpeaker> {
    let revision = stored_revision(fixture).await;
    let result = result_from_turns("fake-deterministic", turns());
    let reconciliation = reconcile(&revision, &result).expect("reconcile");

    let repo = SpeakerRepository::new(fixture.db.clone());
    let run_id = Uuid::now_v7();
    repo.record_run(
        &DiarizationRunInsert {
            id: run_id,
            recording_id: fixture.recording_id,
            media_asset_id: fixture.media_asset_id.clone(),
            provider_id: "fake-deterministic".to_string(),
            provider_version: Some("fixture-1".to_string()),
            privacy_class: "on_device".to_string(),
            status: "succeeded".to_string(),
            input_sha256: Sha256Digest::of(b"fixture-media").to_hex(),
            detected_cluster_count: Some(2),
            created_at: Timestamp::from_millis(10),
            finished_at: Some(Timestamp::from_millis(20)),
        },
        &result
            .turns
            .iter()
            .enumerate()
            .map(|(index, turn)| SpeakerTurnInsert {
                seq_index: u32::try_from(index).unwrap(),
                cluster_key: turn.cluster.to_string(),
                start_ms: turn.start_ms,
                end_ms: turn.end_ms,
                confidence: None,
                confidence_scale: None,
            })
            .collect::<Vec<_>>(),
    )
    .await
    .expect("record run");

    let assignments: Vec<AssignmentInsert> = reconciliation
        .segments
        .iter()
        .map(|segment| match &segment.assignment {
            SegmentAssignment::Single { cluster } => AssignmentInsert {
                seq_index: segment.seq_index,
                assignment: "single".to_string(),
                cluster_key: Some(cluster.to_string()),
                overlapping_keys_json: None,
                covered_ms: segment.covered_ms,
            },
            SegmentAssignment::Overlapping { clusters } => AssignmentInsert {
                seq_index: segment.seq_index,
                assignment: "overlapping".to_string(),
                cluster_key: None,
                overlapping_keys_json: Some(
                    serde_json::to_string(
                        &clusters.iter().map(ToString::to_string).collect::<Vec<_>>(),
                    )
                    .unwrap(),
                ),
                covered_ms: segment.covered_ms,
            },
            SegmentAssignment::Unassigned => AssignmentInsert {
                seq_index: segment.seq_index,
                assignment: "unassigned".to_string(),
                cluster_key: None,
                overlapping_keys_json: None,
                covered_ms: 0,
            },
        })
        .collect();

    repo.apply_reconciliation(
        run_id,
        fixture.recording_id,
        fixture.revision_id,
        &reconciliation
            .cluster_order
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        &assignments,
    )
    .await
    .expect("apply reconciliation")
}

async fn segment_speaker_labels(
    fixture: &Fixture,
) -> Vec<(u32, Option<String>, Option<String>, i64)> {
    sqlx::query(
        "SELECT seg.seq_index AS seq_index, sp.label AS label, sp.name AS name,
                COALESCE(sp.user_confirmed, 0) AS user_confirmed
         FROM transcript_segments seg
         LEFT JOIN speakers sp ON sp.id = seg.speaker_id
         WHERE seg.transcript_revision_id = ?1
         ORDER BY seg.seq_index",
    )
    .bind(fixture.revision_id.to_string())
    .fetch_all(fixture.db.pool())
    .await
    .expect("query")
    .into_iter()
    .map(|row| {
        (
            u32::try_from(row.get::<i64, _>("seq_index")).unwrap(),
            row.get::<Option<String>, _>("label"),
            row.get::<Option<String>, _>("name"),
            row.get::<i64, _>("user_confirmed"),
        )
    })
    .collect()
}

#[tokio::test]
async fn reconciliation_labels_clusters_and_leaves_ambiguity_explicit() {
    let fixture = fixture().await;
    let speakers = persist_reconciliation(&fixture).await;

    assert_eq!(speakers.len(), 2);
    assert_eq!(speakers[0].label, "Speaker 1");
    assert_eq!(speakers[1].label, "Speaker 2");
    assert!(speakers.iter().all(|s| s.provenance == "inferred"));
    assert!(speakers.iter().all(|s| !s.user_confirmed));

    let rows = segment_speaker_labels(&fixture).await;
    assert_eq!(rows[0].1.as_deref(), Some("Speaker 1"));
    assert_eq!(rows[1].1.as_deref(), Some("Speaker 2"));
    // Overlapping and unassigned segments carry no single speaker.
    assert_eq!(rows[2].1, None);
    assert_eq!(rows[3].1, None);
    fixture.db.close().await;
}

#[tokio::test]
async fn the_raw_turns_and_the_reconciled_assignments_are_both_preserved() {
    let fixture = fixture().await;
    persist_reconciliation(&fixture).await;

    let turn_count: i64 = sqlx::query_scalar("SELECT count(*) FROM speaker_turns")
        .fetch_one(fixture.db.pool())
        .await
        .unwrap();
    assert_eq!(turn_count, 4);

    let assignments: Vec<String> = sqlx::query_scalar(
        "SELECT assignment FROM segment_speaker_assignments
         WHERE transcript_revision_id = ?1 ORDER BY seq_index",
    )
    .bind(fixture.revision_id.to_string())
    .fetch_all(fixture.db.pool())
    .await
    .unwrap();
    assert_eq!(
        assignments,
        vec!["single", "single", "overlapping", "unassigned"]
    );
    fixture.db.close().await;
}

#[tokio::test]
async fn renaming_a_cluster_updates_every_segment_of_it_without_touching_transcript_text() {
    let fixture = fixture().await;
    let speakers = persist_reconciliation(&fixture).await;
    let repo = SpeakerRepository::new(fixture.db.clone());

    let before: Vec<String> = sqlx::query_scalar(
        "SELECT text FROM transcript_segments WHERE transcript_revision_id = ?1 ORDER BY seq_index",
    )
    .bind(fixture.revision_id.to_string())
    .fetch_all(fixture.db.pool())
    .await
    .unwrap();

    repo.rename(speakers[0].id, "Priya", Timestamp::from_millis(30))
        .await
        .unwrap();

    let rows = segment_speaker_labels(&fixture).await;
    assert_eq!(rows[0].1.as_deref(), Some("Priya"));
    assert_eq!(rows[0].2.as_deref(), Some("Priya"));
    // Naming alone is not identification.
    assert_eq!(rows[0].3, 0);

    let after: Vec<String> = sqlx::query_scalar(
        "SELECT text FROM transcript_segments WHERE transcript_revision_id = ?1 ORDER BY seq_index",
    )
    .bind(fixture.revision_id.to_string())
    .fetch_all(fixture.db.pool())
    .await
    .unwrap();
    assert_eq!(before, after, "the machine transcript text must not change");
    assert_eq!(before, SEGMENT_TEXTS);
    fixture.db.close().await;
}

#[tokio::test]
async fn a_rename_is_reversible_to_the_machine_label() {
    let fixture = fixture().await;
    let speakers = persist_reconciliation(&fixture).await;
    let repo = SpeakerRepository::new(fixture.db.clone());

    repo.rename(speakers[0].id, "Priya", Timestamp::from_millis(30))
        .await
        .unwrap();
    repo.confirm(speakers[0].id, Timestamp::from_millis(31))
        .await
        .unwrap();
    repo.reset(speakers[0].id).await.unwrap();

    let restored = repo
        .list_for_recording(fixture.recording_id)
        .await
        .unwrap()
        .into_iter()
        .find(|s| s.id == speakers[0].id)
        .unwrap();
    assert_eq!(restored.label, "Speaker 1");
    assert_eq!(restored.name, None);
    assert!(!restored.user_confirmed);
    assert_eq!(restored.provenance, "inferred");
    assert_eq!(restored.renamed_at, None);
    fixture.db.close().await;
}

#[tokio::test]
async fn confirmation_requires_a_name_and_is_withdrawable() {
    let fixture = fixture().await;
    let speakers = persist_reconciliation(&fixture).await;
    let repo = SpeakerRepository::new(fixture.db.clone());

    assert!(
        repo.confirm(speakers[0].id, Timestamp::from_millis(30))
            .await
            .is_err(),
        "an unnamed cluster cannot be confirmed"
    );

    repo.rename(speakers[0].id, "Priya", Timestamp::from_millis(31))
        .await
        .unwrap();
    repo.confirm(speakers[0].id, Timestamp::from_millis(32))
        .await
        .unwrap();

    let confirmed = repo
        .list_for_recording(fixture.recording_id)
        .await
        .unwrap()
        .into_iter()
        .find(|s| s.id == speakers[0].id)
        .unwrap();
    assert!(confirmed.user_confirmed);
    assert_eq!(confirmed.provenance, "user_confirmed");
    assert!(confirmed.confirmed_at.is_some());

    repo.unconfirm(speakers[0].id).await.unwrap();
    let withdrawn = repo
        .list_for_recording(fixture.recording_id)
        .await
        .unwrap()
        .into_iter()
        .find(|s| s.id == speakers[0].id)
        .unwrap();
    assert!(!withdrawn.user_confirmed);
    assert_eq!(withdrawn.name.as_deref(), Some("Priya"));
    fixture.db.close().await;
}

#[tokio::test]
async fn a_rename_of_one_cluster_never_touches_another() {
    let fixture = fixture().await;
    let speakers = persist_reconciliation(&fixture).await;
    let repo = SpeakerRepository::new(fixture.db.clone());

    repo.rename(speakers[0].id, "Priya", Timestamp::from_millis(30))
        .await
        .unwrap();

    let other = repo
        .list_for_recording(fixture.recording_id)
        .await
        .unwrap()
        .into_iter()
        .find(|s| s.id == speakers[1].id)
        .unwrap();
    assert_eq!(other.label, "Speaker 2");
    assert_eq!(other.name, None);
    fixture.db.close().await;
}

#[tokio::test]
async fn labelling_an_unknown_speaker_is_refused() {
    let fixture = fixture().await;
    persist_reconciliation(&fixture).await;
    let repo = SpeakerRepository::new(fixture.db.clone());
    let foreign = plaudon_domain::ids::SpeakerId::new();
    assert!(repo
        .rename(foreign, "X", Timestamp::from_millis(1))
        .await
        .is_err());
    assert!(repo
        .confirm(foreign, Timestamp::from_millis(1))
        .await
        .is_err());
    assert!(repo.reset(foreign).await.is_err());
    fixture.db.close().await;
}

#[tokio::test]
async fn re_running_diarization_updates_assignments_without_duplicating_speakers() {
    let fixture = fixture().await;
    let first = persist_reconciliation(&fixture).await;
    let second = persist_reconciliation(&fixture).await;

    assert_eq!(first.len(), second.len());
    assert_eq!(
        first.iter().map(|s| s.id).collect::<Vec<_>>(),
        second.iter().map(|s| s.id).collect::<Vec<_>>(),
        "a re-run must reuse the existing cluster rows"
    );
    fixture.db.close().await;
}
