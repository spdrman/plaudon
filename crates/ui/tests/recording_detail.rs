//! The recording detail container's two destructive-or-provenance surfaces,
//! driven through AccessKit by `egui_kittest`: the vendor exports recorded
//! alongside the audio (matrix row D17) and the deletion disclosure with its
//! confirmation (matrix row C6).
//!
//! Every assertion is made the way a user or a screen reader would make it:
//! find a control by the name it announces, act on it, then look at the
//! commands the screen asked the application layer for. No test reaches into
//! a private field to decide what happened, and the deletion assertions are
//! about the exact payload — a confirmation token the interface invented
//! instead of echoed would pass a test that only counted commands.

use egui_kittest::kittest::Queryable as _;
use egui_kittest::Harness;
use plaudon_app::command::{AppCommand, AppResponse, CommandId};
use plaudon_app::dto::{
    DeleteRecordingRequest, DeleteRecordingResponse, DeletionCategoryDto, DeletionDisclosureDto,
    DeletionDisclosureRequest, DeletionMediaDto, ListSourceArtifactsRequest,
    ListSourceArtifactsResponse, RecordingDetail, SourceArtifactSummary,
};
use plaudon_app_core::deletion::{DELETION_LIMITS, DISCLOSURE_VERSION};
use plaudon_domain::error::{AppError, AppErrorCode, DiagnosticLog, ErrorEnvelope};
use plaudon_domain::ids::RecordingId;
use plaudon_domain::time::Timestamp;
use plaudon_ui::ai::AiOutput;
use plaudon_ui::screens::library::panels::UnbuiltPanels;
use plaudon_ui::screens::library::testing::{detail as detail_of, row_with};
use plaudon_ui::screens::recording::{
    artifacts, deletion, DetailContext, DetailIntent, RecordingDetailScreen,
};
use plaudon_ui_testkit::a11y;

/// The digest the disclosure carries. Sixty-four lowercase hex characters,
/// because that is what the command layer accepts and what the service
/// recomputes; the interface must send it back unchanged.
const TOKEN: &str = "9c56b1f09c56b1f09c56b1f09c56b1f09c56b1f09c56b1f09c56b1f09c56b1f0";

const DIGEST: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

/* --------------------------------------------------------------- harness */

/// The detail container plus the dispatcher it would talk to, recorded
/// instead of run.
struct DetailApp {
    screen: RecordingDetailScreen,
    panels: UnbuiltPanels,
    sent: Vec<AppCommand>,
    next_id: u64,
    intents: Vec<DetailIntent>,
}

impl DetailApp {
    fn new() -> Self {
        Self {
            screen: RecordingDetailScreen::new(),
            panels: UnbuiltPanels,
            sent: Vec::new(),
            next_id: 0,
            intents: Vec::new(),
        }
    }

    fn view(&mut self, ui: &mut egui::Ui) {
        let Self {
            screen,
            panels,
            sent,
            next_id,
            intents,
        } = self;
        let mut send = |command: AppCommand| {
            *next_id += 1;
            sent.push(command);
            CommandId::from_raw(*next_id)
        };
        // The host re-opens the route's recording every frame; the container
        // is the thing that decides whether that means a fetch.
        screen.open(recording_id(), &mut send);
        let mut out = AiOutput::default();
        let produced = screen.ui(
            ui,
            panels,
            DetailContext { local_only: true },
            &mut send,
            &mut out,
        );
        intents.extend(produced);
    }

    fn respond(&mut self, id: u64, result: Result<AppResponse, ErrorEnvelope>) {
        self.screen.on_response(CommandId::from_raw(id), &result);
    }

    fn command_names(&self) -> Vec<&'static str> {
        self.sent.iter().map(AppCommand::name).collect()
    }
}

fn fixture() -> RecordingDetail {
    detail_of(row_with("Board meeting", |row| {
        row.recorded_at = Some("2026-03-12T14:03:22Z".to_owned());
        row.duration_ms = Some(61_000);
    }))
}

fn recording_id() -> RecordingId {
    fixture().summary.recording_id
}

fn harness() -> Harness<'static, DetailApp> {
    harness_sized(1_280.0)
}

