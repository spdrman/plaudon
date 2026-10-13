//! The People destination: the master speaker list
//! (`VOICE_PROFILES_SPEC` §9A, VP-091, VP-093, VP-210..VP-217).
//!
//! One roster per library, independent of any recording, and the vocabulary
//! every assignment surface picks from. The screen is two things in one
//! destination:
//!
//! * **People** — the searchable list, its per-person facts (how many
//!   recordings they are assigned in, whether a voice is enrolled and what
//!   that enrolment is made of), the roster edits (rename, aliases, contacts,
//!   merge, delete) and the merge candidates the store offers. Every one of
//!   those works with biometric matching off, because a name is not a
//!   voiceprint (VP-011, VP-126).
//! * **Suggestions to review** — the bulk queue of pending suggestions across
//!   recordings (VP-093), keyboard-completable, and *absent* when matching is
//!   off because then there are no suggestions to have an opinion about
//!   (VP-215).
//!
//! The screen renders DTOs and raises [`AppCommand`]s. It reaches no port,
//! opens no dialog and knows no route, so a kittest test drives every state of
//! it with no dispatcher at all.

pub mod picker;
pub mod review;
pub mod roster;
pub mod shell_adapter;

use egui::Ui;
use plaudon_app::command::{AppCommand, AppResponse};
use plaudon_app::dto::{
    CreatePersonRequest, DeletePersonRequest, DeletePersonResponse, DeleteVoiceEnrollmentRequest,
    DeleteVoiceEnrollmentResponse, EnrolmentConsentDto, ListPeopleRequest, ListPeopleResponse,
    ListSpeakerSuggestionsRequest, ListSpeakerSuggestionsResponse, ListVoiceExemplarsRequest,
    ListVoiceExemplarsResponse, MergePeopleRequest, MergePeopleResponse, PersonDto,
    RejectSpeakerSuggestionRequest, SplitVoiceEnrollmentRequest, UpdatePersonRequest,
    VoiceExemplarDto, VoiceProfileSettingsDto,
};
use plaudon_domain::error::ErrorEnvelope;
use plaudon_domain::speaker::MAX_NAME_CHARS;

use crate::ai::speakers::{MAX_RETENTION_MONTHS, MIN_RETENTION_MONTHS};

pub use picker::{PersonPick, PersonPicker, PersonPickerState, PickerPerson, PickerSuggestion};
pub use review::{ReviewQueue, SuggestionDecision};
pub use shell_adapter::PeopleShellScreen;

/// How many suggestions the review queue asks for at a time.
pub const QUEUE_PAGE_SIZE: u32 = 100;

/// Said wherever the roster is explained, so the two-tier boundary of §12.2A
/// is stated where a user can act on it rather than only in Settings.
pub const ROSTER_IS_NOT_BIOMETRIC: &str =
    "Keeping a list of names and assigning them to recordings needs no special consent; \
     computing a voiceprint to recognise someone automatically does.";

/// Which half of the destination is on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PeopleTab {
    /// The roster.
    #[default]
    Roster,
    /// The bulk review queue (VP-093).
    Review,
}

impl PeopleTab {
    /// The tab's label, which is also its accessible name.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Roster => "People",
            Self::Review => "Suggestions to review",
        }
    }
}

/// Everything the application has told this screen.
///
/// Plain data: `None` means "not answered yet" and is rendered as such, never
/// as a zero that would read as a fact.
#[derive(Debug, Default, Clone)]
pub struct PeopleData {
    /// The last `list_people` answer.
    pub people: Option<ListPeopleResponse>,
    /// The last `get_voice_profile_settings` answer.
    pub settings: Option<VoiceProfileSettingsDto>,
    /// The last `list_speaker_suggestions` answer for the global queue.
    pub suggestions: Option<ListSpeakerSuggestionsResponse>,
}

impl PeopleData {
    /// Whether biometric matching is on.
    ///
    /// Unknown settings read as off: the whole feature is off by default
    /// (VP-003), and showing enrolment affordances before the answer arrives
    /// would claim a state the application has not reported.
    #[must_use]
    pub fn matching_on(&self) -> bool {
        self.settings
            .as_ref()
            .is_some_and(|settings| settings.enabled)
    }

    /// The roster, empty until it has been read.
    #[must_use]
    pub fn people(&self) -> &[PersonDto] {
        self.people
            .as_ref()
            .map_or(&[], |response| response.people.as_slice())
    }

    /// One person by id.
    #[must_use]
    pub fn person(&self, person_id: &str) -> Option<&PersonDto> {
        self.people()
            .iter()
            .find(|person| person.person_id.to_string() == person_id)
    }

    /// The roster as the picker offers it (VP-210).
    #[must_use]
    pub fn picker_people(&self) -> Vec<PickerPerson> {
        self.people().iter().map(picker_person).collect()
    }

