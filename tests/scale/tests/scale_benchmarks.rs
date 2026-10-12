//! The spec 17.5 benchmark suite, as tests with assertions.
//!
//! One test loads the dataset through the real persistence layer and then runs
//! every benchmark against it: loading a 10,000-recording library once is the
//! expensive part, and splitting the benchmarks into separate `#[test]`
//! functions would either reload it per benchmark or share mutable state
//! between tests that cargo may run in parallel.
//!
//! Every benchmark asserts on its *results* as well as its latency. A query
//! that returns the wrong rows quickly is not a passing benchmark: spec 18.6A's
//! UI row requires the 100k-segment case to produce a correct timeline, so the
//! viewport benchmark compares every returned segment against the generator.

use std::time::Instant;

use plaudon_domain::ids::RecordingId;
use plaudon_persistence::archive::MediaArchive;
use plaudon_persistence::repo::{MediaAssetRepository, RecordingRepository};
use plaudon_scale_tests::load::{self, ScaleFixture};
use plaudon_scale_tests::memory;
use plaudon_scale_tests::metrics::{Samples, Verdict};
use plaudon_scale_tests::pcm::ChunkWindow;
use plaudon_scale_tests::results::{DatasetReport, Host, MemoryReport, RunReport, DOCUMENT_STEM};
use plaudon_scale_tests::rss;
use plaudon_scale_tests::thresholds::Thresholds;
use plaudon_scale_tests::{DatasetSpec, SyntheticLibrary, SEARCH_NEEDLES};
use sqlx::Row;

/// Rows per library page, matching a desktop list viewport.
const PAGE_SIZE: i64 = 50;

/// Pages sampled across the library.
const PAGES_SAMPLED: usize = 40;

/// Segments in one transcript viewport.
const VIEWPORT_SIZE: i64 = 200;

/// Viewport fetches performed at pseudo-arbitrary offsets.
const VIEWPORT_FETCHES: usize = 40;

/// Recordings whose export manifest is gathered.
const EXPORT_SAMPLES: usize = 120;

/// Audio lengths, in minutes, the memory harness decodes for the report.
const MEMORY_LADDER: [u64; 4] = [5, 15, 30, 60];

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(clippy::too_many_lines)]
async fn library_scale_benchmarks_stay_within_their_budgets() {
    let spec = DatasetSpec::from_env().expect("PLAUDON_SCALE_TIER / PLAUDON_SCALE_SEED");
    let thresholds = Thresholds::load().expect("committed thresholds must parse");
    let tier = spec.tier.as_str();

    let root = tempfile::tempdir().expect("temporary library root");
    eprintln!(
        "scale: loading tier `{tier}` — {} recordings, {} segments, seed {}",
        spec.total_recordings(),
        spec.total_segments(),
        spec.seed
    );
    let started = Instant::now();
    let fixture = load::build(root.path(), spec).await.expect("dataset load");
    eprintln!(
        "scale: loaded in {:?} ({} bytes of database)",
        started.elapsed(),
        fixture.stats.database_bytes
    );

    assert_dataset_is_what_was_asked_for(&fixture).await;

    let mut verdicts = Vec::new();
    verdicts.extend(bench_pagination(&fixture, &thresholds, tier).await);
    verdicts.push(bench_transcript_search(&fixture, &thresholds, tier).await);
    verdicts.push(bench_transcript_viewport(&fixture, &thresholds, tier).await);
    verdicts.push(bench_archive_accounting(&fixture, &thresholds, tier).await);
    verdicts.push(bench_export_manifest(&fixture, &thresholds, tier).await);

    let memory = measure_memory(&thresholds);

    let report = RunReport::new(
        Host::detect(),
        DatasetReport::new(fixture.spec(), fixture.library.fingerprint(), fixture.stats),
        thresholds.policy().expect("policy table"),
        verdicts,
        Some(memory.clone()),
    );

    let directory = plaudon_scale_tests::results::results_directory();
    let (json_path, markdown_path) = report
        .write_to(&directory)
        .expect("results must be writable");
    eprintln!(
        "scale: wrote {} and {}",
        json_path.display(),
        markdown_path.display()
    );
    for verdict in &report.benchmarks {
        eprintln!(
            "scale: {:<24} p50 {:>9.3} ms  p95 {:>9.3} ms  max {:>9.3} ms  budget used {:>5.0}%",
            verdict.measurement.name,
            verdict.measurement.p50_ms,
            verdict.measurement.p95_ms,
            verdict.measurement.max_ms,
            verdict.worst_budget_fraction * 100.0
        );
    }

    assert!(
        report.is_citable(),
        "the run did not identify its machine: {:?}",
        report.host.missing_fields()
    );
    assert_eq!(
        markdown_path.file_name().unwrap(),
        format!("{DOCUMENT_STEM}.md").as_str()
    );

    // Memory assertions come from the same harness the property test uses; a
    // breach here fails the published run too.
    if memory.skipped_reason.is_none() {
        for observation in &memory.observations {
            assert!(
                memory::breach(
                    observation,
                    memory.allowed_growth_bytes,
                    ChunkWindow::DEFAULT
                )
                .is_none(),
                "{}",
                memory::breach(
                    observation,
                    memory.allowed_growth_bytes,
                    ChunkWindow::DEFAULT
                )
                .unwrap_or_default()
            );
        }
    }

    let failures: Vec<String> = report
        .failures()
        .iter()
        .map(|verdict| verdict.failure_report())
        .collect();
    assert!(
        failures.is_empty(),
        "performance budgets breached:\n  {}",
        failures.join("\n  ")
    );
}

