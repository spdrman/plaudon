//! The end-to-end suite that replaces Playwright (`docs/EGUI_BASELINE.md`
//! §6.2: 7 specs, 31 tests).
//!
//! Every test here drives **the whole application** — the same
//! [`plaudon_egui::screens::register_all`] the binary calls, over a real
//! [`plaudon_app::runtime::AppEngine`] on a temporary library — and fakes
//! only the edges the operating system owns: the secret store, the notifier,
//! the audio device, the BLE radio and the file dialogs. Nothing here opens a
//! socket, a keychain, a window or a sound card.
//!
//! Where a flow cannot be driven to its end with the commands this build has,
//! the test asserts the strongest thing that is *true* and says so. Three
//! places do that, and each one names the missing seam:
//!
//! * **a reviewable claim** cannot be created: [`plaudon_app::AppPorts`]'s
//!   `outputs` port offers `list_outputs`, `review_claim` and
//!   `regenerate_output` and no way to insert an output, and
//!   `regenerate_output` answers `CAPABILITY_UNVERIFIED` because the
//!   generation pipeline belongs to another EPIC. The Review tab's honest
//!   empty state is asserted instead of a fabricated claim.
//! * **a queued summary job** cannot be started from the Summarize tab:
//!   `list_models` maps every summary stage to the text-generation task and
//!   answers only for the OpenRouter engine, so the picker can offer nothing
//!   but a cloud model, and `start_processing` refuses every non-transcribe
//!   stage in the cloud. With Local-only Mode enforced the panel refuses
//!   before anything is dispatched, and that refusal is what is asserted.
//! * **the waveform's "still building" state** has no announcement: the peak
//!   worker's status is rendered as no text anywhere, so the test asserts
//!   what the panel does report — the position readout and the scrubber —
//!   and never waits on the worker.

mod support;

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use plaudon_app::backend::devices::{
    DeviceRadio, RadioAvailability, RadioProbe, TransferReporter, TransferRequest,
};
use plaudon_app::command::{AppCommand, AppResponse};
use plaudon_app::dto::{
    Appearance, GetRecordingRequest, JobKind, JobSummary, ListJobsRequest, ListOutputsRequest,
    ListRecordingsRequest, RecordingDetail, RecordingFilter, RecordingSort, RecordingSortField,
    RecordingSummary, SortDirection,
};
use plaudon_app::ports::{MachineRevisionSpec, MachineSegment};
use plaudon_app::runtime::AppEngine;
use plaudon_app::shell_state::ShellStateStore;
use plaudon_audio_playback::time::Micros;
use plaudon_audio_playback::FakeDevice;
use plaudon_domain::error::{AppError, AppErrorCode};
use plaudon_domain::ids::RecordingId;
use plaudon_domain::transcript::AlignmentOrigin;
use plaudon_egui::shell_app::PlaudonShell;
use plaudon_plaud_ble::discovery::ObservedAdvertisement;
use plaudon_plaud_protocol::payload::DeltaListing;
use plaudon_security::secrets::SecretString;
use plaudon_ui::shell::{HostHooks, Route};
use plaudon_ui_testkit::kittest::Queryable as _;
use plaudon_ui_testkit::{a11y, Harness};

/// Sample rate of every fixture, matching the fake device's.
const RATE: u32 = support::FAKE_RATE;

/// A harness over the whole application.
type App = Harness<'static, PlaudonShell>;

/* ------------------------------------------------------------- fixtures */

/// Write `seconds` of a 240 Hz tone as 16-bit mono PCM in a WAV container.
///
/// Synthesised rather than committed, so the duration, the content hash and
/// the decoded samples are all known to the test that wrote them.
fn write_wav(path: &Path, seconds: u32) {
    let frames = RATE * seconds;
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
        let sample = (phase.sin() * 0.6 * f64::from(i16::MAX)) as i16;
        file.write_all(&sample.to_le_bytes()).expect("write");
    }
    file.flush().expect("flush");
}