/// A harness `width` points wide, so both sides of the 900 pt breakpoint are
/// reachable from a test.
fn harness_sized(width: f32) -> Harness<'static, DetailApp> {
    let mut harness = plaudon_ui_testkit::builder()
        .with_size(egui::vec2(width, 820.0))
        .build_ui_state(|ui, app: &mut DetailApp| app.view(ui), DetailApp::new());
    harness.run();
    harness
}

/// A harness whose recording has arrived. Command 1 is the export list and
/// command 2 is the recording itself, in the order the container raises them.
fn harness_loaded(artifacts: Vec<SourceArtifactSummary>) -> Harness<'static, DetailApp> {
    let mut harness = harness();
    harness
        .state_mut()
        .respond(2, Ok(AppResponse::GetRecording(fixture())));
    harness.state_mut().respond(
        1,
        Ok(AppResponse::ListSourceArtifacts(
            ListSourceArtifactsResponse { artifacts },
        )),
    );
    harness.run();
    harness
}

/// Everything the current frame announces, label or text.
fn announced(harness: &Harness<'_, DetailApp>) -> Vec<String> {
    a11y::nodes(harness)
        .into_iter()
        .filter_map(|node| node.announced().map(str::to_owned))
        .collect()
}

fn says(harness: &Harness<'_, DetailApp>, text: &str) -> bool {
    announced(harness).iter().any(|line| line == text)
}

/// Operate a control the way an assistive technology does: an AccessKit
/// `Click` action aimed at that node.
///
/// The disclosure body scrolls, so a pointer at a widget's layout position
/// would sometimes land on the backdrop instead; the action request cannot
/// miss. This is the same convention `settings_backup.rs` settled on for the
/// same reason, and egui still honours it only for an enabled widget.
fn click(harness: &mut Harness<'_, DetailApp>, label: &str) {
    harness.get_by_label(label).click_accesskit();
    harness.run();
}

fn envelope(code: AppErrorCode) -> ErrorEnvelope {
    let mut log = DiagnosticLog::new();
    AppError::new(code, "internal detail").to_envelope(&mut log, Timestamp::now())
}

/* ------------------------------------------------------------- fixtures */

fn artifact(kind: &str, imported_at: Option<&str>) -> SourceArtifactSummary {
    SourceArtifactSummary {
        artifact_id: format!("artifact-{kind}"),
        kind: kind.to_owned(),
        source_vendor: "plaud".to_owned(),
        content_sha256: DIGEST.to_owned(),
        byte_length: 2_048,
        original_filename: Some("meeting-notes.txt".to_owned()),
        imported_at: imported_at.map(str::to_owned),
        import_mode: "observe_only".to_owned(),
        recorded_at: "2026-03-12T14:03:22Z".to_owned(),
    }
}

fn disclosure() -> DeletionDisclosureDto {
    DeletionDisclosureDto {
        disclosure_version: DISCLOSURE_VERSION.to_owned(),
        recording_id: recording_id().to_string(),
        title: "Board meeting".to_owned(),
        has_annotation: true,
        media: vec![
            DeletionMediaDto {
                sha256: DIGEST.to_owned(),
                byte_length: 5_242_880,
                quality_label: "Original recording".to_owned(),
                shared_with_other_recordings: false,
                bytes_will_be_removed: true,
            },
            DeletionMediaDto {
                sha256: "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210"
                    .to_owned(),
                byte_length: 1_048_576,
                quality_label: "Compressed copy".to_owned(),
                shared_with_other_recordings: true,
                bytes_will_be_removed: false,
            },
        ],
        categories: vec![
            DeletionCategoryDto {
                label: "Transcript revisions".to_owned(),
                count: 3,
            },
            DeletionCategoryDto {
                label: "Summaries".to_owned(),
                count: 1,
            },
        ],
        bytes_to_free: 5_242_880,
        limits: DELETION_LIMITS
            .iter()
            .map(|line| (*line).to_owned())
            .collect(),
        confirmation_token: TOKEN.to_owned(),
    }
}

