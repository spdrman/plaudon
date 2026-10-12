//! The demo recording: one scripted run of the whole application, rendered
//! frame by frame into PNGs that `scripts/make-demo-gif.sh` turns into
//! `docs/assets/demo.gif`.
//!
//! It is the same harness every other suite in this crate uses — the real
//! [`plaudon_app::runtime::AppEngine`] over a temporary library, the same
//! [`plaudon_egui::screens::register_all`] the binary calls — with fakes only
//! at the edges the operating system owns: the secret store, the notifier, the
//! audio device, the BLE radio and the file dialogs. Nothing here opens a
//! socket, a keychain, a window or a sound card, and every recording, every
//! transcript and every device in the picture is a fixture this file wrote.
//!
//! Two things make it a *recording* rather than a test:
//!
//! * after each step it calls `Harness::render`, which rasterises the frame
//!   through the software wgpu adapter, and writes the PNG;
//! * it writes an `ffconcat` manifest beside the frames giving each one a
//!   hold, so the assembler does not have to know the story.
//!
//! It is `#[ignore]`d: it is slow, it writes files, and a developer running
//! `cargo test` wants neither. Run it through the script:
//!
//! ```text
//! scripts/make-demo-gif.sh
//! ```
//!
//! The assertions are real ones. A step that cannot be driven fails the run
//! rather than quietly recording a frame of the wrong screen, because a demo
//! of a screen that did not do what the caption says is worse than no demo.

mod support;

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use plaudon_app::backend::devices::{
    DeviceRadio, RadioAvailability, RadioProbe, TransferReporter, TransferRequest,
};
use plaudon_app::command::{AppCommand, AppResponse};
use plaudon_app::dto::{
    JobState, JobSummary, ListJobsRequest, ListRecordingsRequest, ModelPreferenceScope,
    RecordingFilter, RecordingSort, RecordingSortField, RecordingSummary,
    SetModelPreferenceRequest, SortDirection, StartProcessingRequest,
};
use plaudon_app::ports::{MachineRevisionSpec, MachineSegment};
use plaudon_app::runtime::AppEngine;
use plaudon_app::shell_state::ShellStateStore;
use plaudon_app::testing_engines::{ScriptedStt, ScriptedTextModel, SCRIPTED_STT_ENGINE};
use plaudon_audio_playback::engine::AudioOut;
use plaudon_audio_playback::time::Micros;
use plaudon_audio_playback::{FakeAudioOut, FakeDevice};
use plaudon_domain::error::{AppError, AppErrorCode};
use plaudon_domain::ids::RecordingId;
use plaudon_domain::transcript::AlignmentOrigin;
use plaudon_egui::shell_app::PlaudonShell;
use plaudon_plaud_ble::discovery::ObservedAdvertisement;
use plaudon_plaud_protocol::payload::DeltaListing;
use plaudon_security::secrets::SecretString;
use plaudon_ui::screens::recording::AudioOutFactory;
use plaudon_ui::shell::Route;
use plaudon_ui_testkit::kittest::Queryable as _;
use plaudon_ui_testkit::Harness;

/// A harness over the whole application.
type App = Harness<'static, PlaudonShell>;

/// Sample rate of every fixture, matching the fake device's.
const RATE: u32 = support::FAKE_RATE;

/// The window the demo is recorded in.
///
/// 1280 points wide is [`plaudon_ui_testkit::DEFAULT_SIZE`], the width every
/// other suite renders at. The extra height is not cosmetic: the recording
/// detail is a fixed pane with no scroll area of its own, so at 820 points
/// the transcript editor is laid out below the fold and its virtualized rows
/// are never created — the same reason `e2e_flows` opens a taller window for
/// the editor. A demo that showed an empty editor would be showing a layout
/// artefact rather than the product.
const WINDOW: egui::Vec2 = egui::vec2(1280.0, 1000.0);