/// Host hooks that pick `audio` to import and answer `save_to` for a save
/// dialog, recording every file name the application suggested.
fn hooks_with_dialogs(
    store: ShellStateStore,
    audio: Vec<PathBuf>,
    save_to: Option<PathBuf>,
) -> (HostHooks, Arc<Mutex<Vec<String>>>) {
    let suggested = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&suggested);
    let hooks = HostHooks {
        pick_save_path: Arc::new(move |name: &str| {
            seen.lock()
                .expect("the suggestion log")
                .push(name.to_owned());
            save_to.clone()
        }),
        ..support::hooks_picking(store, audio)
    };
    (hooks, suggested)
}

/// A library with `count` recordings already imported, at the Library route.
///
/// The import is driven through the interface — the empty state's own
/// control, the host's file picker and the real `import_files` command — so
/// every test starts from a library the application itself built.
fn imported(dir: &Path, count: usize, hooks: HostHooks) -> (App, FakeDevice) {
    imported_in(dir, count, hooks, plaudon_ui_testkit::DEFAULT_SIZE)
}

/// The same, in a window of `size`.
///
/// The recording detail is a fixed pane with no scroll area of its own, so
/// at the default 1280×820 the transcript editor is laid out below the fold
/// and its virtualized rows are never created. A test that drives the
/// editor opens a taller window rather than asserting on rows the layout
/// never built.
fn imported_in(dir: &Path, count: usize, hooks: HostHooks, size: egui::Vec2) -> (App, FakeDevice) {
    let engine = support::engine(dir);
    // Local-only Mode is enforced from the first run: it is both the mode
    // these assertions are about and the reason no cloud path can be taken
    // by accident.
    support::complete_onboarding(&engine, true);
    let (audio, device) = support::fake_audio_out();
    let app = support::app_over(engine, hooks, audio);
    let mut harness = plaudon_ui_testkit::builder()
        .with_size(size)
        .build_ui_state(|ui, app: &mut PlaudonShell| app.view(ui), app);
    support::settle(&mut harness);
    support::run_until_announced(&mut harness, "Import an audio file");
    harness.get_by_label("Import an audio file").click();
    support::run_until_frame(&mut harness, "the imported recordings", |harness| {
        recordings(harness.state()).len() == count
    });
    (harness, device)
}

/// Every row the library holds, straight from the application.
fn recordings(app: &PlaudonShell) -> Vec<RecordingSummary> {
    let answer = app
        .engine()
        .dispatcher()
        .call_blocking(AppCommand::ListRecordings(ListRecordingsRequest {
            cursor: None,
            limit: 50,
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
                field: RecordingSortField::Title,
                direction: SortDirection::Asc,
            },
        }));
    match answer {
        Ok(AppResponse::ListRecordings(page)) => page.items,
        other => panic!("list_recordings answered {other:?}"),
    }
}

/// The detail of one recording, straight from the application.
fn detail(app: &PlaudonShell, recording_id: RecordingId) -> RecordingDetail {
    let answer = app
        .engine()
        .dispatcher()
        .call_blocking(AppCommand::GetRecording(GetRecordingRequest {
            recording_id: recording_id.to_string(),
        }));
    match answer {
        Ok(AppResponse::GetRecording(detail)) => detail,
        other => panic!("get_recording answered {other:?}"),
    }
}

/// Every job the application holds.
fn jobs(app: &PlaudonShell) -> Vec<JobSummary> {
    let answer = app
        .engine()
        .dispatcher()
        .call_blocking(AppCommand::ListJobs(ListJobsRequest {
            cursor: None,
            limit: 50,
            states: Vec::new(),
            kinds: Vec::new(),
        }));
    match answer {
        Ok(AppResponse::ListJobs(page)) => page.items,
        other => panic!("list_jobs answered {other:?}"),
    }
}