fn deleted() -> DeleteRecordingResponse {
    DeleteRecordingResponse {
        deletion_id: "01932f00-0000-7000-8000-000000000000".to_owned(),
        blobs_removed: 1,
        blobs_retained_shared: 1,
        bytes_freed: 5_242_880,
        tombstoned: true,
    }
}

/// Open the disclosure and answer it, leaving the modal on screen.
fn open_disclosure(harness: &mut Harness<'_, DetailApp>) {
    click(harness, deletion::OPEN_LABEL);
    let id = harness.state().sent.len() as u64;
    harness
        .state_mut()
        .respond(id, Ok(AppResponse::GetDeletionDisclosure(disclosure())));
    harness.run();
}

/* -------------------------------------------- D17: the vendor exports */

#[test]
fn opening_a_recording_asks_for_its_vendor_exports() {
    let harness = harness();
    let request = harness
        .state()
        .sent
        .iter()
        .find_map(|command| match command {
            AppCommand::ListSourceArtifacts(request) => Some(request),
            _ => None,
        })
        .expect("the detail view asked for the vendor exports");
    assert_eq!(
        request,
        &ListSourceArtifactsRequest {
            recording_id: recording_id().to_string(),
        }
    );
}

#[test]
fn an_export_renders_every_field_of_its_provenance() {
    let harness = harness_loaded(vec![artifact("transcript", Some("2026-03-12T15:30:00Z"))]);
    for expected in [
        "Kind: transcript",
        "Vendor: plaud",
        "Original file: meeting-notes.txt",
        "Size: 2.0 kB",
        "Content hash (SHA-256): 0123456789ab…",
        "Import mode: observe_only",
        "Recorded: 12 Mar 2026, 14:03 UTC",
        "Imported: 12 Mar 2026, 15:30 UTC",
    ] {
        assert!(
            says(&harness, expected),
            "the export list never announced {expected:?}; it announced {:#?}",
            announced(&harness)
        );
    }
}

#[test]
fn the_hash_is_a_fingerprint_and_never_the_whole_digest() {
    let harness = harness_loaded(vec![artifact("summary", None)]);
    assert!(
        !announced(&harness).iter().any(|line| line.contains(DIGEST)),
        "the full digest is a value to retype, not a fingerprint to compare"
    );
}

#[test]
fn an_export_that_was_only_recorded_says_so_instead_of_showing_a_date() {
    let harness = harness_loaded(vec![artifact("tags", None)]);
    assert!(
        says(&harness, artifacts::NOT_IMPORTED),
        "a null `imported_at` must be stated, not left blank: {:#?}",
        announced(&harness)
    );
    assert!(
        !announced(&harness)
            .iter()
            .any(|line| line.starts_with("Imported: ")),
        "no import date may be invented for an export that was never imported"
    );
}

#[test]
fn several_exports_announce_their_count_and_their_order() {
    let harness = harness_loaded(vec![
        artifact("transcript", Some("2026-03-12T15:30:00Z")),
        artifact("summary", None),
    ]);
    assert!(
        says(&harness, "2 vendor exports, oldest first."),
        "{:#?}",
        announced(&harness)
    );
}

#[test]
fn a_recording_with_no_exports_gets_an_empty_state_rather_than_a_blank_area() {
    let harness = harness_loaded(Vec::new());
    assert!(says(&harness, artifacts::NO_ARTIFACTS));
    assert!(says(&harness, artifacts::NO_ARTIFACTS_DETAIL));
}

#[test]
fn the_exports_and_the_delete_control_survive_the_stacked_layout() {
    // 800 pt is below the 900 pt breakpoint, so the side column is folded
    // into the stack; neither of this screen's two additions may fall out
    // of the narrow arrangement.
    let mut harness = harness_sized(800.0);
    harness
        .state_mut()
        .respond(2, Ok(AppResponse::GetRecording(fixture())));
    harness.state_mut().respond(
        1,
        Ok(AppResponse::ListSourceArtifacts(
            ListSourceArtifactsResponse {
                artifacts: vec![artifact("transcript", None)],
            },
        )),
    );
    harness.run();

    assert!(says(&harness, "Source artifacts"));
    assert!(says(&harness, artifacts::NOT_IMPORTED));
    assert!(says(&harness, deletion::OPEN_LABEL));
}