    /// The reads the screen issues when it opens, and again after a resync.
    #[must_use]
    pub fn initial_commands() -> Vec<AppCommand> {
        vec![
            AppCommand::ListPeople(ListPeopleRequest { query: None }),
            AppCommand::GetVoiceProfileSettings(plaudon_app::dto::EmptyRequest {}),
            AppCommand::ListSpeakerSuggestions(ListSpeakerSuggestionsRequest {
                recording_id: None,
                limit: Some(QUEUE_PAGE_SIZE),
            }),
        ]
    }

    /// Fold a command outcome into the cache. Returns whether it was one of
    /// this screen's reads.
    pub fn absorb(&mut self, result: &Result<AppResponse, ErrorEnvelope>) -> bool {
        let Ok(response) = result else {
            return false;
        };
        match response {
            AppResponse::ListPeople(people) => {
                self.people = Some(people.clone());
                true
            }
            AppResponse::GetVoiceProfileSettings(settings)
            | AppResponse::SetVoiceProfilesEnabled(settings) => {
                self.settings = Some(settings.clone());
                true
            }
            AppResponse::ListSpeakerSuggestions(suggestions) => {
                self.suggestions = Some(suggestions.clone());
                true
            }
            _ => false,
        }
    }
}

/// One roster row as the picker offers it.
#[must_use]
pub fn picker_person(person: &PersonDto) -> PickerPerson {
    let detail = match person.recordings {
        0 => "not assigned yet".to_owned(),
        1 => "in 1 recording".to_owned(),
        count => format!("in {count} recordings"),
    };
    PickerPerson::new(person.person_id.to_string(), person.display_name.clone())
        .with_aliases(person.aliases.clone())
        .with_detail(detail)
}

/// What one frame of the screen asked for.
#[derive(Debug, Default)]
pub struct PeopleOutput {
    /// Commands to dispatch, in the order they were raised.
    pub commands: Vec<AppCommand>,
    /// A recording the user asked to open, by id.
    pub open_recording: Option<String>,
}

impl PeopleOutput {
    /// Raise a command.
    pub fn command(&mut self, command: AppCommand) {
        self.commands.push(command);
    }

    /// The command names, in order. For assertions.
    #[must_use]
    pub fn names(&self) -> Vec<&'static str> {
        self.commands.iter().map(AppCommand::name).collect()
    }
}

/// An open dialog, and the disclosure it is waiting for.
#[derive(Debug, Clone, PartialEq)]
enum Dialog {
    /// Create a roster entry.
    Create { name: String },
    /// Edit name, aliases and contacts (VP-213a).
    Edit {
        person_id: String,
        name: String,
        aliases: String,
        email: String,
        organisation: String,
        recordings: u32,
    },
    /// Merge two entries (VP-213c).
    Merge {
        loser: String,
        loser_name: String,
        survivor: String,
        survivor_name: String,
        disclosure: Option<MergePeopleResponse>,
    },
    /// Delete an entry (VP-080, VP-213d).
    DeletePerson {
        person_id: String,
        name: String,
        disclosure: Option<DeletePersonResponse>,
    },
    /// Delete one person's voiceprint (VP-123).
    DeleteEnrollment {
        person_id: String,
        name: String,
        reason: String,
        disclosure: Option<DeleteVoiceEnrollmentResponse>,
    },
    /// Split an enrolment's exemplars by provenance (§3.5, VP-091).
    Split {
        person_id: String,
        name: String,
        /// The provenance list, once `list_voice_exemplars` answered.
        exemplars: Option<ListVoiceExemplarsResponse>,
        /// Exemplar ids the user ticked, in the order the list shows them.
        selected: Vec<String>,
        /// Who the ticked exemplars move to.
        target: SplitTargetChoice,
        /// Whether the attestation text has been read to the end.
        read: bool,
        /// Whether the user attested that the *target* agreed. Never
        /// pre-ticked: the speech turned out to be somebody else's, so
        /// somebody else has to have agreed (VP-120 level 3).
        attested: bool,
        /// Retention term for the target's new enrolment.
        retention_months: u8,
        /// The user's own note of the consent basis.
        note: String,
    },
}

/// Where a split's exemplars go.
#[derive(Debug, Clone, PartialEq, Eq)]
enum SplitTargetChoice {
    /// A roster entry to create with this display name.
    New(String),
    /// A roster entry that already exists.
    Existing {
        /// Their id.
        person_id: String,
        /// Their name, for the attestation sentence.
        name: String,
    },
}

/// The People destination.
#[derive(Debug, Default)]
pub struct PeopleScreen {
    data: PeopleData,
    tab: PeopleTab,
    query: String,
    queue: ReviewQueue,
    dialog: Option<Dialog>,
    error: Option<ErrorEnvelope>,
    /// A correction waiting for the roster entry it names to exist: the
    /// suggestion `create_person` was raised for (VP-217's create-and-assign
    /// inside the queue).
    pending_correction: Option<String>,
    /// Wall clock in milliseconds. `None` reads the real clock; a test fixes
    /// it so the retention countdown it asserts on cannot drift.
    now_ms: Option<i64>,
}

impl PeopleScreen {
    /// A screen that has read nothing yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The cached answers, for a host or a test that inspects them.
    #[must_use]
    pub const fn data(&self) -> &PeopleData {
        &self.data
    }