/// Give `recording_id` a machine transcript, as the STT EPIC will.
///
/// [`plaudon_app::ports::TranscriptPort::create_machine_revision`] is the
/// entry point an engine calls when it finishes, so a test that needs a real
/// transcript calls it rather than inventing rows or running a model. The
/// segments carry no times, so the revision's alignment origin is `none` —
/// which is also what the caption-export refusal needs.
fn give_transcript(engine: &AppEngine, recording_id: RecordingId, lines: &[&str]) {
    let spec = MachineRevisionSpec {
        recording_id,
        segments: lines
            .iter()
            .map(|line| MachineSegment::text_only(*line))
            .collect(),
        engine_id: "test-stt".to_owned(),
        model_id: "fixture".to_owned(),
        alignment_origin: AlignmentOrigin::None,
    };
    engine
        .handle()
        .block_on(engine.ports().transcript.create_machine_revision(spec))
        .expect("the machine revision is recorded");
}

/// Open the recording detail of `recording_id` and wait for its header.
fn open_detail(harness: &mut App, recording_id: RecordingId) {
    harness
        .state_mut()
        .shell_mut()
        .navigate(Route::LibraryRecording {
            recording_id: recording_id.to_string(),
        });
    support::run_until_announced(harness, "Content hash (SHA-256)");
}

/// Open one tab of the recording detail's outputs column.
fn open_tab(harness: &mut App, tab: &str) {
    support::run_until_announced(harness, tab);
    activate(harness, tab);
}

/// Operate the control named `label` the way assistive technology does.
///
/// The detail's columns are fixed panes that clip rather than scroll, so a
/// control low in one of them is in the accessibility tree but outside the
/// clip rectangle, where a synthetic pointer cannot reach it. The AccessKit
/// click action is the path a screen-reader user takes and is the one that
/// does not depend on where the layout happened to put the control.
fn activate(harness: &mut App, label: &str) {
    harness.get_by_label(label).click_accesskit();
    harness.run_steps(2);
}

/* ----------------------------------------------------------- fake radio */

/// A scripted BLE radio, shaped like the `FakeRadio` the application's own
/// device tests use. It touches no hardware: a scan answers with whatever
/// the test queued, or refuses the way a denied radio does.
#[derive(Debug, Default)]
struct ScriptedRadio {
    adverts: Mutex<Vec<ObservedAdvertisement>>,
    scan_error: Mutex<Option<AppErrorCode>>,
}

impl ScriptedRadio {
    /// A radio that hears one device advertising the vendor service.
    fn hearing(peripheral_id: &str, name: &str) -> Self {
        Self {
            adverts: Mutex::new(vec![ObservedAdvertisement {
                peripheral_id: peripheral_id.to_owned(),
                local_name: Some(name.to_owned()),
                service_ids: vec![plaudon_plaud_protocol::gatt::SERVICE_VENDOR],
                manufacturer_ids: vec![0x0499],
                rssi: Some(-52),
            }]),
            scan_error: Mutex::new(None),
        }
    }

    /// Make every later scan fail the way the operating system does when
    /// Bluetooth permission has not been granted.
    fn deny(&self) {
        *self.scan_error.lock().expect("the scan error") =
            Some(AppErrorCode::RadioPermissionDenied);
    }
}

#[async_trait::async_trait]
impl DeviceRadio for ScriptedRadio {
    fn availability(&self) -> RadioAvailability {
        RadioAvailability::Present
    }

    async fn scan(&self, _timeout: Duration) -> Result<Vec<ObservedAdvertisement>, AppError> {
        if let Some(code) = *self.scan_error.lock().expect("the scan error") {
            return Err(AppError::new(code, "the operating system refused the scan"));
        }
        Ok(self.adverts.lock().expect("the adverts").clone())
    }

    async fn probe(&self, _peripheral_id: &str) -> Result<RadioProbe, AppError> {
        Ok(RadioProbe {
            model: Some("Note".to_owned()),
            firmware: Some("V0095".to_owned()),
            auth_mode: Some("legacy_static_token".to_owned()),
            battery_percent: Some(81),
            rssi_dbm: Some(-52),
        })
    }

    async fn list(
        &self,
        _peripheral_id: &str,
        _token: &SecretString,
    ) -> Result<DeltaListing, AppError> {
        Err(AppError::new(
            AppErrorCode::SourceUnavailable,
            "this radio lists nothing",
        ))
    }

    async fn transfer(
        &self,
        _request: TransferRequest<'_>,
        _reporter: &dyn TransferReporter,
    ) -> Result<(), AppError> {
        Err(AppError::new(
            AppErrorCode::SourceUnavailable,
            "this radio transfers nothing",
        ))
    }

