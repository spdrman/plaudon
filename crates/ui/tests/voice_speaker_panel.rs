//! The per-recording speaker panel: pickers, suggestions, enrolment
//! (`VOICE_PROFILES_SPEC` VP-092, VP-094, VP-211, VP-215, VP-216, §17.4).
//!
//! The panel is driven directly, with no dispatcher: it renders borrowed DTOs
//! and returns commands, so every state below is reachable without a backend.
//! What is asserted is what a user can see and what the application is asked
//! to do — never private state.
//!
//! The manual-parity rule (VP-215) is asserted as a *pair*: the same fixture
//! is rendered with matching on and with it off, and the only difference
//! allowed is the absence of the suggestion rows and of the enrolment control.

use egui::accesskit::Role;
use egui_kittest::kittest::Queryable as _;
use egui_kittest::Harness;
use plaudon_app::command::{AppCommand, AppResponse};
use plaudon_app::dto::{
    ActivationDisclosureDto, AssignClusterPersonResponse, ConsentGateDto, ListSpeakersResponse,
    PersonRefDto, PersonResponse, SpeakerDto, SpeakerSuggestionDto, VoiceProfileSettingsDto,
};
use plaudon_domain::ids::{PersonId, RecordingId, SpeakerId, SpeakerSuggestionId};
use plaudon_domain::time::Timestamp;
use plaudon_domain::transcript::AlignmentOrigin;
use plaudon_ui::ai::{AiIntent, RecordingContext, SpeakerPanel, VoiceContext};
use plaudon_ui::screens::people::PickerPerson;

const NOW_MS: i64 = 1_770_000_000_000;

/// The panel, its inputs and the commands it produced.
struct PanelApp {
    panel: SpeakerPanel,
    recording_id: RecordingId,
    speakers: ListSpeakersResponse,
    people: Vec<PickerPerson>,
    settings: Option<VoiceProfileSettingsDto>,
    suggestions: Vec<SpeakerSuggestionDto>,
    commands: Vec<AppCommand>,
}

impl PanelApp {
    fn view(&mut self, ui: &mut egui::Ui) {
        let recording_id = self.recording_id.to_string();
        let context = RecordingContext {
            recording_id: &recording_id,
            duration_ms: Some(120_000),
            revision_id: None,
            alignment_origin: AlignmentOrigin::Native,
            title: "Monday stand-up",
            capabilities: None,
        };
        let voice = VoiceContext {
            people: &self.people,
            settings: self.settings.as_ref(),
            suggestions: &self.suggestions,
        };
        let output = self.panel.ui(ui, context, Some(&self.speakers), voice);
        for intent in output.intents {
            if let AiIntent::Command(command) = intent {
                self.commands.push(command);
            }
        }
    }

    fn names(&self) -> Vec<&'static str> {
        self.commands.iter().map(AppCommand::name).collect()
    }

    fn last(&self, name: &str) -> &AppCommand {
        self.commands
            .iter()
            .rev()
            .find(|command| command.name() == name)
            .unwrap_or_else(|| panic!("no {name} was sent; sent {:?}", self.names()))
    }

    fn sent(&self, name: &str) -> bool {
        self.commands.iter().any(|command| command.name() == name)
    }
}

fn harness(app: PanelApp) -> Harness<'static, PanelApp> {
    plaudon_ui_testkit::builder().build_ui_state(|ui, app: &mut PanelApp| app.view(ui), app)
}

fn labels(harness: &Harness<'_, PanelApp>) -> Vec<String> {
    plaudon_ui_testkit::a11y::nodes(harness)
        .into_iter()
        .filter_map(|node| node.announced().map(str::to_owned))
        .collect()
}

fn says(harness: &Harness<'_, PanelApp>, needle: &str) -> bool {
    labels(harness).iter().any(|line| line.contains(needle))
}

/// Activate a control through AccessKit, which is what a keyboard user does.
fn activate(harness: &mut Harness<'_, PanelApp>, label: &str) {
    harness.get_by_label(label).click_accesskit();
    harness.run();
}

/* ------------------------------------------------------------- fixtures */

