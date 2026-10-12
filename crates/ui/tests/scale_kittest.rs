//! The high-DPI suite (parity row S16 of `docs/EGUI_MIGRATION_PLAN.md` §4.1).
//!
//! The React shell was tested at one device-pixel ratio and inherited the
//! browser's scaling; a native window does not. egui lays everything out in
//! *logical points* and rasterises at `pixels_per_point`, so the two things
//! that can break at 1.5× or 2× are different from each other:
//!
//! 1. **The layout.** It must not move. The window is the same 1280×820
//!    points at every scale, so a control that sits somewhere at 1.0 must sit
//!    in the same place at 2.0 — [`POSITION_TOLERANCE`] and
//!    [`SIZE_TOLERANCE`] are the whole budget, and they exist only because a
//!    rect is rounded to whole *pixels* before it is reported, which at 1.5×
//!    is two thirds of a point per rounding.
//! 2. **The pixels.** They must be *rendered*, not stretched: a snapshot per
//!    scale, under a name that carries the scale
//!    ([`plaudon_ui_testkit::visual::scaled_name`]), so a 2× baseline can
//!    never be compared against a 1× frame.
//!
//! Three screens, because they cover the three layout mechanisms the shell
//! has: the shell at rest (panels and a resizable divider), the library list
//! (a virtualized grid with its own row metrics) and the recording detail (a
//! two-column split that folds below a width threshold).
//!
//! Pixel comparison is opt-in (`PLAUDON_VISUAL=1`, MARS only) per
//! `docs/EGUI_TESTING.md` §4; every assertion below runs everywhere.

use std::sync::Arc;
use std::time::{Duration, Instant};

use egui_kittest::kittest::NodeT as _;
use egui_kittest::Harness;
use plaudon_app::command::{AppCommand, AppResponse, CommandId};
use plaudon_app::dto::{
    CompleteOnboardingRequest, MediaLocation, NotificationCategory, NotificationSettings,
    RecordingDetail, TranscriptPage, TranscriptSegmentDto,
};
use plaudon_app::notify::UnsupportedNotifier;
use plaudon_app::runtime::{AppEngine, EngineConfig};
use plaudon_app::secrets::InMemorySecretStore;
use plaudon_app::shell_state::ShellState;
use plaudon_domain::error::ErrorEnvelope;
use plaudon_domain::transcript::AlignmentOrigin;
use plaudon_ui::ai::AiOutput;
use plaudon_ui::screens::library::testing::{detail, page, row_at, rows};
use plaudon_ui::screens::library::LibraryScreen;
use plaudon_ui::screens::recording::{
    no_audio_output, DetailContext, DetailPanels as _, LivePanels, RecordingDetailScreen,
};
use plaudon_ui::shell::{HostHooks, Shell, ShellHost, Startup};
use plaudon_ui_testkit::{a11y, visual, SCALE_AXIS};

/// Longest the shell may take to become usable before the test fails.
const DEADLINE: Duration = Duration::from_secs(60);

/// How far a control's left edge may move between two scales, in logical
/// points.
///
/// Zero is the ideal and not achievable: egui rounds a widget's rectangle to
/// whole physical pixels, which is 1 point at 1.0×, ⅔ of a point at 1.5× and
/// ½ at 2.0×, and the roundings of the containers to the left of a control
/// add up. The horizontal axis does not *accumulate* the way the vertical
/// one does, because a column's width is bounded by its container rather
/// than summed from its contents, so this budget stays tight.
const HORIZONTAL_TOLERANCE: f32 = 2.0;

/// How far a control's top edge may move between two scales, in logical
/// points.
///
/// Looser than [`HORIZONTAL_TOLERANCE`] for one measured reason, and this is
/// the honest statement of what egui guarantees: a galley is laid out line by
/// line and **each line's height is rounded to whole pixels**, which is a
/// different quantity at 1.0× (1 point) and at 1.5× (⅔ of a point). A column
/// of text controls therefore drifts, each one carrying the rounding of
/// everything stacked above it.
///
/// Measured in this suite: the recording detail's transcript column, two
/// segment editors of roughly 130 points each, drifts **3.0 points** at the
/// second segment at 1.5× and nothing at 2.0×. Eight points is that with
/// headroom and still well below a *reflow*, which moves a control by at
/// least one line height (≈ 18 points at the default font) and usually by a
/// whole row or column.
const VERTICAL_TOLERANCE: f32 = 8.0;