    async fn disconnect(&self, _peripheral_id: &str) {}
}

/* ---------------------------------------------------------------- tests */

#[test]
fn the_library_lists_every_imported_recording_and_states_an_unmeasured_duration() {
    let dir = tempfile::tempdir().expect("temporary library");
    let first = dir.path().join("board-meeting.wav");
    let second = dir.path().join("standup.wav");
    write_wav(&first, 2);
    write_wav(&second, 1);
    let store = ShellStateStore::new(dir.path());
    let (hooks, _) = hooks_with_dialogs(store, vec![first, second], None);
    let (mut harness, _device) = imported(dir.path(), 2, hooks);

    // The grid row, not the import panel's echo of the path: a row
    // announces its title followed by the facts §8.6A governs.
    support::run_until_frame(&mut harness, "both rows in the grid", |harness| {
        support::announces(harness, "board-meeting. Recorded")
            && support::announces(harness, "standup. Recorded")
    });

    // §8.6A: manual import measures no duration, and the grid states that
    // rather than drawing a length nothing observed.
    let rows = recordings(harness.state());
    assert_eq!(rows.len(), 2);
    assert!(
        rows.iter().all(|row| row.duration_ms.is_none()),
        "this fixture exists to exercise the unmeasured case"
    );
    let announced = support::announced(&harness);
    assert!(
        announced.iter().any(|text| text == "Unknown"),
        "an unmeasured cell must say so: {announced:?}"
    );
    assert!(
        announced
            .iter()
            .any(|text| text.contains("duration unknown")),
        "the row must announce an unknown duration: {announced:?}"
    );
    assert!(
        !announced.iter().any(|text| text.contains("0:00")),
        "an unmeasured duration must never render as 0:00: {announced:?}"
    );
}

#[test]
fn opening_a_recording_shows_its_provenance_and_the_enforced_privacy_mode() {
    let dir = tempfile::tempdir().expect("temporary library");
    let media = dir.path().join("board-meeting.wav");
    write_wav(&media, 2);
    let store = ShellStateStore::new(dir.path());
    let (hooks, _) = hooks_with_dialogs(store, vec![media], None);
    let (mut harness, _device) = imported(dir.path(), 1, hooks);
    let row = recordings(harness.state()).remove(0);

    open_detail(&mut harness, row.recording_id);

    // Provenance: how a user checks this is the recording they think it is.
    for name in ["Source", "Original file", "Content hash (SHA-256)"] {
        assert!(
            support::announces(&harness, name),
            "the header does not state {name}: {:?}",
            support::announced(&harness)
        );
    }
    assert!(
        support::announces(&harness, "board-meeting.wav"),
        "the original file name is not shown"
    );
    assert!(
        support::announces(&harness, &row.sha256.to_hex()[..12]),
        "the content-hash fingerprint is not shown"
    );
    assert!(support::announces(&harness, &row.title));

    // The badge states the mode the application enforces. Waiting for the
    // engine inventory is what proves the badge is reporting the capability
    // report rather than its own fail-closed default.
    support::run_until_frame(&mut harness, "the engine inventory", |harness| {
        !support::announces(harness, "The engine inventory has not been read yet.")
    });
    assert!(
        support::announces(&harness, "Local only"),
        "the privacy badge does not state the enforced mode: {:?}",
        support::announced(&harness)
    );
}