    /// The cached answers, mutably, for a test that seeds them.
    pub const fn data_mut(&mut self) -> &mut PeopleData {
        &mut self.data
    }

    /// The review queue, for a test that inspects its cursor.
    #[must_use]
    pub const fn queue(&self) -> &ReviewQueue {
        &self.queue
    }

    /// Which half of the destination is shown.
    #[must_use]
    pub const fn tab(&self) -> PeopleTab {
        self.tab
    }

    /// Show one half of the destination.
    pub const fn show_tab(&mut self, tab: PeopleTab) {
        self.tab = tab;
    }

    /// Fix the wall clock the retention countdown is measured against.
    ///
    /// Only a test needs this; the shipped binary reads the real clock.
    pub const fn set_now_ms(&mut self, now_ms: i64) {
        self.now_ms = Some(now_ms);
    }

    /// The wall clock the countdown is measured against.
    fn clock(&self) -> i64 {
        self.now_ms.unwrap_or_else(|| {
            i64::try_from(crate::shell::intents::wall_clock_ms()).unwrap_or(i64::MAX)
        })
    }

    /// Fold a command outcome in, and report the follow-up reads it caused.
    ///
    /// A write that changed the roster invalidates the list the user is
    /// looking at, so the list is read again — once, for the write that
    /// happened, never on a timer.
    pub fn absorb(
        &mut self,
        command: &'static str,
        result: &Result<AppResponse, ErrorEnvelope>,
    ) -> Vec<AppCommand> {
        self.data.absorb(result);
        match result {
            Err(envelope) if is_ours(command) => {
                self.error = Some(envelope.clone());
                return Vec::new();
            }
            Err(_) => return Vec::new(),
            Ok(_) => self.error = None,
        }
        let Ok(response) = result else {
            return Vec::new();
        };
        match response {
            AppResponse::MergePeople(disclosure) => {
                if let Some(Dialog::Merge {
                    disclosure: slot, ..
                }) = self.dialog.as_mut()
                {
                    *slot = Some(disclosure.clone());
                }
                if disclosure.applied {
                    self.dialog = None;
                    return self.reread();
                }
                Vec::new()
            }
            AppResponse::DeletePerson(disclosure) => {
                if let Some(Dialog::DeletePerson {
                    disclosure: slot, ..
                }) = self.dialog.as_mut()
                {
                    *slot = Some(disclosure.clone());
                }
                if disclosure.applied {
                    self.dialog = None;
                    return self.reread();
                }
                Vec::new()
            }
            AppResponse::DeleteVoiceEnrollment(disclosure) => {
                if let Some(Dialog::DeleteEnrollment {
                    disclosure: slot, ..
                }) = self.dialog.as_mut()
                {
                    *slot = Some(disclosure.clone());
                }
                if disclosure.applied {
                    self.dialog = None;
                    return self.reread();
                }
                Vec::new()
            }
            AppResponse::ListVoiceExemplars(list) => {
                if let Some(Dialog::Split {
                    person_id,
                    exemplars,
                    ..
                }) = self.dialog.as_mut()
                {
                    if *person_id == list.person_id.to_string() {
                        *exemplars = Some(list.clone());
                    }
                }
                Vec::new()
            }
            AppResponse::SplitVoiceEnrollment(_) => {
                // The split happened: both enrolments changed, so the list
                // the user is looking at is stale.
                self.dialog = None;
                self.reread()
            }
            AppResponse::CreatePerson(created) => {
                self.dialog = None;
                // The queue's create-and-assign is two commands, and this is
                // the first half's answer: the person now exists, so the
                // correction that was waiting for them is sent with their id.
                let mut follow_ups = self.reread();
                if let Some(suggestion_id) = self.pending_correction.take() {
                    follow_ups.push(AppCommand::RejectSpeakerSuggestion(
                        RejectSpeakerSuggestionRequest {
                            suggestion_id,
                            corrected_person_id: Some(created.person.person_id.to_string()),
                            overwrite_segment_exceptions: false,
                        },
                    ));
                }
                follow_ups
            }
            AppResponse::UpdatePerson(_) => {
                self.dialog = None;
                self.reread()
            }
            AppResponse::ConfirmSpeakerSuggestion(decision)
            | AppResponse::RejectSpeakerSuggestion(decision) => {
                self.queue.absorb_decision(decision);
                if !decision.applied && decision.segment_exceptions > 0 {
                    // Nothing was written, so nothing is stale: the queue is
                    // showing the keep-or-overwrite choice instead.
                    return Vec::new();
                }
                vec![
                    AppCommand::ListSpeakerSuggestions(ListSpeakerSuggestionsRequest {
                        recording_id: None,
                        limit: Some(QUEUE_PAGE_SIZE),
                    }),
                    self.list_command(),
                ]
            }
            _ => Vec::new(),
        }
    }

    /// Read the roster again, with the filter the user is looking at.
    fn reread(&self) -> Vec<AppCommand> {
        vec![self.list_command()]
    }

