//! Interaction tests for the AI workflow panels, driven through AccessKit by
//! `egui_kittest` and the deterministic harness in `plaudon-ui-testkit`.
//!
//! Every assertion is made the way a user or a screen reader would make it:
//! find a control by the words on it, act on it, and then check which
//! [`plaudon_app::command::AppCommand`] the panel produced. No test reaches
//! into a panel's private state.

mod ai_fixtures;

use ai_fixtures as fx;
use egui_kittest::kittest::Queryable as _;
use egui_kittest::Harness;
use plaudon_app::command::AppCommand;
use plaudon_app::dto::{ExportFormat, JobState};
use plaudon_domain::transcript::AlignmentOrigin;
use plaudon_ui::ai::{
    AiIntent, AiOutput, BudgetPanel, ExportMenu, JobCard, ProcessingHistory, RecordingContext,
    TranscriptionPanel,
};

/// Collects what the panels produced across the frames of one `run`.
/// A window tall enough that no control of a panel is clipped.
///
/// The shell hosts these panels inside a scroll area; a test that scrolled
/// before every click would assert on scrolling, not on the panel. The
/// deterministic width from `plaudon-ui-testkit` is kept, and these tests take
/// no pixel snapshot, so the snapshot policy is unaffected.
const TALL: egui::Vec2 = egui::vec2(1280.0, 4000.0);

#[derive(Default)]
struct Collected {
    names: Vec<&'static str>,
    intents: Vec<String>,
    start_processing: Vec<plaudon_app::dto::StartProcessingRequest>,
    grants: Vec<plaudon_app::dto::GrantCloudConsentRequest>,
    retries: Vec<plaudon_app::dto::RetryJobRequest>,
    cancels: Vec<plaudon_app::dto::CancelJobRequest>,
    budgets: Vec<plaudon_app::dto::SetCloudBudgetRequest>,
    jobs_queries: Vec<plaudon_app::dto::ListJobsRequest>,
    export_paths: Vec<(ExportFormat, String)>,
}

impl Collected {
    fn absorb(&mut self, output: AiOutput) {
        for intent in output.intents {
            match intent {
                AiIntent::Command(command) => {
                    self.names.push(command.name());
                    match command {
                        AppCommand::StartProcessing(request) => {
                            self.start_processing.push(request);
                        }
                        AppCommand::GrantCloudConsent(request) => self.grants.push(request),
                        AppCommand::RetryJob(request) => self.retries.push(request),
                        AppCommand::CancelJob(request) => self.cancels.push(request),
                        AppCommand::SetCloudBudget(request) => self.budgets.push(request),
                        AppCommand::ListJobs(request) => self.jobs_queries.push(request),
                        _ => {}
                    }
                }
                AiIntent::Navigate(intent) => self.intents.push(format!("{intent:?}")),
                AiIntent::PickTranscriptExportPath {
                    format,
                    suggested_file_name,
                    ..
                } => self.export_paths.push((format, suggested_file_name)),
                other => self.intents.push(format!("{other:?}")),
            }
        }
    }
}

fn context<'a>(
    capabilities: &'a plaudon_app::dto::Capabilities,
    alignment: AlignmentOrigin,
) -> RecordingContext<'a> {
    RecordingContext {
        recording_id: fx::RECORDING_ID,
        duration_ms: Some(600_000),
        revision_id: Some(fx::REVISION_ID),
        alignment_origin: alignment,
        title: "Board meeting",
        capabilities: Some(capabilities),
    }
}

/* ------------------------------------------------------------ transcription */

struct TranscriptionApp {
    panel: TranscriptionPanel,
    capabilities: plaudon_app::dto::Capabilities,
    models: plaudon_app::dto::ListModelsResponse,
    job: Option<plaudon_app::dto::JobSummary>,
    collected: Collected,
}

