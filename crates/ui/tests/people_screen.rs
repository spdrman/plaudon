//! The People destination and the shared person picker, driven through
//! AccessKit by `egui_kittest` (`VOICE_PROFILES_SPEC` §9A, §17.4).
//!
//! Every assertion is made the way a user or a screen reader would make it:
//! find a control by its accessible name, act on it, and check the command the
//! screen produced and the words it published. Three families of rule are
//! asserted as strings on purpose, because they are the product promises that
//! a refactor can quietly break:
//!
//! * a suggestion is a suggestion ("sounds like", never "identified");
//! * a destructive roster action discloses what it reaches *before* it can be
//!   confirmed, and the application's own counts are what it quotes;
//! * with biometric matching off there is no suggestion surface at all, and
//!   everything else behaves identically (VP-215).

use egui::accesskit::Role;
use egui_kittest::kittest::Queryable as _;
use egui_kittest::Harness;
use plaudon_app::command::{AppCommand, AppResponse};
use plaudon_app::dto::{
    ActivationDisclosureDto, ConsentGateDto, DeletePersonResponse, ListPeopleResponse,
    ListSpeakerSuggestionsResponse, MergeCandidateGroupDto, MergePeopleResponse, PersonDto,
    PersonRefDto, PersonResponse, PersonVoiceDto, SpeakerSuggestionDto, VoiceProfileSettingsDto,
};
use plaudon_domain::error::ErrorEnvelope;
use plaudon_domain::ids::{PersonId, RecordingId, SpeakerSuggestionId};
use plaudon_domain::time::Timestamp;
use plaudon_ui::screens::people::picker::{CREATE_ENTRY, UNKNOWN_ENTRY};
use plaudon_ui::screens::people::{PeopleScreen, PeopleTab};
use plaudon_ui_testkit::a11y;

/// A fixed clock, so the retention countdown a test asserts on cannot drift.
const NOW_MS: i64 = 1_770_000_000_000;

/// The screen plus everything it asked for, which is all a host sees.
struct PeopleApp {
    screen: PeopleScreen,
    commands: Vec<AppCommand>,
}