fn speaker(recording_id: RecordingId, label: &str, cluster: &str) -> SpeakerDto {
    SpeakerDto {
        speaker_id: SpeakerId::new(),
        recording_id,
        cluster_key: Some(cluster.to_owned()),
        person_id: None,
        label: label.to_owned(),
        name: None,
        confirmed: false,
        original_label: label.to_owned(),
        renamed_at: None,
        confirmed_at: None,
    }
}

fn settings(enabled: bool) -> VoiceProfileSettingsDto {
    VoiceProfileSettingsDto {
        enabled,
        consent: ConsentGateDto {
            state: if enabled { "active" } else { "off" }.to_owned(),
            reason: None,
            accepted_version: enabled.then(|| "2026-10-12".to_owned()),
            accepted_at: enabled.then(|| Timestamp::from_millis(NOW_MS)),
            required_version: "2026-10-12".to_owned(),
        },
        disclosure: ActivationDisclosureDto {
            version: "2026-10-12".to_owned(),
            title: "What a voiceprint is".to_owned(),
            body: vec!["A voiceprint is biometric data and stays on this machine.".to_owned()],
            two_tier_sentence: "Keeping a list of names needs no special consent; computing a \
                                voiceprint to recognise someone automatically does."
                .to_owned(),
            responsibility_sentence: "People you enrol must agree first, and that is your \
                                      responsibility, not this software's."
                .to_owned(),
            retention_default_months: 12,
        },
        enrolled_people: 0,
        exemplars: 0,
        pending_suggestions: 1,
        roster_people: 2,
        key_available: true,
        embedder_available: true,
        enrol_speech_ms: 10_000,
        exemplar_speech_ms: 5_000,
    }
}

fn suggestion(speaker: &SpeakerDto, person_id: PersonId, score: f64) -> SpeakerSuggestionDto {
    SpeakerSuggestionDto {
        suggestion_id: SpeakerSuggestionId::new(),
        recording_id: speaker.recording_id,
        recording_title: Some("Monday stand-up".to_owned()),
        diarization_run_id: "run-1".to_owned(),
        cluster_key: speaker.cluster_key.clone().expect("a diarized cluster"),
        speaker_id: Some(speaker.speaker_id),
        speaker_label: Some(speaker.label.clone()),
        person: PersonRefDto {
            person_id,
            display_name: "Priya".to_owned(),
        },
        score,
        score_kind: "as_norm".to_owned(),
        band: "suggest".to_owned(),
        state: "pending".to_owned(),
        created_at: Timestamp::from_millis(NOW_MS),
    }
}

/// A panel over one cluster, a two-person roster and (optionally) a
/// suggestion for that cluster.
fn panel(matching_on: bool, with_suggestion: bool) -> Harness<'static, PanelApp> {
    let recording_id = RecordingId::new();
    let row = speaker(recording_id, "Speaker 1", "S1");
    let priya = PersonId::new();
    let people = vec![
        PickerPerson::new(priya.to_string(), "Priya").with_detail("in 14 recordings"),
        PickerPerson::new(PersonId::new().to_string(), "Dana"),
    ];
    let suggestions = if with_suggestion {
        vec![suggestion(&row, priya, 0.82)]
    } else {
        Vec::new()
    };
    let mut harness = harness(PanelApp {
        panel: SpeakerPanel::default(),
        recording_id,
        speakers: ListSpeakersResponse {
            speakers: vec![row],
        },
        people,
        settings: Some(settings(matching_on)),
        suggestions,
        commands: Vec::new(),
    });
    harness.run();
    harness
}

/* ------------------------------------------------- the per-cluster picker */

#[test]
fn the_cluster_picker_assigns_exactly_the_cluster_the_control_names() {
    let mut harness = panel(false, false);
    let speaker_id = harness.state().speakers.speakers[0].speaker_id.to_string();
    let recording_id = harness.state().recording_id.to_string();
    let priya = harness.state().people[0].id.clone();

    harness
        .get_by_label("Assign all of Speaker 1 to: Speaker 1")
        .click();
    harness.run();
    harness.get_by_label("Priya — in 14 recordings").click();
    harness.run();

    let AppCommand::AssignClusterPerson(request) = harness.state().last("assign_cluster_person")
    else {
        panic!("the picker assigns the cluster");
    };
    assert_eq!(request.recording_id, recording_id, "another recording");
    assert_eq!(request.speaker_id, speaker_id, "another cluster");
    assert_eq!(request.person_id.as_deref(), Some(&*priya));
    assert!(
        !request.overwrite_segment_exceptions,
        "the first attempt must not overwrite segments set by hand"
    );
}