    /// `list_people` with the filter the user is looking at.
    #[must_use]
    pub fn list_command(&self) -> AppCommand {
        let query = self.query.trim();
        AppCommand::ListPeople(ListPeopleRequest {
            query: (!query.is_empty()).then(|| query.to_owned()),
        })
    }

    /// Draw the screen and report what one frame asked for.
    pub fn ui(&mut self, ui: &mut Ui) -> PeopleOutput {
        let mut out = PeopleOutput::default();
        ui.label(egui::RichText::new("People").heading());
        ui.label(
            "One list of people for the whole library. Every recording assigns speakers from it.",
        );

        ui.horizontal_wrapped(|ui| {
            for tab in [PeopleTab::Roster, PeopleTab::Review] {
                if tab == PeopleTab::Review && !self.data.matching_on() {
                    continue;
                }
                if ui.selectable_label(self.tab == tab, tab.label()).clicked() {
                    self.tab = tab;
                }
            }
        });
        if !self.data.matching_on() && self.tab == PeopleTab::Review {
            self.tab = PeopleTab::Roster;
        }

        if let Some(envelope) = self.error.clone() {
            ui.label(format!("The request was refused: {}", envelope.message));
            ui.label(format!("Diagnostic id {}", envelope.diagnostic_id));
            if ui.button("Dismiss this message").clicked() {
                self.error = None;
            }
        }

        match self.tab {
            PeopleTab::Roster => self.roster_tab(ui, &mut out),
            PeopleTab::Review => {
                let people = self.data.picker_people();
                let decisions = self.queue.ui(ui, self.data.suggestions.as_ref(), &people);
                for decision in decisions {
                    if let SuggestionDecision::CreateAndCorrect { suggestion_id, .. } = &decision {
                        self.pending_correction = Some(suggestion_id.clone());
                    }
                    out.command(decision.into_command());
                }
            }
        }

        if self.dialog.is_some() {
            self.dialog_ui(ui, &mut out);
        }
        out
    }

    /// The roster half.
    fn roster_tab(&mut self, ui: &mut Ui, out: &mut PeopleOutput) {
        ui.horizontal_wrapped(|ui| {
            let label = ui.label("Search people");
            let field = ui
                .add(
                    egui::TextEdit::singleline(&mut self.query)
                        .hint_text("Name or alias")
                        .desired_width(240.0),
                )
                .labelled_by(label.id);
            if field.changed() {
                out.command(self.list_command());
            }
            if ui.button("Add a person").clicked() {
                self.dialog = Some(Dialog::Create {
                    name: self.query.trim().to_owned(),
                });
            }
        });

        let matching_on = self.data.matching_on();
        let Some(response) = self.data.people.clone() else {
            ui.label("The list has not been read yet.");
            return;
        };
        let action = roster::list(ui, &response, matching_on, self.clock());
        match action {
            Some(roster::RosterAction::Edit(person_id)) => {
                if let Some(person) = self.data.person(&person_id) {
                    self.dialog = Some(Dialog::Edit {
                        person_id,
                        name: person.display_name.clone(),
                        aliases: person.aliases.join(", "),
                        email: person.email.clone().unwrap_or_default(),
                        organisation: person.organisation.clone().unwrap_or_default(),
                        recordings: person.recordings,
                    });
                }
            }
            Some(roster::RosterAction::Merge { loser, survivor }) => {
                let loser_name = self.name_of(&loser);
                let survivor_name = self.name_of(&survivor);
                self.dialog = Some(Dialog::Merge {
                    loser: loser.clone(),
                    loser_name,
                    survivor: survivor.clone(),
                    survivor_name,
                    disclosure: None,
                });
                out.command(AppCommand::MergePeople(MergePeopleRequest {
                    loser_person_id: loser,
                    survivor_person_id: survivor,
                    apply: false,
                }));
            }
            Some(roster::RosterAction::SplitEnrollment(person_id)) => {
                let name = self.name_of(&person_id);
                self.dialog = Some(Dialog::Split {
                    person_id: person_id.clone(),
                    name,
                    exemplars: None,
                    selected: Vec::new(),
                    target: SplitTargetChoice::New(String::new()),
                    read: false,
                    attested: false,
                    retention_months: self
                        .data
                        .settings
                        .as_ref()
                        .map_or(MAX_RETENTION_MONTHS, |settings| {
                            settings.disclosure.retention_default_months
                        }),
                    note: String::new(),
                });
                out.command(AppCommand::ListVoiceExemplars(ListVoiceExemplarsRequest {
                    person_id,
                }));
            }
            Some(roster::RosterAction::Delete(person_id)) => {
                let name = self.name_of(&person_id);
                self.dialog = Some(Dialog::DeletePerson {
                    person_id: person_id.clone(),
                    name,
                    disclosure: None,
                });
                out.command(AppCommand::DeletePerson(DeletePersonRequest {
                    person_id,
                    apply: false,
                }));
            }
            Some(roster::RosterAction::DeleteEnrollment(person_id)) => {
                let name = self.name_of(&person_id);
                self.dialog = Some(Dialog::DeleteEnrollment {
                    person_id: person_id.clone(),
                    name,
                    reason: String::new(),
                    disclosure: None,
                });
                out.command(AppCommand::DeleteVoiceEnrollment(
                    DeleteVoiceEnrollmentRequest {
                        person_id,
                        apply: false,
                        reason: None,
                    },
                ));
            }
            None => {}
        }
    }

