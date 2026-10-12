//! The O1.3 shell interaction suite.
//!
//! Every assertion here runs against the real [`plaudon_app`] engine on a
//! temporary library — no mock backend, no network, no window — and through
//! the same [`Shell`] the binary runs. Parity rows S1–S14 and S17 of
//! `docs/EGUI_MIGRATION_PLAN.md` §4.1.
//!
//! `scripts/e2e-remote.sh cargo test -p plaudon-ui --test shell_kittest` runs
//! the same file on MARS, where `PLAUDON_VISUAL=1` additionally compares the
//! committed baseline under `tests/visual/`.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use plaudon_app::command::{AppCommand, AppResponse, CommandId};
use plaudon_app::dto::{
    ActivityEntry, ActivityPage, Appearance, CompleteOnboardingRequest, Contrast,
    NotificationCategory, NotificationSettings,
};
use plaudon_app::event::AppEvent;
use plaudon_app::notify::UnsupportedNotifier;
use plaudon_app::runtime::{AppEngine, EngineConfig};
use plaudon_app::secrets::InMemorySecretStore;
use plaudon_app::shell_state::{ShellState, SIDEBAR_WIDTH_RANGE};
use plaudon_domain::error::ErrorEnvelope;
use plaudon_ui::shell::{
    Destination, HostHooks, Route, Screen, ScreenCtx, Shell, ShellHost, Startup,
};
use plaudon_ui::theme::{ThemeKind, Tokens};
use plaudon_ui_testkit::kittest::Queryable as _;
use plaudon_ui_testkit::{a11y, visual, Harness};

/// Longest the shell may take to become usable before the test fails.
const DEADLINE: Duration = Duration::from_secs(60);

/// A shell over a real engine, driven by the harness.
struct ShellApp {
    engine: AppEngine,
    hooks: HostHooks,
    shell: Shell,
    picks: Arc<Mutex<u32>>,
}

impl ShellApp {
    fn new(data_dir: &std::path::Path, state: ShellState) -> Self {
        let picks = Arc::new(Mutex::new(0));
        let counter = Arc::clone(&picks);
        let hooks = HostHooks {
            pick_audio_files: Arc::new(move || {
                *counter.lock().expect("pick counter") += 1;
                Vec::<PathBuf>::new()
            }),
            ..HostHooks::none()
        };
        Self {
            engine: engine(data_dir),
            hooks,
            shell: Shell::new(state),
            picks,
        }
    }

    fn view(&mut self, ui: &mut egui::Ui) {
        let host = ShellHost {
            dispatcher: self.engine.dispatcher(),
            hooks: &self.hooks,
        };
        for event in self.engine.events().drain() {
            self.shell.handle_event(event, &host);
        }
        self.shell.ui(ui, &host);
    }

    /// Complete first-run setup the way a user would: through the published
    /// command, not by reaching into state.
    fn complete_onboarding(&mut self) {
        self.engine
            .dispatcher()
            .send(AppCommand::CompleteOnboarding(CompleteOnboardingRequest {
                default_stt_engine_id: None,
                default_text_engine_id: None,
                local_only_mode: true,
                notifications: NotificationSettings {
                    enabled: false,
                    categories: NotificationCategory::ALL
                        .iter()
                        .copied()
                        .map(|category| (category, false))
                        .collect(),
                },
            }));
    }

    fn run_command(&mut self, id: &str) {
        let host = ShellHost {
            dispatcher: self.engine.dispatcher(),
            hooks: &self.hooks,
        };
        self.shell.run_command(id, &host);
    }

    /// Hand the shell an answer exactly as the engine delivers one.
    ///
    /// The shell cannot tell this apart from a real response — it is the
    /// same `AppEvent` on the same handler — which is what makes it a fair
    /// way to put a known activity history in front of the header bell
    /// without inventing library state the backend would have to produce.
    fn deliver(&mut self, command: &'static str, response: AppResponse) {
        let host = ShellHost {
            dispatcher: self.engine.dispatcher(),
            hooks: &self.hooks,
        };
        self.shell.handle_event(
            AppEvent::Response {
                id: CommandId::from_raw(u64::MAX),
                command,
                result: Box::new(Ok(response)),
            },
            &host,
        );
    }
}