impl PeopleApp {
    fn new() -> Self {
        let mut screen = PeopleScreen::new();
        screen.set_now_ms(NOW_MS);
        Self {
            screen,
            commands: Vec::new(),
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

    fn answer(&mut self, command: &'static str, response: AppResponse) {
        let follow_ups = self.screen.absorb(command, &Ok(response));
        self.commands.extend(follow_ups);
    }
}

/// The default window, deliberately: a modal centres itself in the *window*,
/// and a window taller than the viewport would put its controls where no
/// pointer can reach them.
fn harness(app: PeopleApp) -> Harness<'static, PeopleApp> {
    plaudon_ui_testkit::builder().build_ui_state(
        |ui, app: &mut PeopleApp| {
            let output = app.screen.ui(ui);
            app.commands.extend(output.commands);
        },
        app,
    )
}

fn labels(harness: &Harness<'_, PeopleApp>) -> Vec<String> {
    a11y::nodes(harness)
        .into_iter()
        .filter_map(|node| node.announced().map(str::to_owned))
        .collect()
}

fn says(harness: &Harness<'_, PeopleApp>, needle: &str) -> bool {
    labels(harness).iter().any(|line| line.contains(needle))
}

/// Activate the control named `label` through AccessKit.
///
/// A dialog is drawn in its own layer, and a pointer click landing in a layer
/// that appeared in the same frame is not the interaction under test here —
/// what is under test is that the control exists, is named and does what it
/// says. This is the activation a keyboard or a screen reader performs.
fn activate(harness: &mut Harness<'_, PeopleApp>, label: &str) {
    harness.get_by_label(label).click_accesskit();
    harness.run();
}

/// Type into the field named `label`, the way the settings suite does: focus,
/// let the frame settle, then type into the text input itself.
fn type_into(harness: &mut Harness<'_, PeopleApp>, label: &str, text: &str) {
    harness
        .get_by_role_and_label(Role::TextInput, label)
        .focus();
    harness.run();
    harness
        .get_by_role_and_label(Role::TextInput, label)
        .type_text(text);
    harness.run();
}

/* ------------------------------------------------------------- fixtures */

fn person(name: &str, recordings: u32) -> PersonDto {
    PersonDto {
        person_id: PersonId::new(),
        display_name: name.to_owned(),
        aliases: Vec::new(),
        email: None,
        organisation: None,
        status: "active".to_owned(),
        created_at: Timestamp::from_millis(0),
        updated_at: Timestamp::from_millis(0),
        recordings,
        pending_suggestions: 0,
        voice: None,
    }
}

fn enrolled(mut person: PersonDto) -> PersonDto {
    person.voice = Some(PersonVoiceDto {
        status: "active".to_owned(),
        exemplars: 3,
        source_recordings: 2,
        total_speech_ms: 42_000,
        retention_months: 12,
        consent_version: "2026-10-12".to_owned(),
        enrolled_at: Timestamp::from_millis(NOW_MS),
        last_matched_at: None,
        expires_at: Some(Timestamp::from_millis(NOW_MS + 10 * 24 * 60 * 60 * 1_000)),
    });
    person
}

fn roster(people: Vec<PersonDto>) -> ListPeopleResponse {
    ListPeopleResponse {
        people,
        merge_candidates: Vec::new(),
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
            title: "Voice matching stores a voiceprint".to_owned(),
            body: vec!["A voiceprint is biometric data.".to_owned()],
            two_tier_sentence: "Keeping a list of names needs no special consent.".to_owned(),
            responsibility_sentence: "People you enrol must agree first.".to_owned(),
            retention_default_months: 12,
        },
        enrolled_people: 1,
        exemplars: 3,
        pending_suggestions: 1,
        roster_people: 2,
        key_available: true,
        embedder_available: true,
        enrol_speech_ms: 10_000,
        exemplar_speech_ms: 5_000,
    }
}

fn suggestion(person: &PersonDto, band: &str, score: f64) -> SpeakerSuggestionDto {
    SpeakerSuggestionDto {
        suggestion_id: SpeakerSuggestionId::new(),
        recording_id: RecordingId::new(),
        recording_title: Some("Monday stand-up".to_owned()),
        diarization_run_id: "run-1".to_owned(),
        cluster_key: "S1".to_owned(),
        speaker_id: None,
        speaker_label: Some("Speaker 1".to_owned()),
        person: PersonRefDto {
            person_id: person.person_id,
            display_name: person.display_name.clone(),
        },
        score,
        score_kind: "as_norm".to_owned(),
        band: band.to_owned(),
        state: "pending".to_owned(),
        created_at: Timestamp::from_millis(NOW_MS),
    }
}

/// A screen that has read a two-person roster, with matching on or off.
fn loaded(matching_on: bool) -> Harness<'static, PeopleApp> {
    let mut app = PeopleApp::new();
    let people = vec![person("Priya", 14), person("Dana", 2)];
    app.screen.data_mut().people = Some(roster(people));
    app.screen.data_mut().settings = Some(settings(matching_on));
    let mut harness = harness(app);
    harness.run();
    harness
}

/* ------------------------------------------------------------ the roster */

#[test]
fn the_roster_states_how_many_recordings_each_person_is_assigned_in() {
    let harness = loaded(false);
    assert!(says(&harness, "In 14 recordings"), "{:?}", labels(&harness));
    assert!(says(&harness, "In 2 recordings"));
}

#[test]
fn the_rename_dialog_states_the_blast_radius_before_it_can_be_saved() {
    let mut harness = loaded(false);
    harness.get_by_label("Edit Priya…").click();
    harness.run();

    assert!(
        says(&harness, "This name is shown in 14 recordings."),
        "{:?}",
        labels(&harness)
    );
    assert!(says(&harness, "It changes no assignment"));

    activate(&mut harness, "Save these changes");
    let AppCommand::UpdatePerson(request) = harness.state().last("update_person") else {
        panic!("the dialog saves with update_person");
    };
    assert_eq!(request.display_name, "Priya");
}