/// How much a control may change size between two scales, in logical points.
///
/// Same cause as [`VERTICAL_TOLERANCE`], without the accumulation: one
/// control's own rounding, measured at 3 points on a 130-point-tall segment
/// editor at 1.5×.
const SIZE_TOLERANCE: f32 = 6.0;

/// Bound outside which a coordinate is not a layout but a defect.
///
/// A control legitimately sits outside the viewport when its container
/// scrolls — the library's full column set is wider than 1280 points — so
/// the viewport itself is not the limit. Three windows is: nothing in a
/// 1280×820 shell has any business being painted there.
fn plausible_canvas() -> egui::Rect {
    let size = plaudon_ui_testkit::DEFAULT_SIZE;
    egui::Rect::from_min_max(
        egui::Pos2::new(-size.x, -size.y),
        egui::Pos2::new(size.x * 2.0, size.y * 2.0),
    )
}

/* ------------------------------------------------------- the invariants */

/// One control as the accessibility tree reports it, with where it is.
#[derive(Debug, Clone, PartialEq)]
struct Control {
    role: String,
    label: String,
    rect: egui::Rect,
}

fn is_interactive(role: &str) -> bool {
    a11y::INTERACTIVE_ROLES
        .iter()
        .any(|known| format!("{known:?}") == role)
}

/// Every named control of the current frame, in tree order.
fn controls<State>(harness: &Harness<'_, State>) -> Vec<Control> {
    harness
        .root()
        .children_recursive()
        .filter_map(|node| {
            let accessible = node.accesskit_node();
            if accessible.is_hidden() {
                return None;
            }
            let role = format!("{:?}", accessible.role());
            if !is_interactive(&role) {
                return None;
            }
            let label = accessible.label()?;
            // `rect()` panics on a node with no bounds; a control that has
            // none is a different bug, caught by the geometry assertion
            // below through its absence from both scales' lists.
            accessible.bounding_box()?;
            Some(Control {
                role,
                label,
                rect: node.rect(),
            })
        })
        .collect()
}

/// Assert the things that must hold whatever the pixels are.
///
/// `screen` names what is on screen so a failure says which one.
fn assert_sane_at_any_scale<State>(harness: &Harness<'_, State>, screen: &str, scale: f32) {
    let controls = controls(harness);
    assert!(
        !controls.is_empty(),
        "{screen} at {scale}×: the accessibility tree has no controls at all"
    );

    // Nothing collapsed to nothing, nothing painted off the canvas: a
    // zero-sized control cannot be hit however good the picture looks, and a
    // coordinate a thousand points outside the shell is a layout that ran
    // away rather than one that scrolls.
    let canvas = plausible_canvas();
    for control in &controls {
        assert!(
            control.rect.is_finite(),
            "{screen} at {scale}×: {} {:?} has a non-finite rect {:?}",
            control.role,
            control.label,
            control.rect
        );
        assert!(
            control.rect.width() > 0.0 && control.rect.height() > 0.0,
            "{screen} at {scale}×: {} {:?} is {:?}, which cannot be clicked",
            control.role,
            control.label,
            control.rect.size()
        );
        assert!(
            canvas.contains_rect(control.rect),
            "{screen} at {scale}×: {} {:?} at {:?} is nowhere near the {:?}-point window",
            control.role,
            control.label,
            control.rect,
            plaudon_ui_testkit::DEFAULT_SIZE
        );
    }

    // Every control has a name at every scale: S17's invariant, asserted
    // here too because a scale change is exactly the kind of edit that makes
    // a label disappear into an icon.
    a11y::assert_every_control_is_named(harness, &format!("{screen} at {scale}×"), &[]);
}

