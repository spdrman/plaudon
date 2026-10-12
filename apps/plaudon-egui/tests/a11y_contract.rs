//! The accessibility contract every screen owes (parity row S17 of
//! `docs/EGUI_MIGRATION_PLAN.md` §4.1).
//!
//! WCAG 2.2 AA 4.1.2 asks for a name, a role and a value on every user
//! interface component. egui gives a widget its role and its value; the name
//! is the part a screen has to supply and the part that silently disappears
//! when a control becomes an icon. So this suite walks **the application the
//! binary runs** — the same [`plaudon_egui::screens::register_all`] over a
//! real engine on a temporary library — to every destination and every
//! sub-route, and asserts
//! [`plaudon_ui_testkit::a11y::assert_every_control_is_named`] on each one.
//!
//! What this cannot answer is in `scripts/ui-a11y-egui.md`: whether the words
//! a screen reader says are the right words, in the right order, at the right
//! moment. That script is executed by a human on VoiceOver and Narrator, and
//! nothing here is a substitute for it.
//!
//! No network, no keychain, no sound card, no window: the edges are the fakes
//! `support` builds.

mod support;

use std::io::Write as _;
use std::path::Path;

use plaudon_app::command::{AppCommand, AppResponse};
use plaudon_app::dto::{
    DuplicatePolicy, ImportFilesRequest, ImportOutcome, ListRecordingsRequest, RecordingFilter,
    RecordingSort, RecordingSortField, SortDirection,
};
use plaudon_app::shell_state::ShellStateStore;
use plaudon_domain::ids::RecordingId;
use plaudon_egui::shell_app::PlaudonShell;
use plaudon_ui::shell::route::Destination;
use plaudon_ui::shell::Route;
use plaudon_ui_testkit::a11y::{self, NamedByContainer};
use plaudon_ui_testkit::Harness;

/// A harness over the whole application.
type App = Harness<'static, PlaudonShell>;

/// Controls whose name comes from the container they sit in.
///
/// Empty, and that is the finding worth recording: every control the suite
/// reaches names itself. The constant stays — with the mechanism documented
/// on [`NamedByContainer`] — because the next screen that needs one must add
/// it here, in the open, rather than weakening the assertion.
const WAIVED: &[NamedByContainer] = &[];

/// Sample rate of the fixture, matching the fake device's.
const RATE: u32 = support::FAKE_RATE;

/// Write one second of a 240 Hz tone as 16-bit mono PCM in a WAV container.
fn write_wav(path: &Path) {
    let frames = RATE;
    let data_bytes = frames * 2;
    let mut file = std::fs::File::create(path).expect("create the fixture");
    file.write_all(b"RIFF").expect("write");
    file.write_all(&(36 + data_bytes).to_le_bytes())
        .expect("write");
    file.write_all(b"WAVEfmt ").expect("write");
    file.write_all(&16u32.to_le_bytes()).expect("write");
    file.write_all(&1u16.to_le_bytes()).expect("write");
    file.write_all(&1u16.to_le_bytes()).expect("write");
    file.write_all(&RATE.to_le_bytes()).expect("write");
    file.write_all(&(RATE * 2).to_le_bytes()).expect("write");
    file.write_all(&2u16.to_le_bytes()).expect("write");
    file.write_all(&16u16.to_le_bytes()).expect("write");
    file.write_all(b"data").expect("write");
    file.write_all(&data_bytes.to_le_bytes()).expect("write");
    for frame in 0..frames {
        let phase = f64::from(frame) * 240.0 * std::f64::consts::TAU / f64::from(RATE);
        #[allow(clippy::cast_possible_truncation)]
        let sample = (phase.sin() * 0.6 * f64::from(i16::MAX)) as i16;
        file.write_all(&sample.to_le_bytes()).expect("write");
    }
    file.flush().expect("flush");
}

/// Dispatch `command` and insist it succeeded.
fn call(app: &PlaudonShell, command: AppCommand) -> AppResponse {
    app.engine()
        .dispatcher()
        .call_blocking(command)
        .expect("the command succeeded")
}

/// Import one synthesised recording and answer with its identity.
fn import_one(app: &PlaudonShell, dir: &Path) -> RecordingId {
    let fixture = dir.join("meeting.wav");
    write_wav(&fixture);
    let imported = match call(
        app,
        AppCommand::ImportFiles(ImportFilesRequest {
            paths: vec![fixture.to_string_lossy().into_owned()],
            duplicate_policy: DuplicatePolicy::Ask,
        }),
    ) {
        AppResponse::ImportFiles(response) => response,
        other => panic!("import_files answered {}", other.command()),
    };
    assert!(
        matches!(
            imported.results.first(),
            Some(ImportOutcome::Accepted { .. })
        ),
        "the fixture should import: {:?}",
        imported.results
    );
    match call(
        app,
        AppCommand::ListRecordings(ListRecordingsRequest {
            cursor: None,
            limit: 1,
            filter: RecordingFilter {
                query: None,
                import_status: Vec::new(),
                transcription_status: Vec::new(),
                sources: Vec::new(),
                tags: Vec::new(),
                date_from: None,
                date_to: None,
            },
            sort: RecordingSort {
                field: RecordingSortField::ImportedAt,
                direction: SortDirection::Desc,
            },
        }),
    ) {
        AppResponse::ListRecordings(page) => {
            page.items
                .first()
                .expect("the imported recording")
                .recording_id
        }
        other => panic!("list_recordings answered {}", other.command()),
    }
}