#[test]
fn the_name_field_stops_at_the_stored_maximum_like_every_other_speaker_name() {
    let mut harness = loaded(false);
    harness.get_by_label("Edit Priya…").click();
    harness.run();
    type_into(&mut harness, "Name", &"x".repeat(200));

    activate(&mut harness, "Save these changes");
    let AppCommand::UpdatePerson(request) = harness.state().last("update_person") else {
        panic!("the dialog saves with update_person");
    };
    assert!(
        request.display_name.chars().count() <= plaudon_domain::speaker::MAX_NAME_CHARS,
        "the field let {} characters through",
        request.display_name.chars().count()
    );
}

#[test]
fn a_merge_cannot_be_confirmed_before_the_application_has_counted_what_it_moves() {
    let mut harness = loaded(false);
    harness.get_by_label("Merge Priya into…").click();
    harness.run();
    harness.get_by_label("Merge into Dana").click();
    harness.run();

    // The disclosure request goes first, and it changes nothing.
    let AppCommand::MergePeople(request) = harness.state().last("merge_people") else {
        panic!("the dialog asks what a merge would move");
    };
    assert!(!request.apply, "the first call must not apply the merge");
    assert!(says(&harness, "Counting what this would move…"));

    harness.state_mut().answer(
        "merge_people",
        AppResponse::MergePeople(MergePeopleResponse {
            applied: false,
            speaker_links: 9,
            recordings: 4,
            exemplars: 2,
            pending_suggestions: 1,
            enrollment_moved: true,
            survivor: None,
        }),
    );
    harness.run();

    assert!(
        says(
            &harness,
            "9 speaker rows in 4 recordings, 2 stored exemplars and 1 pending suggestions move."
        ),
        "{:?}",
        labels(&harness)
    );
    assert!(says(&harness, "only way back is to split the enrolment"));

    activate(&mut harness, "Merge them");
    let AppCommand::MergePeople(applied) = harness.state().last("merge_people") else {
        panic!("confirming merges");
    };
    assert!(applied.apply);
}

#[test]
fn duplicate_entries_are_offered_as_candidates_and_never_merged_by_themselves() {
    let mut app = PeopleApp::new();
    let one = person("Priya", 3);
    let two = person("priya", 1);
    let candidates = vec![MergeCandidateGroupDto {
        normalised_name: "priya".to_owned(),
        people: vec![
            PersonRefDto {
                person_id: one.person_id,
                display_name: one.display_name.clone(),
            },
            PersonRefDto {
                person_id: two.person_id,
                display_name: two.display_name.clone(),
            },
        ],
    }];
    app.screen.data_mut().people = Some(ListPeopleResponse {
        people: vec![one, two],
        merge_candidates: candidates,
    });
    app.screen.data_mut().settings = Some(settings(false));
    let mut harness = harness(app);
    harness.run();

    assert!(
        says(
            &harness,
            "Nothing is merged automatically — merging is always your decision."
        ),
        "{:?}",
        labels(&harness)
    );
    // Offering a group sends nothing at all.
    assert!(
        !harness.state().sent("merge_people"),
        "a candidate group merged by itself: {:?}",
        harness.state().names()
    );

    harness.get_by_label("Merge priya into Priya…").click();
    harness.run();
    let AppCommand::MergePeople(request) = harness.state().last("merge_people") else {
        panic!("the candidate opens the merge disclosure");
    };
    assert!(!request.apply);
}