impl std::fmt::Debug for ShellApp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ShellApp")
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

/// A harness over a shell with `state` restored, run until the capabilities
/// have arrived. Onboarding still gates the shell at this point.
fn booted(state: ShellState) -> (tempfile::TempDir, Harness<'static, ShellApp>) {
    let dir = tempfile::tempdir().expect("temporary library");
    let app = ShellApp::new(dir.path(), state);
    let mut harness =
        plaudon_ui_testkit::builder().build_ui_state(|ui, app: &mut ShellApp| app.view(ui), app);
    let deadline = Instant::now() + DEADLINE;
    loop {
        // One step at a time: the loading spinner animates, so `run` — which
        // runs until the frame settles — would never return while the
        // capabilities command is still in flight.
        harness.run_steps(1);
        if *harness.state().shell.startup() == Startup::Ready {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the shell never became ready: {:?}",
            harness.state().shell.startup()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    harness.run();
    (dir, harness)
}

/// A harness past the onboarding gate, which is where every shell assertion
/// below starts.
fn shell_harness(state: ShellState) -> (tempfile::TempDir, Harness<'static, ShellApp>) {
    let (dir, mut harness) = booted(state);
    harness.state_mut().complete_onboarding();
    let deadline = Instant::now() + DEADLINE;
    while harness.state().shell.onboarding_required() {
        harness.run_steps(1);
        assert!(Instant::now() < deadline, "onboarding never completed");
        std::thread::sleep(Duration::from_millis(5));
    }
    harness.run();
    (dir, harness)
}

fn ready() -> (tempfile::TempDir, Harness<'static, ShellApp>) {
    shell_harness(ShellState::default())
}

/// What a registered screen saw, readable from the test while the shell owns
/// the screen itself.
#[derive(Debug, Default)]
struct ScreenLog {
    painted: u32,
    routes: Vec<String>,
    responses: Vec<&'static str>,
}

/// A screen that records what the shell handed it.
#[derive(Debug)]
struct RecordingScreen {
    log: Arc<Mutex<ScreenLog>>,
}

impl Screen for RecordingScreen {
    fn destination(&self) -> Destination {
        Destination::Jobs
    }

    fn ui(&mut self, ui: &mut egui::Ui, cx: &mut ScreenCtx<'_>) {
        {
            let mut log = self.log.lock().expect("screen log");
            log.painted += 1;
            let path = cx.route().path();
            if log.routes.last() != Some(&path) {
                log.routes.push(path);
            }
        }
        ui.label("jobs screen");
        if ui.button("Ask the application").clicked() {
            cx.send(AppCommand::GetCapabilities);
        }
    }

    fn on_response(
        &mut self,
        _id: CommandId,
        command: &'static str,
        _result: &Result<AppResponse, ErrorEnvelope>,
        _cx: &mut ScreenCtx<'_>,
    ) {
        self.log.lock().expect("screen log").responses.push(command);
    }
}

#[test]
fn onboarding_gates_the_shell_until_setup_is_complete() {
    let (_dir, mut harness) = booted(ShellState::default());
    assert!(harness.state().shell.onboarding_required());
    let announced: Vec<String> = a11y::nodes(&harness)
        .into_iter()
        .filter_map(|node| node.announced().map(str::to_owned))
        .collect();
    assert!(
        announced
            .iter()
            .any(|text| text.contains("Welcome to Plaudon")),
        "the gate does not announce itself: {announced:?}"
    );
    // The navigation rail is not reachable while the gate is up.
    assert!(!a11y::has_label(&harness, "Library"));

    harness.state_mut().complete_onboarding();
    let deadline = Instant::now() + DEADLINE;
    while harness.state().shell.onboarding_required() && Instant::now() < deadline {
        harness.run_steps(1);
        std::thread::sleep(Duration::from_millis(5));
    }
    harness.run();
    assert!(!harness.state().shell.onboarding_required());
    assert!(a11y::has_label(&harness, "Library"));
}

#[test]
fn the_six_destinations_are_tabs_and_clicking_one_navigates() {
    let (_dir, mut harness) = ready();

    let tabs: Vec<String> = a11y::nodes(&harness)
        .into_iter()
        .filter(|node| node.role == "Tab")
        .filter_map(|node| node.label)
        .collect();
    assert_eq!(tabs.len(), 6, "six destinations, got {tabs:?}");
    for destination in Destination::ALL {
        assert!(
            tabs.iter()
                .any(|label| label.starts_with(destination.label())),
            "{} is missing from the rail: {tabs:?}",
            destination.label()
        );
    }

    harness.get_by_label_contains("Devices").click();
    harness.run();
    assert_eq!(harness.state().shell.route(), &Route::Devices);
}

#[test]
fn a_destination_without_a_screen_says_so_honestly() {
    let (_dir, mut harness) = ready();
    harness.get_by_label_contains("Templates").click();
    harness.run();

    let announced: Vec<String> = a11y::nodes(&harness)
        .into_iter()
        .filter_map(|node| node.announced().map(str::to_owned))
        .collect();
    assert!(
        announced
            .iter()
            .any(|text| text.contains("not available in this build yet")),
        "the empty state does not say the screen is missing: {announced:?}"
    );
    assert!(
        announced.iter().any(|text| text.contains("O2.2 (#206)")),
        "the empty state does not name the task that owns the screen"
    );
}

#[test]
fn a_registered_screen_paints_its_route_and_receives_its_answers() {
    let (_dir, mut harness) = ready();
    let log = Arc::new(Mutex::new(ScreenLog::default()));
    harness
        .state_mut()
        .shell
        .register(Box::new(RecordingScreen {
            log: Arc::clone(&log),
        }));
    harness.state_mut().shell.navigate(Route::JobsJob {
        job_id: "job-7".to_owned(),
    });
    harness.run();

    // The screen is painted with its own sub-route, not just the
    // destination, so a deep link survives the hand-off.
    assert_eq!(
        log.lock()
            .expect("screen log")
            .routes
            .last()
            .map(String::as_str),
        Some("/jobs/job-7")
    );

    harness.get_by_label("Ask the application").click();
    let deadline = Instant::now() + DEADLINE;
    while log.lock().expect("screen log").responses.is_empty() {
        harness.run_steps(1);
        assert!(Instant::now() < deadline, "the screen was never answered");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        log.lock().expect("screen log").responses.first().copied(),
        Some("get_capabilities")
    );
}

#[test]
fn mod_three_goes_to_jobs_and_mod_k_opens_the_palette() {
    let (_dir, mut harness) = ready();
    harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Num3);
    harness.run();
    assert_eq!(harness.state().shell.route(), &Route::Jobs);

    harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::K);
    harness.run();
    assert!(harness.state().shell.palette().is_open());

    harness.key_press(egui::Key::Escape);
    harness.run();
    assert!(!harness.state().shell.palette().is_open());
}

