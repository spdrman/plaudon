//! The per-recording speaker panel (`VOICE_PROFILES_SPEC` VP-092, VP-094,
//! VP-211, VP-212, VP-215, VP-216).
//!
//! One block per diarization cluster. Each one carries, in this order: the
//! neutral label and its qualifier, the **person picker** (the same widget the
//! transcript chip and the review queue use), the suggestion for that cluster
//! if a match job produced one, the T12 name controls, and — only when
//! biometric matching is on and this build has a model — "Enrol this
//! speaker's voice…".
//!
//! With matching off the panel is the same panel minus the suggestion rows and
//! the enrolment affordance (VP-215): the picker, the assignment, its scope
//! and its undo are the same code, because there is no second manual
//! implementation to drift.
//!
//! Three rules are structural.
//!
//! * **Scope is what the control says.** The picker on a cluster row assigns
//!   that cluster in *this* recording (`assign_cluster_person`); nothing here
//!   can reach another recording (VP-211, VP-213b).
//! * **A refused overwrite is a question, not a silent no-op.** When segments
//!   were attributed individually the application answers `applied: false`
//!   with their count, and the panel asks whether to keep or overwrite them.
//! * **Nothing is auto-assigned.** A suggestion is an offer; the sentence is
//!   [`plaudon_domain::speaker::suggestion`] and the only way it becomes a
//!   name is Confirm, or Not-them followed by a pick (ADR-0082, VP-216).

use std::collections::BTreeSet;

use egui::{accesskit, Ui};
use plaudon_app::command::AppCommand;
use plaudon_app::dto::{
    AssignClusterPersonRequest, ConfirmSpeakerRequest, ConfirmSpeakerSuggestionRequest,
    CreatePersonRequest, EnrollPersonVoiceRequest, EnrolmentConsentDto, ListSpeakersResponse,
    RejectSpeakerSuggestionRequest, RenameSpeakerRequest, ResetSpeakerRequest, SpeakerDto,
    SpeakerSuggestionDto, VoiceEnrollmentDto, VoiceProfileSettingsDto,
};
use plaudon_app::tone::Tone;
use plaudon_domain::speaker::{self, MatchBand, MAX_NAME_CHARS};

use crate::screens::people::picker::{
    PersonPick, PersonPicker, PersonPickerState, PickerPerson, PickerSuggestion,
};

use super::{fact, AiOutput, Badge, RecordingContext};

/// Shortest retention term a profile may carry, in months.
const MIN_RETENTION_MONTHS: u8 = 1;

/// Longest retention term a profile may carry, in months (VP-130).
const MAX_RETENTION_MONTHS: u8 = 12;

/// Said where enrolment is offered, so the consent duty is stated at the
/// control and not only in Settings.
pub const ENROL_LEAD: &str =
    "Enrolling stores a voiceprint of this speaker so later recordings can suggest their name.";

/// Everything the speaker panel needs about people and voice profiles.
///
/// Borrowed, never owned: the host holds the answers and hands them in, which
/// is what lets a kittest test render every state of this panel with no
/// dispatcher.
#[derive(Debug, Clone, Copy, Default)]
pub struct VoiceContext<'a> {
    /// The roster, as the picker offers it.
    pub people: &'a [PickerPerson],
    /// The voice-profile settings, once they have arrived.
    pub settings: Option<&'a VoiceProfileSettingsDto>,
    /// Pending suggestions for this recording.
    pub suggestions: &'a [SpeakerSuggestionDto],
}

impl VoiceContext<'_> {
    /// Whether biometric matching is on.
    ///
    /// Unknown settings read as off: the feature is off by default, and an
    /// enrolment affordance shown before the application answered would claim
    /// a state nobody reported (VP-003).
    #[must_use]
    pub fn matching_on(&self) -> bool {
        self.settings.is_some_and(|settings| settings.enabled)
    }

    /// Whether this build can compute an embedding at all.
    #[must_use]
    pub fn embedder_available(&self) -> bool {
        self.settings
            .is_some_and(|settings| settings.embedder_available)
    }

    /// The suggestion for one speaker row, if a match job produced one.
    #[must_use]
    pub fn suggestion_for(&self, speaker: &SpeakerDto) -> Option<&SpeakerSuggestionDto> {
        self.suggestions.iter().find(|suggestion| {
            suggestion.state == "pending"
                && (suggestion.speaker_id == Some(speaker.speaker_id)
                    || (speaker.cluster_key.is_some()
                        && speaker.cluster_key.as_deref() == Some(suggestion.cluster_key.as_str())))
        })
    }
}