impl TranscriptionApp {
    fn new(local_only: bool, consents: Vec<plaudon_app::dto::ConsentSummary>) -> Self {
        Self {
            panel: TranscriptionPanel::default(),
            capabilities: fx::capabilities(local_only, consents),
            models: fx::models(vec![fx::local_model(), fx::cloud_model()]),
            job: None,
            collected: Collected::default(),
        }
    }

    fn view(&mut self, ui: &mut egui::Ui) {
        let context = context(&self.capabilities, AlignmentOrigin::Native);
        let output = self.panel.ui(
            ui,
            context,
            Some(&self.models),
            self.job.as_ref(),
            fx::NOW_MS,
        );
        self.collected.absorb(output);
    }
}

fn transcription_harness(app: TranscriptionApp) -> Harness<'static, TranscriptionApp> {
    plaudon_ui_testkit::builder()
        .with_size(TALL)
        .build_ui_state(
            |ui, app: &mut TranscriptionApp| {
                app.view(ui);
            },
            app,
        )
}

#[test]
fn the_engine_inventory_lists_every_stt_engine_with_where_it_runs() {
    let mut harness = transcription_harness(TranscriptionApp::new(false, Vec::new()));
    harness.run();
    let labels: Vec<String> = plaudon_ui_testkit::a11y::nodes(&harness)
        .into_iter()
        .filter_map(|node| node.announced().map(str::to_owned))
        .collect();
    let joined = labels.join("\n");
    assert!(joined.contains("Apple native speech"), "{joined}");
    assert!(joined.contains("whisper.cpp"), "{joined}");
    assert!(joined.contains("OpenRouter"), "{joined}");
    assert!(joined.contains("On this computer"), "{joined}");
    assert!(joined.contains("Through a cloud provider"), "{joined}");
}

#[test]
fn a_local_engine_and_model_start_the_job_without_a_consent_sheet() {
    let mut harness = transcription_harness(TranscriptionApp::new(false, Vec::new()));
    harness.run();
    harness.get_by_label("whisper.cpp").click();
    harness.run();
    harness.get_by_label("Whisper small").click();
    harness.run();
    harness.get_by_label("Transcribe this recording").click();
    harness.run();

    let collected = &harness.state().collected;
    assert_eq!(collected.start_processing.len(), 1, "{:?}", collected.names);
    let request = &collected.start_processing[0];
    assert_eq!(request.stage, "transcribe");
    assert_eq!(
        request.model_override.as_ref().expect("override").engine,
        "whisper.cpp"
    );
    assert!(collected.grants.is_empty());
}

#[test]
fn local_only_mode_refuses_a_cloud_engine_and_dispatches_nothing() {
    let mut harness = transcription_harness(TranscriptionApp::new(true, Vec::new()));
    harness.run();
    let joined = plaudon_ui_testkit::a11y::nodes(&harness)
        .into_iter()
        .filter_map(|node| node.announced().map(str::to_owned))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        joined.contains("Blocked by Local-only Mode"),
        "the OpenRouter row must say it is blocked: {joined}"
    );

    // The cloud engine's radio is disabled, so clicking it changes nothing and
    // no command can be produced from it.
    harness.get_by_label("OpenRouter").click();
    harness.run();
    harness.get_by_label("whisper.cpp").click();
    harness.run();
    assert!(harness.state().collected.start_processing.is_empty());
    assert!(harness.state().collected.grants.is_empty());
}

