//! Cross-recording full-text search over a real migrated database (M1.4).
//!
//! Spec 3.1 (cross-recording search), 9.8 (local and privacy-aware), 17.5
//! (10,000 recordings and 100,000+ segments), 18.5 (hostile input).

use plaudon_ai_contracts::stt::{TranscriptDraft, TranscriptSegmentDraft};
use plaudon_ai_contracts::transcript::SourceTime;
use plaudon_app_core::search::{to_match_expression, LibrarySearch};
use plaudon_domain::ids::RecordingId;
use plaudon_domain::media::Sha256Digest;
use plaudon_domain::model::{EngineId, ModelIdentity};
use plaudon_domain::recording::Recording;
use plaudon_domain::time::Timestamp;
use plaudon_domain::transcript::AlignmentOrigin;
use plaudon_persistence::migrations::MigrationRunner;
use plaudon_persistence::repo::RecordingRepository;
use plaudon_persistence::search::SearchPage;
use plaudon_persistence::transcript::{RevisionContext, TranscriptRepository};
use plaudon_persistence::Database;

async fn open() -> (tempfile::TempDir, Database) {
    let dir = tempfile::tempdir().expect("temp dir");
    let db = Database::open(dir.path().join("library.sqlite"))
        .await
        .expect("open");
    MigrationRunner::new().run(&db).await.expect("migrate");
    (dir, db)
}

fn draft(texts: &[&str], origin: AlignmentOrigin) -> TranscriptDraft {
    TranscriptDraft {
        engine_id: EngineId::new("whisper.cpp"),
        model: ModelIdentity::new(EngineId::new("whisper.cpp"), "base.en"),
        input_sha256: Sha256Digest::of(b"fixture-media"),
        locale: None,
        alignment_origin: origin,
        segments: texts
            .iter()
            .enumerate()
            .map(|(index, text)| {
                let seq = u32::try_from(index).unwrap();
                let timed = origin.permits_times();
                TranscriptSegmentDraft {
                    seq_index: seq,
                    start: timed.then(|| SourceTime::from_millis(u64::from(seq) * 1_000, 16_000)),
                    end: timed
                        .then(|| SourceTime::from_millis(u64::from(seq) * 1_000 + 900, 16_000)),
                    text: (*text).to_string(),
                    confidence: None,
                    speaker_id: None,
                    words: Vec::new(),
                }
            })
            .collect(),
        duration_ms: Some(10_000),
        engine_params: std::collections::BTreeMap::new(),
        chunking_strategy: plaudon_ai_contracts::transcript::ChunkingStrategy::default(),
        normalization_profile: None,
        annotations: Vec::new(),
        backend: None,
    }
}

async fn seed_recording(
    db: &Database,
    title: &str,
    texts: &[&str],
    origin: AlignmentOrigin,
) -> RecordingId {
    let recordings = RecordingRepository::new(db.clone());
    let transcripts = TranscriptRepository::new(db.clone());
    let id = RecordingId::new();
    let recording = Recording::new(id, title, Timestamp::from_millis(1));
    recordings
        .insert(&recording)
        .await
        .expect("insert recording");
    transcripts
        .save_draft(
            &draft(texts, origin),
            &RevisionContext::machine(id, Timestamp::from_millis(2)),
        )
        .await
        .expect("save draft");
    id
}

#[tokio::test]
async fn a_term_is_found_across_two_recordings_with_revision_and_segment_identity() {
    let (_dir, db) = open().await;
    let first = seed_recording(
        &db,
        "Platform sync",
        &[
            "We will ship the migration on Friday.",
            "Priya owns the rollback plan.",
        ],
        AlignmentOrigin::Native,
    )
    .await;
    let second = seed_recording(
        &db,
        "Budget review",
        &["The rollback budget is unchanged."],
        AlignmentOrigin::Native,
    )
    .await;

    let search = LibrarySearch::new(db.clone());
    let hits = search
        .search("rollback", SearchPage::default())
        .await
        .unwrap();

    assert_eq!(hits.len(), 2, "{hits:#?}");
    let ids: Vec<RecordingId> = hits.iter().map(|h| h.recording_id).collect();
    assert!(ids.contains(&first));
    assert!(ids.contains(&second));
    for hit in &hits {
        assert!(hit.excerpt.to_lowercase().contains("rollback"));
        assert_ne!(hit.recording_title, "");
    }
    db.close().await;
}

#[tokio::test]
async fn a_hit_seeks_only_when_the_revision_carries_real_times() {
    let (_dir, db) = open().await;
    seed_recording(
        &db,
        "Timed",
        &["The rollback plan is ready."],
        AlignmentOrigin::Native,
    )
    .await;
    seed_recording(
        &db,
        "Text only",
        &["The rollback plan is ready."],
        AlignmentOrigin::None,
    )
    .await;

    let search = LibrarySearch::new(db.clone());
    let hits = search
        .search("rollback", SearchPage::default())
        .await
        .unwrap();
    assert_eq!(hits.len(), 2);

    let timed = hits.iter().find(|h| h.recording_title == "Timed").unwrap();
    let text_only = hits
        .iter()
        .find(|h| h.recording_title == "Text only")
        .unwrap();
    assert_eq!(timed.alignment_origin, AlignmentOrigin::Native);
    assert_eq!(timed.seek_ms, Some(0));
    assert_eq!(text_only.alignment_origin, AlignmentOrigin::None);
    assert_eq!(text_only.seek_ms, None);
    db.close().await;
}

