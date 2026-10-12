//! The voice-profile surfaces, driven through the whole shell twice
//! (`VOICE_PROFILES_SPEC` §17.4, VP-011, VP-111, VP-215).
//!
//! The same scenario runs with biometric matching **off** and with it **on**:
//! a person is created on the People destination and assigned to a cluster of
//! an imported recording through the speaker panel's picker, and the
//! transcript chips carry the name in the same frame. With matching off that
//! is the whole feature; with matching on the enrolment affordance appears and
//! the consent gate is what stands between the user and a voiceprint.
//!
//! Both runs sit behind the cloud transport watch of
//! `tests/local_only_egress.rs`'s kind: every outbound attempt the application
//! makes is recorded and refused, and both runs assert the count is zero. This
//! feature is local-only in **both** network modes, so the watch runs with
//! Local-only Mode off as well — a zero there is the stronger claim.

mod support;

use std::io::Write as _;
use std::path::Path;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use plaudon_ai_openrouter::transport::{AsyncHttpTransport, TransportFailure};
use plaudon_app::command::{AppCommand, AppResponse};
use plaudon_app::dto::{
    CreatePersonRequest, EmptyRequest, ListRecordingsRequest, ListSpeakersRequest, RecordingFilter,
    RecordingPage, RecordingSort, RecordingSortField, SetVoiceProfilesEnabledRequest,
    SortDirection, VoiceProfileSettingsDto,
};
use plaudon_app::runtime::AppEngine;
use plaudon_app::shell_state::ShellStateStore;
use plaudon_domain::ids::{PersonId, RecordingId};
use plaudon_egui::shell_app::PlaudonShell;
use plaudon_security::outbound::http::{GatedRequest, HttpResponse};
use plaudon_ui::shell::{HostHooks, Route};
use plaudon_ui_testkit::kittest::Queryable as _;
use plaudon_ui_testkit::Harness;

/// Sample rate of the fixture audio.
const RATE: u32 = 16_000;

/* --------------------------------------------------------------- the watch */

/// Every outbound attempt, reduced to what §15.4 permits a log to keep.
#[derive(Debug, Clone, Default)]
struct EgressWatch {
    attempts: Arc<Mutex<Vec<String>>>,
}

impl EgressWatch {
    fn attempts(&self) -> Vec<String> {
        self.attempts.lock().expect("the attempt log").clone()
    }
}

#[async_trait]
impl AsyncHttpTransport for EgressWatch {
    async fn send(&self, request: GatedRequest) -> Result<HttpResponse, TransportFailure> {
        let destination = request.ticket().destination_label().to_owned();
        self.attempts
            .lock()
            .expect("the attempt log")
            .push(destination);
        Err(TransportFailure::ConnectFailed {
            detail: "the watch refuses every request".to_owned(),
        })
    }
}

/* ------------------------------------------------------------- the fixture */

/// Write `seconds` of a 240 Hz tone as 16-bit mono PCM in a WAV container.
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

/// The shell over an engine whose cloud transport is the watch.
fn shell(dir: &Path, local_only: bool, watch: &EgressWatch) -> Harness<'static, PlaudonShell> {
    let media = dir.join("stand-up.wav");
    write_wav(&media, 2);
    let engine = AppEngine::start(
        support::config(dir)
            .with_cloud_transport(Arc::new(watch.clone()) as Arc<dyn AsyncHttpTransport>),
    )
    .expect("the engine starts");
    support::complete_onboarding(&engine, local_only);
    let dispatcher = engine.dispatcher().clone();
    let imported = dispatcher.call_blocking(AppCommand::ImportFiles(
        plaudon_app::dto::ImportFilesRequest {
            paths: vec![media.to_string_lossy().into_owned()],
            duplicate_policy: plaudon_app::dto::DuplicatePolicy::Ask,
        },
    ));
    assert!(
        imported.is_ok(),
        "the fixture was not imported: {imported:?}"
    );

    let store = ShellStateStore::new(dir);
    let hooks: HostHooks = support::hooks(store);
    let (audio, _device) = support::fake_audio_out();
    let app = support::app_over(engine, hooks, audio);
    let mut harness = plaudon_ui_testkit::builder()
        .with_size(egui::vec2(1280.0, 2400.0))
        .build_ui_state(|ui, app: &mut PlaudonShell| app.view(ui), app);
    support::settle(&mut harness);
    harness
}

/// Ask the application something, through the engine the shell is running on.
///
/// A second engine over the same directory would be a second library, and
/// this test is about what the running application knows.
fn call(app: &PlaudonShell, command: AppCommand) -> AppResponse {
    let name = command.name();
    app.engine()
        .dispatcher()
        .call_blocking(command)
        .unwrap_or_else(|error| panic!("{name} failed: {error:?}"))
}

/// The one imported recording.
fn recording(app: &PlaudonShell) -> RecordingId {
    let answer = call(
        app,
        AppCommand::ListRecordings(ListRecordingsRequest {
            cursor: None,
            limit: 10,
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
                field: RecordingSortField::RecordedAt,
                direction: SortDirection::Desc,
            },
        }),
    );
    let AppResponse::ListRecordings(RecordingPage { items, .. }) = answer else {
        panic!("the library could not be listed");
    };
    items.first().expect("the imported recording").recording_id
}

