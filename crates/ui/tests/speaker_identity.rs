//! Speaker labelling end to end (parity row T12 of
//! `docs/EGUI_MIGRATION_PLAN.md` §4.1).
//!
//! Two levels, because T12 had two different holes.
//!
//! * The **speaker list** ([`OutputsViewer`]) is where the three labelling
//!   commands are produced. The guards are asserted here: an empty name is
//!   never sent, a speaker with no typed name cannot be confirmed, and the
//!   field stops at the stored maximum instead of letting the store refuse
//!   a whole rename.
//! * The **detail view** ([`LivePanels`]) is where the answers have to land.
//!   A rename answers with the stored row, and that row has to reach both
//!   panels that render speakers — the list and every transcript chip — in
//!   the same frame, without a refetch.
//!
//! Two honesty rules are asserted as strings, deliberately:
//!
//! * the qualifier is [`plaudon_domain::speaker::display`], the same function
//!   `plaudon-export` renders a bundle with, so a renamed-but-unconfirmed row
//!   reads `(inferred cluster)` on screen exactly as it does in an export;
//! * nothing on the wire persists a segment-to-speaker assignment
//!   (`SegmentEdit` is `{index, text}`), so the chip offers none.

use egui_kittest::kittest::{NodeT as _, Queryable as _};
use egui_kittest::Harness;
use plaudon_app::command::{AppCommand, AppResponse, CommandId};
use plaudon_app::dto::{
    Capabilities, ListSpeakersResponse, RecordingDetail, SpeakerDto, SpeakerOperationResponse,
    TranscriptPage, TranscriptSegmentDto,
};
use plaudon_domain::error::{AppErrorCode, ErrorEnvelope};
use plaudon_domain::ids::{RecordingId, SpeakerId};
use plaudon_domain::time::Timestamp;
use plaudon_domain::transcript::AlignmentOrigin;
use plaudon_ui::ai::{AiIntent, AiOutput, OutputsViewer, RecordingContext};
use plaudon_ui::screens::library::testing::{detail, row_at};
use plaudon_ui::screens::recording::{no_audio_output, DetailPanels, LivePanels};

mod ai_fixtures;

use ai_fixtures as fx;

/// Tall enough that no control of the panel is clipped; the shell scrolls
/// these in the shipped binary.
const TALL: egui::Vec2 = egui::vec2(1280.0, 3000.0);

/// The machine label of the one speaker in the fixtures below.
const MACHINE_LABEL: &str = "Speaker 1";

/* ------------------------------------------------- the speaker list alone */

#[derive(Default)]
struct Collected {
    names: Vec<&'static str>,
    renames: Vec<String>,
}

impl Collected {
    fn absorb(&mut self, output: AiOutput) {
        for intent in output.intents {
            if let AiIntent::Command(command) = intent {
                self.names.push(command.name());
                if let AppCommand::RenameSpeaker(request) = command {
                    self.renames.push(request.name);
                }
            }
        }
    }
}

struct ViewerApp {
    viewer: OutputsViewer,
    capabilities: Capabilities,
    speakers: ListSpeakersResponse,
    collected: Collected,
}

/// The speaker list of one recording, with no generated output to read.
fn viewer(speakers: Vec<SpeakerDto>) -> Harness<'static, ViewerApp> {
    let mut harness = plaudon_ui_testkit::builder()
        .with_size(TALL)
        .build_ui_state(
            |ui, app: &mut ViewerApp| {
                egui::Frame::NONE.show(ui, |ui| {
                    let context = RecordingContext {
                        recording_id: fx::RECORDING_ID,
                        duration_ms: Some(600_000),
                        revision_id: Some(fx::REVISION_ID),
                        alignment_origin: AlignmentOrigin::Native,
                        title: "Board meeting",
                        capabilities: Some(&app.capabilities),
                    };
                    let output = app
                        .viewer
                        .ui(ui, context, None, Some(&app.speakers), fx::NOW_MS);
                    app.collected.absorb(output);
                });
            },
            ViewerApp {
                viewer: OutputsViewer::default(),
                capabilities: fx::capabilities(false, Vec::new()),
                speakers: ListSpeakersResponse { speakers },
                collected: Collected::default(),
            },
        );
    harness.run();
    harness
}