    /// The display name of a roster entry, or its id when the list moved
    /// underneath the dialog.
    fn name_of(&self, person_id: &str) -> String {
        self.data.person(person_id).map_or_else(
            || person_id.to_owned(),
            |person| person.display_name.clone(),
        )
    }

    /// Whichever dialog is open.
    fn dialog_ui(&mut self, ui: &mut Ui, out: &mut PeopleOutput) {
        let Some(dialog) = self.dialog.clone() else {
            return;
        };
        let mut next = Some(dialog.clone());
        match dialog {
            Dialog::Create { mut name } => {
                let outcome = modal(ui, "Add a person", "Add this person", |ui| {
                    let label = ui.label("Name");
                    ui.add(
                        egui::TextEdit::singleline(&mut name)
                            .char_limit(MAX_NAME_CHARS)
                            .desired_width(260.0),
                    )
                    .labelled_by(label.id);
                    ui.label(ROSTER_IS_NOT_BIOMETRIC);
                    let typed = name.trim().to_owned();
                    let duplicate = self
                        .data
                        .people()
                        .iter()
                        .any(|person| person.display_name.eq_ignore_ascii_case(&typed));
                    if duplicate && !typed.is_empty() {
                        ui.label(format!(
                            "{typed} is already on the list. Adding a second one is allowed, and \
                             the two can be merged later."
                        ));
                    }
                    !typed.is_empty()
                });
                match outcome {
                    ModalOutcome::Confirmed => {
                        out.command(AppCommand::CreatePerson(CreatePersonRequest {
                            display_name: name.trim().to_owned(),
                            aliases: Vec::new(),
                            email: None,
                            organisation: None,
                        }));
                        next = Some(Dialog::Create { name });
                    }
                    ModalOutcome::Cancelled => next = None,
                    ModalOutcome::Pending => next = Some(Dialog::Create { name }),
                }
            }
            Dialog::Edit {
                person_id,
                mut name,
                mut aliases,
                mut email,
                mut organisation,
                recordings,
            } => {
                let outcome = modal(ui, "Edit this person", "Save these changes", |ui| {
                    let label = ui.label("Name");
                    ui.add(
                        egui::TextEdit::singleline(&mut name)
                            .char_limit(MAX_NAME_CHARS)
                            .desired_width(260.0),
                    )
                    .labelled_by(label.id);
                    ui.label(blast_radius(recordings));
                    ui.label(
                        "Renaming changes the name every recording shows. It changes no \
                         assignment: the recordings still point at this person.",
                    );
                    let label = ui.label("Aliases, separated by commas");
                    ui.add(egui::TextEdit::singleline(&mut aliases).desired_width(260.0))
                        .labelled_by(label.id);
                    let label = ui.label("Email");
                    ui.add(egui::TextEdit::singleline(&mut email).desired_width(260.0))
                        .labelled_by(label.id);
                    let label = ui.label("Organisation");
                    ui.add(egui::TextEdit::singleline(&mut organisation).desired_width(260.0))
                        .labelled_by(label.id);
                    !name.trim().is_empty()
                });
                match outcome {
                    ModalOutcome::Confirmed => {
                        out.command(AppCommand::UpdatePerson(UpdatePersonRequest {
                            person_id: person_id.clone(),
                            display_name: name.trim().to_owned(),
                            aliases: aliases
                                .split(',')
                                .map(str::trim)
                                .filter(|alias| !alias.is_empty())
                                .map(ToOwned::to_owned)
                                .collect(),
                            email: optional(&email),
                            organisation: optional(&organisation),
                        }));
                    }
                    ModalOutcome::Cancelled => next = None,
                    ModalOutcome::Pending => {}
                }
                if let Some(Dialog::Edit { .. }) = next {
                    next = Some(Dialog::Edit {
                        person_id,
                        name,
                        aliases,
                        email,
                        organisation,
                        recordings,
                    });
                }
            }
            Dialog::Merge {
                loser,
                loser_name,
                survivor,
                survivor_name,
                disclosure,
            } => {
                let outcome = modal(ui, "Merge two people", "Merge them", |ui| {
                    ui.label(format!(
                        "Everything {loser_name} is linked to moves to {survivor_name}. \
                         {loser_name} is kept as a merged-away entry so the journal still \
                         explains what happened."
                    ));
                    match &disclosure {
                        None => {
                            ui.label("Counting what this would move…");
                            false
                        }
                        Some(counts) => {
                            ui.label(format!(
                                "{} speaker rows in {} recordings, {} stored exemplars and {} \
                                 pending suggestions move.",
                                counts.speaker_links,
                                counts.recordings,
                                counts.exemplars,
                                counts.pending_suggestions
                            ));
                            if counts.enrollment_moved {
                                ui.label(format!(
                                    "{loser_name}'s enrolled voice becomes {survivor_name}'s."
                                ));
                            }
                            ui.label(
                                "A merge is not in the transcript's undo stack. It is journalled, \
                                 and the only way back is to split the enrolment again.",
                            );
                            true
                        }
                    }
                });
                match outcome {
                    ModalOutcome::Confirmed => {
                        out.command(AppCommand::MergePeople(MergePeopleRequest {
                            loser_person_id: loser.clone(),
                            survivor_person_id: survivor.clone(),
                            apply: true,
                        }));
                    }
                    ModalOutcome::Cancelled => next = None,
                    ModalOutcome::Pending => {}
                }
                if let Some(Dialog::Merge { .. }) = next {
                    next = Some(Dialog::Merge {
                        loser,
                        loser_name,
                        survivor,
                        survivor_name,
                        disclosure,
                    });
                }
            }
            Dialog::DeletePerson {
                person_id,
                name,
                disclosure,
            } => {
                let outcome = modal(ui, "Delete this person", "Delete them for good", |ui| {
                    ui.label(format!(
                        "{name} is removed from the list. Every recording keeps the name it \
                         already showed, and no transcript text is touched."
                    ));
                    match &disclosure {
                        None => {
                            ui.label("Counting what this would reach…");
                            false
                        }
                        Some(counts) => {
                            ui.label(format!(
                                "{} speaker links in {} recordings are severed, {} stored \
                                 exemplars and {} suggestions are destroyed.",
                                counts.severed_speaker_links,
                                counts.recordings,
                                counts.exemplars,
                                counts.suggestions
                            ));
                            true
                        }
                    }
                });
                match outcome {
                    ModalOutcome::Confirmed => {
                        out.command(AppCommand::DeletePerson(DeletePersonRequest {
                            person_id: person_id.clone(),
                            apply: true,
                        }));
                    }
                    ModalOutcome::Cancelled => next = None,
                    ModalOutcome::Pending => {}
                }
                if let Some(Dialog::DeletePerson { .. }) = next {
                    next = Some(Dialog::DeletePerson {
                        person_id,
                        name,
                        disclosure,
                    });
                }
            }
            Dialog::DeleteEnrollment {
                person_id,
                name,
                mut reason,
                disclosure,
            } => {
                let outcome = modal(
                    ui,
                    "Delete this voiceprint",
                    "Delete the voiceprint for good",
                    |ui| {
                        ui.label(format!(
                        "{name} stays on the list. Only the stored voice goes, so this recording \
                         and every other one keep the names they already show."
                    ));
                        let label = ui.label("Why, if you want it recorded");
                        ui.add(
                            egui::TextEdit::singleline(&mut reason)
                                .hint_text("for example: withdrew consent")
                                .desired_width(260.0),
                        )
                        .labelled_by(label.id);
                        match &disclosure {
                            None => {
                                ui.label("Counting what this would destroy…");
                                false
                            }
                            Some(counts) => {
                                ui.label(format!(
                                    "{} stored exemplars and {} suggestions are destroyed.",
                                    counts.exemplars, counts.suggestions
                                ));
                                true
                            }
                        }
                    },
                );
                match outcome {
                    ModalOutcome::Confirmed => {
                        out.command(AppCommand::DeleteVoiceEnrollment(
                            DeleteVoiceEnrollmentRequest {
                                person_id: person_id.clone(),
                                apply: true,
                                reason: optional(&reason),
                            },
                        ));
                    }
                    ModalOutcome::Cancelled => next = None,
                    ModalOutcome::Pending => {}
                }
                if let Some(Dialog::DeleteEnrollment { .. }) = next {
                    next = Some(Dialog::DeleteEnrollment {
                        person_id,
                        name,
                        reason,
                        disclosure,
                    });
                }
            }
            Dialog::Split {
                person_id,
                name,
                exemplars,
                mut selected,
                mut target,
                mut read,
                mut attested,
                mut retention_months,
                mut note,
            } => {
                let roster: Vec<(String, String)> = self
                    .data
                    .people()
                    .iter()
                    .filter(|person| {
                        person.person_id.to_string() != person_id && person.status != "merged"
                    })
                    .map(|person| (person.person_id.to_string(), person.display_name.clone()))
                    .collect();
                let settings = self.data.settings.clone();
                let outcome = modal(
                    ui,
                    "Split this voiceprint",
                    "Move the ticked exemplars",
                    |ui| {
                        ui.label(format!(
                            "{name}'s voiceprint is made of separate exemplars, each from one \
                             recording. Tick the ones that turned out to be somebody else's \
                             voice; the rest stay with {name}."
                        ));
                        let Some(list) = exemplars.as_ref() else {
                            ui.label("Reading what this voiceprint is made of…");
                            return false;
                        };
                        if list.exemplars.is_empty() {
                            ui.label("This voiceprint has no exemplars left to move.");
                            return false;
                        }
                        for exemplar in &list.exemplars {
                            let id = exemplar.exemplar_id.to_string();
                            let mut ticked = selected.contains(&id);
                            if ui.checkbox(&mut ticked, exemplar_line(exemplar)).changed() {
                                if ticked {
                                    selected.push(id);
                                } else {
                                    selected.retain(|held| *held != id);
                                }
                            }
                        }
                        let moving = u32::try_from(selected.len()).unwrap_or(u32::MAX);
                        let total = u32::try_from(list.exemplars.len()).unwrap_or(u32::MAX);
                        ui.label(split_scope(moving, total));

                        ui.separator();
                        ui.label("Move them to");
                        if ui
                            .radio(matches!(target, SplitTargetChoice::New(_)), "a new person")
                            .clicked()
                            && !matches!(target, SplitTargetChoice::New(_))
                        {
                            target = SplitTargetChoice::New(String::new());
                        }
                        if let SplitTargetChoice::New(new_name) = &mut target {
                            let label = ui.label("Their name");
                            ui.add(
                                egui::TextEdit::singleline(new_name)
                                    .char_limit(MAX_NAME_CHARS)
                                    .desired_width(260.0),
                            )
                            .labelled_by(label.id);
                        }
                        ui.menu_button("an existing person…", |ui| {
                            for (id, display_name) in &roster {
                                if ui.button(display_name).clicked() {
                                    target = SplitTargetChoice::Existing {
                                        person_id: id.clone(),
                                        name: display_name.clone(),
                                    };
                                    ui.close();
                                }
                            }
                        });
                        if let SplitTargetChoice::Existing {
                            name: chosen_name, ..
                        } = &target
                        {
                            ui.label(format!("Moving them to {chosen_name}."));
                        }
                        let target_name = split_target_name(&target);

                        ui.separator();
                        ui.label(format!(
                            "The exemplars are re-sealed under {target_name}'s own key binding, \
                             so this is a new voiceprint for them — and it needs their consent, \
                             not {name}'s."
                        ));
                        if let Some(settings) = settings.as_ref() {
                            read |= crate::ai::speakers::disclosure_body(ui, settings);
                        } else {
                            ui.label(
                                "The consent text has not been read from the application yet.",
                            );
                        }
                        ui.add_enabled(
                            read,
                            egui::Checkbox::new(
                                &mut attested,
                                format!("{target_name} has agreed to this"),
                            ),
                        )
                        .on_disabled_hover_text("Read the text above to the end first.");
                        ui.horizontal_wrapped(|ui| {
                            let label = ui.label("Keep their voiceprint for");
                            let mut months = i32::from(retention_months);
                            ui.add(
                                egui::DragValue::new(&mut months)
                                    .range(
                                        i32::from(MIN_RETENTION_MONTHS)
                                            ..=i32::from(MAX_RETENTION_MONTHS),
                                    )
                                    .suffix(" months"),
                            )
                            .labelled_by(label.id);
                            retention_months = u8::try_from(months).unwrap_or(MAX_RETENTION_MONTHS);
                        });
                        let label =
                            ui.label("Your note of how they agreed, if you want one recorded");
                        ui.add(
                            egui::TextEdit::singleline(&mut note)
                                .hint_text("for example: agreed at the start of the meeting")
                                .desired_width(300.0),
                        )
                        .labelled_by(label.id);

                        moving > 0 && moving < total && !target_name.is_empty() && read && attested
                    },
                );
                match outcome {
                    ModalOutcome::Confirmed => {
                        let version = self
                            .data
                            .settings
                            .as_ref()
                            .map(|settings| settings.disclosure.version.clone())
                            .unwrap_or_default();
                        let (target_person_id, new_person_display_name) = match &target {
                            SplitTargetChoice::New(new_name) => {
                                (None, Some(new_name.trim().to_owned()))
                            }
                            SplitTargetChoice::Existing { person_id, .. } => {
                                (Some(person_id.clone()), None)
                            }
                        };
                        out.command(AppCommand::SplitVoiceEnrollment(
                            SplitVoiceEnrollmentRequest {
                                person_id: person_id.clone(),
                                exemplar_ids: selected.clone(),
                                target_person_id,
                                new_person_display_name,
                                consent: EnrolmentConsentDto {
                                    disclosure_version: version,
                                    person_attested: attested,
                                    retention_months: Some(retention_months),
                                    consent_note: optional(&note),
                                },
                            },
                        ));
                    }
                    ModalOutcome::Cancelled => next = None,
                    ModalOutcome::Pending => {}
                }
                if let Some(Dialog::Split { .. }) = next {
                    next = Some(Dialog::Split {
                        person_id,
                        name,
                        exemplars,
                        selected,
                        target,
                        read,
                        attested,
                        retention_months,
                        note,
                    });
                }
            }
        }
        self.dialog = next;
    }
}