#[test]
fn the_palette_ranks_and_runs_a_command_from_the_keyboard() {
    let (_dir, mut harness) = ready();
    harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::K);
    harness.run();

    for character in "devices".chars() {
        harness.event(egui::Event::Text(character.to_string()));
    }
    harness.run();
    harness.run();
    assert_eq!(harness.state().shell.palette().query(), "devices");

    harness.key_press(egui::Key::Enter);
    harness.run();
    assert_eq!(harness.state().shell.route(), &Route::Devices);
    assert!(!harness.state().shell.palette().is_open());
}

#[test]
fn the_keyboard_reference_is_generated_from_the_live_command_map() {
    let (_dir, mut harness) = ready();
    harness.key_press(egui::Key::Questionmark);
    harness.run();
    assert!(harness.state().shell.shortcuts_open());

    let announced: Vec<String> = a11y::nodes(&harness)
        .into_iter()
        .filter_map(|node| node.announced().map(str::to_owned))
        .collect();
    // The reported operating system is macOS, so `Mod` renders as `Cmd`.
    assert!(
        announced.iter().any(|text| text.contains("Cmd+1")),
        "the reference does not show the destination shortcuts: {announced:?}"
    );
    assert!(announced.iter().any(|text| text == "Keyboard shortcuts"));

    harness.key_press(egui::Key::Escape);
    harness.run();
    assert!(!harness.state().shell.shortcuts_open());
}