#[tokio::test]
async fn a_tombstoned_recording_never_appears_in_results() {
    let (_dir, db) = open().await;
    let id = seed_recording(
        &db,
        "To delete",
        &["The rollback plan is ready."],
        AlignmentOrigin::Native,
    )
    .await;
    let search = LibrarySearch::new(db.clone());
    assert_eq!(
        search
            .search("rollback", SearchPage::default())
            .await
            .unwrap()
            .len(),
        1
    );

    RecordingRepository::new(db.clone())
        .tombstone(id, "user_request", Timestamp::from_millis(10), None)
        .await
        .expect("tombstone");

    assert_eq!(
        search
            .search("rollback", SearchPage::default())
            .await
            .unwrap(),
        [] as [plaudon_app_core::SearchResult; 0]
    );
    db.close().await;
}

#[tokio::test]
async fn the_index_follows_new_revisions_and_deletions() {
    let (_dir, db) = open().await;
    let id = seed_recording(
        &db,
        "Evolving",
        &["The original wording mentions kittens."],
        AlignmentOrigin::Native,
    )
    .await;
    let search = LibrarySearch::new(db.clone());
    assert_eq!(
        search
            .search("kittens", SearchPage::default())
            .await
            .unwrap()
            .len(),
        1
    );

    // A correction is a new revision, not an edit of the old one.
    let transcripts = TranscriptRepository::new(db.clone());
    transcripts
        .save_draft(
            &draft(
                &["The corrected wording mentions puppies."],
                AlignmentOrigin::Native,
            ),
            &RevisionContext::machine(id, Timestamp::from_millis(5)),
        )
        .await
        .expect("save correction");

    assert_eq!(
        search
            .search("puppies", SearchPage::default())
            .await
            .unwrap()
            .len(),
        1
    );
    // The superseded revision is still searchable: revisions are immutable and
    // both exist.
    assert_eq!(
        search
            .search("kittens", SearchPage::default())
            .await
            .unwrap()
            .len(),
        1
    );

    let removed = transcripts.delete_revisions_for(id).await.expect("delete");
    assert!(removed > 0);
    assert_eq!(
        search
            .search("kittens", SearchPage::default())
            .await
            .unwrap(),
        [] as [plaudon_app_core::SearchResult; 0]
    );
    assert_eq!(
        search
            .search("puppies", SearchPage::default())
            .await
            .unwrap(),
        [] as [plaudon_app_core::SearchResult; 0]
    );
    db.close().await;
}

#[tokio::test]
async fn a_rebuild_restores_an_index_damaged_out_of_band() {
    let (_dir, db) = open().await;
    seed_recording(
        &db,
        "Restored",
        &["The rollback plan is ready.", "Another segment entirely."],
        AlignmentOrigin::Native,
    )
    .await;
    let search = LibrarySearch::new(db.clone());
    let before = search.indexed_row_count().await.unwrap();
    assert_eq!(before, 2);

    // A portable restore writes rows through a path the triggers never see.
    sqlx::query("DELETE FROM transcript_search")
        .execute(db.pool())
        .await
        .unwrap();
    assert_eq!(
        search
            .search("rollback", SearchPage::default())
            .await
            .unwrap(),
        [] as [plaudon_app_core::SearchResult; 0]
    );

    let rebuilt = search.rebuild_index().await.unwrap();
    assert_eq!(rebuilt, 2);
    assert_eq!(search.indexed_row_count().await.unwrap(), before);
    assert_eq!(
        search
            .search("rollback", SearchPage::default())
            .await
            .unwrap()
            .len(),
        1
    );
    db.close().await;
}

#[tokio::test]
async fn hostile_queries_are_handled_without_error_and_without_matching_everything() {
    let (_dir, db) = open().await;
    seed_recording(
        &db,
        "Hostile input",
        &["The rollback plan is ready."],
        AlignmentOrigin::Native,
    )
    .await;
    let search = LibrarySearch::new(db.clone());

    for query in [
        "\"; DROP TABLE recordings; --",
        "rollback NEAR/2 plan",
        "roll* OR *",
        "^rollback",
        "rollback:plan",
        "$(rm -rf /)",
        "`id`",
        "\u{0}\u{1}",
    ] {
        let result = search.search(query, SearchPage::default()).await;
        assert!(result.is_ok(), "query {query:?} failed: {result:?}");
    }

    // The table is still there.
    assert_eq!(
        search
            .search("rollback", SearchPage::default())
            .await
            .unwrap()
            .len(),
        1
    );
    db.close().await;
}

#[tokio::test]
async fn an_empty_query_returns_nothing_without_touching_the_database() {
    assert_eq!(to_match_expression("   "), None);
    let (_dir, db) = open().await;
    let search = LibrarySearch::new(db.clone());
    assert_eq!(
        search.search("!!!", SearchPage::default()).await.unwrap(),
        [] as [plaudon_app_core::SearchResult; 0]
    );
    db.close().await;
}

#[tokio::test]
async fn results_paginate_deterministically() {
    let (_dir, db) = open().await;
    for index in 0..5 {
        seed_recording(
            &db,
            &format!("Recording {index}"),
            &["The rollback plan is ready."],
            AlignmentOrigin::Native,
        )
        .await;
    }
    let search = LibrarySearch::new(db.clone());
    let all = search
        .search(
            "rollback",
            SearchPage {
                limit: 10,
                offset: 0,
            },
        )
        .await
        .unwrap();
    assert_eq!(all.len(), 5);

    let first_two = search
        .search(
            "rollback",
            SearchPage {
                limit: 2,
                offset: 0,
            },
        )
        .await
        .unwrap();
    let next_two = search
        .search(
            "rollback",
            SearchPage {
                limit: 2,
                offset: 2,
            },
        )
        .await
        .unwrap();
    assert_eq!(first_two, all[..2]);
    assert_eq!(next_two, all[2..4]);
    db.close().await;
}