/// The database must contain exactly the dataset the generator describes.
async fn assert_dataset_is_what_was_asked_for(fixture: &ScaleFixture) {
    let pool = fixture.db.pool();
    let spec = fixture.spec();

    let recordings: i64 = sqlx::query("SELECT COUNT(*) FROM recordings")
        .fetch_one(pool)
        .await
        .unwrap()
        .get(0);
    let segments: i64 = sqlx::query("SELECT COUNT(*) FROM transcript_segments")
        .fetch_one(pool)
        .await
        .unwrap()
        .get(0);
    let revisions: i64 = sqlx::query("SELECT COUNT(*) FROM transcript_revisions")
        .fetch_one(pool)
        .await
        .unwrap()
        .get(0);
    let assets: i64 = sqlx::query("SELECT COUNT(*) FROM media_assets")
        .fetch_one(pool)
        .await
        .unwrap()
        .get(0);
    let items: i64 = sqlx::query("SELECT COUNT(*) FROM source_items")
        .fetch_one(pool)
        .await
        .unwrap()
        .get(0);

    assert_eq!(recordings as u64, spec.total_recordings());
    assert_eq!(segments as u64, spec.total_segments());
    assert_eq!(revisions as u64, spec.total_recordings());
    assert_eq!(assets as u64, spec.total_recordings());
    assert_eq!(items as u64, spec.total_recordings());
    assert_eq!(fixture.stats.transcript_segments, spec.total_segments());

    let marathon: i64 =
        sqlx::query("SELECT COUNT(*) FROM transcript_segments WHERE transcript_revision_id = ?1")
            .bind(fixture.marathon.revision_id.to_string())
            .fetch_one(pool)
            .await
            .unwrap()
            .get(0);
    assert_eq!(
        marathon as u32, spec.marathon_segments,
        "the marathon transcript is the spec 18.6A 100k-segment case"
    );
}