/// The recording the story opens, and the one the search finds.
const SUBJECT: &str = "weekly-team-sync";

/// Length of the subject recording, in seconds.
const SUBJECT_SECONDS: u32 = 10;

/// The library toolbar's own search field.
const SEARCH_FIELD: &str = "Search recordings";

/// The fixture library: twelve recordings, with the seconds of tone each one
/// holds. Two of them match the search the demo types.
const LIBRARY: [(&str, u32); 12] = [
    (SUBJECT, SUBJECT_SECONDS),
    ("architecture-sync", 7),
    ("product-standup", 3),
    ("board-meeting", 6),
    ("design-review", 5),
    ("customer-interview-acme", 8),
    ("release-retro", 4),
    ("budget-planning", 6),
    ("hiring-debrief", 3),
    ("roadmap-workshop", 9),
    ("support-triage", 2),
    ("incident-postmortem", 5),
];

/// The transcript the subject recording carries, with the times a speech
/// engine that aligns its output would have reported.
///
/// The first segment starts at 1.5 s rather than at 0, so the seek a source
/// anchor performs lands somewhere only that anchor could have chosen.
const TRANSCRIPT: [(&str, u64, u64); 3] = [
    ("We agreed to ship the beta on Friday.", 1_500, 4_000),
    ("Dana owns the release notes.", 4_200, 7_000),
    ("Risk: the installer is still unsigned.", 7_200, 9_800),
];

/* ------------------------------------------------------------- recorder */

/// Where the frames and their manifest are written.
///
/// `target/` by default, which the remote runner prunes from its artifact
/// sweep, so a capture never lands in the working tree by accident.
fn frames_dir() -> PathBuf {
    std::env::var_os("PLAUDON_DEMO_FRAMES").map_or_else(
        || Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/demo-frames"),
        PathBuf::from,
    )
}

/// The frames of the demo, and how long each one is held.
struct Recorder {
    dir: PathBuf,
    index: usize,
    manifest: String,
    last: String,
}

impl Recorder {
    /// An empty recorder over a freshly created directory.
    fn new() -> Self {
        let dir = frames_dir();
        if dir.exists() {
            std::fs::remove_dir_all(&dir).expect("the previous capture is removed");
        }
        std::fs::create_dir_all(&dir).expect("the frame directory is created");
        Self {
            dir,
            index: 0,
            manifest: "ffconcat version 1.0\n".to_owned(),
            last: String::new(),
        }
    }

    /// Render the current frame and hold it for `hold_ms`.
    fn shot(&mut self, harness: &mut App, hold_ms: u32) {
        let frame = harness.render().expect("the frame renders");
        let name = format!("frame-{:04}.png", self.index);
        frame
            .save(self.dir.join(&name))
            .expect("the frame is written");
        write!(
            self.manifest,
            "file {name}\nduration {:.3}\n",
            f64::from(hold_ms) / 1000.0
        )
        .expect("writing to a String never fails");
        self.last = name;
        self.index += 1;
    }

    /// Step `steps` frames of the application, then record one, held for
    /// `hold_ms`.
    fn step_and_shot(&mut self, harness: &mut App, steps: usize, hold_ms: u32) {
        harness.run_steps(steps);
        self.shot(harness, hold_ms);
    }

    /// Write the manifest.
    ///
    /// The last file is repeated without a duration: the concat demuxer
    /// needs it to flush the final entry, and ffmpeg ignores the repeat.
    fn finish(mut self) -> (usize, f64) {
        assert!(self.index > 0, "the demo recorded no frame at all");
        writeln!(self.manifest, "file {}", self.last).expect("writing to a String never fails");
        let path = self.dir.join("frames.txt");
        let mut file = std::fs::File::create(&path).expect("the manifest is created");
        file.write_all(self.manifest.as_bytes())
            .expect("the manifest is written");
        let seconds: f64 = self
            .manifest
            .lines()
            .filter_map(|line| line.strip_prefix("duration "))
            .filter_map(|value| value.parse::<f64>().ok())
            .sum();
        (self.index, seconds)
    }
}

