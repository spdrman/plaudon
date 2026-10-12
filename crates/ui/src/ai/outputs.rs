//! The outputs viewer (§9.2): summary, decisions, action items, risks, open
//! questions, topics — and who was speaking.
//!
//! Every field set is the one §9.2 fixes, rendered from
//! [`plaudon_app::dto::RenderedClaimFields`] exactly as the backend produced
//! it: an action item without an owner shows `Unassigned` and one without a
//! date shows `Not specified`, because those are the literal values Rust
//! sends, not placeholders invented here.
//!
//! A claim's grounding is shown wherever the claim is: the review panel is the
//! place to act on it, but a reader of the summary is never shown an
//! unverified statement that looks verified (§4.5).

use egui::Ui;
use plaudon_app::command::AppCommand;
use plaudon_app::dto::{
    ClaimDto, ConfirmSpeakerRequest, ListOutputsResponse, ListSpeakersResponse, OutputDto,
    RenameSpeakerRequest, RenderedClaimFields, ResetSpeakerRequest,
};
use plaudon_app::tone::Tone;
use plaudon_domain::speaker::MAX_NAME_CHARS;

use super::review::{claim_state_badge, stale_badge};
use super::{fact, AiOutput, Badge, RecordingContext};

/// One speaker's name field between frames.
#[derive(Debug)]
struct NameEdit {
    /// The row this buffer belongs to.
    speaker_id: String,
    /// What the user has typed.
    text: String,
    /// The stored name the buffer was seeded from, so a rename, a reset or
    /// an answer that arrived from elsewhere reseeds it instead of leaving
    /// a stale draft behind.
    seeded_from: Option<String>,
}

/// Reading the generated outputs of one recording.
#[derive(Debug, Default)]
pub struct OutputsViewer {
    output_index: usize,
    speaker_edits: Vec<NameEdit>,
}

impl OutputsViewer {
    /// Draw the viewer.
    pub fn ui(
        &mut self,
        ui: &mut Ui,
        context: RecordingContext<'_>,
        outputs: Option<&ListOutputsResponse>,
        speakers: Option<&ListSpeakersResponse>,
        now_ms: i128,
    ) -> AiOutput {
        let _ = now_ms;
        let mut output = AiOutput::default();
        ui.label(egui::RichText::new("Outputs").heading());

        match outputs {
            None => {
                ui.label("Outputs have not been read yet.");
            }
            Some(response) if response.outputs.is_empty() => {
                ui.label("No output has been generated for this recording yet.");
            }
            Some(response) => {
                self.output_index = self.output_index.min(response.outputs.len() - 1);
                if response.outputs.len() > 1 {
                    ui.horizontal_wrapped(|ui| {
                        for (index, candidate) in response.outputs.iter().enumerate() {
                            let label =
                                format!("{} · {}", candidate.output_type, candidate.created_at);
                            ui.radio_value(&mut self.output_index, index, label);
                        }
                    });
                }
                let current = &response.outputs[self.output_index];
                output_header(ui, current);
                claims_section(ui, current);
            }
        }

        ui.separator();
        output.extend(self.speakers(ui, context, speakers));
        output
    }

    fn speakers(
        &mut self,
        ui: &mut Ui,
        context: RecordingContext<'_>,
        speakers: Option<&ListSpeakersResponse>,
    ) -> AiOutput {
        let mut output = AiOutput::default();
        ui.label(egui::RichText::new("Speaker identification").heading());
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
            let slot = self.name_slot(&speaker_id, speaker.name.as_deref());
            ui.push_id(speaker_id.clone(), |ui| {
                ui.group(|ui| {
                    // One wording, shared with the export bundle: an
                    // unconfirmed row is a cluster whatever has been typed
                    // into it (`plaudon_domain::speaker::display`).
                    fact(
                        ui,
                        "Speaker",
                        &plaudon_domain::speaker::display(
                            &speaker.label,
                            speaker.name.as_deref(),
                            speaker.confirmed,
                        ),
                    );
                    fact(ui, "Machine label", &speaker.original_label);
                    if speaker.confirmed {
                        Badge::new("✓", "Confirmed by you", Tone::Success).ui(ui);
                    } else {
                        if let Some(name) = speaker.name.as_deref() {
                            fact(ui, "Name you typed, not yet confirmed", name);
                        }
                        Badge::new("?", "Not confirmed", Tone::Neutral)
                            .description(
                                "The engine proposed this cluster; nobody has confirmed whose \
                                 voice it is.",
                            )
                            .ui(ui);
                    }

                    ui.horizontal(|ui| {
                        let name_label = ui.label("Speaker name");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.speaker_edits[slot].text)
                                .desired_width(180.0)
                                .char_limit(MAX_NAME_CHARS),
                        )
                        .labelled_by(name_label.id)
                        .on_hover_text(format!("At most {MAX_NAME_CHARS} characters."));

                        // A command that is known to be refused is not sent:
                        // the store rejects a blank name, so the control
                        // that would send one is disabled and says why.
                        let typed = self.speaker_edits[slot].text.trim();
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

                        // §17.6: there is nothing to confirm until a human
                        // has supplied a name.
                        if !speaker.confirmed
                            && ui
                                .add_enabled(
                                    speaker.name.is_some(),
                                    egui::Button::new("Confirm speaker"),
                                )
                                .on_disabled_hover_text(
                                    "Name this speaker first; confirming is your claim that the \
                                     name is right.",
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
                            .on_disabled_hover_text(
                                "This speaker still carries the label the engine gave it.",
                            )
                            .clicked()
                        {
                            output.command(AppCommand::ResetSpeaker(ResetSpeakerRequest {
                                recording_id: context.recording_id.to_owned(),
                                speaker_id: speaker_id.clone(),
                            }));
                        }
                    });
                });
            });
        }
        output
    }

    /// The edit buffer for one speaker, reseeded whenever the stored name
    /// moved underneath it.
    ///
    /// Without the reseed a reset would leave the discarded name sitting in
    /// the field, one click away from being stored again.
    fn name_slot(&mut self, speaker_id: &str, stored: Option<&str>) -> usize {
        if let Some(index) = self
            .speaker_edits
            .iter()
            .position(|edit| edit.speaker_id == speaker_id)
        {
            if self.speaker_edits[index].seeded_from.as_deref() != stored {
                self.speaker_edits[index].seeded_from = stored.map(ToOwned::to_owned);
                self.speaker_edits[index].text = stored.unwrap_or_default().to_owned();
            }
            return index;
        }
        self.speaker_edits.push(NameEdit {
            speaker_id: speaker_id.to_owned(),
            text: stored.unwrap_or_default().to_owned(),
            seeded_from: stored.map(ToOwned::to_owned),
        });
        self.speaker_edits.len() - 1
    }
}