#[test]
fn unknown_clears_the_assignment_back_to_the_neutral_label() {
    let mut harness = panel(false, false);
    harness
        .get_by_label("Assign all of Speaker 1 to: Speaker 1")
        .click();
    harness.run();
    harness
        .get_by_label(plaudon_ui::screens::people::picker::UNKNOWN_ENTRY)
        .click();
    harness.run();

    let AppCommand::AssignClusterPerson(request) = harness.state().last("assign_cluster_person")
    else {
        panic!("the picker clears the cluster");
    };
    assert_eq!(request.person_id, None);
}

#[test]
fn an_assignment_that_would_overwrite_handmade_segments_asks_first() {
    let mut harness = panel(false, false);
    let row = harness.state().speakers.speakers[0].clone();
    let priya = harness.state().people[0].id.clone();
    harness
        .get_by_label("Assign all of Speaker 1 to: Speaker 1")
        .click();
    harness.run();
    harness.get_by_label("Priya — in 14 recordings").click();
    harness.run();

    harness.state_mut().panel.on_response(
        "assign_cluster_person",
        &Ok(AppResponse::AssignClusterPerson(
            AssignClusterPersonResponse {
                applied: false,
                speaker: row,
                person: None,
                assigned_segments: Vec::new(),
                segment_exceptions: 3,
            },
        )),
    );
    harness.run();
    assert!(
        says(
            &harness,
            "3 segment(s) were set individually — keep them or overwrite them?"
        ),
        "{:?}",
        labels(&harness)
    );

    activate(&mut harness, "Overwrite them");
    let AppCommand::AssignClusterPerson(request) = harness.state().last("assign_cluster_person")
    else {
        panic!("overwriting assigns again");
    };
    assert!(request.overwrite_segment_exceptions);
    assert_eq!(request.person_id.as_deref(), Some(&*priya));
}

#[test]
fn creating_a_person_from_a_cluster_row_creates_then_assigns_that_cluster() {
    let mut harness = panel(false, false);
    let speaker_id = harness.state().speakers.speakers[0].speaker_id.to_string();
    harness
        .get_by_label("Assign all of Speaker 1 to: Speaker 1")
        .click();
    harness.run();
    harness
        .get_by_label(plaudon_ui::screens::people::picker::CREATE_ENTRY)
        .click();
    harness.run();
    harness
        .get_by_role_and_label(Role::TextInput, "New person's name")
        .focus();
    harness.run();
    harness
        .get_by_role_and_label(Role::TextInput, "New person's name")
        .type_text("Ana");
    harness.run();
    activate(&mut harness, "Create and assign");

    let AppCommand::CreatePerson(request) = harness.state().last("create_person") else {
        panic!("the picker creates the person first");
    };
    assert_eq!(request.display_name, "Ana");

    let created = PersonResponse {
        person: plaudon_app::dto::PersonDto {
            person_id: PersonId::new(),
            display_name: "Ana".to_owned(),
            aliases: Vec::new(),
            email: None,
            organisation: None,
            status: "active".to_owned(),
            created_at: Timestamp::from_millis(NOW_MS),
            updated_at: Timestamp::from_millis(NOW_MS),
            recordings: 0,
            pending_suggestions: 0,
            voice: None,
        },
    };
    let person_id = created.person.person_id.to_string();
    let follow_ups = harness
        .state_mut()
        .panel
        .on_response("create_person", &Ok(AppResponse::CreatePerson(created)));
    let [AppCommand::AssignClusterPerson(request)] = follow_ups.as_slice() else {
        panic!("creating a person assigns them to the cluster that asked: {follow_ups:?}");
    };
    assert_eq!(request.person_id.as_deref(), Some(&*person_id));
    assert_eq!(request.speaker_id, speaker_id);
}

/* ---------------------------------------------------------- suggestions */