/// Assert that a scaled frame offers the same controls, in the same places,
/// as the 1.0× frame of the same screen.
fn assert_matches_the_baseline_scale(
    baseline: &[Control],
    scaled: &[Control],
    screen: &str,
    scale: f32,
) {
    let names = |controls: &[Control]| -> Vec<String> {
        let mut names: Vec<String> = controls
            .iter()
            .map(|control| format!("{} {:?}", control.role, control.label))
            .collect();
        names.sort();
        names
    };
    assert_eq!(
        names(scaled),
        names(baseline),
        "{screen} at {scale}×: the control set differs from 1.0×, so something was \
         clipped out of the accessibility tree or added to it"
    );

    for (expected, actual) in baseline.iter().zip(scaled.iter()) {
        assert_eq!(
            (&actual.role, &actual.label),
            (&expected.role, &expected.label),
            "{screen} at {scale}×: the tree order differs from 1.0×"
        );
        let moved = (actual.rect.min - expected.rect.min).abs();
        let resized = (actual.rect.size() - expected.rect.size()).abs();
        assert!(
            moved.x <= HORIZONTAL_TOLERANCE
                && moved.y <= VERTICAL_TOLERANCE
                && resized.max_elem() <= SIZE_TOLERANCE,
            "{screen} at {scale}×: {} {:?} is at {:?} but at 1.0× it is at {:?}; the layout \
             is in points and must not depend on the scale",
            actual.role,
            actual.label,
            actual.rect,
            expected.rect
        );
    }
}

/* ------------------------------------------------------------- the shell */

/// A shell over a real engine, driven by the harness.
struct ShellApp {
    engine: AppEngine,
    hooks: HostHooks,
    shell: Shell,
}

impl ShellApp {
    fn new(data_dir: &std::path::Path) -> Self {
        Self {
            engine: engine(data_dir),
            hooks: HostHooks::none(),
            shell: Shell::new(ShellState::default()),
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

/// A shell past the onboarding gate, rendering at `scale`.
fn ready_shell(scale: f32) -> (tempfile::TempDir, Harness<'static, ShellApp>) {
    let dir = tempfile::tempdir().expect("temporary library");
    let app = ShellApp::new(dir.path());
    let mut harness = plaudon_ui_testkit::builder_at_scale(scale)
        .build_ui_state(|ui, app: &mut ShellApp| app.view(ui), app);
    let deadline = Instant::now() + DEADLINE;
    loop {
        // One step at a time: the loading spinner animates, so `run` — which
        // runs until the frame settles — would never return while the
        // capabilities command is still in flight.
        harness.run_steps(1);
        if *harness.state().shell.startup() == Startup::Ready {
            break;
        }
        assert!(Instant::now() < deadline, "the shell never became ready");
        std::thread::sleep(Duration::from_millis(10));
    }
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

/* ----------------------------------------------------------- the library */

/// The library screen plus the dispatcher it would talk to, recorded instead
/// of run.
struct LibraryApp {
    screen: LibraryScreen,
    sent: Vec<AppCommand>,
    next_id: u64,
}

impl std::fmt::Debug for LibraryApp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("LibraryApp")
    }
}

impl LibraryApp {
    fn view(&mut self, ui: &mut egui::Ui) {
        let Self {
            screen,
            sent,
            next_id,
        } = self;
        let mut send = |command: AppCommand| {
            *next_id += 1;
            sent.push(command);
            CommandId::from_raw(*next_id)
        };
        let _intents = screen.ui(ui, &mut send);
    }
}

/// A library whose first page has already been answered with `count` rows,
/// rendering at `scale`.
fn library(scale: f32, count: usize) -> Harness<'static, LibraryApp> {
    let mut harness = plaudon_ui_testkit::builder_at_scale(scale).build_ui_state(
        |ui, app: &mut LibraryApp| app.view(ui),
        LibraryApp {
            screen: LibraryScreen::new(),
            sent: Vec::new(),
            next_id: 0,
        },
    );
    harness.run();
    let items = rows(count);
    let answer = Ok(AppResponse::ListRecordings(page(items, None, count as u64)));
    let state = harness.state_mut();
    let LibraryApp {
        screen,
        sent,
        next_id,
    } = state;
    let mut send = |command: AppCommand| {
        *next_id += 1;
        sent.push(command);
        CommandId::from_raw(*next_id)
    };
    screen.on_response(CommandId::from_raw(1), &answer, &mut send);
    harness.run();
    harness
}

/* --------------------------------------------------- the recording detail */

/// The detail container with the panels the binary registers.
///
/// The audio factory is [`no_audio_output`], which is the honest
/// configuration of a build with no device — the MARS container is one — so
/// the player panel renders its refusal instead of opening a stream and
/// starting the off-thread peak worker, whose completion time no snapshot
/// may depend on (`docs/EGUI_TESTING.md` §4.5).
struct DetailApp {
    screen: RecordingDetailScreen,
    panels: LivePanels,
    sent: Vec<AppCommand>,
    produced: Vec<&'static str>,
    next_id: u64,
}

impl std::fmt::Debug for DetailApp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DetailApp")
    }
}

