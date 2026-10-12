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

use super::review::{claim_state_badge, stale_badge};
use super::{fact, AiOutput, Badge, RecordingContext};

/// Reading the generated outputs of one recording.
#[derive(Debug, Default)]
pub struct OutputsViewer {
    output_index: usize,
    speaker_edits: Vec<(String, String)>,
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
            ui.push_id(speaker_id.clone(), |ui| {
                ui.group(|ui| {
                    fact(ui, "Speaker", &speaker.label);
                    fact(ui, "Machine label", &speaker.original_label);
                    if speaker.confirmed {
                        Badge::new("✓", "Confirmed by you", Tone::Success).ui(ui);
                    } else {
                        Badge::new("?", "Not confirmed", Tone::Neutral)
                            .description(
                                "The engine proposed this identity; it has not been confirmed.",
                            )
                            .ui(ui);
                    }

                    let slot = match self
                        .speaker_edits
                        .iter()
                        .position(|(id, _)| *id == speaker_id)
                    {
                        Some(index) => index,
                        None => {
                            self.speaker_edits
                                .push((speaker_id.clone(), speaker.label.clone()));
                            self.speaker_edits.len() - 1
                        }
                    };
                    ui.horizontal(|ui| {
                        let name_label = ui.label("Speaker name");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.speaker_edits[slot].1)
                                .desired_width(180.0),
                        )
                        .labelled_by(name_label.id);
                        if ui.button("Rename speaker").clicked() {
                            output.command(AppCommand::RenameSpeaker(RenameSpeakerRequest {
                                recording_id: context.recording_id.to_owned(),
                                speaker_id: speaker_id.clone(),
                                name: self.speaker_edits[slot].1.clone(),
                            }));
                        }
                        if !speaker.confirmed && ui.button("Confirm speaker").clicked() {
                            output.command(AppCommand::ConfirmSpeaker(ConfirmSpeakerRequest {
                                recording_id: context.recording_id.to_owned(),
                                speaker_id: speaker_id.clone(),
                            }));
                        }
                        if ui.button("Reset to the machine label").clicked() {
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
