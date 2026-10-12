//! Interaction tests for the jobs screen, driven through AccessKit by
//! `egui_kittest` and the deterministic harness in `plaudon-ui-testkit`.
//!
//! Every assertion is made the way a user or a screen reader would make it:
//! find a control by the words on it, act on it, and check which
//! [`plaudon_app::command::AppCommand`] or [`plaudon_ui::ai::AiIntent`] the
//! screen produced. No test reaches into the screen's private state except
//! through the public deep-link and resync entry points, which is what the
//! shell uses too.

use egui_kittest::kittest::Queryable as _;
use egui_kittest::Harness;
use plaudon_app::command::AppCommand;
use plaudon_app::dto::{
    CostValue, JobKind, JobPage, JobState, JobSummary, ListJobsRequest, StorageCapability,
};
use plaudon_domain::error::{AppErrorCode, ErrorEnvelope};
use plaudon_ui::ai::{AiIntent, AiOutput};
use plaudon_ui::jobs::JobsScreen;
use std::collections::BTreeMap;

/// A window tall enough that no row of the screen is clipped.
const TALL: egui::Vec2 = egui::vec2(1280.0, 6000.0);

/// A fixed wall clock for elapsed labels.
const NOW_MS: i128 = 1_770_000_000_000;

const RECORDING_ID: &str = "11111111-1111-1111-1111-111111111111";

/* ------------------------------------------------------------- fixtures */

fn job_id(index: u32) -> String {
    format!("{index:08x}-0000-0000-0000-000000000000")
}

fn job(index: u32, state: JobState, progress: Option<f64>) -> JobSummary {
    JobSummary {
        job_id: job_id(index).parse().expect("job id"),
        kind: JobKind::Transcribe,
        state,
        stage: Some("transcribe".to_owned()),
        progress,
        created_at: "2026-02-01T10:00:00Z".to_owned(),
        updated_at: "2026-02-01T10:00:10Z".to_owned(),
        started_at: Some("2026-02-01T10:00:00Z".to_owned()),
        finished_at: None,
        recording_id: Some(RECORDING_ID.parse().expect("recording id")),
        label: format!("Recording {index}"),
        error: None,
        cost_acknowledgement_required: false,
        provider_request_id: None,
        output_link: None,
        cost: CostValue::unknown(),
    }
}

fn envelope(code: AppErrorCode) -> ErrorEnvelope {
    ErrorEnvelope {
        code,
        message: code.canonical_message().to_owned(),
        retryable: code.is_retryable(),
        action: code.action(),
        diagnostic_id: "88888888-8888-8888-8888-888888888888"
            .parse()
            .expect("diagnostic id"),
    }
}

fn page(items: Vec<JobSummary>) -> JobPage {
    JobPage {
        items,
        next_cursor: None,
        state_counts: BTreeMap::new(),
    }
}

fn storage() -> StorageCapability {
    StorageCapability {
        library_path: "/library".to_owned(),
        free_bytes: 104_857_600,
        total_bytes: 500_107_862_016,
    }
}

/* ---------------------------------------------------------------- harness */

#[derive(Default)]
struct Collected {
    names: Vec<&'static str>,
    intents: Vec<String>,
    jobs_queries: Vec<ListJobsRequest>,
    retries: Vec<plaudon_app::dto::RetryJobRequest>,
    cancels: Vec<plaudon_app::dto::CancelJobRequest>,
}

impl Collected {
    fn absorb(&mut self, output: AiOutput) {
        for intent in output.intents {
            match intent {
                AiIntent::Command(command) => {
                    self.names.push(command.name());
                    match command {
                        AppCommand::ListJobs(request) => self.jobs_queries.push(request),
                        AppCommand::RetryJob(request) => self.retries.push(request),
                        AppCommand::CancelJob(request) => self.cancels.push(request),
                        _ => {}
                    }
                }
                AiIntent::Navigate(intent) => self.intents.push(format!("{intent:?}")),
                other => self.intents.push(format!("{other:?}")),
            }
        }
    }
}

struct JobsApp {
    screen: JobsScreen,
    page: Option<JobPage>,
    collected: Collected,
}

fn harness(page: Option<JobPage>) -> Harness<'static, JobsApp> {
    harness_sized(page, TALL)
}