/// Library listing by `LIMIT/OFFSET` and by keyset, at identical depths.
async fn bench_pagination(
    fixture: &ScaleFixture,
    thresholds: &Thresholds,
    tier: &str,
) -> Vec<Verdict> {
    let pool = fixture.db.pool();
    let total = fixture.spec().total_recordings();
    let page_size = u64::try_from(PAGE_SIZE).unwrap();
    let pages = total.div_ceil(page_size);
    let sampled = sample_offsets(pages, PAGES_SAMPLED)
        .into_iter()
        .map(|page| i64::try_from(page * page_size).unwrap())
        .collect::<Vec<_>>();

    let mut offset_samples = Samples::new("library_page_offset", "page");
    let mut keyset_samples = Samples::new("library_page_keyset", "page");

    for offset in sampled {
        // Timed: the straightforward implementation a UI reaches for first.
        let started = Instant::now();
        let page = sqlx::query(
            "SELECT id, title, captured_at, duration_ms, preferred_media_asset_id
             FROM recordings
             WHERE deleted_at IS NULL
             ORDER BY captured_at DESC, id DESC
             LIMIT ?1 OFFSET ?2",
        )
        .bind(PAGE_SIZE)
        .bind(offset)
        .fetch_all(pool)
        .await
        .expect("offset page");
        offset_samples.push(started.elapsed());

        assert!(!page.is_empty(), "sampled page {offset} is empty");
        let mut previous: Option<(i64, String)> = None;
        for row in &page {
            let key = (
                row.get::<Option<i64>, _>("captured_at").unwrap_or(i64::MIN),
                row.get::<String, _>("id"),
            );
            if let Some(previous) = &previous {
                assert!(
                    key.0 < previous.0 || (key.0 == previous.0 && key.1 < previous.1),
                    "library page is not ordered newest first"
                );
            }
            assert_ne!(row.get::<String, _>("title"), "", "page row has no title");
            previous = Some(key);
        }

        // The cursor for the same depth, fetched outside the measurement.
        let cursor = if offset == 0 {
            None
        } else {
            let row = sqlx::query(
                "SELECT captured_at, id FROM recordings WHERE deleted_at IS NULL
                 ORDER BY captured_at DESC, id DESC LIMIT 1 OFFSET ?1",
            )
            .bind(offset - 1)
            .fetch_one(pool)
            .await
            .expect("cursor row");
            Some((
                row.get::<Option<i64>, _>("captured_at").unwrap_or(i64::MIN),
                row.get::<String, _>("id"),
            ))
        };

        let started = Instant::now();
        let keyed = match &cursor {
            None => {
                sqlx::query(
                    "SELECT id, title, captured_at, duration_ms, preferred_media_asset_id
                     FROM recordings WHERE deleted_at IS NULL
                     ORDER BY captured_at DESC, id DESC LIMIT ?1",
                )
                .bind(PAGE_SIZE)
                .fetch_all(pool)
                .await
            }
            Some((captured_at, id)) => {
                sqlx::query(
                    "SELECT id, title, captured_at, duration_ms, preferred_media_asset_id
                     FROM recordings
                     WHERE deleted_at IS NULL
                       AND (captured_at < ?1 OR (captured_at = ?1 AND id < ?2))
                     ORDER BY captured_at DESC, id DESC LIMIT ?3",
                )
                .bind(captured_at)
                .bind(id)
                .bind(PAGE_SIZE)
                .fetch_all(pool)
                .await
            }
        }
        .expect("keyset page");
        keyset_samples.push(started.elapsed());

        let offset_ids: Vec<String> = page.iter().map(|row| row.get("id")).collect();
        let keyset_ids: Vec<String> = keyed.iter().map(|row| row.get("id")).collect();
        assert_eq!(
            offset_ids, keyset_ids,
            "keyset pagination returned a different page than offset pagination at {offset}"
        );
    }

    vec![
        judge(&offset_samples, thresholds, tier),
        judge(&keyset_samples, thresholds, tier),
    ]
}