#[test]
fn a_suggestion_is_offered_in_the_one_wording_and_never_as_an_identity() {
    let harness = panel(true, true);
    let lines = labels(&harness);
    assert!(
        lines
            .iter()
            .any(|line| line.contains("sounds like Priya — strong match, 0.82")),
        "{lines:?}"
    );
    for forbidden in ["identified", "verified", "recognised as"] {
        assert!(
            !lines.iter().any(|line| line.contains(forbidden)),
            "the panel claimed an identity with {forbidden:?}"
        );
    }
}

#[test]
fn confirming_a_suggestion_names_the_suggestion_and_nothing_else() {
    let mut harness = panel(true, true);
    let suggestion_id = harness.state().suggestions[0].suggestion_id.to_string();
    activate(&mut harness, "Confirm Priya");

    let AppCommand::ConfirmSpeakerSuggestion(request) =
        harness.state().last("confirm_speaker_suggestion")
    else {
        panic!("confirming dispatches confirm_speaker_suggestion");
    };
    assert_eq!(request.suggestion_id, suggestion_id);
    assert!(!request.overwrite_segment_exceptions);
    assert!(
        !harness.state().sent("assign_cluster_person"),
        "confirming assigned through a second path: {:?}",
        harness.state().names()
    );
}

#[test]
fn rejecting_renames_nobody_and_then_asks_who_it_was() {
    let mut harness = panel(true, true);
    let suggestion_id = harness.state().suggestions[0].suggestion_id.to_string();
    activate(&mut harness, "Not them");

    let AppCommand::RejectSpeakerSuggestion(request) =
        harness.state().last("reject_speaker_suggestion")
    else {
        panic!("rejecting dispatches reject_speaker_suggestion");
    };
    assert_eq!(request.suggestion_id, suggestion_id);
    assert_eq!(
        request.corrected_person_id, None,
        "a plain rejection named somebody"
    );
    assert!(
        says(&harness, "Rejected. Who was it?"),
        "the picker did not open: {:?}",
        labels(&harness)
    );

    harness
        .get_by_label("Who was speaking in this cluster: Nobody yet")
        .click();
    harness.run();
    harness.get_by_label("Dana").click();
    harness.run();

    let AppCommand::RejectSpeakerSuggestion(correction) =
        harness.state().last("reject_speaker_suggestion")
    else {
        panic!("picking records the correction");
    };
    assert_eq!(correction.suggestion_id, suggestion_id);
    assert_eq!(
        correction.corrected_person_id.as_deref(),
        Some(harness.state().people[1].id.as_str()),
        "the correction named the wrong person"
    );
}

/* ------------------------------------------------------------ enrolment */

#[test]
fn enrolment_is_offered_only_once_somebody_is_named() {
    let mut harness = panel(true, false);
    assert!(
        says(&harness, "Enrol this speaker's voice…"),
        "{:?}",
        labels(&harness)
    );
    // Nobody is assigned, so it cannot be used yet.
    activate(&mut harness, "Enrol this speaker's voice…");
    assert!(
        !says(&harness, "Enrol the voice of"),
        "the flow opened for a cluster nobody is assigned to"
    );

    let person_id = harness.state().people[0].id.clone();
    harness.state_mut().speakers.speakers[0].person_id =
        Some(person_id.parse().expect("a person id"));
    harness.run();
    activate(&mut harness, "Enrol this speaker's voice…");
    assert!(
        says(&harness, "Enrol the voice of Priya"),
        "{:?}",
        labels(&harness)
    );
}