/* ------------------------------------------------------------- fixtures */

/// Write `seconds` of a quiet two-tone signal as 16-bit mono PCM in a WAV
/// container.
///
/// Synthesised rather than committed, so the duration, the content hash and
/// the decoded samples are all known to the run that wrote them. The two
/// tones and the slow envelope exist so the rendered waveform has a shape
/// instead of a solid block.
///
/// `voice` shifts the pitch per fixture. It is not decoration: the archive is
/// content-addressed and deduplicates on the hash, so two fixtures of the
/// same length would otherwise be one recording.
fn write_wav(path: &Path, seconds: u32, voice: usize) {
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
        let time = f64::from(frame) / f64::from(RATE);
        let pitch = 150.0 + 11.0 * voice as f64;
        let voice = (time * pitch * std::f64::consts::TAU).sin()
            + 0.4 * (time * (pitch * 2.4) * std::f64::consts::TAU).sin();
        // Syllable-rate envelope: the waveform then reads as speech-shaped
        // rather than as one unbroken bar.
        let envelope = 0.25 + 0.75 * (time * 3.1 * std::f64::consts::TAU).sin().abs();
        let sample = (voice * envelope * 0.45 * f64::from(i16::MAX)) as i16;
        file.write_all(&sample.to_le_bytes()).expect("write");
    }
    file.flush().expect("flush");
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

/// Whether `job_id` reached a terminal state.
fn job_is_done(app: &PlaudonShell, job_id: &str) -> bool {
    jobs(app)
        .into_iter()
        .find(|job| job.job_id.to_string() == job_id)
        .is_some_and(|job| job.state.is_terminal())
}

/// Give `recording_id` an aligned machine transcript, as an engine that
/// reports times does.
///
/// [`plaudon_app::ports::TranscriptPort::create_machine_revision`] is the
/// entry point a speech engine's executor calls when it finishes, so the
/// fixture goes in the same way a real run's would. The times are what make
/// a source anchor seekable: without an alignment origin the review panel
/// states that a passage has no playback position rather than inventing one.
fn give_aligned_transcript(engine: &AppEngine, recording_id: RecordingId) {
    let spec = MachineRevisionSpec {
        recording_id,
        segments: TRANSCRIPT
            .iter()
            .map(|(text, start, end)| MachineSegment::timed(*text, *start, *end))
            .collect(),
        engine_id: "demo-fixture-stt".to_owned(),
        model_id: "demo-fixture".to_owned(),
        alignment_origin: AlignmentOrigin::Native,
    };
    engine
        .handle()
        .block_on(engine.ports().transcript.create_machine_revision(spec))
        .expect("the machine revision is recorded");
}

/// Point one stage at an engine, the way the model picker does.
fn prefer_engine(app: &PlaudonShell, stage: &str, engine: &str) {
    let answer = app
        .engine()
        .dispatcher()
        .call_blocking(AppCommand::SetModelPreference(SetModelPreferenceRequest {
            stage: stage.to_owned(),
            scope: ModelPreferenceScope {
                kind: "default".to_owned(),
                recording_id: None,
            },
            engine: engine.to_owned(),
            model_id: Some("scripted-small".to_owned()),
        }));
    assert!(
        matches!(answer, Ok(AppResponse::SetModelPreference(_))),
        "set_model_preference answered {answer:?}"
    );
}