/// Cross-recording transcript search over every segment in the library.
async fn bench_transcript_search(
    fixture: &ScaleFixture,
    thresholds: &Thresholds,
    tier: &str,
) -> Verdict {
    let pool = fixture.db.pool();
    let mut samples = Samples::new("transcript_search", "query");

    for round in 0..4 {
        for needle in SEARCH_NEEDLES {
            let pattern = format!("%{needle}%");
            let started = Instant::now();
            let hits = sqlx::query(
                "SELECT s.id, s.seq_index, s.start_ms, s.text, r.id AS recording_id, r.title
                 FROM transcript_segments s
                 JOIN transcript_revisions t ON t.id = s.transcript_revision_id
                 JOIN recordings r ON r.id = t.recording_id
                 WHERE s.text LIKE ?1 AND r.deleted_at IS NULL
                 ORDER BY s.start_ms
                 LIMIT ?2",
            )
            .bind(&pattern)
            .bind(PAGE_SIZE)
            .fetch_all(pool)
            .await
            .expect("search query");
            samples.push(started.elapsed());

            assert!(
                !hits.is_empty(),
                "round {round}: searching for `{needle}` returned nothing"
            );
            for row in &hits {
                assert!(
                    row.get::<String, _>("text").contains(needle),
                    "search returned a segment without the term"
                );
                assert_ne!(row.get::<String, _>("title"), "", "hit has no recording");
            }
        }
    }

    // Correctness of the whole result set, not just the first page. Untimed:
    // this is the assertion that the dataset and the index agree.
    for needle in SEARCH_NEEDLES {
        let found: i64 = sqlx::query("SELECT COUNT(*) FROM transcript_segments WHERE text LIKE ?1")
            .bind(format!("%{needle}%"))
            .fetch_one(pool)
            .await
            .unwrap()
            .get(0);
        assert_eq!(
            found as u64,
            fixture.library.expected_needle_matches(needle),
            "stored match count for `{needle}` disagrees with the generator"
        );
    }

    judge(&samples, thresholds, tier)
}

/// A 200-segment viewport at arbitrary offsets in the marathon transcript.
async fn bench_transcript_viewport(
    fixture: &ScaleFixture,
    thresholds: &Thresholds,
    tier: &str,
) -> Verdict {
    let pool = fixture.db.pool();
    let marathon = fixture.library.recording(fixture.spec().marathon_index());
    let viewport = u32::try_from(VIEWPORT_SIZE).unwrap();
    let last_offset = marathon.segment_count.saturating_sub(viewport);
    let mut samples = Samples::new("transcript_viewport", "viewport");

    for step in 0..VIEWPORT_FETCHES {
        // Spread the offsets across the transcript, always including the very
        // end: a windowed fetch must be as cheap at segment 99,800 as at 0.
        let offset = if step + 1 == VIEWPORT_FETCHES {
            last_offset
        } else {
            u32::try_from((u64::from(last_offset) * step as u64) / (VIEWPORT_FETCHES as u64 - 1))
                .unwrap()
        };

        let started = Instant::now();
        let rows = sqlx::query(
            "SELECT seq_index, start_ms, end_ms, text, speaker_id
             FROM transcript_segments
             WHERE transcript_revision_id = ?1 AND seq_index >= ?2
             ORDER BY seq_index
             LIMIT ?3",
        )
        .bind(fixture.marathon.revision_id.to_string())
        .bind(i64::from(offset))
        .bind(VIEWPORT_SIZE)
        .fetch_all(pool)
        .await
        .expect("viewport query");
        samples.push(started.elapsed());

        let expected = fixture.library.segments_of(&marathon, offset, viewport);
        assert_eq!(
            rows.len(),
            expected.len(),
            "viewport at {offset} returned the wrong number of segments"
        );
        for (row, generated) in rows.iter().zip(&expected) {
            assert_eq!(row.get::<i64, _>("seq_index") as u32, generated.seq_index);
            assert_eq!(row.get::<String, _>("text"), generated.text);
            assert_eq!(
                row.get::<i64, _>("start_ms") as u64,
                generated.start_ms,
                "a viewport must not fabricate a timeline (spec 18.6A UI row)"
            );
            assert_eq!(row.get::<i64, _>("end_ms") as u64, generated.end_ms);
        }
    }

    judge(&samples, thresholds, tier)
}

