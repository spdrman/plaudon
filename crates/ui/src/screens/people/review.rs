//! The bulk review queue (`VOICE_PROFILES_SPEC` VP-093, VP-216, VP-217).
//!
//! One list of pending suggestions across recordings, completable from the
//! keyboard: `j`/`k` move, `Enter` confirms, `Delete` rejects, and `n` rejects
//! *and opens the picker*, so correcting a wrong suggestion never requires
//! leaving the queue.
//!
//! Two rules are structural rather than decorative.
//!
//! * **Nothing is pre-selected.** A suggestion is an offer; the row's
//!   sentence is [`plaudon_domain::speaker::suggestion`] and the only way a
//!   name reaches a recording is a key press or a click (ADR-0082).
//! * **A rejection renames nothing.** `Delete` marks the suggestion rejected
//!   and stops. Only the reject-then-pick path assigns anybody, and then it
//!   assigns whoever the user picked — not the person the score was about.

use egui::{accesskit, Key, Ui};
use plaudon_app::command::AppCommand;
use plaudon_app::dto::{
    ConfirmSpeakerSuggestionRequest, ListSpeakerSuggestionsResponse,
    RejectSpeakerSuggestionRequest, SpeakerSuggestionDecisionDto, SpeakerSuggestionDto,
};
use plaudon_domain::speaker::{self, MatchBand};

use super::picker::{PersonPick, PersonPicker, PersonPickerState, PickerPerson};

/// What the user decided about one suggestion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SuggestionDecision {
    /// Confirm it: the person is assigned to the cluster.
    Confirm {
        /// The suggestion.
        suggestion_id: String,
        /// Whether segments attributed individually may be overwritten.
        overwrite: bool,
    },
    /// Reject it, naming nobody. Nothing is renamed (VP-216).
    Reject {
        /// The suggestion.
        suggestion_id: String,
    },
    /// Reject it and say who it actually was.
    Correct {
        /// The suggestion.
        suggestion_id: String,
        /// Who it was.
        person_id: String,
    },
    /// Reject it, create a roster entry with this name, and assign them.
    CreateAndCorrect {
        /// The suggestion.
        suggestion_id: String,
        /// Name of the person to create.
        name: String,
    },
}

impl SuggestionDecision {
    /// The command this decision is, for the decisions that are one command.
    ///
    /// [`SuggestionDecision::CreateAndCorrect`] is two commands and is carried
    /// out by the screen, which holds the correction until the roster entry
    /// exists; it is reported here as the `create_person` half.
    #[must_use]
    pub fn into_command(self) -> AppCommand {
        match self {
            Self::Confirm {
                suggestion_id,
                overwrite,
            } => AppCommand::ConfirmSpeakerSuggestion(ConfirmSpeakerSuggestionRequest {
                suggestion_id,
                overwrite_segment_exceptions: overwrite,
            }),
            Self::Reject { suggestion_id } => {
                AppCommand::RejectSpeakerSuggestion(RejectSpeakerSuggestionRequest {
                    suggestion_id,
                    corrected_person_id: None,
                    overwrite_segment_exceptions: false,
                })
            }
            Self::Correct {
                suggestion_id,
                person_id,
            } => AppCommand::RejectSpeakerSuggestion(RejectSpeakerSuggestionRequest {
                suggestion_id,
                corrected_person_id: Some(person_id),
                overwrite_segment_exceptions: false,
            }),
            Self::CreateAndCorrect { name, .. } => {
                AppCommand::CreatePerson(plaudon_app::dto::CreatePersonRequest {
                    display_name: name,
                    aliases: Vec::new(),
                    email: None,
                    organisation: None,
                })
            }
        }
    }

    /// The suggestion this decision is about.
    #[must_use]
    pub fn suggestion_id(&self) -> &str {
        match self {
            Self::Confirm { suggestion_id, .. }
            | Self::Reject { suggestion_id }
            | Self::Correct { suggestion_id, .. }
            | Self::CreateAndCorrect { suggestion_id, .. } => suggestion_id,
        }
    }
}

/// The queue's own state: where the keyboard is, and which row is picking.
#[derive(Debug, Default)]
pub struct ReviewQueue {
    cursor: usize,
    picking: Option<String>,
    picker: PersonPickerState,
    /// A decision the application refused because segments were attributed
    /// individually: the suggestion, and how many (VP-211).
    blocked: Option<(String, u32)>,
}

impl ReviewQueue {
    /// Where the keyboard is.
    #[must_use]
    pub const fn cursor(&self) -> usize {
        self.cursor
    }

    /// The suggestion whose picker is open, if any.
    #[must_use]
    pub fn picking(&self) -> Option<&str> {
        self.picking.as_deref()
    }

    /// A decision was answered: the row it was about is gone, so the cursor
    /// stays where it is and the picker closes.
    pub fn settled(&mut self) {
        self.picking = None;
        self.picker.reset();
    }