#[test]
fn deleting_a_person_discloses_what_it_severs_and_what_it_leaves_alone() {
    let mut harness = loaded(false);
    harness.get_by_label("Delete Priya…").click();
    harness.run();
    let AppCommand::DeletePerson(request) = harness.state().last("delete_person") else {
        panic!("the dialog asks what deletion reaches");
    };
    assert!(!request.apply);
    assert!(says(
        &harness,
        "Every recording keeps the name it already showed, and no transcript text is touched."
    ));

    harness.state_mut().answer(
        "delete_person",
        AppResponse::DeletePerson(DeletePersonResponse {
            applied: false,
            severed_speaker_links: 14,
            recordings: 14,
            exemplars: 0,
            suggestions: 0,
        }),
    );
    harness.run();
    assert!(
        says(&harness, "14 speaker links in 14 recordings are severed"),
        "{:?}",
        labels(&harness)
    );
}

#[test]
fn an_enrolment_is_only_mentioned_where_biometric_matching_is_on() {
    let mut app = PeopleApp::new();
    app.screen.data_mut().people = Some(roster(vec![enrolled(person("Priya", 14))]));
    app.screen.data_mut().settings = Some(settings(false));
    let mut harness = harness(app);
    harness.run();
    assert!(
        !says(&harness, "voice: active"),
        "an enrolment badge with matching off: {:?}",
        labels(&harness)
    );
    assert!(!says(&harness, "Delete Priya's voiceprint…"));

    harness.state_mut().screen.data_mut().settings = Some(settings(true));
    harness.run();
    assert!(says(&harness, "voice: active"), "{:?}", labels(&harness));
    assert!(says(
        &harness,
        "3 exemplar(s) from 2 recording(s), 42 s of speech, retention 12 month(s)"
    ));
    assert!(says(&harness, "expires in 10 day(s)"));
    assert!(says(&harness, "Delete Priya's voiceprint…"));
}

/* ------------------------------------------------------------ the picker */

/// The review queue is where the picker is reachable without a recording, so
/// the picker tests drive it there.
fn with_queue(matching_on: bool) -> Harness<'static, PeopleApp> {
    let mut app = PeopleApp::new();
    let priya = person("Priya", 14);
    let mut dana = person("Dana", 2);
    dana.aliases = vec!["D. Okafor".to_owned()];
    let suggestions = vec![suggestion(&priya, "suggest", 0.82)];
    app.screen.data_mut().people = Some(roster(vec![priya, dana]));
    app.screen.data_mut().settings = Some(settings(matching_on));
    app.screen.data_mut().suggestions = Some(ListSpeakerSuggestionsResponse {
        pending_count: u32::try_from(suggestions.len()).expect("one suggestion"),
        suggestions,
    });
    app.screen.show_tab(PeopleTab::Review);
    let mut harness = harness(app);
    harness.run();
    harness
}

fn open_picker(harness: &mut Harness<'_, PeopleApp>) {
    harness.get_by_label("Not them — say who it was").click();
    harness.run();
    harness
        .get_by_label("Who was speaking in cluster S1: Nobody yet")
        .click();
    harness.run();
}

#[test]
fn the_picker_is_a_combo_box_that_announces_how_many_people_match() {
    let mut harness = with_queue(true);
    open_picker(&mut harness);

    let combo = a11y::nodes(&harness)
        .into_iter()
        .find(|node| node.role == "ComboBox")
        .expect("the picker is a combo box");
    assert!(
        combo
            .announced()
            .is_some_and(|name| name.starts_with("Who was speaking in cluster S1")),
        "{combo:?}"
    );
    assert!(
        says(&harness, "2 of 2 people match"),
        "{:?}",
        labels(&harness)
    );

    type_into(&mut harness, "Filter people by name or alias", "oka");
    assert!(
        says(&harness, "1 of 2 people match"),
        "{:?}",
        labels(&harness)
    );
    // An alias matched, and the person is offered by their display name.
    assert!(says(&harness, "Dana (also D. Okafor)"));
    assert!(!says(&harness, "Priya — in 14 recordings"));
}

#[test]
fn the_picker_offers_unknown_and_create_new_as_fixed_entries() {
    let mut harness = with_queue(true);
    open_picker(&mut harness);
    assert!(says(&harness, UNKNOWN_ENTRY), "{:?}", labels(&harness));
    assert!(says(&harness, CREATE_ENTRY));
}