/// Full archive enumeration reconciled against `media_assets`.
async fn bench_archive_accounting(
    fixture: &ScaleFixture,
    thresholds: &Thresholds,
    tier: &str,
) -> Verdict {
    let pool = fixture.db.pool();
    let mut samples = Samples::new("archive_accounting", "full sweep");

    let row =
        sqlx::query("SELECT COUNT(*) AS rows_count, SUM(byte_length) AS bytes FROM media_assets")
            .fetch_one(pool)
            .await
            .unwrap();
    let recorded_rows: i64 = row.get("rows_count");
    let recorded_bytes: i64 = row.get("bytes");

    for _ in 0..3 {
        let started = Instant::now();
        let blobs = fixture.archive.list_blobs().expect("archive enumeration");
        let bytes: u64 = blobs.iter().map(|(_, length)| *length).sum();
        samples.push(started.elapsed());

        assert_eq!(
            blobs.len() as i64,
            recorded_rows,
            "the archive holds a different number of blobs than media_assets records"
        );
        assert_eq!(
            bytes as i64, recorded_bytes,
            "archive bytes do not reconcile with recorded byte lengths"
        );
        assert_eq!(bytes, fixture.stats.archive_bytes);
    }

    // Every recorded relative path must resolve inside the archive: accounting
    // that counts files without checking they are the recorded ones is not
    // accounting.
    let paths =
        sqlx::query("SELECT relative_path FROM media_assets ORDER BY relative_path LIMIT 500")
            .fetch_all(pool)
            .await
            .unwrap();
    for row in &paths {
        let relative: String = row.get("relative_path");
        assert!(
            fixture.archive.exists(&relative).unwrap(),
            "media_assets references a blob that is not in the archive: {relative}"
        );
        assert!(relative.starts_with("media/sha256/"));
    }

    judge(&samples, thresholds, tier)
}

/// Per-recording export accounting through the repositories.
async fn bench_export_manifest(
    fixture: &ScaleFixture,
    thresholds: &Thresholds,
    tier: &str,
) -> Verdict {
    let recordings = RecordingRepository::new(fixture.db.clone());
    let assets = MediaAssetRepository::new(fixture.db.clone());
    let archive: &MediaArchive = &fixture.archive;
    let spec = *fixture.spec();
    let mut samples = Samples::new("export_manifest", "recording");

    for step in 0..EXPORT_SAMPLES {
        // Spread across the library rather than taking a contiguous prefix,
        // so the sample is not one warm page of the B-tree.
        let index =
            u32::try_from((u64::from(spec.recordings) * step as u64) / EXPORT_SAMPLES as u64)
                .unwrap();
        let generated = fixture.library.recording(index);
        let id: RecordingId = generated.id;

        let started = Instant::now();
        let recording = recordings.get(id).await.expect("recording row");
        let preferred = recording
            .preferred_media_asset_id()
            .expect("every loaded recording has preferred media");
        let asset = assets.get(preferred).await.expect("media asset row");
        let present = archive.exists(&asset.relative_path).expect("archive probe");
        let segment_count: i64 = sqlx::query(
            "SELECT COUNT(*) FROM transcript_segments s
             JOIN transcript_revisions t ON t.id = s.transcript_revision_id
             WHERE t.recording_id = ?1",
        )
        .bind(id.to_string())
        .fetch_one(fixture.db.pool())
        .await
        .unwrap()
        .get(0);
        samples.push(started.elapsed());

        assert!(present, "export would reference a missing blob");
        assert_eq!(recording.title, generated.title);
        assert_eq!(recording.duration_ms, Some(generated.duration_ms));
        assert_eq!(
            recording.tags.iter().cloned().collect::<Vec<_>>(),
            generated.tags
        );
        assert_eq!(asset.identity, generated.media.identity);
        assert_eq!(segment_count as u32, generated.segment_count);
    }

    judge(&samples, thresholds, tier)
}

/// Run the memory ladder for the published report.
fn measure_memory(thresholds: &Thresholds) -> MemoryReport {
    let budget = thresholds.memory().expect("memory budget");
    let window = ChunkWindow::DEFAULT;
    let allowed = memory::allowed_growth_bytes(budget, window);
    let source = rss::source().as_str().to_owned();

    if rss::sample().is_none() {
        return MemoryReport {
            rss_source: source,
            skipped_reason: Some(rss::unsupported_reason()),
            window_bytes: window.window_bytes(),
            allowed_growth_bytes: allowed,
            observations: Vec::new(),
            control_growth_bytes: None,
            control_total_pcm_bytes: None,
        };
    }

    let observations = MEMORY_LADDER
        .iter()
        .filter_map(|minutes| memory::observe_windowed(*minutes, window))
        .collect::<Vec<_>>();
    // The control runs last: it is the only part of the suite that is
    // supposed to retain hundreds of megabytes.
    let control = memory::observe_unbounded(MEMORY_LADDER[1], window);

    MemoryReport {
        rss_source: source,
        skipped_reason: None,
        window_bytes: window.window_bytes(),
        allowed_growth_bytes: allowed,
        observations,
        control_growth_bytes: control.map(|c| c.peak_growth_bytes),
        control_total_pcm_bytes: control.map(|c| c.total_pcm_bytes),
    }
}