fn announced<State>(harness: &Harness<'_, State>) -> String {
    plaudon_ui_testkit::a11y::nodes(harness)
        .into_iter()
        .filter_map(|node| node.announced().map(str::to_owned))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Whether the control with this label is present but refuses activation.
fn disabled<State>(harness: &Harness<'_, State>, label: &str) -> bool {
    harness.get_by_label(label).accesskit_node().is_disabled()
}

#[test]
fn an_unnamed_speaker_offers_nothing_it_would_be_refused_for() {
    let harness = viewer(vec![fx::speaker(None, false)]);

    let text = announced(&harness);
    assert!(
        text.contains("Speaker: Speaker 1 (inferred cluster)"),
        "{text}"
    );
    assert!(text.contains("Machine label: SPEAKER_00"), "{text}");

    assert!(
        disabled(&harness, "Rename speaker"),
        "the store refuses an empty name, so the control that would send one is not offered"
    );
    assert!(
        disabled(&harness, "Confirm speaker"),
        "§17.6: a speaker must be named before there is anything to confirm"
    );
    assert!(
        disabled(&harness, "Reset to the machine label"),
        "a row that still carries its machine label has nothing to restore"
    );
}

#[test]
fn a_rename_is_dispatched_with_the_trimmed_name() {
    let mut harness = viewer(vec![fx::speaker(None, false)]);

    harness.get_by_label("Speaker name").focus();
    harness.run();
    harness.get_by_label("Speaker name").type_text("  Dana  ");
    harness.run();
    assert!(
        !disabled(&harness, "Rename speaker"),
        "a name was typed, so the rename is now sendable"
    );
    harness.get_by_label("Rename speaker").click();
    harness.run();

    assert_eq!(harness.state().collected.names, vec!["rename_speaker"]);
    assert_eq!(harness.state().collected.renames, vec!["Dana".to_owned()]);
}

#[test]
fn the_name_field_stops_at_the_stored_maximum() {
    let limit = plaudon_domain::speaker::MAX_NAME_CHARS;
    let mut harness = viewer(vec![fx::speaker(None, false)]);

    harness.get_by_label("Speaker name").focus();
    harness.run();
    harness
        .get_by_label("Speaker name")
        .type_text(&"n".repeat(limit + 25));
    harness.run();
    harness.get_by_label("Rename speaker").click();
    harness.run();

    let renames = &harness.state().collected.renames;
    assert_eq!(renames.len(), 1, "{:?}", harness.state().collected.names);
    assert_eq!(
        renames[0].chars().count(),
        limit,
        "the field refuses the characters past the limit rather than letting the store refuse \
         the whole rename"
    );
}

#[test]
fn a_typed_name_is_shown_as_a_proposal_until_it_is_confirmed() {
    let mut harness = viewer(vec![fx::speaker(Some("Dana"), false)]);

    let text = announced(&harness);
    assert!(
        text.contains("Speaker: Dana (inferred cluster)"),
        "a rename records a guess, not a claim: {text}"
    );
    assert!(
        text.contains("Name you typed, not yet confirmed: Dana"),
        "{text}"
    );
    assert!(text.contains("Machine label: SPEAKER_00"), "{text}");

    harness.get_by_label("Confirm speaker").click();
    harness.run();
    assert_eq!(harness.state().collected.names, vec!["confirm_speaker"]);
}

#[test]
fn a_confirmed_speaker_reads_as_identity_and_can_be_reset() {
    let mut harness = viewer(vec![fx::speaker(Some("Dana"), true)]);

    let text = announced(&harness);
    assert!(text.contains("Speaker: Dana (confirmed)"), "{text}");
    assert!(
        !text.contains("inferred cluster"),
        "a confirmed row is no longer a cluster: {text}"
    );
    assert!(
        harness.query_by_label("Confirm speaker").is_none(),
        "there is nothing left to confirm"
    );

    harness.get_by_label("Reset to the machine label").click();
    harness.run();
    assert_eq!(harness.state().collected.names, vec!["reset_speaker"]);
}

#[test]
fn the_name_field_is_reseeded_when_the_stored_name_changes() {
    let mut harness = viewer(vec![fx::speaker(Some("Dana"), false)]);
    assert!(
        announced(&harness).contains("Dana"),
        "the field opens on the stored name"
    );

    // A reset answered elsewhere: the discarded name must not stay in the
    // field, one click away from being stored again.
    harness.state_mut().speakers = ListSpeakersResponse {
        speakers: vec![fx::speaker(None, false)],
    };
    harness.run();

    assert!(
        disabled(&harness, "Rename speaker"),
        "the field was reseeded from a row that has no name: {}",
        announced(&harness)
    );
}

/* ------------------------------------------- the whole detail view wiring */

struct DetailApp {
    panels: LivePanels,
    detail: RecordingDetail,
    names: Vec<&'static str>,
}

impl DetailApp {
    fn view(&mut self, ui: &mut egui::Ui) {
        let mut out = AiOutput::default();
        // The inherent accessors shadow the trait, so the panels are driven
        // through `DetailPanels` explicitly.
        DetailPanels::transcript(&mut self.panels, ui, &self.detail, &mut out);
        DetailPanels::outputs(&mut self.panels, ui, &self.detail, &mut out);
        self.absorb(out);
    }

    /// Answer a command one of the panels sent.
    ///
    /// The identifier is irrelevant: panels match an answer by command name
    /// and by the recording it names, which is what lets a test answer out
    /// of band.
    fn answer(&mut self, command: &'static str, result: &Result<AppResponse, ErrorEnvelope>) {
        let mut out = AiOutput::default();
        self.panels
            .on_response(CommandId::from_raw(0), command, result, &mut out);
        self.absorb(out);
    }

    fn absorb(&mut self, out: AiOutput) {
        for intent in out.intents {
            if let AiIntent::Command(command) = intent {
                self.names.push(command.name());
            }
        }
    }
}

/// One speaker row of `recording_id`, as the store would answer with it.
fn row(
    recording_id: RecordingId,
    speaker_id: SpeakerId,
    name: Option<&str>,
    confirmed: bool,
) -> SpeakerDto {
    SpeakerDto {
        speaker_id,
        recording_id,
        // A rename overwrites the label; a reset restores `original_label`.
        label: name.unwrap_or(MACHINE_LABEL).to_owned(),
        name: name.map(ToOwned::to_owned),
        confirmed,
        original_label: MACHINE_LABEL.to_owned(),
        renamed_at: name.map(|_| Timestamp::from_millis(1_770_000_000_000)),
        confirmed_at: confirmed.then(|| Timestamp::from_millis(1_770_000_060_000)),
    }
}

fn speaker_answer(dto: SpeakerDto) -> SpeakerOperationResponse {
    SpeakerOperationResponse { speaker: dto }
}

/// Two attributed segments for the fixture's head revision.
fn transcript_page(detail: &RecordingDetail) -> TranscriptPage {
    let revision = detail.revisions[0].clone();
    let segments: Vec<TranscriptSegmentDto> = ["Everyone is here.", "The release is Friday."]
        .into_iter()
        .enumerate()
        .map(|(index, text)| TranscriptSegmentDto {
            index: u32::try_from(index).expect("two segments"),
            text: text.to_owned(),
            start_ms: Some(index as u64 * 4_000),
            end_ms: Some(index as u64 * 4_000 + 3_500),
            alignment_origin: AlignmentOrigin::Native,
            confidence: None,
            speaker_label: Some(MACHINE_LABEL.to_owned()),
            annotations: Vec::new(),
        })
        .collect();
    let total_segments = u32::try_from(segments.len()).expect("two segments");
    TranscriptPage {
        revision,
        segments,
        next_cursor: None,
        total_segments,
    }
}

/// A detail view with the transcript loaded, one speaker known, and the
/// outputs column showing the speaker list.
fn opened(
    speaker_id: SpeakerId,
    name: Option<&str>,
    confirmed: bool,
) -> Harness<'static, DetailApp> {
    let detail = detail(row_at(1));
    let recording_id = detail.summary.recording_id;
    let page = transcript_page(&detail);

    let mut panels = LivePanels::new(no_audio_output(), None);
    panels.set_now_ms(1_780_000_000_000);
    let mut out = AiOutput::default();
    panels.opened(recording_id, &mut out);

    let mut harness = plaudon_ui_testkit::builder()
        .with_size(TALL)
        .build_ui_state(
            |ui, app: &mut DetailApp| app.view(ui),
            DetailApp {
                panels,
                detail,
                names: Vec::new(),
            },
        );
    harness.run();

    let speakers = Ok(AppResponse::ListSpeakers(ListSpeakersResponse {
        speakers: vec![row(recording_id, speaker_id, name, confirmed)],
    }));
    harness.state_mut().answer("list_speakers", &speakers);
    let page = Ok(AppResponse::GetTranscript(page));
    harness.state_mut().answer("get_transcript", &page);
    harness.run();

    // The speaker list lives on the Outputs tab; selecting it before it is
    // drawn keeps the tab's label unambiguous.
    harness.get_by_label("Outputs").click();
    harness.run();
    harness
}