/// The sentence a rename shows before it is saved (VP-213a).
#[must_use]
pub fn blast_radius(recordings: u32) -> String {
    match recordings {
        0 => "This name is shown in no recording yet.".to_owned(),
        1 => "This name is shown in 1 recording.".to_owned(),
        count => format!("This name is shown in {count} recordings."),
    }
}

/// One exemplar's provenance line: where the speech came from and how much of
/// it there was (VP-091, §3.5).
///
/// Provenance and duration only. The vector is not in the DTO and is not
/// anywhere on this screen; what a user needs to decide whether an exemplar
/// is somebody else's voice is which recording and which span it came from.
#[must_use]
pub fn exemplar_line(exemplar: &VoiceExemplarDto) -> String {
    let source = exemplar.recording_title.as_deref().map_or_else(
        || {
            exemplar.recording_id.map_or_else(
                || "a deleted recording".to_owned(),
                |id| format!("recording {id}"),
            )
        },
        ToOwned::to_owned,
    );
    let cluster = exemplar.cluster_key.as_deref().map_or_else(
        || "cluster unknown".to_owned(),
        |key| format!("cluster {key}"),
    );
    let span = match (exemplar.source_start_ms, exemplar.source_end_ms) {
        (Some(start), Some(end)) => format!("{} to {}", clock(start), clock(end)),
        _ => "span unknown".to_owned(),
    };
    format!(
        "{source} — {cluster} — {span} — {} s of speech",
        exemplar.speech_ms / 1_000
    )
}