#[test]
fn the_enrolment_gate_states_the_duty_and_is_not_pre_ticked() {
    let mut harness = panel(true, false);
    let person_id = harness.state().people[0].id.clone();
    harness.state_mut().speakers.speakers[0].person_id =
        Some(person_id.parse().expect("a person id"));
    harness.run();
    activate(&mut harness, "Enrol this speaker's voice…");

    let lines = labels(&harness);
    let disclosure = settings(true).disclosure;
    for sentence in [
        &disclosure.two_tier_sentence,
        &disclosure.responsibility_sentence,
    ] {
        assert!(
            lines.iter().any(|line| line.contains(sentence.as_str())),
            "the gate did not state, verbatim: {sentence:?}"
        );
    }
    assert!(lines
        .iter()
        .any(|line| line.contains("at least 10 s of clean speech")));

    // The attestation is a claim the user makes; it is never made for them.
    let attestation = plaudon_ui_testkit::a11y::nodes(&harness)
        .into_iter()
        .find(|node| node.announced() == Some("Priya has agreed to this"))
        .expect("the attestation is a named control");
    assert_eq!(
        attestation.role, "CheckBox",
        "the attestation is a checkbox the user ticks"
    );
    assert!(
        !says(&harness, "Enrol this voice is available"),
        "nothing claims the gate is passed before it is"
    );

    activate(&mut harness, "Priya has agreed to this");
    activate(&mut harness, "Enrol this voice");
    let AppCommand::EnrollPersonVoice(request) = harness.state().last("enroll_person_voice") else {
        panic!("the flow enrols");
    };
    assert_eq!(request.person_id, person_id);
    assert_eq!(request.cluster_key, "S1");
    assert!(request.consent.person_attested);
    assert_eq!(request.consent.disclosure_version, disclosure.version);
    assert_eq!(
        request.consent.retention_months,
        Some(disclosure.retention_default_months)
    );
}

#[test]
fn an_enrolment_refusal_is_announced_where_the_action_was_taken() {
    let mut harness = panel(true, false);
    harness.state_mut().panel.on_response(
        "enroll_person_voice",
        &Err(plaudon_domain::error::ErrorEnvelope {
            code: plaudon_domain::error::AppErrorCode::InvalidArgument,
            message: "not enough clean speech from this speaker".to_owned(),
            retryable: false,
            action: None,
            diagnostic_id: plaudon_domain::ids::DiagnosticId::new(),
        }),
    );
    harness.run();
    let status = plaudon_ui_testkit::a11y::nodes(&harness)
        .into_iter()
        .find(|node| node.role == "Status")
        .expect("a refusal is announced politely");
    assert_eq!(
        status.announced(),
        Some("not enough clean speech from this speaker")
    );
}

/* ----------------------------------------------- manual-only parity rule */

#[test]
fn with_matching_off_the_panel_is_the_same_panel_minus_the_suggestions() {
    let on = panel(true, true);
    let off = panel(false, true);
    let on_lines = labels(&on);
    let off_lines = labels(&off);

    assert!(on_lines.iter().any(|line| line.contains("sounds like")));
    assert!(
        !off_lines.iter().any(|line| line.contains("sounds like")),
        "a suggestion survived matching being off: {off_lines:?}"
    );
    assert!(
        !off_lines
            .iter()
            .any(|line| line.contains("Enrol this speaker's voice…")),
        "the enrolment control survived matching being off"
    );
    assert!(
        off_lines
            .iter()
            .any(|line| line.contains("Voice matching is off.")),
        "the panel did not say why there are no suggestions"
    );

    // Everything else is the same panel: the picker, its entries, and the
    // T12 name controls.
    for shared in [
        "Assign all of Speaker 1 to: Speaker 1",
        "Rename speaker",
        "Confirm speaker",
        "Reset to the machine label",
    ] {
        assert!(
            off_lines.iter().any(|line| line.contains(shared)),
            "{shared:?} is missing with matching off: {off_lines:?}"
        );
        assert!(on_lines.iter().any(|line| line.contains(shared)));
    }
}

/* ------------------------------------------------------------------ a11y */

#[test]
fn every_control_of_the_speaker_panel_is_named_with_matching_on() {
    let harness = panel(true, true);
    plaudon_ui_testkit::a11y::assert_every_control_is_named(
        &harness,
        "the speaker panel, matching on",
        &[],
    );
}

#[test]
fn every_control_of_the_speaker_panel_is_named_with_matching_off() {
    let harness = panel(false, false);
    plaudon_ui_testkit::a11y::assert_every_control_is_named(
        &harness,
        "the speaker panel, matching off",
        &[],
    );
}

#[test]
fn every_control_of_the_enrolment_flow_is_named() {
    let mut harness = panel(true, false);
    let person_id = harness.state().people[0].id.clone();
    harness.state_mut().speakers.speakers[0].person_id =
        Some(person_id.parse().expect("a person id"));
    harness.run();
    activate(&mut harness, "Enrol this speaker's voice…");
    plaudon_ui_testkit::a11y::assert_every_control_is_named(&harness, "the enrolment flow", &[]);
}
