//! Search performance at the spec 17.5 scale: 10,000 recordings and more than
//! 100,000 transcript segments (M1.4).
//!
//! Seeding six figures of rows takes long enough that this is `#[ignore]`d by
//! default and run explicitly:
//!
//! ```text
//! cargo test -p plaudon-app-core --test search_scale -- --ignored --nocapture
//! ```
//!
//! The budget asserted here is the spec 17.5 interaction budget: an ordinary
//! library navigation, which includes running a search, completes in under
//! 100 ms. The assertion is deliberately on the query, not on the seeding.

use std::time::Instant;

use plaudon_app_core::search::LibrarySearch;
use plaudon_persistence::migrations::MigrationRunner;
use plaudon_persistence::search::SearchPage;
use plaudon_persistence::Database;
use uuid::Uuid;

const RECORDINGS: usize = 10_000;
const SEGMENTS_PER_RECORDING: usize = 11;
const INTERACTION_BUDGET_MS: u128 = 100;

/// Vocabulary the generated transcripts are built from. The needle term
/// appears in exactly one segment of one recording.
const FILLER: &[&str] = &[
    "the quarterly plan covers staffing and tooling",
    "we reviewed the deployment schedule for next month",
    "the latency budget did not change this quarter",
    "support volume is flat compared with last period",
    "nobody raised an objection to the revised timeline",
];
const NEEDLE: &str = "the zarquon telemetry spike remains unexplained";

async fn seed(db: &Database) {
    let mut tx = db.pool().begin().await.expect("begin");
    for recording_index in 0..RECORDINGS {
        let recording_id = Uuid::now_v7().to_string();
        let revision_id = Uuid::now_v7().to_string();
        sqlx::query(
            "INSERT INTO recordings (id, title, captured_at, duration_ms,
                 preferred_media_asset_id, annotation, created_at, deleted_at)
             VALUES (?1, ?2, NULL, NULL, NULL, NULL, 1, NULL)",
        )
        .bind(&recording_id)
        .bind(format!("Recording {recording_index}"))
        .execute(&mut *tx)
        .await
        .expect("insert recording");

        sqlx::query(
            "INSERT INTO transcript_revisions (id, recording_id, parent_id, origin,
                 input_sha256, engine, model_id, model_version, locale, alignment_origin,
                 status, normalization_profile, chunking_strategy, duration_ms, created_at)
             VALUES (?1, ?2, NULL, 'machine', ?3, 'whisper.cpp', 'base.en', NULL, NULL,
                     'native', 'complete', 'none', 'fixed', 600000, 1)",
        )
        .bind(&revision_id)
        .bind(&recording_id)
        .bind("0".repeat(64))
        .execute(&mut *tx)
        .await
        .expect("insert revision");

        for segment_index in 0..SEGMENTS_PER_RECORDING {
            let text = if recording_index == RECORDINGS / 2 && segment_index == 3 {
                NEEDLE.to_string()
            } else {
                format!(
                    "{} {}",
                    FILLER[(recording_index + segment_index) % FILLER.len()],
                    segment_index
                )
            };
            sqlx::query(
                "INSERT INTO transcript_segments (id, transcript_revision_id, seq_index,
                     start_ms, end_ms, speaker_id, text, confidence, confidence_scale,
                     alignment_origin)
                 VALUES (?1, ?2, ?3, ?4, ?5, NULL, ?6, NULL, NULL, 'native')",
            )
            .bind(Uuid::now_v7().to_string())
            .bind(&revision_id)
            .bind(i64::try_from(segment_index).unwrap())
            .bind(i64::try_from(segment_index * 5_000).unwrap())
            .bind(i64::try_from(segment_index * 5_000 + 4_500).unwrap())
            .bind(text)
            .execute(&mut *tx)
            .await
            .expect("insert segment");
        }
    }
    tx.commit().await.expect("commit");
}

#[tokio::test]
#[ignore = "seeds 110,000 rows; run explicitly with --ignored"]
async fn search_meets_the_interaction_budget_at_spec_scale() {
    let dir = tempfile::tempdir().expect("temp dir");
    let db = Database::open(dir.path().join("library.sqlite"))
        .await
        .expect("open");
    MigrationRunner::new().run(&db).await.expect("migrate");

    let seeding = Instant::now();
    seed(&db).await;
    println!("seeded in {:?}", seeding.elapsed());

    let search = LibrarySearch::new(db.clone());
    let indexed = search.indexed_row_count().await.unwrap();
    assert!(
        indexed >= 100_000,
        "expected at least 100,000 indexed segments, got {indexed}"
    );

    // Warm the page cache the way a user's second keystroke would.
    search
        .search("zarquon", SearchPage::default())
        .await
        .unwrap();

    let started = Instant::now();
    let rare = search
        .search("zarquon", SearchPage::default())
        .await
        .unwrap();
    let rare_ms = started.elapsed().as_millis();
    assert_eq!(rare.len(), 1, "the needle term should match exactly once");
    println!("rare-term query: {rare_ms} ms over {indexed} segments");
    assert!(
        rare_ms < INTERACTION_BUDGET_MS,
        "rare-term search took {rare_ms} ms, budget is {INTERACTION_BUDGET_MS} ms"
    );

    // A common term with a page limit must also stay inside the budget: the
    // limit is what keeps a 100,000-row match set from being materialized.
    let started = Instant::now();
    let common = search
        .search(
            "deployment schedule",
            SearchPage {
                limit: 50,
                offset: 0,
            },
        )
        .await
        .unwrap();
    let common_ms = started.elapsed().as_millis();
    assert_eq!(common.len(), 50);
    println!("common-term query: {common_ms} ms");
    assert!(
        common_ms < INTERACTION_BUDGET_MS,
        "common-term search took {common_ms} ms, budget is {INTERACTION_BUDGET_MS} ms"
    );

    db.close().await;
}