fn judge(samples: &Samples, thresholds: &Thresholds, tier: &str) -> Verdict {
    let budget = thresholds
        .budget(tier, samples.name())
        .unwrap_or_else(|error| panic!("{error}"));
    assert!(
        !samples.is_empty(),
        "{} recorded no samples",
        samples.name()
    );
    Verdict::judge(samples.summarize(), budget)
}

/// `count` page indexes spread over `pages`, always including the last page.
fn sample_offsets(pages: u64, count: usize) -> Vec<u64> {
    if pages == 0 {
        return Vec::new();
    }
    let count = u64::try_from(count).unwrap_or(1).min(pages);
    if count <= 1 {
        return vec![pages - 1];
    }
    (0..count)
        .map(|step| (pages - 1) * step / (count - 1))
        .collect()
}

/// A dataset-free check that the generator and the suite agree about tiers.
#[test]
fn every_benchmark_has_a_budget_in_every_tier() {
    let thresholds = Thresholds::load().unwrap();
    let policy = thresholds.policy().unwrap();
    let expected = [
        "library_page_offset",
        "library_page_keyset",
        "transcript_search",
        "transcript_viewport",
        "archive_accounting",
        "export_manifest",
    ];
    for tier in ["ci", "full"] {
        let mut declared = thresholds.benchmark_ids(tier);
        declared.sort();
        let mut wanted: Vec<String> = expected.iter().map(|name| (*name).to_owned()).collect();
        wanted.sort();
        assert_eq!(
            declared, wanted,
            "tier `{tier}` must declare a budget for exactly the benchmarks that run"
        );
        for name in expected {
            let budget = thresholds.budget(tier, name).unwrap();
            assert!(budget.p50_ms > 0.0 && budget.p95_ms >= budget.p50_ms);
            assert!(
                budget.max_ms >= budget.p95_ms,
                "`{name}` in tier `{tier}` has a max budget below its p95"
            );
            // A budget inside the machine's own noise is a flaky test, not a
            // stricter guard; the floors are part of the committed policy.
            assert!(
                policy.respects_floors(budget),
                "`{name}` in tier `{tier}` is budgeted below the policy floors \
                 (p50 >= {}, p95 >= {}, max >= {}): {budget:?}",
                policy.noise_floor_ms,
                policy.tail_floor_p95_ms,
                policy.tail_floor_max_ms
            );
        }
    }
}

/// Page sampling must cover the library, including its deepest page.
#[test]
fn offset_sampling_covers_the_whole_library() {
    assert_eq!(sample_offsets(0, 10), Vec::<u64>::new());
    assert_eq!(sample_offsets(1, 10), vec![0]);
    assert_eq!(sample_offsets(5, 10), vec![0, 1, 2, 3, 4]);
    let wide = sample_offsets(201, 5);
    assert_eq!(wide.first(), Some(&0));
    assert_eq!(wide.last(), Some(&200));
    assert_eq!(wide.len(), 5);
}

/// The generator used by the suite is the one the results claim.
#[test]
fn the_reported_fingerprint_is_the_dataset_that_ran() {
    let spec = DatasetSpec::ci(7);
    let library = SyntheticLibrary::new(spec);
    assert_eq!(
        library.fingerprint(),
        SyntheticLibrary::new(spec).fingerprint()
    );
    assert_ne!(
        library.fingerprint(),
        SyntheticLibrary::new(DatasetSpec::ci(8)).fingerprint()
    );
}