/// A fake output device that can be opened more than once, and the handle on
/// whichever one is open now.
///
/// `support::fake_audio_out` opens exactly one device and refuses the second
/// request, which is the right shape for a test that opens one recording. The
/// demo leaves the recording and comes back, so the player is built twice;
/// this factory answers every time and publishes the device it just made.
fn reopenable_audio_out() -> (AudioOutFactory, Arc<Mutex<Option<FakeDevice>>>) {
    let latest: Arc<Mutex<Option<FakeDevice>>> = Arc::new(Mutex::new(None));
    let sink = Arc::clone(&latest);
    let factory: AudioOutFactory = Arc::new(move || {
        let out = FakeAudioOut::new(support::FAKE_RATE, 1);
        *sink.lock().expect("the device slot") = Some(out.device());
        Ok(Box::new(out) as Box<dyn AudioOut>)
    });
    (factory, latest)
}

/// The device the player is rendering through right now.
fn device(latest: &Arc<Mutex<Option<FakeDevice>>>) -> FakeDevice {
    latest
        .lock()
        .expect("the device slot")
        .clone()
        .expect("the player opened an output device")
}

/// Ask for one stage, the way the stage panel's run control does.
fn start_stage(app: &PlaudonShell, recording_id: RecordingId, stage: &str) -> String {
    let answer = app
        .engine()
        .dispatcher()
        .call_blocking(AppCommand::StartProcessing(StartProcessingRequest {
            recording_id: recording_id.to_string(),
            stage: stage.to_owned(),
            model_override: None,
            cost_acknowledged: false,
            rerun: false,
        }));
    match answer {
        Ok(AppResponse::StartProcessing(response)) => response.job_id,
        other => panic!("start_processing answered {other:?}"),
    }
}

/// Operate the control named `label` the way assistive technology does.
///
/// The recording detail's columns are fixed panes that clip rather than
/// scroll, so a control low in one of them is in the accessibility tree but
/// outside the clip rectangle, where a synthetic pointer cannot reach it.
fn activate(harness: &mut App, label: &str) {
    harness.get_by_label(label).click_accesskit();
    harness.run_steps(2);
}

/* ----------------------------------------------------------- fake radio */

/// A scripted BLE radio, shaped like the one `e2e_flows` drives. It touches
/// no hardware: a scan answers with whatever this file queued.
#[derive(Debug)]
struct ScriptedRadio {
    adverts: Vec<ObservedAdvertisement>,
}

impl ScriptedRadio {
    /// A radio that hears one device advertising the vendor service.
    fn hearing(peripheral_id: &str, name: &str) -> Self {
        Self {
            adverts: vec![ObservedAdvertisement {
                peripheral_id: peripheral_id.to_owned(),
                local_name: Some(name.to_owned()),
                service_ids: vec![plaudon_plaud_protocol::gatt::SERVICE_VENDOR],
                manufacturer_ids: vec![0x0499],
                rssi: Some(-52),
            }],
        }
    }
}

#[async_trait::async_trait]
impl DeviceRadio for ScriptedRadio {
    fn availability(&self) -> RadioAvailability {
        RadioAvailability::Present
    }