#[test]
fn creating_a_person_from_the_picker_creates_and_assigns_in_one_step() {
    let mut harness = with_queue(true);
    open_picker(&mut harness);
    harness.get_by_label(CREATE_ENTRY).click();
    harness.run();

    type_into(&mut harness, "New person's name", "Ana");
    eprintln!("BEFORE CLICK: {:?}", labels(&harness));
    harness.get_by_label("Create and assign").click();
    harness.run();

    let AppCommand::CreatePerson(request) = harness.state().last("create_person") else {
        panic!("the picker creates the roster entry");
    };
    assert_eq!(request.display_name, "Ana");

    // The second half: the correction waits for the person to exist, and then
    // names them.
    let created = PersonResponse {
        person: person("Ana", 0),
    };
    let person_id = created.person.person_id.to_string();
    harness
        .state_mut()
        .answer("create_person", AppResponse::CreatePerson(created));
    harness.run();
    let AppCommand::RejectSpeakerSuggestion(correction) =
        harness.state().last("reject_speaker_suggestion")
    else {
        panic!("the correction follows the creation");
    };
    assert_eq!(correction.corrected_person_id.as_deref(), Some(&*person_id));
}

#[test]
fn the_picker_warns_when_the_typed_name_is_one_that_already_exists() {
    let mut harness = with_queue(true);
    open_picker(&mut harness);
    harness.get_by_label(CREATE_ENTRY).click();
    harness.run();
    type_into(&mut harness, "New person's name", "priya");

    assert!(
        says(
            &harness,
            "Priya is already on the list. Assign to the existing Priya instead?"
        ),
        "{:?}",
        labels(&harness)
    );
    harness.get_by_label("Assign to the existing Priya").click();
    harness.run();
    assert!(
        !harness.state().sent("create_person"),
        "the warning's own control created a second Priya"
    );
    let AppCommand::RejectSpeakerSuggestion(correction) =
        harness.state().last("reject_speaker_suggestion")
    else {
        panic!("picking the existing person corrects the suggestion");
    };
    assert!(correction.corrected_person_id.is_some());
}

#[test]
fn the_picker_can_be_driven_from_the_keyboard_alone() {
    let mut harness = with_queue(true);
    open_picker(&mut harness);
    // The filter has focus on opening, so typing filters without a click.
    harness.key_press(egui::Key::ArrowDown);
    harness.run();
    harness.key_press(egui::Key::Enter);
    harness.run();

    let AppCommand::RejectSpeakerSuggestion(correction) =
        harness.state().last("reject_speaker_suggestion")
    else {
        panic!("Enter on a highlighted entry assigns it");
    };
    assert!(
        correction.corrected_person_id.is_some(),
        "the keyboard path named nobody"
    );
}

/* ------------------------------------------------------- the review queue */

#[test]
fn the_queue_reads_as_a_suggestion_and_never_as_an_identity() {
    let harness = with_queue(true);
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
            "the queue claimed an identity with {forbidden:?}: {lines:?}"
        );
    }
    assert!(lines
        .iter()
        .any(|line| line.contains("Nothing has been assigned.")));
}

#[test]
fn the_queue_is_completable_from_the_keyboard() {
    let mut harness = with_queue(true);
    harness.key_press(egui::Key::Enter);
    harness.run();
    assert!(
        harness.state().sent("confirm_speaker_suggestion"),
        "Enter did not confirm: {:?}",
        harness.state().names()
    );

    let mut harness = with_queue(true);
    harness.key_press(egui::Key::Delete);
    harness.run();
    let AppCommand::RejectSpeakerSuggestion(request) =
        harness.state().last("reject_speaker_suggestion")
    else {
        panic!("Delete rejects");
    };
    assert_eq!(
        request.corrected_person_id, None,
        "a plain rejection renamed somebody"
    );

    let mut harness = with_queue(true);
    harness.key_press(egui::Key::N);
    harness.run();
    assert!(
        !harness.state().sent("reject_speaker_suggestion"),
        "n rejected before the user said who it was"
    );
    assert!(
        says(&harness, "Who was speaking in cluster S1"),
        "n did not open the picker: {:?}",
        labels(&harness)
    );
}