impl DetailApp {
    fn view(&mut self, ui: &mut egui::Ui) {
        let Self {
            screen,
            panels,
            sent,
            produced,
            next_id,
        } = self;
        let mut send = |command: AppCommand| {
            *next_id += 1;
            sent.push(command);
            CommandId::from_raw(*next_id)
        };
        let mut out = AiOutput::default();
        let _intents = screen.ui(
            ui,
            panels,
            DetailContext { local_only: true },
            &mut send,
            &mut out,
        );
        produced.extend(out.command_names());
    }

    /// Answer a command one of the panels sent.
    ///
    /// The panels match an answer by command name and by the recording it
    /// names, never by identifier, so the identifier here is irrelevant —
    /// which is exactly what `DetailPanels::on_response` documents.
    fn answer_panels(
        &mut self,
        command: &'static str,
        result: &Result<AppResponse, ErrorEnvelope>,
    ) {
        let mut out = AiOutput::default();
        self.panels
            .on_response(CommandId::from_raw(0), command, result, &mut out);
        self.produced.extend(out.command_names());
    }
}

/// The fixture recording every scale renders.
fn fixture() -> RecordingDetail {
    // Row 1 has a measured duration, a capture instant, a transcription
    // status and two tags, so the header shows every field it can show.
    detail(row_at(1))
}

/// A transcript page for `detail`'s head revision: two aligned segments, so
/// the editor renders rows rather than its loading state.
fn transcript_page(detail: &RecordingDetail) -> TranscriptPage {
    let revision = detail.revisions[0].clone();
    let segments = [
        "Everyone is here, so let us start.",
        "The release is on Friday.",
    ]
    .into_iter()
    .enumerate()
    .map(|(index, text)| TranscriptSegmentDto {
        index: index as u32,
        text: text.to_owned(),
        start_ms: Some(index as u64 * 4_000),
        end_ms: Some(index as u64 * 4_000 + 3_500),
        alignment_origin: AlignmentOrigin::Native,
        confidence: None,
        speaker_label: None,
        annotations: Vec::new(),
    })
    .collect::<Vec<_>>();
    let total_segments = segments.len() as u32;
    TranscriptPage {
        revision,
        segments,
        next_cursor: None,
        total_segments,
    }
}

/// The recording detail of [`fixture`], loaded, rendering at `scale`.
fn recording_detail(scale: f32) -> Harness<'static, DetailApp> {
    let detail = fixture();
    let recording_id = detail.summary.recording_id;
    let mut panels = LivePanels::new(no_audio_output(), None);
    // A fixed wall clock: the outputs column prints elapsed times.
    panels.set_now_ms(1_780_000_000_000);
    let mut harness = plaudon_ui_testkit::builder_at_scale(scale).build_ui_state(
        |ui, app: &mut DetailApp| app.view(ui),
        DetailApp {
            screen: RecordingDetailScreen::new(),
            panels,
            sent: Vec::new(),
            produced: Vec::new(),
            next_id: 0,
        },
    );
    harness.run();

    // Open the recording the way the route does, and answer the fetch.
    {
        let DetailApp {
            screen,
            sent,
            next_id,
            ..
        } = harness.state_mut();
        let mut send = |command: AppCommand| {
            *next_id += 1;
            sent.push(command);
            CommandId::from_raw(*next_id)
        };
        screen.open(recording_id, &mut send);
    }
    let answer = Ok(AppResponse::GetRecording(detail.clone()));
    let fetch = CommandId::from_raw(harness.state().next_id);
    assert!(
        harness.state_mut().screen.on_response(fetch, &answer),
        "the container was waiting for its own get_recording"
    );
    harness.run();

    // The panels asked for what they need; answer the two whose answers are
    // deterministic. The rest render their honest empty states.
    let location = Ok(AppResponse::ResolveMedia(MediaLocation {
        recording_id,
        path: "/dev/null".to_owned(),
        content_type: "audio/wav".to_owned(),
        byte_length: detail.summary.byte_length,
    }));
    harness
        .state_mut()
        .answer_panels("resolve_media", &location);
    let page = Ok(AppResponse::GetTranscript(transcript_page(&detail)));
    harness.state_mut().answer_panels("get_transcript", &page);
    harness.run();
    harness
}