#[test]
fn a_cloud_model_without_a_grant_opens_the_consent_sheet_before_sending() {
    let mut harness = transcription_harness(TranscriptionApp::new(false, Vec::new()));
    harness.run();
    harness.get_by_label("OpenRouter").click();
    harness.run();
    harness.get_by_label("Vendor Whisper Large").click();
    harness.run();
    harness.get_by_label("Transcribe this recording").click();
    harness.run();

    assert!(
        harness.state().collected.start_processing.is_empty(),
        "nothing may be dispatched before the disclosure is answered"
    );
    let joined = plaudon_ui_testkit::a11y::nodes(&harness)
        .into_iter()
        .filter_map(|node| node.announced().map(str::to_owned))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        joined.contains("Send this content to a cloud provider?"),
        "{joined}"
    );
    assert!(joined.contains("openrouter → vendor"), "{joined}");
    assert!(joined.contains("Provider default retention"), "{joined}");
    assert!(joined.contains("Estimated USD"), "{joined}");
    assert!(joined.contains("Disclosure version 2026-02-01"), "{joined}");

    harness.get_by_label("Allow for this recording").click();
    harness.run();
    harness.get_by_label("Approve and continue").click();
    harness.run();

    let collected = &harness.state().collected;
    assert_eq!(collected.grants.len(), 1, "{:?}", collected.names);
    assert_eq!(collected.grants[0].scope.kind, "recording");
    assert_eq!(
        collected.grants[0].scope.recording_id.as_deref(),
        Some(fx::RECORDING_ID)
    );
    assert_eq!(
        collected.start_processing.len(),
        1,
        "an authorising scope dispatches the run the user asked for"
    );
}

#[test]
fn ask_every_time_stores_a_preference_and_sends_nothing() {
    let mut harness = transcription_harness(TranscriptionApp::new(false, Vec::new()));
    harness.run();
    harness.get_by_label("OpenRouter").click();
    harness.run();
    harness.get_by_label("Vendor Whisper Large").click();
    harness.run();
    harness.get_by_label("Transcribe this recording").click();
    harness.run();
    harness
        .get_by_label("Save preference without sending")
        .click();
    harness.run();

    let collected = &harness.state().collected;
    assert_eq!(collected.grants.len(), 1);
    assert_eq!(collected.grants[0].scope.kind, "ask_every_time");
    assert!(
        collected.start_processing.is_empty(),
        "ask every time authorises nothing"
    );
}

#[test]
fn requiring_zero_retention_on_a_route_without_it_fails_closed() {
    let mut harness = transcription_harness(TranscriptionApp::new(false, Vec::new()));
    harness.run();
    harness.get_by_label("OpenRouter").click();
    harness.run();
    harness.get_by_label("Vendor Whisper Large").click();
    harness.run();
    harness.get_by_label("Transcribe this recording").click();
    harness.run();
    harness
        .get_by_label(
            "Require documented zero retention. If it cannot be confirmed for this exact route, \
             the request is refused rather than sent.",
        )
        .click();
    harness.run();

    let joined = plaudon_ui_testkit::a11y::nodes(&harness)
        .into_iter()
        .filter_map(|node| node.announced().map(str::to_owned))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        joined.contains("Zero retention cannot be confirmed"),
        "{joined}"
    );

    harness
        .get_by_label("Save preference without sending")
        .click();
    harness.run();
    let collected = &harness.state().collected;
    assert!(
        collected.grants.is_empty(),
        "the approve control is disabled while the requirement cannot be met"
    );
    assert!(collected.start_processing.is_empty());
}

#[test]
fn an_existing_model_policy_grant_skips_the_sheet() {
    let consents = vec![fx::grant("model_policy", "vendor/whisper-large", None)];
    let mut harness = transcription_harness(TranscriptionApp::new(false, consents));
    harness.run();
    harness.get_by_label("OpenRouter").click();
    harness.run();
    harness.get_by_label("Vendor Whisper Large").click();
    harness.run();
    harness.get_by_label("Transcribe this recording").click();
    harness.run();

    let collected = &harness.state().collected;
    assert!(collected.grants.is_empty());
    assert_eq!(collected.start_processing.len(), 1);
}

#[test]
fn a_per_recording_override_never_writes_the_task_default() {
    let mut harness = transcription_harness(TranscriptionApp::new(false, Vec::new()));
    harness.run();
    harness.get_by_label("whisper.cpp").click();
    harness.run();
    harness.get_by_label("Whisper small").click();
    harness.run();
    harness
        .get_by_label("Use this model for this recording only")
        .click();
    harness.run();

    let names = &harness.state().collected.names;
    assert_eq!(names, &["set_model_preference"]);
}