#[test]
fn playing_and_seeking_moves_the_position_the_transport_reports() {
    let dir = tempfile::tempdir().expect("temporary library");
    let media = dir.path().join("board-meeting.wav");
    write_wav(&media, 8);
    let store = ShellStateStore::new(dir.path());
    let (hooks, _) = hooks_with_dialogs(store, vec![media], None);
    let (mut harness, device) = imported(dir.path(), 1, hooks);
    let row = recordings(harness.state()).remove(0);
    open_detail(&mut harness, row.recording_id);

    // `resolve_media` answered, the archived file opened through
    // `SymphoniaPcmSource`, and the source stated its own length — the
    // library row carries no measured duration at all.
    support::run_until_announced(&mut harness, "0:00 / 0:08");

    harness.get_by_label("Play").click();
    harness.run_steps(2);
    assert!(
        support::announces(&harness, "Pause"),
        "the transport did not start: {:?}",
        support::announced(&harness)
    );

    // The device renders on demand, so "two and a half seconds of audio" is
    // exactly that and the clock cannot drift.
    device.render_micros(Micros::from_millis(2_500));
    harness.run_steps(2);
    assert!(
        support::announces(&harness, "0:02 / 0:08"),
        "the position did not follow the device clock: {:?}",
        support::announced(&harness)
    );

    // Seek through the scrubber's own keyboard map. The waveform is a
    // `Role::Slider` whose label is the position; it is drawn, focusable and
    // seekable whether or not the peak worker has finished, which is why
    // nothing here waits on it.
    let scrubber = a11y::nodes(&harness)
        .into_iter()
        .find(|node| {
            node.announced()
                .is_some_and(|text| text.contains(" of 0:08"))
        })
        .expect("the scrubber is in the accessibility tree");
    assert_eq!(
        scrubber.role, "Slider",
        "the position scrubber is a slider, as the React scrubber was"
    );
    harness.get_by_label_contains(" of 0:08").focus();
    harness.run_steps(2);

    // One arrow is a five-second step from where the clock is, so the
    // landing point is a figure only the seek can produce: 2.5 s + 5 s.
    harness.key_press(egui::Key::ArrowRight);
    harness.run_steps(3);
    assert!(
        support::announces(&harness, "0:07 / 0:08"),
        "the five-second step did not land at 7.5 s: {:?}",
        support::announced(&harness)
    );

    // And End asks for the very end, which is where it lands.
    harness.key_press(egui::Key::End);
    harness.run_steps(3);
    assert!(
        support::announces(&harness, "0:08 / 0:08"),
        "the seek did not land where it was asked to: {:?}",
        support::announced(&harness)
    );
}

#[test]
fn editing_a_segment_undoing_it_and_letting_autosave_fire_creates_a_revision() {
    let dir = tempfile::tempdir().expect("temporary library");
    let media = dir.path().join("board-meeting.wav");
    write_wav(&media, 2);
    let store = ShellStateStore::new(dir.path());
    let (hooks, _) = hooks_with_dialogs(store, vec![media], None);
    let (mut harness, _device) = imported_in(
        dir.path(),
        1,
        hooks,
        egui::vec2(plaudon_ui_testkit::DEFAULT_SIZE.x, 1_800.0),
    );
    let row = recordings(harness.state()).remove(0);
    give_transcript(
        harness.state().engine(),
        row.recording_id,
        &["We agreed to ship on Friday.", "Anna will write the notes."],
    );
    let machine = detail(harness.state(), row.recording_id)
        .current_revision_id
        .expect("the machine revision is the head");

    open_detail(&mut harness, row.recording_id);
    // A segment row announces its name and carries its text as the value a
    // screen reader reads out, which is what `segment_text` asks for.
    support::run_until_announced(&mut harness, "Segment 1");
    assert_eq!(
        segment_text(&harness),
        "We agreed to ship on Friday.",
        "the editor opened on the machine revision's own text"
    );

    let before = segment_text(&harness);
    harness.get_by_label_contains("Segment 1").click_accesskit();
    harness.run_steps(2);
    harness
        .get_by_label_contains("Segment 1")
        .type_text("Yes. ");
    harness.run_steps(2);
    let typed = segment_text(&harness);
    assert_ne!(typed, before, "typing did not change the document");
    assert!(
        typed.contains("Yes. "),
        "the typed text is not in the segment: {typed}"
    );

    harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    harness.run_steps(2);
    assert_eq!(
        segment_text(&harness),
        before,
        "undo did not restore the segment"
    );

    // Type again and let the session's own debounce elapse. Nothing sleeps
    // on a wall clock: the harness advances egui's frame clock, which is the
    // clock `plaudon_transcript_doc::autosave` measures against.
    harness
        .get_by_label_contains("Segment 1")
        .type_text("Note: ");
    harness.run_steps(2);
    support::run_until_frame(&mut harness, "the autosaved revision", |harness| {
        detail(harness.state(), row.recording_id).revisions.len() > 1
    });

    let saved = detail(harness.state(), row.recording_id);
    assert_eq!(saved.revisions.len(), 2, "autosave wrote no child revision");
    assert_ne!(
        saved.current_revision_id.expect("a head revision"),
        machine,
        "the head is still the machine revision"
    );
    assert!(
        saved
            .revisions
            .iter()
            .any(|revision| revision.revision_id == machine),
        "the original revision must stay retrievable"
    );
}