#[test]
fn a_shortcut_does_not_fire_while_the_user_is_typing() {
    let (_dir, mut harness) = ready();
    harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::K);
    harness.run();
    assert!(harness.state().shell.palette().is_open());

    // The palette's field has focus: `?` is a character, not the help
    // shortcut.
    harness.event(egui::Event::Text("?".to_owned()));
    harness.run();
    assert!(!harness.state().shell.shortcuts_open());
    assert_eq!(harness.state().shell.palette().query(), "?");
}

#[test]
fn switching_appearance_repaints_in_the_other_theme() {
    let (_dir, mut harness) = ready();
    assert_eq!(harness.state().shell.theme().appearance, Appearance::System);

    harness.state_mut().run_command("appearance_dark");
    harness.run();
    let dark = Tokens::get(&harness.ctx);
    assert_eq!(dark.theme, ThemeKind::Dark);

    harness.state_mut().run_command("contrast_high");
    harness.run();
    assert_eq!(Tokens::get(&harness.ctx).theme, ThemeKind::HighContrastDark);

    harness.state_mut().run_command("appearance_light");
    harness.run();
    assert_eq!(
        Tokens::get(&harness.ctx).theme,
        ThemeKind::HighContrastLight
    );
    assert_eq!(harness.state().shell.theme().contrast, Contrast::High);
}

#[test]
fn the_sidebar_divider_is_a_splitter_and_the_keyboard_resizes_it() {
    let (_dir, mut harness) = ready();
    let splitters: Vec<_> = a11y::nodes(&harness)
        .into_iter()
        .filter(|node| node.role == "Splitter")
        .collect();
    assert_eq!(
        splitters.len(),
        1,
        "the sidebar divider is the only splitter in the default layout"
    );
    assert_eq!(
        splitters[0].label.as_deref(),
        Some("Resize the sidebar"),
        "the splitter is unlabelled"
    );

    let before = harness.state().shell.panels().sidebar_width;
    harness.get_by_label("Resize the sidebar").focus();
    harness.run();
    for _ in 0..3 {
        harness.key_press(egui::Key::ArrowRight);
        harness.run();
    }
    let after = harness.state().shell.panels().sidebar_width;
    assert!(
        after > before,
        "the arrow keys did not widen the sidebar ({before} -> {after})"
    );
    assert!(after <= SIDEBAR_WIDTH_RANGE.1);
}

/// A screen with a side pane, for the two-divider layout.
#[derive(Debug)]
struct AsideScreen;

impl Screen for AsideScreen {
    fn destination(&self) -> Destination {
        Destination::Library
    }

    fn ui(&mut self, ui: &mut egui::Ui, _cx: &mut ScreenCtx<'_>) {
        ui.label("library screen");
    }

    fn has_aside(&self) -> bool {
        true
    }

    fn aside(&mut self, ui: &mut egui::Ui, _cx: &mut ScreenCtx<'_>) {
        ui.label("recording detail");
    }
}