/* ------------------------------------------------------------------- jobs */

struct JobApp {
    card: JobCard,
    job: plaudon_app::dto::JobSummary,
    collected: Collected,
}

fn job_harness(job: plaudon_app::dto::JobSummary) -> Harness<'static, JobApp> {
    plaudon_ui_testkit::builder()
        .with_size(TALL)
        .build_ui_state(
            |ui, app: &mut JobApp| {
                egui::Frame::NONE.show(ui, |ui| {
                    let output = app.card.ui(ui, &app.job, fx::NOW_MS);
                    app.collected.absorb(output);
                });
            },
            JobApp {
                card: JobCard::default(),
                job,
                collected: Collected::default(),
            },
        )
}

#[test]
fn a_stage_without_a_ratio_shows_indeterminate_progress_not_zero() {
    let mut harness = job_harness(fx::job(JobState::RunningLocal, None));
    harness.run_steps(4);
    let joined = plaudon_ui_testkit::a11y::nodes(&harness)
        .into_iter()
        .filter_map(|node| node.announced().map(str::to_owned))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(joined.contains("progress not reported"), "{joined}");
    assert!(!joined.contains("0% complete"), "{joined}");
}

#[test]
fn a_reported_fraction_is_rendered_as_that_fraction() {
    let mut harness = job_harness(fx::job(JobState::RunningLocal, Some(0.42)));
    harness.run_steps(4);
    let joined = plaudon_ui_testkit::a11y::nodes(&harness)
        .into_iter()
        .filter_map(|node| node.announced().map(str::to_owned))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(joined.contains("42% complete"), "{joined}");
}

#[test]
fn cancelling_discloses_that_an_accepted_cloud_request_is_not_recalled() {
    let mut harness = job_harness(fx::job(JobState::CloudSending, None));
    harness.run_steps(4);
    harness.get_by_label("Cancel job").click();
    harness.run_steps(4);

    let joined = plaudon_ui_testkit::a11y::nodes(&harness)
        .into_iter()
        .filter_map(|node| node.announced().map(str::to_owned))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        joined.contains("A cloud request that was already accepted may not be reversed"),
        "{joined}"
    );
    assert!(harness.state().collected.cancels.is_empty());

    harness.get_by_label("Cancel the job").click();
    harness.run_steps(4);
    assert_eq!(harness.state().collected.cancels.len(), 1);
}