/// One speaker's name field between frames.
#[derive(Debug)]
struct NameEdit {
    /// The row this buffer belongs to.
    speaker_id: String,
    /// What the user has typed.
    text: String,
    /// The stored name the buffer was seeded from, so a rename, a reset or an
    /// answer that arrived from elsewhere reseeds it instead of leaving a
    /// stale draft behind.
    seeded_from: Option<String>,
}

/// The enrolment flow, while it is open (VP-094, VP-120 level 3).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Enrolment {
    /// Roster person the voiceprint is attached to.
    person_id: String,
    /// Their name, for the attestation sentence.
    person_name: String,
    /// Recording the speech comes from.
    recording_id: String,
    /// Cluster within this recording's newest diarization run.
    cluster_key: String,
    /// The neutral label of that cluster, so the user knows which voice.
    speaker_label: String,
    /// Whether the gate text has been read to the end.
    read: bool,
    /// Whether the user attested that this person agreed. Never pre-ticked.
    attested: bool,
    /// Retention term in months.
    retention_months: u8,
    /// The user's own note of the consent basis.
    note: String,
}

/// A cluster assignment the application refused, and what it was.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Blocked {
    /// Speaker row the assignment was for.
    speaker_id: String,
    /// Person the user chose, `None` for "Unknown/none".
    person_id: Option<String>,
    /// Segments that had been attributed individually.
    exceptions: u32,
}

/// A create-and-assign waiting for the roster entry it names to exist.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PendingCreate {
    /// Speaker row the new person will be assigned to.
    speaker_id: String,
    /// Recording that row belongs to.
    recording_id: String,
}

/// The speaker panel of one recording.
#[derive(Debug, Default)]
pub struct SpeakerPanel {
    name_edits: Vec<NameEdit>,
    pickers: Vec<(String, PersonPickerState)>,
    correcting: Option<String>,
    correct_picker: PersonPickerState,
    enrolment: Option<Enrolment>,
    pending_create: Option<PendingCreate>,
    /// A cluster assignment the application refused because segments were
    /// attributed individually: the speaker row, who was being assigned and
    /// how many segments were set by hand (VP-211).
    blocked: Option<Blocked>,
    /// The assignment waiting for an answer, so a refusal can be offered
    /// again with the person the user actually chose.
    pending_assignment: Option<(String, Option<String>)>,
    /// Rows whose current name arrived by confirming a voice match in this
    /// session, which is the only way the panel can know that provenance
    /// (VP-172).
    via_voice_match: BTreeSet<String>,
    /// The enrolment receipt, once one has been produced.
    enrolled: Option<VoiceEnrollmentDto>,
    /// A refusal to show where the action was taken.
    refusal: Option<String>,
}

impl SpeakerPanel {
    /// Draw the panel.
    pub fn ui(
        &mut self,
        ui: &mut Ui,
        context: RecordingContext<'_>,
        speakers: Option<&ListSpeakersResponse>,
        voice: VoiceContext<'_>,
    ) -> AiOutput {
        let mut output = AiOutput::default();
        ui.label(egui::RichText::new("Speaker identification").heading());
        if !voice.matching_on() {
            ui.label(
                "Voice matching is off. Names are yours to assign, and no voiceprint is computed \
                 or stored.",
            );
        }
        if let Some(refusal) = self.refusal.clone() {
            announce(ui, &refusal);
            if ui.button("Dismiss this message").clicked() {
                self.refusal = None;
            }
        }
        let Some(response) = speakers else {
            ui.label("Speakers have not been read yet.");
            return output;
        };
        if response.speakers.is_empty() {
            ui.label("No speaker was identified for this recording.");
            return output;
        }
        for speaker in &response.speakers {
            let speaker_id = speaker.speaker_id.to_string();
            ui.push_id(speaker_id.clone(), |ui| {
                ui.group(|ui| {
                    self.row(ui, context, speaker, voice, &mut output);
                });
            });
        }
        if self.enrolment.is_some() {
            self.enrolment_modal(ui, voice, &mut output);
        }
        output
    }