/* ------------------------------------------------------------- the cases */

#[test]
fn the_shell_at_rest_lays_out_the_same_at_every_scale() {
    let mut baseline: Option<Vec<Control>> = None;
    for scale in SCALE_AXIS {
        let (_dir, mut harness) = ready_shell(scale);
        assert_eq!(harness.state().shell.status(), "Ready");
        assert_sane_at_any_scale(&harness, "the shell at rest", scale);

        let controls = controls(&harness);
        // The whole navigation rail, the toolbar and the divider are there
        // whatever the scale: the rail collapses on *width in points*, which
        // does not change, so a collapse here would be a scaling bug.
        //
        // Prefix, not equality: this crate's shell registers no screens, so
        // every destination announces "<name> (not yet available)" — the
        // registry's honest state, and the same one `shell_kittest`'s
        // baseline shows.
        for required in [
            "Library",
            "Inbox",
            "Jobs",
            "Templates",
            "Settings",
            "Devices",
            "Resize the sidebar",
        ] {
            assert!(
                controls
                    .iter()
                    .any(|control| control.label.starts_with(required)),
                "the shell at {scale}×: {required:?} is not in the tree"
            );
        }
        match baseline.as_ref() {
            None => baseline = Some(controls),
            Some(expected) => {
                assert_matches_the_baseline_scale(expected, &controls, "the shell at rest", scale);
            }
        }

        assert_eq!(
            visual::snapshot_name(&harness, "scale-shell-ready"),
            visual::scaled_name("scale-shell-ready", scale)
        );
        visual::assert_snapshot_at_scale(&mut harness, "scale-shell-ready");
    }
}

#[test]
fn the_library_list_lays_out_the_same_at_every_scale() {
    let mut baseline: Option<Vec<Control>> = None;
    for scale in SCALE_AXIS {
        let mut harness = library(scale, 100);
        assert!(a11y::has_label(
            &harness,
            "Showing 100 of 100 recordings (snapshot at query time)"
        ));
        assert_sane_at_any_scale(&harness, "the library list", scale);

        let controls = controls(&harness);
        match baseline.as_ref() {
            None => {
                // The virtualized grid lays out only what fits. How much
                // fits is decided in points, so the same rows are laid out
                // at every scale — which the control comparison below is
                // what proves.
                assert!(
                    controls.len() > 4,
                    "a hundred-row library offers more than {} controls",
                    controls.len()
                );
                baseline = Some(controls);
            }
            Some(expected) => {
                assert_matches_the_baseline_scale(expected, &controls, "the library list", scale);
            }
        }

        visual::assert_snapshot_at_scale(&mut harness, "scale-library-100-rows");
    }
}

#[test]
fn the_recording_detail_lays_out_the_same_at_every_scale() {
    let mut baseline: Option<Vec<Control>> = None;
    for scale in SCALE_AXIS {
        let mut harness = recording_detail(scale);
        assert!(
            a11y::has_label(&harness, "← Back to library"),
            "the detail container did not render at {scale}×"
        );
        assert_sane_at_any_scale(&harness, "the recording detail", scale);

        let controls = controls(&harness);
        match baseline.as_ref() {
            None => baseline = Some(controls),
            Some(expected) => {
                assert_matches_the_baseline_scale(
                    expected,
                    &controls,
                    "the recording detail",
                    scale,
                );
            }
        }

        visual::assert_snapshot_at_scale(&mut harness, "scale-recording-detail");
    }
}

#[test]
fn a_scaled_baseline_is_named_after_its_scale() {
    // The guard that makes the three cases above safe: the names differ, so
    // kittest cannot be handed a 1.0× baseline for a 2.0× frame.
    let names: Vec<String> = SCALE_AXIS
        .iter()
        .map(|scale| visual::scaled_name("scale-shell-ready", *scale))
        .collect();
    assert_eq!(
        names,
        vec![
            "scale-shell-ready@1x".to_owned(),
            "scale-shell-ready@1_5x".to_owned(),
            "scale-shell-ready@2x".to_owned(),
        ]
    );
}