#[test]
fn with_matching_off_there_is_no_queue_and_no_suggestion_anywhere() {
    let harness = with_queue(false);
    let lines = labels(&harness);
    assert!(
        !lines.iter().any(|line| line.contains("sounds like")),
        "a suggestion survived matching being off: {lines:?}"
    );
    assert!(
        !lines
            .iter()
            .any(|line| line.contains(PeopleTab::Review.label())),
        "the review tab survived matching being off: {lines:?}"
    );
    // The roster is still the roster: this is the manual-only parity rule.
    assert!(lines.iter().any(|line| line.contains("Priya")), "{lines:?}");
}

#[test]
fn a_refused_cluster_assignment_asks_about_the_segments_that_were_set_by_hand() {
    let mut harness = with_queue(true);
    let suggestion_id = harness
        .state()
        .screen
        .data()
        .suggestions
        .as_ref()
        .expect("the queue")
        .suggestions[0]
        .suggestion_id;
    harness.state_mut().answer(
        "confirm_speaker_suggestion",
        AppResponse::ConfirmSpeakerSuggestion(plaudon_app::dto::SpeakerSuggestionDecisionDto {
            suggestion_id,
            state: "pending".to_owned(),
            applied: false,
            speaker: None,
            person: None,
            assigned_segments: Vec::new(),
            segment_exceptions: 3,
        }),
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

    harness.get_by_label("Overwrite them").click();
    harness.run();
    let AppCommand::ConfirmSpeakerSuggestion(request) =
        harness.state().last("confirm_speaker_suggestion")
    else {
        panic!("overwriting confirms again");
    };
    assert!(request.overwrite_segment_exceptions);
}

/* ------------------------------------------------------------------ a11y */

#[test]
fn every_control_of_the_people_screen_is_named_with_matching_on() {
    let harness = with_queue(true);
    a11y::assert_every_control_is_named(&harness, "People — suggestions to review", &[]);
}

#[test]
fn every_control_of_the_people_screen_is_named_with_matching_off() {
    let harness = loaded(false);
    a11y::assert_every_control_is_named(&harness, "People — roster, matching off", &[]);
}

#[test]
fn every_control_of_the_open_picker_is_named() {
    let mut harness = with_queue(true);
    open_picker(&mut harness);
    a11y::assert_every_control_is_named(&harness, "People — the person picker", &[]);
}

#[test]
fn every_control_of_the_merge_disclosure_is_named() {
    let mut harness = loaded(false);
    harness.get_by_label("Merge Priya into…").click();
    harness.run();
    harness.get_by_label("Merge into Dana").click();
    harness.run();
    a11y::assert_every_control_is_named(&harness, "People — the merge disclosure", &[]);
}

/* --------------------------------------------------------- refusals */

#[test]
fn a_refused_read_is_shown_with_its_diagnostic_id_and_can_be_dismissed() {
    let mut app = PeopleApp::new();
    app.screen.data_mut().settings = Some(settings(false));
    let diagnostic_id = plaudon_domain::ids::DiagnosticId::new();
    let envelope = ErrorEnvelope {
        code: plaudon_domain::error::AppErrorCode::InternalError,
        message: "the library is locked".to_owned(),
        retryable: false,
        action: None,
        diagnostic_id,
    };
    app.screen.absorb("list_people", &Err(envelope));
    let mut harness = harness(app);
    harness.run();
    assert!(
        says(&harness, "the library is locked"),
        "{:?}",
        labels(&harness)
    );
    assert!(says(&harness, &format!("Diagnostic id {diagnostic_id}")));
    harness.get_by_label("Dismiss this message").click();
    harness.run();
    assert!(!says(&harness, "the library is locked"));
}