    /// One cluster's block.
    fn row(
        &mut self,
        ui: &mut Ui,
        context: RecordingContext<'_>,
        speaker: &SpeakerDto,
        voice: VoiceContext<'_>,
        output: &mut AiOutput,
    ) {
        let speaker_id = speaker.speaker_id.to_string();
        let via_match = self.via_voice_match.contains(&speaker_id);
        let shown = if via_match {
            speaker::display_via_voice_match(
                &speaker.label,
                speaker.name.as_deref(),
                speaker.confirmed,
            )
        } else {
            speaker::display(&speaker.label, speaker.name.as_deref(), speaker.confirmed)
        };
        fact(ui, "Speaker", &shown);
        fact(ui, "Machine label", &speaker.original_label);
        if speaker.confirmed {
            Badge::new("✓", "Confirmed by you", Tone::Success).ui(ui);
        } else {
            if let Some(name) = speaker.name.as_deref() {
                fact(ui, "Name you typed, not yet confirmed", name);
            }
            Badge::new("?", "Not confirmed", Tone::Neutral)
                .description(
                    "The engine proposed this cluster; nobody has confirmed whose voice it is.",
                )
                .ui(ui);
        }

        self.picker_row(ui, context, speaker, voice, output);
        if voice.matching_on() {
            if let Some(suggestion) = voice.suggestion_for(speaker) {
                self.suggestion_row(ui, suggestion, voice, output);
            }
        }
        self.name_controls(ui, context, speaker, output);
        self.enrol_control(ui, context, speaker, voice);
    }

    /// The person picker and the scope question it can raise.
    fn picker_row(
        &mut self,
        ui: &mut Ui,
        context: RecordingContext<'_>,
        speaker: &SpeakerDto,
        voice: VoiceContext<'_>,
        output: &mut AiOutput,
    ) {
        let speaker_id = speaker.speaker_id.to_string();
        let selected = speaker.person_id.map(|id| id.to_string());
        let label = format!("Assign all of {} to", speaker.original_label);
        let suggestion = voice
            .matching_on()
            .then(|| voice.suggestion_for(speaker))
            .flatten()
            .and_then(|suggestion| {
                MatchBand::from_column(&suggestion.band).map(|band| PickerSuggestion {
                    person: PickerPerson::new(
                        suggestion.person.person_id.to_string(),
                        suggestion.person.display_name.clone(),
                    ),
                    band,
                    score: suggestion.score,
                })
            });
        let slot = self.picker_slot(&speaker_id);
        let picked = PersonPicker::new("cluster-picker", &label, voice.people)
            .selected(selected.as_deref())
            .neutral_label(&speaker.original_label)
            .suggestion(suggestion.as_ref())
            .allow_create(true)
            .show(ui, &mut self.pickers[slot].1);

        match picked {
            Some(PersonPick::Person(person_id)) => {
                // Remembered, so a refusal can be offered again as the same
                // decision rather than one recomposed from the screen.
                self.pending_assignment = Some((speaker_id.clone(), Some(person_id.clone())));
                output.command(AppCommand::AssignClusterPerson(
                    AssignClusterPersonRequest {
                        recording_id: context.recording_id.to_owned(),
                        speaker_id: speaker_id.clone(),
                        person_id: Some(person_id),
                        overwrite_segment_exceptions: false,
                    },
                ));
            }
            Some(PersonPick::Unknown) => {
                self.pending_assignment = Some((speaker_id.clone(), None));
                output.command(AppCommand::AssignClusterPerson(
                    AssignClusterPersonRequest {
                        recording_id: context.recording_id.to_owned(),
                        speaker_id: speaker_id.clone(),
                        person_id: None,
                        overwrite_segment_exceptions: false,
                    },
                ));
            }
            Some(PersonPick::Create(name)) => {
                self.pending_create = Some(PendingCreate {
                    speaker_id: speaker_id.clone(),
                    recording_id: context.recording_id.to_owned(),
                });
                output.command(AppCommand::CreatePerson(CreatePersonRequest {
                    display_name: name,
                    aliases: Vec::new(),
                    email: None,
                    organisation: None,
                }));
            }
            None => {}
        }

        if let Some(blocked) = self.blocked.clone() {
            if blocked.speaker_id == speaker_id {
                announce(ui, &exception_warning(blocked.exceptions));
                ui.horizontal_wrapped(|ui| {
                    if ui.button("Keep those segments as they are").clicked() {
                        self.blocked = None;
                    }
                    if ui.button("Overwrite them").clicked() {
                        self.blocked = None;
                        // The same assignment the user asked for, not one
                        // recomposed from what is on screen: the refusal
                        // changed nothing, so the choice still stands.
                        output.command(AppCommand::AssignClusterPerson(
                            AssignClusterPersonRequest {
                                recording_id: context.recording_id.to_owned(),
                                speaker_id: speaker_id.clone(),
                                person_id: blocked.person_id.clone(),
                                overwrite_segment_exceptions: true,
                            },
                        ));
                    }
                });
            }
        }
    }