#[test]
fn a_screen_with_a_side_pane_gets_a_second_resizable_divider() {
    let (_dir, mut harness) = ready();
    harness.state_mut().shell.register(Box::new(AsideScreen));
    harness.run();

    let splitters: Vec<String> = a11y::nodes(&harness)
        .into_iter()
        .filter(|node| node.role == "Splitter")
        .filter_map(|node| node.label)
        .collect();
    assert_eq!(
        splitters,
        vec![
            "Resize the sidebar".to_owned(),
            "Resize the side pane".to_owned()
        ]
    );
    assert!(a11y::has_label(&harness, "recording detail"));

    let before = harness.state().shell.panels().aside_width;
    harness.get_by_label("Resize the side pane").focus();
    harness.run();
    harness.key_press(egui::Key::ArrowLeft);
    harness.run();
    let after = harness.state().shell.panels().aside_width;
    assert!(
        after > before,
        "dragging the divider left did not widen the side pane ({before} -> {after})"
    );

    // Hiding the pane is a command, and the width it had is kept for when it
    // comes back.
    harness.state_mut().run_command("toggle_aside");
    harness.run();
    assert!(!a11y::has_label(&harness, "recording detail"));
    assert!((harness.state().shell.panels().aside_width - after).abs() < f32::EPSILON);
}

#[test]
fn a_narrow_window_collapses_the_rail_and_explains_itself() {
    let dir = tempfile::tempdir().expect("temporary library");
    let app = ShellApp::new(dir.path(), ShellState::default());
    let mut harness = plaudon_ui_testkit::builder()
        .with_size(egui::vec2(840.0, 700.0))
        .build_ui_state(|ui, app: &mut ShellApp| app.view(ui), app);
    let deadline = Instant::now() + DEADLINE;
    loop {
        harness.run_steps(1);
        if *harness.state().shell.startup() == Startup::Ready {
            break;
        }
        assert!(Instant::now() < deadline, "the shell never became ready");
        std::thread::sleep(Duration::from_millis(10));
    }
    harness.state_mut().complete_onboarding();
    while harness.state().shell.onboarding_required() && Instant::now() < deadline {
        harness.run_steps(1);
        std::thread::sleep(Duration::from_millis(5));
    }
    harness.run();

    let announced: Vec<String> = a11y::nodes(&harness)
        .into_iter()
        .filter_map(|node| node.announced().map(str::to_owned))
        .collect();
    assert!(
        announced
            .iter()
            .any(|text| text.contains("narrower than the 880pt minimum")),
        "the narrow-window notice is missing: {announced:?}"
    );
    // The rail collapses to glyphs, but every destination keeps its name in
    // the accessibility tree.
    let tabs: Vec<String> = a11y::nodes(&harness)
        .into_iter()
        .filter(|node| node.role == "Tab")
        .filter_map(|node| node.label)
        .collect();
    assert_eq!(tabs.len(), 6, "{tabs:?}");
    assert!(tabs.iter().any(|label| label.starts_with("Library")));
}

#[test]
fn the_layout_the_user_chose_survives_a_restart() {
    let (_dir, mut harness) = ready();
    harness.state_mut().run_command("appearance_dark");
    harness.state_mut().run_command("toggle_sidebar");
    harness.run();

    let state = harness.state().shell.shell_state();
    assert_eq!(state.appearance, Appearance::Dark);
    assert!(state.panels.sidebar_collapsed);

    // Restart with exactly what would have been written to disk.
    let json = state.to_json().expect("serialises");
    let restored = ShellState::from_json(&json);
    let (_dir2, harness2) = shell_harness(restored);
    assert_eq!(harness2.state().shell.theme().appearance, Appearance::Dark);
    assert!(harness2.state().shell.panels().sidebar_collapsed);
    assert_eq!(Tokens::get(&harness2.ctx).theme, ThemeKind::Dark);
}

#[test]
fn a_changed_binding_replaces_the_default_and_is_persisted() {
    let (_dir, mut harness) = ready();
    let mut overrides = std::collections::BTreeMap::new();
    overrides.insert("command_palette".to_owned(), vec!["Mod+Shift+P".to_owned()]);
    let refused = harness.state_mut().shell.apply_bindings(&overrides);
    assert!(refused.is_empty(), "{refused:?}");
    harness.run();

    harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::K);
    harness.run();
    assert!(
        !harness.state().shell.palette().is_open(),
        "the default chord still fires after it was rebound"
    );

    harness.key_press_modifiers(
        egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
        egui::Key::P,
    );
    harness.run();
    assert!(harness.state().shell.palette().is_open());
    assert_eq!(
        harness
            .state()
            .shell
            .shell_state()
            .bindings
            .get("command_palette"),
        Some(&vec!["Mod+Shift+P".to_owned()])
    );
}