#[test]
fn one_speaker_list_feeds_both_the_chips_and_the_speaker_panel() {
    let speaker_id = SpeakerId::new();
    let harness = opened(speaker_id, None, false);
    let text = announced(&harness);

    // Before this wiring existed the answer reached whichever panel asked
    // first, and the other said "Speakers have not been read yet."
    assert!(
        !text.contains("Speakers have not been read yet"),
        "the list answer has to reach the outputs column too: {text}"
    );
    assert!(text.contains("Machine label: Speaker 1"), "{text}");
    assert_eq!(
        text.matches("Speaker: Speaker 1 (inferred cluster)")
            .count(),
        3,
        "two transcript chips and the speaker panel, all saying the same thing: {text}"
    );
}

#[test]
fn a_labelling_answer_updates_the_chips_in_the_same_frame_without_a_refetch() {
    let speaker_id = SpeakerId::new();
    let mut harness = opened(speaker_id, None, false);
    let recording_id = harness.state().detail.summary.recording_id;
    let before = harness.state().names.len();

    let renamed = Ok(AppResponse::RenameSpeaker(speaker_answer(row(
        recording_id,
        speaker_id,
        Some("Dana"),
        false,
    ))));
    harness.state_mut().answer("rename_speaker", &renamed);
    harness.run();

    let text = announced(&harness);
    assert!(
        text.contains("Speaker: Dana (inferred cluster)"),
        "a rename is a guess, not a claim: {text}"
    );
    assert!(
        text.contains("Name you typed, not yet confirmed: Dana"),
        "{text}"
    );
    assert!(
        text.contains("Speaker: Dana (inferred cluster), machine label Speaker 1"),
        "the chip keeps the machine label visible while the name is unconfirmed: {text}"
    );
    assert_eq!(
        harness.state().names.len(),
        before,
        "the answer carries the stored row, so nothing is refetched"
    );

    // Confirmation is the claim, and only now is the name an identity.
    let confirmed = Ok(AppResponse::ConfirmSpeaker(speaker_answer(row(
        recording_id,
        speaker_id,
        Some("Dana"),
        true,
    ))));
    harness.state_mut().answer("confirm_speaker", &confirmed);
    harness.run();
    let text = announced(&harness);
    assert!(text.contains("Speaker: Dana (confirmed)"), "{text}");
    assert!(
        !text.contains("inferred cluster"),
        "neither the panel nor a chip may still call it a cluster: {text}"
    );

    // Reset restores the machine label everywhere.
    let reset = Ok(AppResponse::ResetSpeaker(speaker_answer(row(
        recording_id,
        speaker_id,
        None,
        false,
    ))));
    harness.state_mut().answer("reset_speaker", &reset);
    harness.run();
    let text = announced(&harness);
    assert!(!text.contains("Dana"), "the name was discarded: {text}");
    assert_eq!(
        text.matches("Speaker: Speaker 1 (inferred cluster)")
            .count(),
        3,
        "{text}"
    );
}