    /// The suggestion for one cluster: confirm, or not-them then pick.
    fn suggestion_row(
        &mut self,
        ui: &mut Ui,
        suggestion: &SpeakerSuggestionDto,
        voice: VoiceContext<'_>,
        output: &mut AiOutput,
    ) {
        let suggestion_id = suggestion.suggestion_id.to_string();
        let sentence = MatchBand::from_column(&suggestion.band).map_or_else(
            || {
                format!(
                    "sounds like {} — match score {:.2}",
                    suggestion.person.display_name, suggestion.score
                )
            },
            |band| speaker::suggestion(&suggestion.person.display_name, band, suggestion.score),
        );
        ui.label(&sentence).on_hover_text(speaker::SCORE_TOOLTIP);
        ui.horizontal_wrapped(|ui| {
            if ui
                .button(format!("Confirm {}", suggestion.person.display_name))
                .clicked()
            {
                self.via_voice_match
                    .extend(suggestion.speaker_id.map(|id| id.to_string()));
                output.command(AppCommand::ConfirmSpeakerSuggestion(
                    ConfirmSpeakerSuggestionRequest {
                        suggestion_id: suggestion_id.clone(),
                        overwrite_segment_exceptions: false,
                    },
                ));
            }
            if ui.button("Not them").clicked() {
                self.correcting = Some(suggestion_id.clone());
                self.correct_picker.reset();
                output.command(AppCommand::RejectSpeakerSuggestion(
                    RejectSpeakerSuggestionRequest {
                        suggestion_id: suggestion_id.clone(),
                        corrected_person_id: None,
                        overwrite_segment_exceptions: false,
                    },
                ));
            }
        });

        if self.correcting.as_deref() == Some(suggestion_id.as_str()) {
            ui.label("Rejected. Who was it?");
            let picked = PersonPicker::new(
                "correction-picker",
                "Who was speaking in this cluster",
                voice.people,
            )
            .neutral_label("Nobody yet")
            .show(ui, &mut self.correct_picker);
            if let Some(PersonPick::Person(person_id)) = picked {
                self.correcting = None;
                output.command(AppCommand::RejectSpeakerSuggestion(
                    RejectSpeakerSuggestionRequest {
                        suggestion_id,
                        corrected_person_id: Some(person_id),
                        overwrite_segment_exceptions: false,
                    },
                ));
            }
        }
    }