    async fn scan(&self, _timeout: Duration) -> Result<Vec<ObservedAdvertisement>, AppError> {
        Ok(self.adverts.clone())
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

/* ---------------------------------------------------------------- story */

#[test]
#[ignore = "renders the demo frames; run it through scripts/make-demo-gif.sh"]
fn the_demo_records_the_whole_story() {
    let dir = tempfile::tempdir().expect("temporary library");
    let media: Vec<PathBuf> = LIBRARY
        .iter()
        .enumerate()
        .map(|(index, (name, seconds))| {
            let path = dir.path().join(format!("{name}.wav"));
            write_wav(&path, *seconds, index);
            path
        })
        .collect();

    let store = ShellStateStore::new(dir.path());
    let config = support::config(dir.path())
        .with_radio(Arc::new(ScriptedRadio::hearing(
            "AABBCCDD-0011-2233-4455-667788990011",
            "PLAUD_NOTE",
        )))
        .with_stt_engines(vec![ScriptedStt::new(&[TRANSCRIPT[0].0]).shared()])
        .with_text_model(ScriptedTextModel::new(serde_json::Value::Null).shared());
    let engine = AppEngine::start(config).expect("the engine starts over the library");
    // Local-only Mode from the first run: it is the mode the demo shows, and
    // the reason no cloud route can be taken by accident while recording.
    support::complete_onboarding(&engine, true);

    let (audio, playing_on) = reopenable_audio_out();
    let app = support::app_over(engine, support::hooks_picking(store, media), audio);
    let mut harness = plaudon_ui_testkit::builder()
        .with_size(WINDOW)
        .build_ui_state(|ui, app: &mut PlaudonShell| app.view(ui), app);
    support::settle(&mut harness);

    let mut recorder = Recorder::new();

    // ---------------------------------------------------- 1. the library
    //
    // The import is driven through the interface — the empty state's own
    // control, the host's file picker and the real `import_files` command —
    // so the library in the picture is one the application itself built.
    support::run_until_announced(&mut harness, "Import an audio file");
    harness.get_by_label("Import an audio file").click();
    support::run_until_frame(&mut harness, "the imported recordings", |harness| {
        recordings(harness.state()).len() == LIBRARY.len()
    });
    support::run_until_announced(&mut harness, &format!("{SUBJECT}. Recorded"));
    recorder.step_and_shot(&mut harness, 2, 2_200);

    // The import toasts are an overlay: left up, they would sit over every
    // later frame. Dismissing them is the control a user presses. The
    // sentence they carry stays in the activity list afterwards, which is
    // where the record belongs, so the loop ends when no toast is left
    // rather than when the words are gone.
    support::run_until_announced(&mut harness, "Imported 12 recording(s)");
    for _ in 0..8 {
        if harness
            .query_all_by_label("Dismiss")
            .next()
            .map(|toast| toast.click())
            .is_none()
        {
            break;
        }
        harness.run_steps(2);
    }

    // ------------------------------------------------------- 2. the search
    //
    // Typed into the toolbar's own field, debounced by the toolbar, and
    // answered by a fresh `list_recordings` with the query in its filter.
    // The role is part of the query because the command palette registers a
    // command of the same name, and a bare label matches both.
    harness
        .get_by_role_and_label(egui::accesskit::Role::TextInput, SEARCH_FIELD)
        .click();
    harness.run_steps(1);
    for chunk in ["sy", "nc"] {
        harness
            .get_by_role_and_label(egui::accesskit::Role::TextInput, SEARCH_FIELD)
            .type_text(chunk);
        recorder.step_and_shot(&mut harness, 2, 350);
    }
    support::run_until_frame(&mut harness, "the filtered library", |harness| {
        !support::announces(harness, "product-standup. Recorded")
            && support::announces(harness, "architecture-sync. Recorded")
    });
    recorder.step_and_shot(&mut harness, 1, 1_900);

    // ------------------------------------------- 3. open one recording
    let subject = recordings(harness.state())
        .into_iter()
        .find(|row| row.title.contains(SUBJECT))
        .expect("the subject recording was imported");
    give_aligned_transcript(harness.state().engine(), subject.recording_id);
    // Select the row the search found, then open it. The grid opens a row on
    // a double click, on Enter when the grid itself has the focus, and from
    // its context menu; a synthetic double click is not reproducible here and
    // the keyboard focus is still in the search field, so the demo takes the
    // route those three produce, exactly as `e2e_flows` does.
    harness
        .get_by_label_contains(&format!("{SUBJECT}. Recorded"))
        .click();
    harness.run_steps(2);
    harness
        .state_mut()
        .shell_mut()
        .navigate(Route::LibraryRecording {
            recording_id: subject.recording_id.to_string(),
        });
    support::run_until_announced(&mut harness, "Content hash (SHA-256)");
    support::run_until_frame(&mut harness, "the engine inventory", |harness| {
        !support::announces(harness, "The engine inventory has not been read yet.")
    });
    assert!(
        support::announces(&harness, "Local only"),
        "the privacy badge must state the enforced mode: {:?}",
        support::announced(&harness)
    );
    recorder.step_and_shot(&mut harness, 2, 2_000);

    // --------------------------------------------------- 4. play and seek
    //
    // `resolve_media` answered, the archived file opened through
    // `SymphoniaPcmSource`, and the source stated its own length.
    let clock = format!("0:00 / 0:{SUBJECT_SECONDS}");
    support::run_until_announced(&mut harness, &clock);
    harness.get_by_label("Play").click();
    harness.run_steps(2);
    assert!(
        support::announces(&harness, "Pause"),
        "the transport did not start: {:?}",
        support::announced(&harness)
    );
    // The device renders on demand, so each step is exactly the audio it
    // says it is and the clock cannot drift.
    for _ in 0..7 {
        device(&playing_on).render_micros(Micros::from_millis(1_000));
        recorder.step_and_shot(&mut harness, 2, 260);
    }
    assert!(
        support::announces(&harness, "0:07 / 0:10"),
        "the position did not follow the device clock: {:?}",
        support::announced(&harness)
    );

    // The scrubber is a slider whose label is the position; it is seekable
    // from the keyboard whether or not the peak worker has finished.
    harness.get_by_label_contains(" of 0:10").focus();
    harness.run_steps(2);
    harness.key_press(egui::Key::Home);
    support::run_until_announced(&mut harness, "0:00 / 0:10");
    recorder.step_and_shot(&mut harness, 1, 1_100);

    // --------------------------------------- 5. the summary, as a real job
    //
    // Asked for while the recording is open, because the panels learn that an
    // output exists from the `ToolCompleted` event, and a screen that is not
    // on display when it goes past does not re-read on the way back: the
    // binding keeps the recording it already has.
    prefer_engine(
        harness.state(),
        "structured_extraction",
        SCRIPTED_STT_ENGINE,
    );
    let job_id = start_stage(
        harness.state(),
        subject.recording_id,
        "structured_extraction",
    );
    support::run_until_frame(&mut harness, "the job to finish", |harness| {
        job_is_done(harness.state(), &job_id)
    });
    assert_eq!(
        jobs(harness.state())
            .into_iter()
            .find(|job| job.job_id.to_string() == job_id)
            .expect("the job exists")
            .state,
        JobState::Completed,
        "the summary job did not complete"
    );

    // The job list, where a finished job says what it produced and what to
    // open.
    harness.state_mut().shell_mut().navigate(Route::Jobs);
    support::run_until_announced(&mut harness, "Completed");
    assert!(
        support::announces(&harness, "Process (structured_extraction)"),
        "the job list does not name the stage that ran: {:?}",
        support::announced(&harness)
    );
    recorder.step_and_shot(&mut harness, 2, 1_600);

    // ------------------------------- 6. the grounded output and its anchor
    //
    // Back through the library, and in again. The outputs column reads its
    // recording when the detail is *opened*, and the detail is only closed
    // and reopened when the library screen itself is drawn with and without
    // a recording — a screen that was never on display while the job ran
    // learns nothing. Going back the way a user does is what re-reads it.
    harness.state_mut().shell_mut().navigate(Route::Library);
    support::run_until_announced(&mut harness, &format!("{SUBJECT}. Recorded"));
    harness.run_steps(2);
    harness
        .state_mut()
        .shell_mut()
        .navigate(Route::LibraryRecording {
            recording_id: subject.recording_id.to_string(),
        });
    support::run_until_announced(&mut harness, "Content hash (SHA-256)");
    support::run_until_announced(&mut harness, "Review");
    activate(&mut harness, "Review");
    support::run_until_announced(&mut harness, "Seek to this passage");
    assert!(
        support::announces(&harness, "Anchor resolved in the transcript"),
        "the claim is not grounded in the transcript: {:?}",
        support::announced(&harness)
    );
    assert!(
        support::announces(&harness, TRANSCRIPT[0].0),
        "the claim does not quote the transcript it was built from"
    );
    recorder.step_and_shot(&mut harness, 2, 2_400);

    // The anchor's own control, which seeks the player to the passage the
    // claim cites — 1.5 s, which is where that segment starts and nowhere a
    // reset could land. The transport is waited for first: the detail was
    // reopened, so the media is resolved again before a seek can land.
    support::run_until_announced(&mut harness, "0:00 / 0:10");
    activate(&mut harness, "Seek to this passage");
    harness.run_steps(2);
    support::run_until_announced(&mut harness, "0:01 / 0:10");
    recorder.step_and_shot(&mut harness, 1, 1_900);

    // ------------------------------------------- 7. edit the transcript
    support::run_until_announced(&mut harness, "Segment 1");
    let before = segment_text(&harness);
    harness.get_by_label_contains("Segment 1").click_accesskit();
    harness.run_steps(2);
    harness
        .get_by_label_contains("Segment 1")
        .type_text("Confirmed: ");
    recorder.step_and_shot(&mut harness, 2, 1_500);
    assert_ne!(segment_text(&harness), before, "typing changed nothing");

    harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    harness.run_steps(2);
    assert_eq!(
        segment_text(&harness),
        before,
        "undo did not restore the segment"
    );
    recorder.step_and_shot(&mut harness, 1, 1_500);

    // ------------------------------------------------------- 8. devices
    harness.state_mut().shell_mut().navigate(Route::Devices);
    support::run_until_announced(&mut harness, "Scan for nearby devices");
    recorder.step_and_shot(&mut harness, 2, 1_100);
    harness.get_by_label("Scan for nearby devices").click();
    support::run_until_announced(&mut harness, "PLAUD_NOTE");
    // Five sections, always all five, each with its own count: a section the
    // scan could not fill says so rather than reading as empty.
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
        support::announces(&harness, "Not evaluated"),
        "a freshly scanned device must carry its register status: {:?}",
        support::announced(&harness)
    );
    recorder.step_and_shot(&mut harness, 2, 2_100);

    // --------------------------------------- 9. settings and Local-only mode
    harness.state_mut().shell_mut().navigate(Route::Settings);
    support::run_until_announced(&mut harness, "Follow the system");
    let toggle = "Local-only mode (refuse all outbound network access)";
    support::run_until_announced(&mut harness, toggle);
    harness.get_by_label_contains(toggle).scroll_to_me();
    recorder.step_and_shot(&mut harness, 3, 2_300);

    // ------------------------------ 10. the command palette and the theme
    //
    // The appearance the window is painted with is shell state, not the
    // stored preference the settings radios write; the command that changes
    // it lives in the one command registry, and the palette is how a command
    // is run without a pointer.
    harness
        .get_by_role_and_label(egui::accesskit::Role::Button, "Command palette")
        .click();
    harness.run_steps(2);
    harness.event(egui::Event::Text("appearance dark".to_owned()));
    recorder.step_and_shot(&mut harness, 2, 1_700);
    harness.key_press(egui::Key::Enter);
    support::run_until_frame(&mut harness, "the dark theme", |harness| {
        harness.ctx.global_style().visuals.dark_mode
    });
    recorder.step_and_shot(&mut harness, 2, 1_500);

    harness.state_mut().shell_mut().navigate(Route::Library);
    support::run_until_announced(&mut harness, &format!("{SUBJECT}. Recorded"));
    recorder.step_and_shot(&mut harness, 2, 1_900);

    let (frames, seconds) = recorder.finish();
    println!("demo: {frames} frames, {seconds:.1} s of story");
}

/// The text the first transcript segment reports to a screen reader.
fn segment_text(harness: &App) -> String {
    harness
        .get_by_label_contains("Segment 1")
        .value()
        .expect("a segment reports its text")
}