/// The text the first transcript segment reports to a screen reader.
fn segment_text(harness: &App) -> String {
    harness
        .get_by_label_contains("Segment 1")
        .value()
        .expect("a segment reports its text")
}

#[test]
fn the_summarize_tab_refuses_a_cloud_summary_and_queues_nothing() {
    let dir = tempfile::tempdir().expect("temporary library");
    let media = dir.path().join("board-meeting.wav");
    write_wav(&media, 2);
    let store = ShellStateStore::new(dir.path());
    let (hooks, _) = hooks_with_dialogs(store, vec![media], None);
    let (mut harness, _device) = imported(dir.path(), 1, hooks);
    let row = recordings(harness.state()).remove(0);
    open_detail(&mut harness, row.recording_id);
    open_tab(&mut harness, "Summarize");

    assert!(
        support::announces(&harness, "Summarization"),
        "the summarize tab did not open: {:?}",
        support::announced(&harness)
    );

    // The catalogue arrives from the pinned allowlist, which is what proves
    // no provider was contacted to build it.
    support::run_until_announced(&mut harness, "Pinned allowlist");

    // Every model the picker can offer for a summary stage is a cloud model —
    // `list_models` maps summary stages to the text-generation task and
    // answers only for the OpenRouter engine — so with Local-only Mode
    // enforced the panel refuses before anything is dispatched. There is no
    // run control to press and nothing is queued. A QUEUED summary row is
    // unreachable from this tab in this build; see the module comment.
    assert!(
        support::announces(&harness, "Blocked by Local-only Mode"),
        "a cloud model must be refused while Local-only Mode is on: {:?}",
        support::announced(&harness)
    );
    assert!(
        support::announces(
            &harness,
            "Estimated charge before dispatch: choose a model to see an estimate."
        ),
        "no model can be chosen, so no estimate and no dispatch is offered"
    );
    assert!(
        !support::announces(&harness, "Run consolidated summary"),
        "a run control was offered for a model that cannot be chosen"
    );
    // The only job this library has is the import that put the recording in
    // it; no processing job was queued, which is the whole point.
    let queued = jobs(harness.state());
    assert!(
        queued.iter().all(|job| job.kind == JobKind::Import),
        "a processing job was queued by a refused model: {queued:?}"
    );
}

#[test]
fn the_review_tab_states_that_this_recording_has_nothing_to_review() {
    let dir = tempfile::tempdir().expect("temporary library");
    let media = dir.path().join("board-meeting.wav");
    write_wav(&media, 2);
    let store = ShellStateStore::new(dir.path());
    let (hooks, _) = hooks_with_dialogs(store, vec![media], None);
    let (mut harness, _device) = imported(dir.path(), 1, hooks);
    let row = recordings(harness.state()).remove(0);
    give_transcript(
        harness.state().engine(),
        row.recording_id,
        &["A claim would have to be anchored in this."],
    );
    open_detail(&mut harness, row.recording_id);
    open_tab(&mut harness, "Review");

    assert!(
        support::announces(&harness, "Review the sources"),
        "the review tab did not open: {:?}",
        support::announced(&harness)
    );

    // No claim can be created hermetically: the outputs port lists, reviews
    // and regenerates, and nothing inserts. `regenerate_output` answers
    // `CAPABILITY_UNVERIFIED` because the generation pipeline is another
    // EPIC, so this empty state is the whole truth the tab can tell today.
    support::run_until_announced(
        &mut harness,
        "No output has been generated for this recording yet, so there is nothing to review.",
    );
    let answer = harness
        .state()
        .engine()
        .dispatcher()
        .call_blocking(AppCommand::ListOutputs(ListOutputsRequest {
            recording_id: row.recording_id.to_string(),
        }));
    match answer {
        Ok(AppResponse::ListOutputs(response)) => assert!(
            response.outputs.is_empty(),
            "the empty state must match what the application holds"
        ),
        other => panic!("list_outputs answered {other:?}"),
    }
}