    /// The T12 name controls: rename, confirm, reset.
    fn name_controls(
        &mut self,
        ui: &mut Ui,
        context: RecordingContext<'_>,
        speaker: &SpeakerDto,
        output: &mut AiOutput,
    ) {
        let speaker_id = speaker.speaker_id.to_string();
        let slot = self.name_slot(&speaker_id, speaker.name.as_deref());
        ui.horizontal_wrapped(|ui| {
            let name_label = ui.label("Speaker name");
            ui.add(
                egui::TextEdit::singleline(&mut self.name_edits[slot].text)
                    .desired_width(180.0)
                    .char_limit(MAX_NAME_CHARS),
            )
            .labelled_by(name_label.id)
            .on_hover_text(format!("At most {MAX_NAME_CHARS} characters."));

            let typed = self.name_edits[slot].text.trim();
            if ui
                .add_enabled(!typed.is_empty(), egui::Button::new("Rename speaker"))
                .on_disabled_hover_text(
                    "Type a name first — a speaker cannot be given an empty name.",
                )
                .clicked()
            {
                output.command(AppCommand::RenameSpeaker(RenameSpeakerRequest {
                    recording_id: context.recording_id.to_owned(),
                    speaker_id: speaker_id.clone(),
                    name: typed.to_owned(),
                }));
            }

            if !speaker.confirmed
                && ui
                    .add_enabled(speaker.name.is_some(), egui::Button::new("Confirm speaker"))
                    .on_disabled_hover_text(
                        "Name this speaker first; confirming is your claim that the name is right.",
                    )
                    .clicked()
            {
                output.command(AppCommand::ConfirmSpeaker(ConfirmSpeakerRequest {
                    recording_id: context.recording_id.to_owned(),
                    speaker_id: speaker_id.clone(),
                }));
            }

            let named = speaker.name.is_some() || speaker.confirmed;
            if ui
                .add_enabled(named, egui::Button::new("Reset to the machine label"))
                .on_disabled_hover_text("This speaker still carries the label the engine gave it.")
                .clicked()
            {
                output.command(AppCommand::ResetSpeaker(ResetSpeakerRequest {
                    recording_id: context.recording_id.to_owned(),
                    speaker_id: speaker_id.clone(),
                }));
            }
        });
    }

    /// "Enrol this speaker's voice…", where that is possible at all.
    fn enrol_control(
        &mut self,
        ui: &mut Ui,
        context: RecordingContext<'_>,
        speaker: &SpeakerDto,
        voice: VoiceContext<'_>,
    ) {
        if !voice.matching_on() {
            return;
        }
        let Some(cluster_key) = speaker.cluster_key.clone() else {
            return;
        };
        let person = speaker.person_id.map(|id| id.to_string());
        let enabled = person.is_some() && voice.embedder_available();
        let disabled_reason = if person.is_none() {
            "Assign a person to this speaker first: a voiceprint belongs to somebody named."
        } else {
            "This build has no speaker-embedding model, so nothing can be enrolled yet."
        };
        if ui
            .add_enabled(enabled, egui::Button::new("Enrol this speaker's voice…"))
            .on_disabled_hover_text(disabled_reason)
            .clicked()
        {
            let person_id = person.unwrap_or_default();
            let person_name = voice
                .people
                .iter()
                .find(|candidate| candidate.id == person_id)
                .map_or_else(|| "this person".to_owned(), |found| found.name.clone());
            self.enrolment = Some(Enrolment {
                person_id,
                person_name,
                recording_id: context.recording_id.to_owned(),
                cluster_key,
                speaker_label: speaker.original_label.clone(),
                read: false,
                attested: false,
                retention_months: voice.settings.map_or(MAX_RETENTION_MONTHS, |settings| {
                    settings.disclosure.retention_default_months
                }),
                note: String::new(),
            });
        }
    }