#[test]
fn the_toolbar_import_action_asks_the_host_and_never_touches_rfd() {
    let (_dir, mut harness) = ready();
    harness.get_by_label("Import audio…").click();
    harness.run();
    assert_eq!(*harness.state().picks.lock().expect("picks"), 1);
    // The hook answered with no files, so the shell says so instead of
    // pretending an import started.
    assert_eq!(harness.state().shell.status(), "Import cancelled");
}

#[test]
fn the_window_has_the_landmarks_and_the_status_line_a_screen_reader_needs() {
    let (_dir, harness) = ready();
    let roles: Vec<String> = a11y::nodes(&harness)
        .into_iter()
        .map(|node| node.role)
        .collect();
    for required in ["Navigation", "Main", "Toolbar", "Status"] {
        assert!(
            roles.iter().any(|role| role == required),
            "no {required} landmark in the tree: {roles:?}"
        );
    }
    assert!(a11y::has_label(&harness, "Skip to main content"));
}

#[test]
fn every_interactive_element_is_reachable_by_keyboard() {
    let (_dir, mut harness) = ready();
    let order = a11y::focus_order(&mut harness, 40);
    assert!(
        order.len() >= 8,
        "the Tab order is suspiciously short: {order:?}"
    );
    assert!(
        order.iter().any(|label| label == "Skip to main content"),
        "the skip link is not in the Tab order: {order:?}"
    );
    for destination in Destination::ALL {
        assert!(
            order
                .iter()
                .any(|label| label.starts_with(destination.label())),
            "{} cannot be reached by Tab: {order:?}",
            destination.label()
        );
    }
    assert!(
        order.iter().any(|label| label == "Resize the sidebar"),
        "the divider is not keyboard reachable: {order:?}"
    );
}

#[test]
fn the_state_is_handed_to_the_host_once_and_only_when_it_changed() {
    let (_dir, mut harness) = ready();
    // The first frames settle the window geometry, so drain whatever that
    // produced before asserting on a deliberate change.
    let _ = harness.state_mut().shell.take_dirty_state();
    assert!(harness.state_mut().shell.take_dirty_state().is_none());

    harness.state_mut().run_command("toggle_sidebar");
    let saved = harness
        .state_mut()
        .shell
        .take_dirty_state()
        .expect("a layout change is worth persisting");
    assert!(saved.panels.sidebar_collapsed);
    assert!(harness.state_mut().shell.take_dirty_state().is_none());
}

#[test]
fn the_ready_shell_matches_its_baseline() {
    let (_dir, mut harness) = ready();
    // Deterministic frame: nothing in it is minted during the test — no
    // identifiers, no timestamps, no durations (docs/EGUI_TESTING.md §4.5).
    assert_eq!(harness.state().shell.status(), "Ready");
    // "Follow the system" on a host that reports no appearance is light, as
    // it was in the shell being replaced (`systemDark: false`).
    assert_eq!(Tokens::get(&harness.ctx).theme, ThemeKind::Light);
    visual::assert_snapshot(&mut harness, "shell-ready");
}

/* ------------------------------------------------------------------ J12 */

/// One activity entry, read or unread.
fn entry(id: &str, title: &str, read: bool) -> ActivityEntry {
    ActivityEntry {
        activity_id: id.to_owned(),
        category: NotificationCategory::ImportFinished,
        occurred_at: "2026-02-01T10:00:00Z".to_owned(),
        title: title.to_owned(),
        body: "Two speakers, 42 minutes.".to_owned(),
        notification_delivered: true,
        notification_suppressed_reason: None,
        target: None,
        read,
    }
}