#[test]
fn exporting_a_transcript_writes_the_chosen_file_and_refuses_captions_without_times() {
    let dir = tempfile::tempdir().expect("temporary library");
    let media = dir.path().join("board-meeting.wav");
    write_wav(&media, 2);
    let destination = dir.path().join("exported-transcript.txt");
    let store = ShellStateStore::new(dir.path());
    let (hooks, suggested) = hooks_with_dialogs(store, vec![media], Some(destination.clone()));
    let (mut harness, _device) = imported(dir.path(), 1, hooks);
    let row = recordings(harness.state()).remove(0);
    give_transcript(
        harness.state().engine(),
        row.recording_id,
        &["We agreed to ship on Friday."],
    );
    open_detail(&mut harness, row.recording_id);
    open_tab(&mut harness, "Export");
    support::run_until_announced(&mut harness, "Transcript formats");

    // §8.6A: a revision with no alignment cannot become captions, and the
    // panel states which reason that is rather than hiding the format.
    assert!(
        support::announces(&harness, "SubRip captions (unavailable)"),
        "SRT must be refused for an unaligned revision: {:?}",
        support::announced(&harness)
    );
    assert!(
        support::announces(&harness, "WebVTT captions (unavailable)"),
        "VTT must be refused for an unaligned revision"
    );
    assert!(
        support::announces(&harness, plaudon_ui::ai::NO_TIMESTAMPS_REASON),
        "the refusal must state the reason"
    );

    activate(&mut harness, "Plain text");
    support::run_until_frame(&mut harness, "the exported file", |_| destination.is_file());

    assert_eq!(
        suggested.lock().expect("the suggestion log").as_slice(),
        [format!("{}.txt", row.title)],
        "the host was asked for a path under the recording's own name"
    );
    let written = std::fs::read_to_string(&destination).expect("the export is readable");
    assert!(
        written.contains("We agreed to ship on Friday."),
        "the export does not carry the transcript: {written:?}"
    );
    assert!(
        jobs(harness.state())
            .iter()
            .any(|job| job.kind == JobKind::Export),
        "an export runs as a job and is recorded as one"
    );
}

#[test]
fn a_scan_fills_the_five_sections_and_a_denied_radio_is_reported_as_denied() {
    let dir = tempfile::tempdir().expect("temporary library");
    let store = ShellStateStore::new(dir.path());
    let radio = Arc::new(ScriptedRadio::hearing(
        "AABBCCDD-0011-2233-4455-667788990011",
        "PLAUD_NOTE",
    ));
    let engine = AppEngine::start(support::config(dir.path()).with_radio(radio.clone()))
        .expect("the engine starts with a scripted radio");
    support::complete_onboarding(&engine, true);
    let (audio, _device) = support::fake_audio_out();
    let mut harness = support::harness(support::app_over(engine, support::hooks(store), audio));

    harness.state_mut().shell_mut().navigate(Route::Devices);
    support::run_until_announced(&mut harness, "Scan for nearby devices");
    harness.get_by_label("Scan for nearby devices").click();
    support::run_until_announced(&mut harness, "PLAUD_NOTE");

    // App. B.8: five sections, always all five, each with its own count.
    for section in [
        "Nearby (",
        "Connected (",
        "Needs authorization (",
        "Unsupported firmware (",
        "Previously linked (",
    ] {
        assert!(
            support::announces(&harness, section),
            "the {section}…) section is missing: {:?}",
            support::announced(&harness)
        );
    }
    assert!(
        !support::announces(&harness, "The scan ran and heard no PLAUD device."),
        "a scan that heard a device must not report that it heard none"
    );

    // A radio the operating system refuses is reported as refused, with the
    // machine reason, and never as an empty neighbourhood.
    radio.deny();
    harness.get_by_label("Scan for nearby devices").click();
    support::run_until_announced(&mut harness, "Bluetooth access is not permitted");
    assert!(
        support::announces(&harness, AppErrorCode::RadioPermissionDenied.as_str()),
        "the reason code is not reported: {:?}",
        support::announced(&harness)
    );
    assert!(
        !support::announces(&harness, "The scan ran and heard no PLAUD device."),
        "a denied radio must never be reported as nothing nearby"
    );
    assert!(
        support::announces(
            &harness,
            "Not checked: the radio could not be used for this scan."
        ),
        "a section the radio could not fill must say so rather than read as empty"
    );
}