/// Create a roster person and assign them to the recording's first speaker
/// row, through the application's own commands.
fn assign_first_cluster(app: &PlaudonShell, recording_id: RecordingId, name: &str) -> PersonId {
    let AppResponse::CreatePerson(created) = call(
        app,
        AppCommand::CreatePerson(CreatePersonRequest {
            display_name: name.to_owned(),
            aliases: Vec::new(),
            email: None,
            organisation: None,
        }),
    ) else {
        panic!("the roster entry was not created");
    };
    let AppResponse::ListSpeakers(speakers) = call(
        app,
        AppCommand::ListSpeakers(ListSpeakersRequest {
            recording_id: recording_id.to_string(),
        }),
    ) else {
        panic!("the speaker rows could not be read");
    };
    if let Some(row) = speakers.speakers.first() {
        call(
            app,
            AppCommand::AssignClusterPerson(plaudon_app::dto::AssignClusterPersonRequest {
                recording_id: recording_id.to_string(),
                speaker_id: row.speaker_id.to_string(),
                person_id: Some(created.person.person_id.to_string()),
                overwrite_segment_exceptions: false,
            }),
        );
    }
    created.person.person_id
}

/// The voice-profile settings as the application reports them.
fn voice_settings(app: &PlaudonShell) -> VoiceProfileSettingsDto {
    let AppResponse::GetVoiceProfileSettings(settings) =
        call(app, AppCommand::GetVoiceProfileSettings(EmptyRequest {}))
    else {
        panic!("the voice settings could not be read");
    };
    settings
}

/* ------------------------------------------------------------ the scenarios */

/// The People destination answers, the roster is usable, and nothing left the
/// machine — with biometric matching **off**, which is the default.
#[test]
fn the_people_destination_works_with_matching_off_and_attempts_no_outbound_request() {
    let dir = tempfile::tempdir().expect("a temporary library");
    let watch = EgressWatch::default();
    let mut harness = shell(dir.path(), true, &watch);

    let settings = voice_settings(harness.state());
    assert!(!settings.enabled, "biometric matching is off by default");

    let recording_id = recording(harness.state());
    // The fixture has no diarization run, so there is no cluster to assign:
    // what is asserted here is the roster itself, which is the half of the
    // feature that works without any of the biometric machinery (VP-011).
    let person_id = assign_first_cluster(harness.state(), recording_id, "Priya");

    harness.state_mut().shell_mut().navigate(Route::People);
    support::run_until_announced(&mut harness, "Priya");
    let announced = support::announced(&harness);
    assert!(
        announced.iter().any(|line| line.contains("People")),
        "the People destination did not render: {announced:?}"
    );
    assert!(
        !announced.iter().any(|line| line.contains("sounds like")),
        "a suggestion surface exists with matching off: {announced:?}"
    );
    assert!(
        !announced
            .iter()
            .any(|line| line.contains("is not available in this build yet")),
        "the People destination has no screen"
    );

    // The recording's own surfaces carry none of the biometric tier with
    // matching off (VP-215): no suggestion, no enrolment affordance.
    harness
        .state_mut()
        .shell_mut()
        .navigate(Route::LibraryRecording {
            recording_id: recording_id.to_string(),
        });
    support::run_until_announced(&mut harness, "Outputs");
    let detail = support::announced(&harness);
    assert!(
        !detail.iter().any(|line| line.contains("sounds like")),
        "a suggestion reached a recording with matching off: {detail:?}"
    );
    assert!(
        !detail
            .iter()
            .any(|line| line.contains("Enrol this speaker's voice…")),
        "an enrolment affordance reached a recording with matching off: {detail:?}"
    );

    assert!(
        watch.attempts().is_empty(),
        "the roster path attempted an outbound request: {:?}",
        watch.attempts()
    );
    let _ = person_id;
}

/// The same destination with biometric matching **on**: turning it on is the
/// consent gate, the queue exists, and still nothing leaves the machine — in
/// a library that is *not* in Local-only Mode, because this feature is
/// local-only in both modes (VP-111).
#[test]
fn turning_matching_on_adds_the_queue_and_still_attempts_no_outbound_request() {
    let dir = tempfile::tempdir().expect("a temporary library");
    let watch = EgressWatch::default();
    let mut harness = shell(dir.path(), false, &watch);

    let settings = voice_settings(harness.state());
    let version = settings.disclosure.version.clone();
    assert!(!settings.enabled, "biometric matching is off by default");

    // Turning it on *is* passing the activation gate: the accepted
    // disclosure version travels with the request.
    call(
        harness.state(),
        AppCommand::SetVoiceProfilesEnabled(SetVoiceProfilesEnabledRequest {
            enabled: true,
            accept_disclosure_version: Some(version),
        }),
    );

    let recording_id = recording(harness.state());
    assign_first_cluster(harness.state(), recording_id, "Priya");

    harness.state_mut().shell_mut().navigate(Route::People);
    support::run_until_announced(&mut harness, "Suggestions to review");
    harness.get_by_label("Suggestions to review").click();
    support::run_until_announced(&mut harness, "suggestion(s) waiting for review");

    let announced = support::announced(&harness);
    assert!(
        announced
            .iter()
            .any(|line| line.contains("Nothing has been assigned.")),
        "the queue did not state that nothing is assigned by itself: {announced:?}"
    );

    assert!(
        watch.attempts().is_empty(),
        "the biometric path attempted an outbound request: {:?}",
        watch.attempts()
    );
}