/// A history page, counted the way the history itself counts it.
fn activity_page(items: Vec<ActivityEntry>) -> ActivityPage {
    let unread_count = items.iter().filter(|entry| !entry.read).count() as u64;
    ActivityPage {
        items,
        next_cursor: None,
        unread_count,
    }
}

/// A ready shell with nothing left in flight.
///
/// The shell asks for the activity history itself as soon as the
/// capabilities arrive, and a test that handed it a page while that answer
/// was still travelling would be overwritten by it. The status bar's
/// activity indicator is the observable "something is in flight", so this
/// waits for it to go quiet rather than for a guessed number of frames.
fn ready_with_history() -> (tempfile::TempDir, Harness<'static, ShellApp>) {
    let (dir, mut harness) = ready();
    let deadline = Instant::now() + DEADLINE;
    loop {
        harness.run_steps(1);
        let busy = a11y::nodes(&harness).into_iter().any(|node| {
            node.announced()
                .is_some_and(|text| text.contains("task running") || text.contains("tasks running"))
        });
        if !busy {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the shell never finished its start-up commands"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    (dir, harness)
}

#[test]
fn the_header_bell_states_the_unread_count_in_words() {
    let (_dir, mut harness) = ready_with_history();
    harness.state_mut().deliver(
        "list_activity",
        AppResponse::ListActivity(activity_page(vec![
            entry("a1", "Board meeting", true),
            entry("a2", "Standup", false),
            entry("a3", "Retro", false),
        ])),
    );
    harness.run();

    assert_eq!(harness.state().shell.activity_unread(), 2);
    // Not a dot, not a colour: the control announces the number.
    assert!(
        a11y::has_label(&harness, "Activity: 2 unread notifications"),
        "the bell does not announce its count: {:?}",
        a11y::nodes(&harness)
            .into_iter()
            .filter_map(|node| node.announced().map(str::to_owned))
            .collect::<Vec<_>>()
    );
}

#[test]
fn activating_the_header_bell_opens_the_activity_history() {
    let (_dir, mut harness) = ready_with_history();
    harness.state_mut().deliver(
        "list_activity",
        AppResponse::ListActivity(activity_page(vec![entry("a1", "Standup", false)])),
    );
    harness.run();

    assert_ne!(harness.state().shell.route(), &Route::SettingsActivity);
    harness
        .get_by_label("Activity: 1 unread notification")
        .click();
    harness.run();
    assert_eq!(harness.state().shell.route(), &Route::SettingsActivity);
}

#[test]
fn an_empty_history_is_announced_as_nothing_unread_rather_than_decorated() {
    let (_dir, harness) = ready_with_history();
    assert_eq!(harness.state().shell.activity_unread(), 0);
    assert!(
        a11y::has_label(&harness, "Activity: no unread notifications"),
        "the bell must still say what it is when there is nothing to report"
    );
    let announced: Vec<String> = a11y::nodes(&harness)
        .into_iter()
        .filter_map(|node| node.announced().map(str::to_owned))
        .collect();
    assert!(
        !announced.iter().any(|text| text.contains("Activity 0")),
        "a zero count must not be drawn as a badge: {announced:?}"
    );
}

#[test]
fn the_header_bell_is_reachable_by_keyboard() {
    let (_dir, mut harness) = ready_with_history();
    harness.state_mut().deliver(
        "list_activity",
        AppResponse::ListActivity(activity_page(vec![entry("a1", "Standup", false)])),
    );
    harness.run();

    let order = a11y::focus_order(&mut harness, 40);
    assert!(
        order
            .iter()
            .any(|label| label == "Activity: 1 unread notification"),
        "the bell cannot be reached by Tab: {order:?}"
    );
}

#[test]
fn the_header_bell_is_a_named_control_like_every_other() {
    let (_dir, mut harness) = ready_with_history();
    harness.state_mut().deliver(
        "list_activity",
        AppResponse::ListActivity(activity_page(vec![entry("a1", "Standup", false)])),
    );
    harness.run();
    a11y::assert_every_control_is_named(&harness, "shell header with the activity bell", &[]);
}