    /// The enrolment flow: the gate, the attestation, the minima, the term.
    fn enrolment_modal(&mut self, ui: &mut Ui, voice: VoiceContext<'_>, output: &mut AiOutput) {
        let Some(mut flow) = self.enrolment.clone() else {
            return;
        };
        let mut close = false;
        egui::Modal::new(egui::Id::new("voice-enrolment")).show(ui.ctx(), |ui| {
            ui.set_min_width(460.0);
            ui.label(
                egui::RichText::new(format!("Enrol the voice of {}", flow.person_name))
                    .heading()
                    .strong(),
            );
            ui.label(format!(
                "The speech comes from {} in this recording.",
                flow.speaker_label
            ));
            ui.label(ENROL_LEAD);
            if let Some(settings) = voice.settings {
                ui.label(format!(
                    "An enrolment needs at least {} s of clean speech from this speaker; the \
                     application refuses it rather than storing a weak voiceprint.",
                    settings.enrol_speech_ms / 1_000
                ));
                let read = disclosure_body(ui, settings);
                flow.read |= read;
            } else {
                ui.label("The consent text has not been read from the application yet.");
            }

            ui.separator();
            ui.add_enabled(
                flow.read,
                egui::Checkbox::new(
                    &mut flow.attested,
                    format!("{} has agreed to this", flow.person_name),
                ),
            )
            .on_disabled_hover_text("Read the text above to the end first.");

            ui.horizontal_wrapped(|ui| {
                let label = ui.label("Keep the voiceprint for");
                let mut months = i32::from(flow.retention_months);
                ui.add(
                    egui::DragValue::new(&mut months)
                        .range(i32::from(MIN_RETENTION_MONTHS)..=i32::from(MAX_RETENTION_MONTHS))
                        .suffix(" months"),
                )
                .labelled_by(label.id);
                flow.retention_months = u8::try_from(months).unwrap_or(MAX_RETENTION_MONTHS);
            });

            let label = ui.label("Your note of how they agreed, if you want one recorded");
            ui.add(
                egui::TextEdit::singleline(&mut flow.note)
                    .hint_text("for example: agreed at the start of the meeting")
                    .desired_width(380.0),
            )
            .labelled_by(label.id);

            ui.add_space(12.0);
            ui.horizontal(|ui| {
                let ready = flow.read && flow.attested;
                if ui
                    .add_enabled(ready, egui::Button::new("Enrol this voice"))
                    .on_disabled_hover_text(
                        "Read the text and confirm that this person agreed before enrolling.",
                    )
                    .clicked()
                {
                    let version = voice
                        .settings
                        .map(|settings| settings.disclosure.version.clone())
                        .unwrap_or_default();
                    output.command(AppCommand::EnrollPersonVoice(EnrollPersonVoiceRequest {
                        person_id: flow.person_id.clone(),
                        recording_id: flow.recording_id.clone(),
                        cluster_key: flow.cluster_key.clone(),
                        consent: EnrolmentConsentDto {
                            disclosure_version: version,
                            person_attested: flow.attested,
                            retention_months: Some(flow.retention_months),
                            consent_note: (!flow.note.trim().is_empty())
                                .then(|| flow.note.trim().to_owned()),
                        },
                    }));
                    close = true;
                }
                if ui.button("Cancel").clicked() {
                    close = true;
                }
            });
        });
        if ui.input(|input| input.key_pressed(egui::Key::Escape)) {
            close = true;
        }
        self.enrolment = if close { None } else { Some(flow) };
    }