fn harness_sized(page: Option<JobPage>, size: egui::Vec2) -> Harness<'static, JobsApp> {
    plaudon_ui_testkit::builder()
        .with_size(size)
        .build_ui_state(
            |ui, app: &mut JobsApp| {
                let output = app.screen.ui(ui, app.page.as_ref(), NOW_MS);
                app.collected.absorb(output);
            },
            JobsApp {
                screen: JobsScreen::default(),
                page,
                collected: Collected::default(),
            },
        )
}

fn announced(harness: &Harness<'_, JobsApp>) -> String {
    plaudon_ui_testkit::a11y::nodes(harness)
        .into_iter()
        .filter_map(|node| node.announced().map(str::to_owned))
        .collect::<Vec<_>>()
        .join("\n")
}

/* ------------------------------------------------------------------- J1 */

#[test]
fn changing_a_kind_filter_refetches_the_first_page_of_fifty() {
    let mut harness = harness(Some(page(vec![job(1, JobState::Completed, Some(1.0))])));
    harness.run();
    harness.get_by_label("Transcription").click();
    harness.run();

    let queries = &harness.state().collected.jobs_queries;
    assert_eq!(queries.len(), 1, "{:?}", harness.state().collected.names);
    assert_eq!(queries[0].kinds, vec![JobKind::Transcribe]);
    assert_eq!(queries[0].limit, 50);
    assert_eq!(queries[0].cursor, None, "a filter change starts over");
}

#[test]
fn changing_a_state_filter_refetches_under_that_state() {
    let mut counts = BTreeMap::new();
    counts.insert(JobState::Completed, 3_u64);
    let mut loaded = page(vec![job(1, JobState::Completed, Some(1.0))]);
    loaded.state_counts = counts;

    let mut harness = harness(Some(loaded));
    harness.run();
    // Only states the page reports rows in are offered, so the filter cannot
    // promise jobs that do not exist.
    harness.get_by_label("Completed").click();
    harness.run();

    let queries = &harness.state().collected.jobs_queries;
    assert_eq!(queries.len(), 1);
    assert_eq!(queries[0].states, vec![JobState::Completed]);
    assert_eq!(queries[0].limit, 50);
}

#[test]
fn a_full_page_shows_fifty_rows_and_asks_for_the_next_fifty() {
    let items: Vec<JobSummary> = (1..=50)
        .map(|index| job(index, JobState::Completed, Some(1.0)))
        .collect();
    let mut loaded = page(items);
    loaded.next_cursor = Some("cursor-2".to_owned());

    // Fifty job cards are taller than any real window; the test gives itself
    // room for all of them so the paging control at the end is clickable
    // without asserting on scrolling, which the shell owns.
    let mut harness = harness_sized(Some(loaded), egui::vec2(1280.0, 30_000.0));
    harness.run();

    let mut rows: Vec<String> = plaudon_ui_testkit::a11y::nodes(&harness)
        .into_iter()
        .filter_map(|node| node.announced().map(str::to_owned))
        .filter(|label| label.starts_with("Job: Recording "))
        .collect();
    rows.sort_unstable();
    rows.dedup();
    assert_eq!(
        rows.len(),
        50,
        "every row of the page is rendered: {rows:?}"
    );

    harness.get_by_label("Show more jobs").click();
    harness.run();
    let queries = &harness.state().collected.jobs_queries;
    assert_eq!(queries.len(), 1);
    assert_eq!(queries[0].cursor.as_deref(), Some("cursor-2"));
    assert_eq!(queries[0].limit, 50);
}

#[test]
fn the_state_counts_are_the_backend_s_and_there_is_no_total() {
    let mut counts = BTreeMap::new();
    counts.insert(JobState::Completed, 3_u64);
    counts.insert(JobState::FailedRetryable, 1_u64);
    let mut loaded = page(vec![job(1, JobState::Completed, Some(1.0))]);
    loaded.state_counts = counts;

    let mut harness = harness(Some(loaded));
    harness.run();
    let joined = announced(&harness);
    assert!(joined.contains("Completed: 3"), "{joined}");
    assert!(joined.contains("Failed — can retry: 1"), "{joined}");
    assert!(
        !joined.contains("4 jobs"),
        "no total was sent, so none is shown"
    );
}

#[test]
fn an_empty_page_says_so_instead_of_showing_nothing() {
    let mut harness = harness(Some(page(Vec::new())));
    harness.run();
    assert!(announced(&harness).contains("No job matches these filters"));
}

#[test]
fn before_the_first_response_the_screen_says_it_has_not_read_the_list() {
    let mut harness = harness(None);
    harness.run();
    assert!(announced(&harness).contains("has not been read yet"));
}