#[test]
fn a_failed_export_list_offers_the_standard_error_affordance() {
    let mut harness = harness();
    harness
        .state_mut()
        .respond(2, Ok(AppResponse::GetRecording(fixture())));
    harness
        .state_mut()
        .respond(1, Err(envelope(AppErrorCode::StorageUnavailable)));
    harness.run();

    let lines = announced(&harness);
    assert!(
        lines
            .iter()
            .any(|line| line.contains("Vendor exports could not be loaded.")),
        "{lines:#?}"
    );
    assert!(
        !lines.iter().any(|line| line.contains("internal detail")),
        "an envelope's internal detail never reaches the view: {lines:#?}"
    );
}

/* ---------------------------------------------------- C6: the deletion */

#[test]
fn the_delete_control_asks_for_the_disclosure_and_deletes_nothing_yet() {
    let mut harness = harness_loaded(Vec::new());
    click(&mut harness, deletion::OPEN_LABEL);

    let names = harness.state().command_names();
    assert!(
        names.contains(&"get_deletion_disclosure"),
        "the control must disclose before it deletes: {names:?}"
    );
    assert!(
        !names.contains(&"delete_recording"),
        "pressing the trigger must not delete anything: {names:?}"
    );
    let request = harness
        .state()
        .sent
        .iter()
        .find_map(|command| match command {
            AppCommand::GetDeletionDisclosure(request) => Some(request),
            _ => None,
        })
        .expect("the disclosure request");
    assert_eq!(
        request,
        &DeletionDisclosureRequest {
            recording_id: recording_id().to_string(),
        }
    );
}

#[test]
fn the_disclosure_states_every_limit_sentence_verbatim() {
    let mut harness = harness_loaded(Vec::new());
    open_disclosure(&mut harness);

    let lines = announced(&harness);
    assert_eq!(DELETION_LIMITS.len(), 5, "the contract has five sentences");
    for limit in DELETION_LIMITS {
        assert!(
            lines.iter().any(|line| line == limit),
            "the disclosure did not state, verbatim: {limit:?}\nit announced {lines:#?}"
        );
    }
}

#[test]
fn the_disclosure_states_what_goes_what_stays_and_what_it_frees() {
    let mut harness = harness_loaded(Vec::new());
    open_disclosure(&mut harness);

    for expected in [
        "Transcript revisions: 3",
        "Summaries: 1",
        "Original recording (5.0 MB, 0123456789ab…) — removed from disk",
        "Compressed copy (1.0 MB, fedcba987654…) — kept — another recording uses the same audio",
        "Storage freed: 5.0 MB",
        deletion::ANNOTATION_WARNING,
    ] {
        assert!(
            says(&harness, expected),
            "the disclosure never stated {expected:?}; it stated {:#?}",
            announced(&harness)
        );
    }
}

#[test]
fn confirming_sends_the_token_the_disclosure_returned_and_nothing_else() {
    let mut harness = harness_loaded(Vec::new());
    open_disclosure(&mut harness);

    click(&mut harness, deletion::CONFIRM_LABEL);

    let request = harness
        .state()
        .sent
        .iter()
        .find_map(|command| match command {
            AppCommand::DeleteRecording(request) => Some(request),
            _ => None,
        })
        .expect("the deletion was dispatched");
    assert_eq!(
        request,
        &DeleteRecordingRequest {
            recording_id: recording_id().to_string(),
            confirmation_token: TOKEN.to_owned(),
        },
        "the token must be the one the disclosure carried, character for character"
    );
    assert_eq!(
        harness.state().command_names(),
        vec![
            "list_source_artifacts",
            "get_recording",
            "get_deletion_disclosure",
            "delete_recording",
        ],
        "exactly one disclosure and one deletion"
    );
}

#[test]
fn cancelling_the_disclosure_dispatches_no_deletion() {
    let mut harness = harness_loaded(Vec::new());
    open_disclosure(&mut harness);

    click(&mut harness, deletion::CANCEL_LABEL);

    assert!(
        !harness
            .state()
            .command_names()
            .contains(&"delete_recording"),
        "backing out must delete nothing"
    );
    assert!(
        !says(&harness, deletion::CONFIRM_LABEL),
        "the disclosure is gone, so its confirm control is gone with it"
    );
}