#[test]
fn an_unknown_cloud_outcome_cannot_be_retried_without_acknowledging_the_cost() {
    let mut harness = job_harness(fx::job(JobState::CloudOutcomeUnknown, None));
    harness.run_steps(4);
    let joined = plaudon_ui_testkit::a11y::nodes(&harness)
        .into_iter()
        .filter_map(|node| node.announced().map(str::to_owned))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(joined.contains("Cloud outcome unknown"), "{joined}");

    harness.get_by_label("Retry job").click();
    harness.run_steps(4);
    assert!(
        harness.state().collected.retries.is_empty(),
        "the retry control is disabled until the cost is acknowledged"
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
}

#[test]
fn a_provider_outage_offers_retry_local_engine_and_keep_queued() {
    let mut job = fx::job(JobState::FailedRetryable, None);
    job.error = Some(fx::envelope(
        plaudon_domain::error::AppErrorCode::SourceUnavailable,
    ));
    let mut harness = job_harness(job);
    harness.run_steps(4);
    let joined = plaudon_ui_testkit::a11y::nodes(&harness)
        .into_iter()
        .filter_map(|node| node.announced().map(str::to_owned))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(joined.contains("OpenRouter is unavailable"), "{joined}");
    assert!(joined.contains("Choose local engine"), "{joined}");
    assert!(joined.contains("Keep queued"), "{joined}");

    harness.get_by_label("Choose local engine").click();
    harness.run_steps(4);
    assert_eq!(
        harness.state().collected.intents,
        vec!["ChooseLocalEngine".to_owned()]
    );

    harness.get_by_label("Keep queued").click();
    harness.run_steps(4);
    assert!(
        harness.state().collected.names.is_empty(),
        "keeping a job queued dispatches nothing"
    );
}

#[test]
fn exhausted_credits_never_offer_a_billable_retry() {
    let mut job = fx::job(JobState::FailedRetryable, None);
    job.error = Some(fx::envelope(
        plaudon_domain::error::AppErrorCode::BudgetExceeded,
    ));
    let mut harness = job_harness(job);
    harness.run_steps(4);
    let joined = plaudon_ui_testkit::a11y::nodes(&harness)
        .into_iter()
        .filter_map(|node| node.announced().map(str::to_owned))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(joined.contains("Provider credits exhausted"), "{joined}");
    assert!(joined.contains("Open key settings"), "{joined}");
    assert!(!joined.contains("Retry job"), "{joined}");
}

#[test]
fn a_full_disk_asks_for_the_capacity_comparison() {
    let mut job = fx::job(JobState::FailedRetryable, None);
    job.error = Some(fx::envelope(
        plaudon_domain::error::AppErrorCode::StorageFull,
    ));
    let mut harness = job_harness(job);
    harness.run_steps(4);
    let joined = plaudon_ui_testkit::a11y::nodes(&harness)
        .into_iter()
        .filter_map(|node| node.announced().map(str::to_owned))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(joined.contains("Not enough space"), "{joined}");
    assert!(
        joined.contains("Compare the space this job needs"),
        "{joined}"
    );
    assert!(joined.contains("Open storage settings"), "{joined}");
}

/* ------------------------------------------------------------------ export */

struct ExportApp {
    menu: ExportMenu,
    alignment: AlignmentOrigin,
    capabilities: plaudon_app::dto::Capabilities,
    collected: Collected,
}

fn export_harness(alignment: AlignmentOrigin) -> Harness<'static, ExportApp> {
    plaudon_ui_testkit::builder()
        .with_size(TALL)
        .build_ui_state(
            |ui, app: &mut ExportApp| {
                egui::Frame::NONE.show(ui, |ui| {
                    let context = context(&app.capabilities, app.alignment);
                    let output = app.menu.ui(ui, context, None);
                    app.collected.absorb(output);
                });
            },
            ExportApp {
                menu: ExportMenu::default(),
                alignment,
                capabilities: fx::capabilities(false, Vec::new()),
                collected: Collected::default(),
            },
        )
}

#[test]
fn all_five_transcript_formats_are_offered() {
    let mut harness = export_harness(AlignmentOrigin::Native);
    harness.run();
    for label in [
        "Plain text",
        "Markdown",
        "Structured JSON",
        "SubRip captions",
        "WebVTT captions",
    ] {
        assert!(
            plaudon_ui_testkit::a11y::has_label(&harness, label),
            "missing {label}"
        );
    }
}

#[test]
fn captions_are_unavailable_without_real_timestamps() {
    let mut harness = export_harness(AlignmentOrigin::None);
    harness.run();
    let joined = plaudon_ui_testkit::a11y::nodes(&harness)
        .into_iter()
        .filter_map(|node| node.announced().map(str::to_owned))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(joined.contains("SubRip captions (unavailable)"), "{joined}");
    assert!(
        joined.contains("This transcript has no real timestamps; we do not invent seek times."),
        "{joined}"
    );

    harness
        .get_by_label("SubRip captions (unavailable)")
        .click();
    harness.run();
    assert!(harness.state().collected.export_paths.is_empty());
}