fn output_header(ui: &mut Ui, output: &OutputDto) {
    fact(ui, "Output", &output.output_type);
    fact(ui, "Template", &output.template_id);
    fact(ui, "Template version", &output.template_version);
    fact(ui, "Pipeline version", &output.pipeline_version);
    fact(ui, "Generated", &output.created_at);
    fact(
        ui,
        "Transcript revision",
        &output.transcript_revision_id.to_string(),
    );

    if output.status == "needs_review" {
        Badge::new("!", "Needs review", Tone::Warning)
            .description("At least one claim in this output has to be reviewed by a person.")
            .ui(ui);
    }
    if output.coverage.state != "full" {
        let chunks = output.coverage.chunks.as_ref().map_or_else(
            || "not reported".to_owned(),
            |chunks| {
                chunks
                    .iter()
                    .map(u32::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            },
        );
        Badge::new("◐", "Partial coverage", Tone::Warning)
            .description(&format!(
                "The model reported that it covered only part of the transcript. Chunks: {chunks}."
            ))
            .ui(ui);
    }
    stale_badge(ui, output);
}

fn claims_section(ui: &mut Ui, output: &OutputDto) {
    section(ui, "Summary", output, |claim| {
        matches!(claim.fields, RenderedClaimFields::SummaryStatement)
    });
    section(ui, "Key decisions", output, |claim| {
        matches!(claim.fields, RenderedClaimFields::Decision(_))
    });
    section(ui, "Action items", output, |claim| {
        matches!(claim.fields, RenderedClaimFields::ActionItem(_))
    });
    section(ui, "Risks", output, |claim| {
        matches!(claim.fields, RenderedClaimFields::Risk(_))
    });
    section(ui, "Open questions", output, |claim| {
        matches!(claim.fields, RenderedClaimFields::Question(_))
    });
    section(ui, "Topics", output, |claim| {
        matches!(claim.fields, RenderedClaimFields::Topic(_))
    });
}

fn section(ui: &mut Ui, heading: &str, output: &OutputDto, select: impl Fn(&ClaimDto) -> bool) {
    let claims: Vec<&ClaimDto> = output.claims.iter().filter(|claim| select(claim)).collect();
    if claims.is_empty() {
        return;
    }
    ui.label(egui::RichText::new(heading).strong());
    for claim in claims {
        ui.push_id(claim.claim_id.to_string(), |ui| {
            ui.group(|ui| {
                ui.label(&claim.text);
                claim_fields(ui, claim);
                claim_state_badge(ui, claim);
            });
        });
    }
}

/// The §9.2 field set of one claim, rendered verbatim.
pub(crate) fn claim_fields(ui: &mut Ui, claim: &ClaimDto) {
    match &claim.fields {
        RenderedClaimFields::SummaryStatement => {}
        RenderedClaimFields::Decision(fields) => {
            fact(ui, "Decision", &fields.statement);
            let participants = if fields.participants.is_empty() {
                "Not evidenced".to_owned()
            } else {
                fields.participants.join(", ")
            };
            fact(ui, "Participants", &participants);
            fact(ui, "Time reference", &fields.time_reference);
            fact(ui, "Assertion", &fields.assertion);
        }
        RenderedClaimFields::ActionItem(fields) => {
            fact(ui, "Action", &fields.verb);
            fact(ui, "Detail", &fields.description);
            fact(ui, "Owner", &fields.owner);
            fact(ui, "Due date", &fields.due_date);
            fact(ui, "Status", &fields.status);
            fact(ui, "Assertion", &fields.assertion);
        }
        RenderedClaimFields::Risk(fields) => {
            fact(ui, "Risk", &fields.description);
            fact(ui, "Consequence", &fields.consequence);
            fact(ui, "Mitigation", &fields.mitigation);
        }
        RenderedClaimFields::Question(fields) => {
            fact(ui, "Question", &fields.question);
            fact(ui, "Owner", &fields.owner);
        }
        RenderedClaimFields::Topic(fields) => {
            fact(ui, "Topic", &fields.heading);
            fact(
                ui,
                "Segments",
                &format!("{} to {}", fields.first_segment, fields.last_segment),
            );
        }
    }
}