    /// Fold a decision's answer in, so a refused assignment is offered again
    /// with its scope stated.
    pub fn absorb_decision(&mut self, decision: &SpeakerSuggestionDecisionDto) {
        if decision.applied || decision.segment_exceptions == 0 {
            self.blocked = None;
            self.settled();
            return;
        }
        self.blocked = Some((
            decision.suggestion_id.to_string(),
            decision.segment_exceptions,
        ));
    }

    /// Draw the queue and report the decisions one frame produced.
    pub fn ui(
        &mut self,
        ui: &mut Ui,
        suggestions: Option<&ListSpeakerSuggestionsResponse>,
        people: &[PickerPerson],
    ) -> Vec<SuggestionDecision> {
        let mut decisions = Vec::new();
        let Some(response) = suggestions else {
            ui.label("The review queue has not been read yet.");
            return decisions;
        };
        let rows: Vec<&SpeakerSuggestionDto> = response
            .suggestions
            .iter()
            .filter(|row| row.state == "pending")
            .collect();

        announce(
            ui,
            &format!(
                "{} suggestion(s) waiting for review. Nothing has been assigned.",
                response.pending_count
            ),
        );
        ui.label(
            "j and k move, Enter confirms, Delete rejects, n rejects and asks who it really was.",
        );
        if rows.is_empty() {
            ui.label("No suggestion is waiting. A match job produces these; they are never acted on by themselves.");
            return decisions;
        }
        self.cursor = self.cursor.min(rows.len() - 1);

        // While a picker is open it owns the keyboard: Enter there names a
        // person, and the queue must not read it as "confirm this suggestion".
        let key = if self.picking.is_some() {
            None
        } else {
            keyboard(ui, rows.len(), self.cursor)
        };
        if let Some(key) = key {
            match key {
                QueueKey::Move(to) => self.cursor = to,
                QueueKey::Confirm => {
                    decisions.push(SuggestionDecision::Confirm {
                        suggestion_id: rows[self.cursor].suggestion_id.to_string(),
                        overwrite: false,
                    });
                }
                QueueKey::Reject => {
                    decisions.push(SuggestionDecision::Reject {
                        suggestion_id: rows[self.cursor].suggestion_id.to_string(),
                    });
                }
                QueueKey::RejectAndPick => {
                    self.picking = Some(rows[self.cursor].suggestion_id.to_string());
                    self.picker.reset();
                }
            }
        }

        for (index, row) in rows.iter().enumerate() {
            let suggestion_id = row.suggestion_id.to_string();
            ui.push_id(suggestion_id.clone(), |ui| {
                ui.group(|ui| {
                    if index == self.cursor {
                        ui.label("▸ reviewing this one");
                    }
                    ui.label(
                        row.recording_title
                            .clone()
                            .unwrap_or_else(|| "A recording with no title".to_owned()),
                    );
                    ui.label(format!(
                        "Cluster {}{}",
                        row.cluster_key,
                        row.speaker_label
                            .as_ref()
                            .map(|label| format!(", shown as {label}"))
                            .unwrap_or_default()
                    ));
                    ui.label(sentence(row))
                        .on_hover_text(speaker::SCORE_TOOLTIP);

                    ui.horizontal_wrapped(|ui| {
                        if ui
                            .button(format!("Confirm {}", row.person.display_name))
                            .clicked()
                        {
                            decisions.push(SuggestionDecision::Confirm {
                                suggestion_id: suggestion_id.clone(),
                                overwrite: false,
                            });
                        }
                        if ui.button("Not them").clicked() {
                            decisions.push(SuggestionDecision::Reject {
                                suggestion_id: suggestion_id.clone(),
                            });
                        }
                        if ui.button("Not them — say who it was").clicked() {
                            self.picking = Some(suggestion_id.clone());
                            self.picker.reset();
                        }
                    });

                    if self.picking.as_deref() == Some(suggestion_id.as_str()) {
                        let label = format!("Who was speaking in cluster {}", row.cluster_key);
                        let picked = PersonPicker::new("review-picker", &label, people)
                            .neutral_label("Nobody yet")
                            .allow_create(true)
                            .show(ui, &mut self.picker);
                        match picked {
                            Some(PersonPick::Person(person_id)) => {
                                decisions.push(SuggestionDecision::Correct {
                                    suggestion_id: suggestion_id.clone(),
                                    person_id,
                                });
                            }
                            Some(PersonPick::Create(name)) => {
                                decisions.push(SuggestionDecision::CreateAndCorrect {
                                    suggestion_id: suggestion_id.clone(),
                                    name,
                                });
                            }
                            Some(PersonPick::Unknown) => {
                                decisions.push(SuggestionDecision::Reject {
                                    suggestion_id: suggestion_id.clone(),
                                });
                            }
                            None => {}
                        }
                    }

                    if let Some((blocked, exceptions)) = self.blocked.clone() {
                        if blocked == suggestion_id {
                            ui.label(exception_warning(exceptions));
                            ui.horizontal_wrapped(|ui| {
                                if ui.button("Keep those segments as they are").clicked() {
                                    self.blocked = None;
                                }
                                if ui.button("Overwrite them").clicked() {
                                    decisions.push(SuggestionDecision::Confirm {
                                        suggestion_id: suggestion_id.clone(),
                                        overwrite: true,
                                    });
                                    self.blocked = None;
                                }
                            });
                        }
                    }
                });
            });
        }
        decisions
    }
}