#[test]
fn choosing_a_format_asks_the_host_for_a_path() {
    let mut harness = export_harness(AlignmentOrigin::Native);
    harness.run();
    harness.get_by_label("WebVTT captions").click();
    harness.run();
    let paths = &harness.state().collected.export_paths;
    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0].0, ExportFormat::Vtt);
    assert_eq!(paths[0].1, "Board meeting.vtt");
}

/* ------------------------------------------------------------------ budget */

struct BudgetApp {
    panel: BudgetPanel,
    usage: plaudon_app::dto::GetUsageResponse,
    collected: Collected,
}

#[test]
fn unknown_spend_is_counted_and_never_added_in_as_zero() {
    let mut harness = plaudon_ui_testkit::builder()
        .with_size(TALL)
        .build_ui_state(
            |ui, app: &mut BudgetApp| {
                egui::Frame::NONE.show(ui, |ui| {
                    let output = app.panel.ui(ui, Some(&app.usage));
                    app.collected.absorb(output);
                });
            },
            BudgetApp {
                panel: BudgetPanel::default(),
                usage: fx::usage(3),
                collected: Collected::default(),
            },
        );
    harness.run();
    let joined = plaudon_ui_testkit::a11y::nodes(&harness)
        .into_iter()
        .filter_map(|node| node.announced().map(str::to_owned))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        joined.contains("Attempts with no derivable cost: 3"),
        "{joined}"
    );
    assert!(
        joined.contains("Provider-reported (finalized): USD 1.250000"),
        "{joined}"
    );
    assert!(joined.contains("Estimated: USD 0.400000"), "{joined}");
    assert!(
        joined.contains("A local cap cannot prevent a charge a provider has already accepted."),
        "{joined}"
    );

    harness.get_by_label("Save budget").click();
    harness.run();
    let budgets = &harness.state().collected.budgets;
    assert_eq!(budgets.len(), 1);
    assert_eq!(budgets[0].per_job_limit.as_deref(), Some("0.500000"));
    assert_eq!(budgets[0].monthly_limit, None);
}

/* ----------------------------------------------------------------- history */

struct HistoryApp {
    panel: ProcessingHistory,
    page: plaudon_app::dto::JobPage,
    collected: Collected,
}

#[test]
fn filtering_the_history_asks_for_a_fresh_page() {
    let mut harness = plaudon_ui_testkit::builder()
        .with_size(TALL)
        .build_ui_state(
            |ui, app: &mut HistoryApp| {
                egui::Frame::NONE.show(ui, |ui| {
                    let output = app.panel.ui(ui, Some(&app.page), fx::NOW_MS);
                    app.collected.absorb(output);
                });
            },
            HistoryApp {
                panel: ProcessingHistory::default(),
                page: fx::job_page(vec![fx::job(JobState::Completed, Some(1.0))]),
                collected: Collected::default(),
            },
        );
    harness.run();
    harness.get_by_label("Transcription").click();
    harness.run();
    let queries = &harness.state().collected.jobs_queries;
    assert_eq!(queries.len(), 1, "{:?}", harness.state().collected.names);
    assert_eq!(
        queries[0].kinds,
        vec![plaudon_app::dto::JobKind::Transcribe]
    );
    assert_eq!(queries[0].limit, 50);
}

#[test]
fn the_transcription_panel_is_fully_reachable_by_keyboard() {
    let mut harness = transcription_harness(TranscriptionApp::new(false, Vec::new()));
    harness.run();
    // The model rows exist only once an engine is chosen, which is the point:
    // the panel offers no model until it knows which engine would run it.
    harness.get_by_label("whisper.cpp").click();
    harness.run();
    let order = plaudon_ui_testkit::a11y::focus_order(&mut harness, 40);
    assert!(
        order.iter().any(|label| label.contains("whisper.cpp")),
        "{order:?}"
    );
    assert!(
        order
            .iter()
            .any(|label| label.contains("Add custom model ID")),
        "{order:?}"
    );
}