/* ---------------------------------------------------------------- J2–J5 */

#[test]
fn a_stage_without_a_ratio_shows_indeterminate_progress_not_zero() {
    let mut harness = harness(Some(page(vec![job(1, JobState::RunningLocal, None)])));
    harness.run_steps(4);
    let joined = announced(&harness);
    assert!(joined.contains("progress not reported"), "{joined}");
    assert!(!joined.contains("0% complete"), "{joined}");
}

#[test]
fn a_reported_fraction_is_rendered_as_that_fraction() {
    let mut harness = harness(Some(page(vec![job(1, JobState::RunningLocal, Some(0.42))])));
    harness.run_steps(4);
    assert!(announced(&harness).contains("42% complete"));
}

#[test]
fn cancelling_a_row_discloses_that_an_accepted_request_is_not_recalled() {
    let mut harness = harness(Some(page(vec![job(1, JobState::CloudSending, None)])));
    harness.run_steps(4);
    harness.get_by_label("Cancel job").click();
    harness.run_steps(4);

    let joined = announced(&harness);
    assert!(
        joined.contains("A cloud request that was already accepted may not be reversed"),
        "{joined}"
    );
    assert_eq!(
        harness.state().collected.cancels.len(),
        0,
        "nothing is cancelled until the disclosure is confirmed"
    );

    harness.get_by_label("Cancel the job").click();
    harness.run_steps(4);
    let cancels = &harness.state().collected.cancels;
    assert_eq!(cancels.len(), 1);
    assert_eq!(cancels[0].job_id, job_id(1));
}

#[test]
fn an_uncertain_cloud_outcome_is_retried_only_after_the_cost_is_acknowledged() {
    let mut harness = harness(Some(page(vec![job(
        1,
        JobState::CloudOutcomeUnknown,
        None,
    )])));
    harness.run_steps(4);
    assert!(announced(&harness).contains("Outcome unknown"));

    harness.get_by_label("Retry job").click();
    harness.run_steps(4);
    assert!(
        harness.state().collected.retries.is_empty(),
        "retry is disabled until the cost is acknowledged"
    );

    harness
        .get_by_label(
            "I understand this request may already have been billed and that retrying may be \
             billed again",
        )
        .click();
    harness.run_steps(4);
    harness.get_by_label("Retry job").click();
    harness.run_steps(4);

    let retries = &harness.state().collected.retries;
    assert_eq!(retries.len(), 1);
    assert!(retries[0].cost_acknowledged);
    assert_eq!(retries[0].job_id, job_id(1));
}

/* ------------------------------------------------------------------- J6 */

#[test]
fn a_provider_outage_offers_retry_local_engine_and_keep_queued() {
    let mut failed = job(1, JobState::FailedRetryable, None);
    failed.error = Some(envelope(AppErrorCode::SourceUnavailable));
    let mut harness = harness(Some(page(vec![failed])));
    harness.run_steps(4);

    let joined = announced(&harness);
    assert!(joined.contains("OpenRouter is unavailable"), "{joined}");
    assert!(joined.contains("Choose local engine"), "{joined}");
    assert!(joined.contains("Keep queued"), "{joined}");

    harness.get_by_label("Choose local engine").click();
    harness.run_steps(4);
    assert_eq!(
        harness.state().collected.intents,
        vec!["ChooseLocalEngine".to_owned()]
    );

    // "Keep queued" is the absence of an action: the job stays exactly where
    // it is, so nothing is dispatched for it.
    harness.get_by_label("Keep queued").click();
    harness.run_steps(4);
    assert_eq!(harness.state().collected.retries.len(), 0);
    assert_eq!(harness.state().collected.intents.len(), 1);
}

/* ------------------------------------------------------------------- J7 */

#[test]
fn a_full_disk_compares_free_space_with_the_volume_and_opens_storage_settings() {
    let mut failed = job(1, JobState::FailedRetryable, None);
    failed.error = Some(envelope(AppErrorCode::StorageFull));
    let mut harness = harness(Some(page(vec![failed])));
    harness.state_mut().screen.set_storage(Some(storage()));
    harness.run_steps(4);

    let joined = announced(&harness);
    assert!(joined.contains("Not enough space"), "{joined}");
    assert!(joined.contains("Library volume: /library"), "{joined}");
    // Both figures verbatim: no rounding into a unit the backend never sent.
    assert!(
        joined.contains("Free space: 104857600 bytes of 500107862016 bytes"),
        "{joined}"
    );

    harness
        .get_by_label("Open storage settings for the library volume")
        .click();
    harness.run_steps(4);
    assert!(harness
        .state()
        .collected
        .intents
        .contains(&"OpenStorageSettings".to_owned()));
}

