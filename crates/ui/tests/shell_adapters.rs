//! Interaction tests for the shell adapters of EPIC O task O2.7.
//!
//! An adapter is the seam between a screen that knows nothing about the shell
//! and the shell that knows nothing about the screen, so it cannot be tested
//! against a stub on either side: these run the real [`plaudon_app`] engine on
//! a temporary library and drive the real [`Screen`] through the real
//! [`ScreenCtx`]. No mock backend, no network, no window.
//!
//! What a frame dispatched is read back from the engine's own event stream —
//! the published name of every command it answered — because that is the only
//! place a dispatched command is observable without reaching inside the
//! dispatcher.

mod fixtures;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use plaudon_app::command::{AppResponse, CommandId};
use plaudon_app::dto::{
    ActivityEntry, ActivityPage, ActivityTarget, ActivityTargetKind, CostValue, DeviceSection,
    JobKind, JobPage, JobState, JobSummary, NotificationCategory,
};
use plaudon_app::event::AppEvent;
use plaudon_app::job_presentation::job_kind_label;
use plaudon_app::notify::UnsupportedNotifier;
use plaudon_app::runtime::{AppEngine, EngineConfig};
use plaudon_app::secrets::InMemorySecretStore;
use plaudon_ui::activity::shell_adapter::ActivityShellScreen;
use plaudon_ui::jobs::shell_adapter::JobsShellScreen;
use plaudon_ui::screens::devices::shell_adapter::DevicesShellScreen;
use plaudon_ui::screens::sources::shell_adapter::SourcesShellScreen;
use plaudon_ui::settings::SettingsDestinationScreen;
use plaudon_ui::shell::screen::ShellBus;
use plaudon_ui::shell::{HostHooks, Route, Screen, ScreenCtx};
use plaudon_ui::theme::{ThemeKind, Tokens};
use plaudon_ui_testkit::kittest::Queryable as _;
use plaudon_ui_testkit::{a11y, Harness};

/// Longest a command may take to answer before the test fails.
const DEADLINE: Duration = Duration::from_secs(60);

/// A window tall enough that no control is scrolled out of reach.
const TALL: egui::Vec2 = egui::vec2(1280.0, 4000.0);

const JOB_ID: &str = "0000002a-0000-0000-0000-000000000000";
const RECORDING_ID: &str = "11111111-1111-1111-1111-111111111111";

/* ---------------------------------------------------------------- harness */

/// One adapter, the engine behind it and the shell calls it made.
struct ScreenApp {
    engine: AppEngine,
    hooks: HostHooks,
    screen: Box<dyn Screen>,
    route: Route,
    bus: ShellBus,
    /// The published name of every command the engine answered, which is what
    /// a frame actually dispatched.
    answered: Vec<&'static str>,
    picked_files: Arc<Mutex<u32>>,
    picked_folders: Arc<Mutex<u32>>,
}

impl std::fmt::Debug for ScreenApp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ScreenApp")
    }
}

impl ScreenApp {
    fn new(data_dir: &std::path::Path, screen: Box<dyn Screen>, route: Route) -> Self {
        let picked_files = Arc::new(Mutex::new(0));
        let picked_folders = Arc::new(Mutex::new(0));
        let files = Arc::clone(&picked_files);
        // A real path under the approved root: the import is rejected because
        // the file does not exist, which is still an honest `import_files`
        // answer and is all these tests assert on.
        let chosen = data_dir.join("chosen.wav");
        let folders = Arc::clone(&picked_folders);
        let folder = data_dir.to_path_buf();
        let hooks = HostHooks {
            pick_audio_files: Arc::new(move || {
                *files.lock().expect("pick counter") += 1;
                vec![chosen.clone()]
            }),
            pick_folder: Arc::new(move || {
                *folders.lock().expect("folder counter") += 1;
                Some(folder.clone())
            }),
            ..HostHooks::none()
        };
        Self {
            engine: engine(data_dir),
            hooks,
            screen,
            route,
            bus: ShellBus::default(),
            answered: Vec::new(),
            picked_files,
            picked_folders,
        }
    }