/// The application, past the first-run gate, with nothing in the library.
fn ready() -> (tempfile::TempDir, App) {
    let dir = tempfile::tempdir().expect("temporary library");
    let store = ShellStateStore::new(dir.path());
    let (audio, _device) = support::fake_audio_out();
    let app = support::app(dir.path(), support::hooks(store), audio);
    support::complete_onboarding(app.engine(), true);
    let harness = support::harness(app);
    (dir, harness)
}

/// Settle on a route and assert the contract there.
fn assert_route(harness: &mut App, route: Route) {
    let path = route.path();
    harness.state_mut().shell_mut().navigate(route);
    // A destination fetches its first page on arrival; the contract is about
    // what the user ends up looking at, not about the frame in flight.
    harness.run_steps(12);
    a11y::assert_every_control_is_named(harness, &path, WAIVED);
}

/* ------------------------------------------------------- the first run */

#[test]
fn the_first_run_flow_names_every_control() {
    let dir = tempfile::tempdir().expect("temporary library");
    let store = ShellStateStore::new(dir.path());
    let (audio, _device) = support::fake_audio_out();
    // Onboarding deliberately *not* completed: a fresh library gates the
    // shell on the first-run flow, which is therefore what renders.
    let mut harness = support::harness(support::app(dir.path(), support::hooks(store), audio));
    harness.run_steps(12);
    assert!(
        harness.state().shell().onboarding_required(),
        "the first-run flow is what this case is about"
    );
    a11y::assert_every_control_is_named(&harness, "the first-run flow", WAIVED);

    harness.state_mut().shutdown();
}

/* ----------------------------------------------------- the destinations */

#[test]
fn every_destination_names_every_control() {
    let (_dir, mut harness) = ready();
    for destination in Destination::ALL {
        assert_route(&mut harness, destination.route());
    }
    harness.state_mut().shutdown();
}

#[test]
fn every_sub_route_names_every_control() {
    let (_dir, mut harness) = ready();
    for route in [
        Route::LibrarySearch,
        Route::SettingsActivity,
        Route::SettingsSources,
    ] {
        assert_route(&mut harness, route);
    }
    harness.state_mut().shutdown();
}

#[test]
fn the_recording_detail_names_every_control() {
    let dir = tempfile::tempdir().expect("temporary library");
    let store = ShellStateStore::new(dir.path());
    let (audio, _device) = support::fake_audio_out();
    let app = support::app(dir.path(), support::hooks(store), audio);
    support::complete_onboarding(app.engine(), true);
    let recording_id = import_one(&app, dir.path());
    let mut harness = support::harness(app);

    // The library first, because that is how a user reaches a recording, and
    // then the detail itself.
    assert_route(&mut harness, Route::Library);
    harness
        .state_mut()
        .shell_mut()
        .navigate(Route::LibraryRecording {
            recording_id: recording_id.to_string(),
        });
    support::run_until_announced(&mut harness, "Content hash (SHA-256)");
    a11y::assert_every_control_is_named(&harness, "the recording detail", WAIVED);

    harness.state_mut().shutdown();
}

/* ---------------------------------------------------------- the overlays */

#[test]
fn the_command_palette_names_every_control() {
    let (_dir, mut harness) = ready();
    harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::K);
    harness.run_steps(4);
    assert!(
        harness.state().shell().palette().is_open(),
        "Cmd+K did not open the palette"
    );
    a11y::assert_every_control_is_named(&harness, "the command palette", WAIVED);

    harness.key_press(egui::Key::Escape);
    harness.run_steps(2);
    harness.state_mut().shutdown();
}

/* ------------------------------------------------- the contract itself */

#[test]
fn the_contract_covers_every_destination_the_router_knows() {
    // A destination added to the router without a line in
    // `every_destination_names_every_control` would never be checked; the
    // loop there iterates `Destination::ALL`, and this is what makes that a
    // deliberate guarantee rather than a coincidence.
    assert_eq!(
        Destination::ALL.len(),
        6,
        "a destination was added or removed; the S17 sweep iterates \
         Destination::ALL, and the manual script in scripts/ui-a11y-egui.md \
         has one section per destination that must follow"
    );
}