/// A position in a recording, as a transcript reads it.
fn clock(ms: u64) -> String {
    let total = ms / 1_000;
    format!("{}:{:02}", total / 60, total % 60)
}

/// What the ticked exemplars would do, said before anything moves.
///
/// A split that moves everything is a merge, and the application refuses it
/// as one, so the sentence says that rather than letting the user find out
/// from an error (§3.5).
#[must_use]
pub fn split_scope(moving: u32, total: u32) -> String {
    if moving == 0 {
        return "Nothing is ticked yet, so nothing would move.".to_owned();
    }
    if moving >= total {
        return "Every exemplar is ticked. Moving all of them is a merge, not a split — untick \
                at least one, or merge the two people instead."
            .to_owned();
    }
    format!(
        "{moving} of {total} exemplar(s) move; {} stay.",
        total - moving
    )
}

/// The name a split's target is attested under.
fn split_target_name(target: &SplitTargetChoice) -> String {
    match target {
        SplitTargetChoice::New(name) => name.trim().to_owned(),
        SplitTargetChoice::Existing { name, .. } => name.clone(),
    }
}

/// `None` for an empty or whitespace-only field, so "cleared" and "typed one
/// space" are the same stored state.
fn optional(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

/// What a dialog returned this frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ModalOutcome {
    /// Still open.
    Pending,
    /// The user confirmed.
    Confirmed,
    /// The user backed out, with Escape or with Cancel.
    Cancelled,
}