    /// Fold an answer in, and report the follow-up commands it caused.
    ///
    /// The panel patches rather than refetches wherever the answer *is* what
    /// a read would say; the one follow-up it does raise is the second half of
    /// create-and-assign, which is two commands by construction.
    pub fn on_response(
        &mut self,
        command: &'static str,
        result: &Result<plaudon_app::command::AppResponse, plaudon_domain::error::ErrorEnvelope>,
    ) -> Vec<AppCommand> {
        use plaudon_app::command::AppResponse;
        match result {
            Err(envelope) => {
                if matches!(
                    command,
                    "assign_cluster_person"
                        | "enroll_person_voice"
                        | "confirm_speaker_suggestion"
                        | "reject_speaker_suggestion"
                        | "create_person"
                ) {
                    self.refusal = Some(envelope.message.clone());
                    self.pending_create = None;
                }
                Vec::new()
            }
            Ok(AppResponse::AssignClusterPerson(answer)) => {
                self.refusal = None;
                let speaker_id = answer.speaker.speaker_id.to_string();
                let asked = self.pending_assignment.take();
                if answer.applied || answer.segment_exceptions == 0 {
                    self.blocked = None;
                } else {
                    self.blocked = Some(Blocked {
                        person_id: asked
                            .filter(|(speaker, _)| *speaker == speaker_id)
                            .and_then(|(_, person)| person),
                        speaker_id,
                        exceptions: answer.segment_exceptions,
                    });
                }
                Vec::new()
            }
            Ok(
                AppResponse::ConfirmSpeakerSuggestion(decision)
                | AppResponse::RejectSpeakerSuggestion(decision),
            ) => {
                self.refusal = None;
                if !decision.applied && decision.segment_exceptions > 0 {
                    if let Some(speaker) = decision.speaker.as_ref() {
                        self.blocked = Some(Blocked {
                            speaker_id: speaker.speaker_id.to_string(),
                            person_id: decision
                                .person
                                .as_ref()
                                .map(|person| person.person_id.to_string()),
                            exceptions: decision.segment_exceptions,
                        });
                    }
                }
                Vec::new()
            }
            Ok(AppResponse::CreatePerson(created)) => {
                let Some(pending) = self.pending_create.take() else {
                    return Vec::new();
                };
                vec![AppCommand::AssignClusterPerson(
                    AssignClusterPersonRequest {
                        recording_id: pending.recording_id,
                        speaker_id: pending.speaker_id,
                        person_id: Some(created.person.person_id.to_string()),
                        overwrite_segment_exceptions: false,
                    },
                )]
            }
            Ok(AppResponse::EnrollPersonVoice(enrolment)) => {
                self.enrolled = Some(enrolment.clone());
                self.refusal = None;
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    /// The enrolment receipt, once one has been produced.
    #[must_use]
    pub const fn enrolled(&self) -> Option<&VoiceEnrollmentDto> {
        self.enrolled.as_ref()
    }

    /// The scope question on screen, if one is: the speaker row it is about
    /// and how many segments were set by hand.
    #[must_use]
    pub fn blocked(&self) -> Option<(&str, u32)> {
        self.blocked
            .as_ref()
            .map(|blocked| (blocked.speaker_id.as_str(), blocked.exceptions))
    }

    /// The picker state of one row, created on first use.
    fn picker_slot(&mut self, speaker_id: &str) -> usize {
        if let Some(index) = self.pickers.iter().position(|(held, _)| held == speaker_id) {
            return index;
        }
        self.pickers
            .push((speaker_id.to_owned(), PersonPickerState::default()));
        self.pickers.len() - 1
    }

    /// The edit buffer for one speaker, reseeded whenever the stored name
    /// moved underneath it.
    fn name_slot(&mut self, speaker_id: &str, stored: Option<&str>) -> usize {
        if let Some(index) = self
            .name_edits
            .iter()
            .position(|edit| edit.speaker_id == speaker_id)
        {
            if self.name_edits[index].seeded_from.as_deref() != stored {
                self.name_edits[index].seeded_from = stored.map(ToOwned::to_owned);
                stored
                    .unwrap_or_default()
                    .clone_into(&mut self.name_edits[index].text);
            }
            return index;
        }
        self.name_edits.push(NameEdit {
            speaker_id: speaker_id.to_owned(),
            text: stored.unwrap_or_default().to_owned(),
            seeded_from: stored.map(ToOwned::to_owned),
        });
        self.name_edits.len() - 1
    }
}

/// The activation disclosure, scrolled to its end.
///
/// Returns whether the reader has reached the bottom — which is also true for
/// a text short enough that there is nothing to scroll, because a gate nobody
/// can satisfy is not a gate.
fn disclosure_body(ui: &mut Ui, settings: &VoiceProfileSettingsDto) -> bool {
    let disclosure = &settings.disclosure;
    let output = egui::ScrollArea::vertical()
        .max_height(180.0)
        .id_salt("enrolment-gate")
        .show(ui, |ui| {
            ui.label(egui::RichText::new(&disclosure.title).strong());
            for paragraph in &disclosure.body {
                ui.label(paragraph);
            }
            ui.label(&disclosure.two_tier_sentence);
            ui.label(&disclosure.responsibility_sentence);
        });
    let viewport = output.inner_rect.height();
    let content = output.content_size.y;
    output.state.offset.y + viewport >= content - 1.0
}

/// The warning a cluster assignment shows when segments were set one by one
/// (VP-211).
#[must_use]
pub fn exception_warning(exceptions: u32) -> String {
    format!("{exceptions} segment(s) were set individually — keep them or overwrite them?")
}

/// A polite live region.
fn announce(ui: &mut Ui, text: &str) {
    let response = ui.label(text);
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, text));
    let announcement = text.to_owned();
    ui.ctx().accesskit_node_builder(response.id, |node| {
        node.set_role(accesskit::Role::Status);
        node.set_label(announcement);
        node.set_live(accesskit::Live::Polite);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matching_is_off_until_the_application_says_otherwise() {
        let context = VoiceContext::default();
        assert!(!context.matching_on());
        assert!(!context.embedder_available());
    }

    #[test]
    fn the_scope_question_names_the_segments_it_is_about() {
        assert_eq!(
            exception_warning(3),
            "3 segment(s) were set individually — keep them or overwrite them?"
        );
    }
}