#[test]
fn escape_dismisses_the_disclosure_without_deleting() {
    let mut harness = harness_loaded(Vec::new());
    open_disclosure(&mut harness);
    assert!(says(&harness, deletion::CONFIRM_LABEL));

    harness.key_press(egui::Key::Escape);
    harness.run();
    harness.run();

    assert!(
        !says(&harness, deletion::CONFIRM_LABEL),
        "Escape must dismiss the overlay: {:#?}",
        announced(&harness)
    );
    assert!(!harness
        .state()
        .command_names()
        .contains(&"delete_recording"));
}

#[test]
fn the_destructive_confirmation_is_reachable_from_the_keyboard() {
    let mut harness = harness_loaded(Vec::new());
    open_disclosure(&mut harness);
    let stops = a11y::focus_order(&mut harness, 16);
    assert!(
        stops.iter().any(|stop| stop == deletion::CONFIRM_LABEL),
        "Tab never reached the confirmation: {stops:?}"
    );
    assert!(
        stops.iter().any(|stop| stop == deletion::CANCEL_LABEL),
        "Tab never reached the way out: {stops:?}"
    );
}

#[test]
fn a_refused_confirmation_keeps_the_user_here_and_explains_why() {
    let mut harness = harness_loaded(Vec::new());
    open_disclosure(&mut harness);
    click(&mut harness, deletion::CONFIRM_LABEL);

    let id = harness.state().sent.len() as u64;
    harness
        .state_mut()
        .respond(id, Err(envelope(AppErrorCode::ConfirmationRequired)));
    harness.run();

    assert!(
        says(&harness, deletion::MISMATCH_EXPLANATION),
        "a refused confirmation must say what happened: {:#?}",
        announced(&harness)
    );
    assert!(
        !harness
            .state()
            .intents
            .iter()
            .any(|intent| matches!(intent, DetailIntent::Deleted { .. })),
        "a refusal must not navigate away"
    );
    assert!(
        !says(&harness, deletion::CONFIRM_LABEL),
        "a token known to be stale must not be offered for a blind retry"
    );

    click(&mut harness, deletion::REFETCH_LABEL);
    assert_eq!(
        harness
            .state()
            .command_names()
            .iter()
            .filter(|name| **name == "get_deletion_disclosure")
            .count(),
        2,
        "the refusal is answered by fetching the disclosure again"
    );
}

#[test]
fn a_successful_deletion_reports_what_happened_and_leaves_the_detail() {
    let mut harness = harness_loaded(Vec::new());
    open_disclosure(&mut harness);
    click(&mut harness, deletion::CONFIRM_LABEL);

    let id = harness.state().sent.len() as u64;
    harness
        .state_mut()
        .respond(id, Ok(AppResponse::DeleteRecording(deleted())));
    harness.run();

    let summary = harness
        .state()
        .intents
        .iter()
        .find_map(|intent| match intent {
            DetailIntent::Deleted { summary } => Some(summary.clone()),
            _ => None,
        })
        .expect("the detail view reported the deletion and asked to leave");
    assert_eq!(summary, deletion::deletion_summary(&deleted()));
    for fact in [
        "1 audio file removed",
        "1 kept because another recording shares it",
        "5.0 MB freed",
        "a tombstone was recorded",
    ] {
        assert!(summary.contains(fact), "{summary}");
    }
    assert!(
        !says(&harness, deletion::CONFIRM_LABEL),
        "the disclosure closes with the recording"
    );
}

#[test]
fn the_disclosure_names_blobs_by_digest_and_never_by_location() {
    let mut harness = harness_loaded(vec![artifact("transcript", Some("2026-03-12T15:30:00Z"))]);
    open_disclosure(&mut harness);
    // The disclosure DTO carries digests, quality labels and counts; a line
    // that looked like a location would mean the view had composed one.
    for line in announced(&harness) {
        assert!(
            !line.contains(std::path::MAIN_SEPARATOR) && !line.contains(".plaudon"),
            "a location-shaped string reached the interface: {line:?}"
        );
    }
}