#[test]
fn without_a_storage_capability_the_free_space_is_reported_unknown() {
    let mut failed = job(1, JobState::FailedRetryable, None);
    failed.error = Some(envelope(AppErrorCode::StorageFull));
    let mut harness = harness(Some(page(vec![failed])));
    harness.run_steps(4);

    let joined = announced(&harness);
    assert!(
        joined.contains("Free space on the library volume is unknown"),
        "{joined}"
    );
    assert!(!joined.contains("bytes of"), "{joined}");
}

#[test]
fn a_healthy_row_shows_no_capacity_comparison() {
    let mut harness = harness(Some(page(vec![job(1, JobState::Completed, Some(1.0))])));
    harness.state_mut().screen.set_storage(Some(storage()));
    harness.run_steps(4);
    let joined = announced(&harness);
    assert!(!joined.contains("Free space"), "{joined}");
}

/* ------------------------------------------------------------------- J8 */

#[test]
fn a_deep_link_focuses_the_row_and_says_it_was_opened_from_a_link() {
    let items = (1..=6)
        .map(|index| job(index, JobState::Completed, Some(1.0)))
        .collect();
    let mut harness = harness(Some(page(items)));
    harness.state_mut().screen.focus_job(&job_id(4));
    harness.run_steps(4);

    let joined = announced(&harness);
    assert!(
        joined.contains("Job: Recording 4 — opened from a link"),
        "{joined}"
    );
    assert!(
        !joined.contains("Job: Recording 3 — opened"),
        "only the linked row claims the link"
    );
    assert_eq!(
        plaudon_ui_testkit::a11y::focused_label(&harness).as_deref(),
        Some("Job: Recording 4 — opened from a link")
    );
}

/* ------------------------------------------------------------------- J9 */

#[test]
fn the_resync_request_repeats_the_page_on_screen_under_the_current_filters() {
    let mut loaded = page(vec![job(1, JobState::Completed, Some(1.0))]);
    loaded.next_cursor = Some("cursor-2".to_owned());
    let mut harness = harness(Some(loaded));
    harness.run();

    // Nothing fetched yet: the first page, under no filters.
    let initial = harness.state().screen.resync_request();
    assert_eq!(initial.cursor, None);
    assert_eq!(initial.limit, 50);
    assert!(initial.states.is_empty() && initial.kinds.is_empty());

    harness.get_by_label("Transcription").click();
    harness.run();
    harness.get_by_label("Show more jobs").click();
    harness.run();

    // After paging, a dropped `job.updated` is recovered by asking for the
    // very page the user is looking at, under the very filters they set.
    let resync = harness.state().screen.resync_request();
    assert_eq!(resync.cursor.as_deref(), Some("cursor-2"));
    assert_eq!(resync.limit, 50);
    assert_eq!(resync.kinds, vec![JobKind::Transcribe]);
}

/* ------------------------------------------------------------- keyboard */

#[test]
fn every_control_of_the_jobs_screen_is_reachable_by_keyboard() {
    let mut counts = BTreeMap::new();
    counts.insert(JobState::CloudSending, 1_u64);
    // A terminal failure, so no indeterminate bar animates: `focus_order`
    // steps the harness frame by frame and a continuously repainting frame
    // would never settle.
    let mut failed = job(2, JobState::FailedTerminal, None);
    failed.error = Some(envelope(AppErrorCode::StorageFull));
    let mut loaded = page(vec![job(1, JobState::CloudSending, Some(0.5)), failed]);
    loaded.state_counts = counts;
    loaded.next_cursor = Some("cursor-2".to_owned());

    let mut harness = harness(Some(loaded));
    harness.state_mut().screen.set_storage(Some(storage()));
    harness.run_steps(4);

    let order = plaudon_ui_testkit::a11y::focus_order(&mut harness, 60);
    for expected in [
        "Sending to provider",
        "Transcription",
        "Job: Recording 1",
        "Cancel job",
        "Job: Recording 2",
        "Free up space",
        "Open storage settings",
        "Open storage settings for the library volume",
        "Show more jobs",
    ] {
        assert!(
            order.iter().any(|label| label == expected),
            "{expected:?} is not keyboard reachable: {order:?}"
        );
    }
}