    /// Run `f` with a context built exactly as the shell builds one.
    fn with_ctx<R>(&mut self, f: impl FnOnce(&mut dyn Screen, &mut ScreenCtx<'_>) -> R) -> R {
        let mut cx = ScreenCtx::new(
            self.engine.dispatcher(),
            &self.hooks,
            Tokens::for_theme(ThemeKind::Light),
            &self.route,
            &mut self.bus,
        );
        f(self.screen.as_mut(), &mut cx)
    }

    /// Hand the adapter one answer, as the shell's fan-out would.
    fn answer(&mut self, response: AppResponse) {
        let command = response.command();
        let result = Ok(response);
        self.with_ctx(|screen, cx| {
            screen.on_response(CommandId::from_raw(0), command, &result, cx);
        });
        self.follow_navigation();
    }

    fn follow_navigation(&mut self) {
        if let Some(route) = self.bus.navigation.take() {
            self.route = route;
        }
    }

    fn view(&mut self, ui: &mut egui::Ui) {
        for event in self.engine.events().drain() {
            match event {
                AppEvent::Response {
                    id,
                    command,
                    result,
                } => {
                    self.answered.push(command);
                    self.with_ctx(|screen, cx| screen.on_response(id, command, &result, cx));
                }
                AppEvent::Ui(event) => {
                    self.with_ctx(|screen, cx| screen.on_ui_event(&event, cx));
                }
                AppEvent::Resync { reason, .. } => {
                    self.with_ctx(|screen, cx| screen.on_resync(reason, cx));
                }
            }
        }
        let mut cx = ScreenCtx::new(
            self.engine.dispatcher(),
            &self.hooks,
            Tokens::for_theme(ThemeKind::Light),
            &self.route,
            &mut self.bus,
        );
        self.screen.ui(ui, &mut cx);
        self.follow_navigation();
    }

    fn answered(&self, command: &str) -> usize {
        self.answered
            .iter()
            .filter(|name| **name == command)
            .count()
    }
}

fn engine(data_dir: &std::path::Path) -> AppEngine {
    AppEngine::start(
        EngineConfig::new(data_dir)
            .with_secrets(Arc::new(InMemorySecretStore::new()))
            .with_notifier(Arc::new(UnsupportedNotifier))
            .with_worker_threads(2)
            .with_approved_roots(vec![data_dir.to_path_buf()]),
    )
    .expect("the engine starts over a fresh library")
}

fn app(screen: Box<dyn Screen>, route: Route) -> (tempfile::TempDir, Harness<'static, ScreenApp>) {
    let dir = tempfile::tempdir().expect("temporary library");
    let state = ScreenApp::new(dir.path(), screen, route);
    let harness = plaudon_ui_testkit::builder()
        .with_size(TALL)
        .build_ui_state(|ui, app: &mut ScreenApp| app.view(ui), state);
    (dir, harness)
}

/// Run frames until every named command has been answered at least `want`
/// times in total.
fn pump(harness: &mut Harness<'static, ScreenApp>, wanted: &[(&str, usize)]) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        harness.run_steps(1);
        let done = wanted
            .iter()
            .all(|(name, count)| harness.state().answered(name) >= *count);
        if done {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "waited for {wanted:?}; the engine answered {:?}",
            harness.state().answered
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    harness.run_steps(2);
}

/// Every word the window announces, as a screen reader would read it.
fn announced(harness: &Harness<'_, ScreenApp>) -> Vec<String> {
    a11y::nodes(harness)
        .into_iter()
        .filter_map(|node| node.announced().map(str::to_owned))
        .collect()
}

fn says(harness: &Harness<'_, ScreenApp>, needle: &str) -> bool {
    announced(harness)
        .iter()
        .any(|label| label.contains(needle))
}

fn click_button(harness: &mut Harness<'static, ScreenApp>, label: &str) {
    harness
        .get_by_role_and_label(egui::accesskit::Role::Button, label)
        .click();
    harness.run_steps(2);
}

/* --------------------------------------------------------------- fixtures */

fn job() -> JobSummary {
    JobSummary {
        job_id: JOB_ID.parse().expect("job id"),
        kind: JobKind::Transcribe,
        state: JobState::RunningLocal,
        stage: Some("transcribe".to_owned()),
        progress: None,
        created_at: "2026-02-01T10:00:00Z".to_owned(),
        updated_at: "2026-02-01T10:00:10Z".to_owned(),
        started_at: Some("2026-02-01T10:00:00Z".to_owned()),
        finished_at: None,
        recording_id: Some(RECORDING_ID.parse().expect("recording id")),
        label: "Board meeting".to_owned(),
        error: None,
        cost_acknowledgement_required: false,
        provider_request_id: None,
        output_link: None,
        cost: CostValue::unknown(),
    }
}

fn activity_page() -> ActivityPage {
    ActivityPage {
        items: vec![ActivityEntry {
            activity_id: "act-1".to_owned(),
            category: NotificationCategory::TranscriptionFinished,
            occurred_at: "2026-02-01T10:00:00Z".to_owned(),
            title: "Board meeting".to_owned(),
            body: "Two speakers, 42 minutes.".to_owned(),
            notification_delivered: true,
            notification_suppressed_reason: None,
            target: Some(ActivityTarget {
                kind: ActivityTargetKind::Job,
                id: JOB_ID.to_owned(),
            }),
            read: false,
        }],
        next_cursor: None,
        unread_count: 1,
    }
}

/* ------------------------------------------------------------------- jobs */

#[test]
fn the_jobs_screen_reads_its_page_and_its_storage_facts_and_refetches_on_a_filter() {
    let (_dir, mut harness) = app(Box::new(JobsShellScreen::new()), Route::Jobs);

    // The first frame reads the page and the storage facts the §J7 capacity
    // comparison needs; nothing else.
    pump(&mut harness, &[("list_jobs", 1), ("get_capabilities", 1)]);

    // The answer is absorbed: the screen stops saying it has not read the list
    // and renders the empty page the fresh library really has.
    assert!(
        !says(&harness, "The job list has not been read yet"),
        "the page answer was not absorbed"
    );
    assert!(says(&harness, "No job matches these filters"));

    // One user action: ticking a kind filter restarts the list from the first
    // page. The label is the application's, never this test's.
    let before = harness.state().answered("list_jobs");
    harness
        .get_by_label(job_kind_label(JobKind::Transcribe))
        .click();
    pump(&mut harness, &[("list_jobs", before + 1)]);
}

#[test]
fn a_jobs_deep_link_is_announced_on_the_row_it_names() {
    let (_dir, mut harness) = app(
        Box::new(JobsShellScreen::new()),
        Route::JobsJob {
            job_id: JOB_ID.to_owned(),
        },
    );
    // Let the real empty page land first, so the seeded one is not overwritten
    // by an answer still in flight.
    pump(&mut harness, &[("list_jobs", 1)]);

    harness.state_mut().answer(AppResponse::ListJobs(JobPage {
        items: vec![job()],
        next_cursor: None,
        state_counts: BTreeMap::new(),
    }));
    harness.run_steps(4);

    assert!(
        says(&harness, "Job: Board meeting — opened from a link"),
        "the route's job id was not handed to the screen; the window announced {:?}",
        announced(&harness)
    );
}

/* --------------------------------------------------------------- activity */

#[test]
fn the_activity_history_reads_its_page_and_its_links_navigate() {
    let (_dir, mut harness) = app(
        Box::new(ActivityShellScreen::new()),
        Route::SettingsActivity,
    );

    pump(&mut harness, &[("list_activity", 1)]);
    assert!(
        !says(&harness, "The activity history has not been read yet"),
        "the page answer was not absorbed"
    );

    harness
        .state_mut()
        .answer(AppResponse::ListActivity(activity_page()));
    harness.run_steps(2);
    assert!(says(&harness, "Board meeting"), "{:?}", announced(&harness));

    click_button(&mut harness, "Open the job for Board meeting");
    assert_eq!(
        harness.state().route,
        Route::JobsJob {
            job_id: JOB_ID.to_owned()
        },
        "an activity deep link must open the job it names"
    );
}

/* ---------------------------------------------------------------- devices */

#[test]
fn the_devices_screen_scans_only_when_the_user_asks_and_renders_the_answer() {
    let (_dir, mut harness) = app(Box::new(DevicesShellScreen::new()), Route::Devices);
    harness.run_steps(3);

    // §15.2: a scan uses the radio and asks for Bluetooth permission, so the
    // first frame dispatches nothing at all.
    assert!(
        harness.state().answered.is_empty(),
        "the devices screen dispatched {:?} without being asked",
        harness.state().answered
    );
    assert!(says(&harness, "No scan yet"));

    click_button(&mut harness, "Start the first scan");
    pump(&mut harness, &[("list_nearby_devices", 1)]);

    // A scan answer is absorbed and the device it found is rendered. The
    // engine's own answer is an empty scan on a machine with no radio, so the
    // row under test is seeded the way the shell would deliver it.
    harness
        .state_mut()
        .answer(AppResponse::ListNearbyDevices(fixtures::scan(vec![
            fixtures::candidate("dev-1", DeviceSection::Nearby),
        ])));
    harness.run_steps(2);
    assert!(says(&harness, "PLAUD NotePin"));
}

/* ---------------------------------------------------------------- sources */

#[test]
fn the_sources_screen_probes_the_connector_and_imports_what_the_user_chooses() {
    let (_dir, mut harness) = app(Box::new(SourcesShellScreen::new()), Route::SettingsSources);

    pump(&mut harness, &[("plaud_probe", 1)]);

    harness
        .state_mut()
        .answer(AppResponse::PlaudProbe(fixtures::ready_connector()));
    harness.run_steps(2);
    assert!(
        says(&harness, "The PLAUD tool is signed in."),
        "the connector snapshot was not absorbed"
    );

    click_button(&mut harness, "Choose audio files");
    pump(&mut harness, &[("import_files", 1)]);
    assert_eq!(
        *harness.state().picked_files.lock().expect("pick counter"),
        1,
        "the screen must ask the host for the files, never rfd"
    );
    assert_eq!(
        *harness
            .state()
            .picked_folders
            .lock()
            .expect("folder counter"),
        0,
        "choosing files must not open a directory dialog"
    );
}

/* --------------------------------------------------------------- settings */

#[test]
fn the_settings_destination_renders_a_child_for_each_of_its_three_routes() {
    let (_dir, mut harness) = app(Box::new(SettingsDestinationScreen::new()), Route::Settings);
    harness.run_steps(3);

    // The settings sections themselves, plus the two links the navigation rail
    // cannot reach on its own.
    assert!(says(&harness, "Appearance"));

    click_button(&mut harness, Route::SettingsActivity.title());
    assert_eq!(harness.state().route, Route::SettingsActivity);
    pump(&mut harness, &[("list_activity", 1)]);
    assert!(
        says(
            &harness,
            "Every completion and attention event is recorded here"
        ),
        "the activity child must render at its own route"
    );

    click_button(&mut harness, Route::Settings.title());
    assert_eq!(harness.state().route, Route::Settings);
    assert!(says(&harness, "Appearance"));

    click_button(&mut harness, Route::SettingsSources.title());
    assert_eq!(harness.state().route, Route::SettingsSources);
    pump(&mut harness, &[("plaud_probe", 1)]);
    assert!(
        says(&harness, "PLAUD Cloud and sources"),
        "the sources child must render at its own route"
    );
}