#[test]
fn a_refused_labelling_command_is_reported_without_leaking_anything() {
    let speaker_id = SpeakerId::new();
    let mut harness = opened(speaker_id, Some("Dana"), false);
    let recording_id = harness.state().detail.summary.recording_id;

    let envelope = fx::envelope(AppErrorCode::InvalidArgument);
    harness
        .state_mut()
        .answer("confirm_speaker", &Err(envelope.clone()));
    harness.run();

    let text = announced(&harness);
    assert!(text.contains(&envelope.message), "{text}");
    assert!(
        text.contains(&format!("Diagnostic id {}", envelope.diagnostic_id)),
        "{text}"
    );
    assert!(
        !text.contains(".sqlite") && !text.contains('/'),
        "a refusal names no store and no path: {text}"
    );

    // A later success clears it: the refusal no longer describes anything.
    let confirmed = Ok(AppResponse::ConfirmSpeaker(speaker_answer(row(
        recording_id,
        speaker_id,
        Some("Dana"),
        true,
    ))));
    harness.state_mut().answer("confirm_speaker", &confirmed);
    harness.run();
    let text = announced(&harness);
    assert!(!text.contains("Diagnostic id"), "{text}");
    assert!(text.contains("Speaker: Dana (confirmed)"), "{text}");
}

#[test]
fn the_screen_and_an_export_bundle_word_a_speaker_the_same_way() {
    // `plaudon-export` renders `plaudon_domain::speaker::display` over the
    // same three values; the chip has to be that function, not a copy of it.
    let unconfirmed = plaudon_ui::transcript::SpeakerChoice {
        id: SpeakerId::new(),
        label: "Dana".to_owned(),
        machine_label: "Speaker 1".to_owned(),
        name: Some("Dana".to_owned()),
        confirmed: false,
    };
    assert_eq!(
        unconfirmed.render(),
        plaudon_domain::speaker::display("Dana", Some("Dana"), false)
    );
    assert_eq!(unconfirmed.render(), "Dana (inferred cluster)");

    let confirmed = plaudon_ui::transcript::SpeakerChoice {
        confirmed: true,
        ..unconfirmed.clone()
    };
    assert_eq!(
        confirmed.render(),
        plaudon_domain::speaker::display("Dana", Some("Dana"), true)
    );
    assert_eq!(confirmed.render(), "Dana (confirmed)");

    // The machine label rides along only while the two differ.
    assert_eq!(
        unconfirmed.render_with_origin(),
        "Dana (inferred cluster), machine label Speaker 1"
    );
    let untouched = plaudon_ui::transcript::SpeakerChoice {
        label: "Speaker 1".to_owned(),
        name: None,
        ..unconfirmed
    };
    assert_eq!(
        untouched.render_with_origin(),
        "Speaker 1 (inferred cluster)"
    );
}