/// A dialog whose body reports whether confirming is possible yet.
///
/// Escape cancels, which is the shell-wide rule for an overlay, and a
/// confirmation that has not been told what it would do cannot be given.
fn modal(
    ui: &mut Ui,
    title: &str,
    confirm_label: &str,
    body: impl FnOnce(&mut Ui) -> bool,
) -> ModalOutcome {
    let mut outcome = ModalOutcome::Pending;
    egui::Modal::new(egui::Id::new(("people-dialog", title))).show(ui.ctx(), |ui| {
        ui.set_min_width(380.0);
        ui.label(egui::RichText::new(title).heading().strong());
        ui.add_space(8.0);
        let ready = body(ui);
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            if ui
                .add_enabled(ready, egui::Button::new(confirm_label))
                .on_disabled_hover_text("Waiting for the application to say what this would do.")
                .clicked()
            {
                outcome = ModalOutcome::Confirmed;
            }
            if ui.button("Cancel").clicked() {
                outcome = ModalOutcome::Cancelled;
            }
        });
    });
    if ui.input(|input| input.key_pressed(egui::Key::Escape)) {
        outcome = ModalOutcome::Cancelled;
    }
    outcome
}

/// Whether a refused command was one of this screen's.
fn is_ours(command: &'static str) -> bool {
    matches!(
        command,
        "list_people"
            | "create_person"
            | "update_person"
            | "merge_people"
            | "delete_person"
            | "get_voice_profile_settings"
            | "list_speaker_suggestions"
            | "confirm_speaker_suggestion"
            | "reject_speaker_suggestion"
            | "delete_voice_enrollment"
            | "list_voice_exemplars"
            | "split_voice_enrollment"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_screen_opens_by_reading_the_roster_the_settings_and_the_queue() {
        let names: Vec<&str> = PeopleData::initial_commands()
            .iter()
            .map(AppCommand::name)
            .collect();
        assert_eq!(
            names,
            vec![
                "list_people",
                "get_voice_profile_settings",
                "list_speaker_suggestions"
            ]
        );
    }

    #[test]
    fn an_unanswered_settings_read_is_matching_off_not_matching_unknown() {
        let data = PeopleData::default();
        assert!(!data.matching_on());
    }

    #[test]
    fn the_rename_dialog_states_the_blast_radius_in_recordings() {
        assert_eq!(blast_radius(0), "This name is shown in no recording yet.");
        assert_eq!(blast_radius(1), "This name is shown in 1 recording.");
        assert_eq!(blast_radius(14), "This name is shown in 14 recordings.");
    }
}