/// The sentence one suggestion reads as.
///
/// A band the store did not write is not invented here: an unknown band value
/// renders the score alone, because the qualitative word is the store's.
#[must_use]
pub fn sentence(row: &SpeakerSuggestionDto) -> String {
    MatchBand::from_column(&row.band).map_or_else(
        || {
            format!(
                "sounds like {} — match score {:.2}",
                row.person.display_name, row.score
            )
        },
        |band| speaker::suggestion(&row.person.display_name, band, row.score),
    )
}

/// The warning a cluster assignment shows when segments were set one by one
/// (VP-211).
#[must_use]
pub fn exception_warning(exceptions: u32) -> String {
    format!("{exceptions} segment(s) were set individually — keep them or overwrite them?")
}

/// What the keyboard asked for this frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QueueKey {
    /// Move the cursor.
    Move(usize),
    /// Confirm the suggestion under the cursor.
    Confirm,
    /// Reject it, naming nobody.
    Reject,
    /// Reject it and open the picker.
    RejectAndPick,
}

/// Read the queue's keys.
fn keyboard(ui: &Ui, count: usize, cursor: usize) -> Option<QueueKey> {
    if count == 0 {
        return None;
    }
    ui.input_mut(|input| {
        if input.consume_key(egui::Modifiers::NONE, Key::J)
            || input.consume_key(egui::Modifiers::NONE, Key::ArrowDown)
        {
            return Some(QueueKey::Move((cursor + 1) % count));
        }
        if input.consume_key(egui::Modifiers::NONE, Key::K)
            || input.consume_key(egui::Modifiers::NONE, Key::ArrowUp)
        {
            return Some(QueueKey::Move((cursor + count - 1) % count));
        }
        if input.consume_key(egui::Modifiers::NONE, Key::Enter) {
            return Some(QueueKey::Confirm);
        }
        if input.consume_key(egui::Modifiers::NONE, Key::Delete)
            || input.consume_key(egui::Modifiers::NONE, Key::Backspace)
        {
            return Some(QueueKey::Reject);
        }
        if input.consume_key(egui::Modifiers::NONE, Key::N) {
            return Some(QueueKey::RejectAndPick);
        }
        None
    })
}

/// A polite live region, like the toasts and the library's selection line.
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
    use plaudon_app::dto::PersonRefDto;
    use plaudon_domain::ids::{PersonId, RecordingId, SpeakerSuggestionId};
    use plaudon_domain::time::Timestamp;

    fn suggestion(band: &str, score: f64) -> SpeakerSuggestionDto {
        SpeakerSuggestionDto {
            suggestion_id: SpeakerSuggestionId::new(),
            recording_id: RecordingId::new(),
            recording_title: Some("Monday stand-up".to_owned()),
            diarization_run_id: "run-1".to_owned(),
            cluster_key: "S1".to_owned(),
            speaker_id: None,
            speaker_label: Some("Speaker 1".to_owned()),
            person: PersonRefDto {
                person_id: PersonId::new(),
                display_name: "Priya".to_owned(),
            },
            score,
            score_kind: "as_norm".to_owned(),
            band: band.to_owned(),
            state: "pending".to_owned(),
            created_at: Timestamp::from_millis(0),
        }
    }

    #[test]
    fn a_row_reads_as_a_suggestion_and_never_as_an_identity() {
        let text = sentence(&suggestion("suggest", 0.82));
        assert_eq!(text, speaker::suggestion("Priya", MatchBand::Strong, 0.82));
        assert!(text.contains("sounds like"));
        for forbidden in ["identified", "verified", "recognised", "is Priya"] {
            assert!(!text.contains(forbidden), "{text} claims an identity");
        }
    }

    #[test]
    fn a_band_the_store_did_not_write_renders_the_score_without_inventing_a_word() {
        let text = sentence(&suggestion("something-else", 0.5));
        assert!(text.contains("match score 0.50"), "{text}");
        assert!(!text.contains("strong"), "{text}");
    }

    #[test]
    fn a_plain_rejection_names_nobody() {
        let decision = SuggestionDecision::Reject {
            suggestion_id: "s1".to_owned(),
        };
        let AppCommand::RejectSpeakerSuggestion(request) = decision.into_command() else {
            panic!("a rejection is a reject command");
        };
        assert_eq!(request.corrected_person_id, None);
        assert!(!request.overwrite_segment_exceptions);
    }

    #[test]
    fn a_correction_carries_the_person_the_user_picked() {
        let decision = SuggestionDecision::Correct {
            suggestion_id: "s1".to_owned(),
            person_id: "p9".to_owned(),
        };
        let AppCommand::RejectSpeakerSuggestion(request) = decision.into_command() else {
            panic!("a correction is a reject command carrying the corrected person");
        };
        assert_eq!(request.corrected_person_id.as_deref(), Some("p9"));
    }
}