#[test]
fn changing_the_appearance_reaches_the_preferences_and_the_shell_state() {
    let dir = tempfile::tempdir().expect("temporary library");
    let store = ShellStateStore::new(dir.path());
    let engine = support::engine(dir.path());
    support::complete_onboarding(&engine, true);
    let (audio, _device) = support::fake_audio_out();
    let mut harness = support::harness(support::app_over(
        engine,
        support::hooks(store.clone()),
        audio,
    ));

    harness.state_mut().shell_mut().navigate(Route::Settings);
    support::run_until_announced(&mut harness, "Follow the system");
    harness.get_by_label("Dark").click();

    // `set_appearance` is a real command against the real store: the proof
    // it round-tripped is the application answering with it afterwards.
    support::run_until_frame(&mut harness, "the stored appearance", |harness| {
        stored_appearance(harness.state()) == Appearance::Dark
    });

    // The shell's own presentation state is a different thing from the
    // preference, and it reaches the disk through the host hook. The command
    // palette is the surface that runs a shell command without a pointer:
    // open it, type, and press Enter on the ranked hit.
    //
    // The menu bar offers the same command under View ▸ Appearance, but a
    // submenu opens on a real pointer that dwells over it, which a synthetic
    // click cannot reproduce; the palette is the same command through the
    // same registry.
    harness.state_mut().shell_mut().navigate(Route::Library);
    support::run_until_announced(&mut harness, "Import an audio file");
    harness.get_by_label("Command palette").click();
    harness.run_steps(2);
    harness.event(egui::Event::Text("appearance dark".to_owned()));
    harness.run_steps(2);
    harness.key_press(egui::Key::Enter);
    harness.run_steps(2);
    harness.state_mut().flush_state();
    assert_eq!(
        store.load().appearance,
        Appearance::Dark,
        "the shell state was not written on flush"
    );
}

/// The appearance the application has stored.
fn stored_appearance(app: &PlaudonShell) -> Appearance {
    let answer = app
        .engine()
        .dispatcher()
        .call_blocking(AppCommand::GetCapabilities);
    match answer {
        Ok(AppResponse::GetCapabilities(capabilities)) => capabilities.preferences.appearance,
        other => panic!("get_capabilities answered {other:?}"),
    }
}

#[test]
fn every_destination_is_reachable_with_the_tab_key_alone() {
    let dir = tempfile::tempdir().expect("temporary library");
    let store = ShellStateStore::new(dir.path());
    let engine = support::engine(dir.path());
    support::complete_onboarding(&engine, true);
    let (audio, _device) = support::fake_audio_out();
    let mut harness = support::harness(support::app_over(engine, support::hooks(store), audio));
    support::run_until_announced(&mut harness, "Import an audio file");

    // The Playwright check this replaces is `e2e/keyboard-flow.spec.ts`: no
    // interactive element unreachable by keyboard on the main screens.
    let order = a11y::focus_order(&mut harness, 200);
    for destination in [
        "Library",
        "Inbox",
        "Jobs",
        "Templates",
        "Settings",
        "Devices",
    ] {
        assert!(
            order.iter().any(|stop| stop == destination),
            "{destination} is unreachable by Tab; the order was {order:?}"
        );
    }
    assert!(
        order.iter().any(|stop| stop == "Import an audio file"),
        "the library's own corrective control is unreachable: {order:?}"
    );
}
