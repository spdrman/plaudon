//! The source-grounded review panel (§4.5, §9.2, §9.5).
//!
//! This is the screen the React task I2.3 (#136) never finished: the commands
//! `list_outputs`, `review_claim` and `regenerate_output` existed with no user
//! interface over them. It shows, for every extracted claim:
//!
//! * its **grounding state** — `SourceLinked`, `HumanConfirmed` or
//!   `Unverified` — with a glyph and a label, never colour alone, plus the
//!   `Review required` marker and the machine reasons behind it;
//! * its **anchors**: the excerpt, whether the anchor resolved to real text,
//!   the rejection code when it did not, and the alignment origin. An anchor
//!   the backend gave a time for offers **Seek to this passage**, which emits
//!   a [`SeekRequest`]; an anchor without one offers no seek affordance and no
//!   time is invented for it (§8.6A, acceptance criterion 2 of #136);
//! * the **verified view**, which excludes claims whose grounding failed, so a
//!   reader never mistakes an unverified statement for a checked one;
//! * **staleness**: an output generated from a superseded transcript revision
//!   is marked with the instant and the reason, and
//! * **targeted regeneration**: regenerating is an explicit, acknowledged act
//!   (`regenerate_output.acknowledged`), because it dispatches a new, possibly
//!   billable run.
//!
//! Accepting or rejecting a claim dispatches `review_claim`; the optional note
//! travels with it. The panel stores no verdict of its own — the backend owns
//! the claim's state and the panel renders whatever the next `list_outputs`
//! returns.

use egui::Ui;
use plaudon_app::command::AppCommand;
use plaudon_app::dto::{
    AnchorDto, ClaimDto, ListOutputsResponse, OutputDto, RegenerateOutputRequest,
    ReviewClaimRequest,
};
use plaudon_app::tone::Tone;

use super::outputs::claim_fields;
use super::{fact, AiIntent, AiOutput, Badge, RecordingContext, SeekRequest};

/// Which claims the panel lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ClaimFilter {
    /// Every claim, whatever its grounding.
    #[default]
    All,
    /// Only claims that are source-linked or human-confirmed: the view a
    /// reader can rely on.
    VerifiedOnly,
    /// Only claims the backend marked as needing human review.
    ReviewRequired,
}

impl ClaimFilter {
    /// Label of the choice.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::All => "All claims",
            Self::VerifiedOnly => "Verified only",
            Self::ReviewRequired => "Review required",
        }
    }

    fn admits(self, claim: &ClaimDto) -> bool {
        match self {
            Self::All => true,
            Self::VerifiedOnly => {
                matches!(claim.state.as_str(), "source_linked" | "human_confirmed")
            }
            Self::ReviewRequired => claim.review_required,
        }
    }
}

/// Reviewing the claims of one output.
#[derive(Debug, Default)]
pub struct ReviewPanel {
    /// Which claims are listed.
    pub filter: ClaimFilter,
    output_index: usize,
    notes: Vec<(String, String)>,
    confirming_regenerate: Option<String>,
    regenerate_acknowledged: bool,
}

impl ReviewPanel {
    /// Draw the panel.
    pub fn ui(
        &mut self,
        ui: &mut Ui,
        context: RecordingContext<'_>,
        outputs: Option<&ListOutputsResponse>,
    ) -> AiOutput {
        let mut output = AiOutput::default();
        ui.label(egui::RichText::new("Review the sources").heading());

        let Some(response) = outputs else {
            ui.label("Outputs have not been read yet.");
            return output;
        };
        if response.outputs.is_empty() {
            ui.label("No output has been generated for this recording yet, so there is nothing to review.");
            return output;
        }
        self.output_index = self.output_index.min(response.outputs.len() - 1);
        if response.outputs.len() > 1 {
            ui.horizontal_wrapped(|ui| {
                for (index, candidate) in response.outputs.iter().enumerate() {
                    ui.radio_value(
                        &mut self.output_index,
                        index,
                        format!("{} · {}", candidate.output_type, candidate.created_at),
                    );
                }
            });
        }
        let current = &response.outputs[self.output_index];

        stale_badge(ui, current);
        let review_required = current
            .claims
            .iter()
            .filter(|claim| claim.review_required)
            .count();
        if review_required > 0 {
            Badge::new("!", "Review required", Tone::Warning)
                .description(&format!(
                    "{review_required} of {} claims have to be checked against the transcript.",
                    current.claims.len()
                ))
                .ui(ui);
        }

        ui.horizontal_wrapped(|ui| {
            for filter in [
                ClaimFilter::All,
                ClaimFilter::VerifiedOnly,
                ClaimFilter::ReviewRequired,
            ] {
                ui.radio_value(&mut self.filter, filter, filter.label());
            }
        });

        let listed: Vec<&ClaimDto> = current
            .claims
            .iter()
            .filter(|claim| self.filter.admits(claim))
            .collect();
        if listed.is_empty() {
            ui.label(match self.filter {
                ClaimFilter::All => "This output contains no claims.",
                ClaimFilter::VerifiedOnly => {
                    "No claim in this output is source-linked or confirmed."
                }
                ClaimFilter::ReviewRequired => "No claim in this output is waiting for review.",
            });
        }
        for claim in listed {
            self.claim(ui, context, current, claim, &mut output);
        }

        self.regenerate(ui, current, &mut output);
        output
    }

    fn claim(
        &mut self,
        ui: &mut Ui,
        context: RecordingContext<'_>,
        output_dto: &OutputDto,
        claim: &ClaimDto,
        output: &mut AiOutput,
    ) {
        let claim_id = claim.claim_id.to_string();
        ui.push_id(claim_id.clone(), |ui| {
            ui.group(|ui| {
                ui.label(&claim.text);
                claim_fields(ui, claim);
                claim_state_badge(ui, claim);
                fact(ui, "Category", &claim.category);
                fact(ui, "Entailment check", entailment_label(&claim.entailment));

                if claim.anchors.is_empty() {
                    Badge::new("∅", "No source anchor", Tone::Warning)
                        .description(
                            "Nothing in the transcript was cited for this claim, so it cannot be \
                             checked against the recording.",
                        )
                        .ui(ui);
                }
                for (index, anchor) in claim.anchors.iter().enumerate() {
                    Self::anchor(ui, context, claim, index, anchor, output);
                }

                let slot =
                    if let Some(index) = self.notes.iter().position(|(id, _)| *id == claim_id) {
                        index
                    } else {
                        self.notes.push((claim_id.clone(), String::new()));
                        self.notes.len() - 1
                    };
                let note_label = ui.label("Review note (optional)");
                ui.add(egui::TextEdit::singleline(&mut self.notes[slot].1).desired_width(320.0))
                    .labelled_by(note_label.id);
                ui.horizontal_wrapped(|ui| {
                    if ui.button("Confirm this claim").clicked() {
                        output.command(AppCommand::ReviewClaim(ReviewClaimRequest {
                            output_id: output_dto.output_id.to_string(),
                            claim_id: claim_id.clone(),
                            accepted: true,
                            note: note_of(&self.notes[slot].1),
                        }));
                    }
                    if ui.button("Reject this claim").clicked() {
                        output.command(AppCommand::ReviewClaim(ReviewClaimRequest {
                            output_id: output_dto.output_id.to_string(),
                            claim_id: claim_id.clone(),
                            accepted: false,
                            note: note_of(&self.notes[slot].1),
                        }));
                    }
                });
            });
        });
    }

    fn anchor(
        ui: &mut Ui,
        context: RecordingContext<'_>,
        claim: &ClaimDto,
        index: usize,
        anchor: &AnchorDto,
        output: &mut AiOutput,
    ) {
        ui.push_id(index, |ui| {
            ui.group(|ui| {
                ui.label(egui::RichText::new(format!("“{}”", anchor.excerpt)).italics());
                fact(ui, "Segment", &anchor.seq_index.to_string());
                fact(
                    ui,
                    "Characters",
                    &format!("{}–{}", anchor.char_start, anchor.char_end),
                );
                fact(ui, "Alignment origin", &anchor.alignment_origin);

                if anchor.verified {
                    Badge::new("✓", "Anchor resolved in the transcript", Tone::Success).ui(ui);
                    if let Some(matched) = anchor.matched_text.as_deref() {
                        fact(ui, "Matched text", matched);
                    }
                } else {
                    Badge::new("!", "Anchor did not resolve", Tone::Danger)
                        .description(
                            anchor
                                .rejection_code
                                .as_deref()
                                .unwrap_or("No rejection code was reported."),
                        )
                        .ui(ui);
                }

                match anchor.seek_ms {
                    Some(position_ms) => {
                        if ui.button("Seek to this passage").clicked() {
                            output.push(AiIntent::Seek(SeekRequest {
                                recording_id: context.recording_id.to_owned(),
                                position_ms,
                                claim_id: Some(claim.claim_id.to_string()),
                                seq_index: anchor.seq_index,
                            }));
                        }
                    }
                    // No time was reported for this anchor, so there is no
                    // seek control at all: a seek affordance that guessed a
                    // position would be inventing evidence.
                    None => {
                        ui.label(
                            "This passage has no playback position, so it cannot be played from \
                             here.",
                        );
                    }
                }
            });
        });
    }

    fn regenerate(&mut self, ui: &mut Ui, output_dto: &OutputDto, output: &mut AiOutput) {
        ui.separator();
        let output_id = output_dto.output_id.to_string();
        if ui.button("Regenerate this output").clicked() {
            self.confirming_regenerate = Some(output_id.clone());
            self.regenerate_acknowledged = false;
        }
        if self.confirming_regenerate.as_deref() != Some(output_id.as_str()) {
            return;
        }
        let mut close = false;
        egui::Modal::new(egui::Id::new(("regenerate-output", output_id.clone()))).show(
            ui.ctx(),
            |ui| {
                ui.label(egui::RichText::new("Regenerate this output?").heading());
                ui.label(
                    "Regeneration starts a new run over the current transcript revision. The \
                     existing output and the review decisions recorded against it are kept; the \
                     new run produces its own claims, and if it uses a cloud model it may be \
                     billed.",
                );
                ui.checkbox(
                    &mut self.regenerate_acknowledged,
                    "I understand this starts a new run that may be billed",
                );
                ui.horizontal(|ui| {
                    if ui.button("Keep this output").clicked() {
                        close = true;
                    }
                    if ui
                        .add_enabled(
                            self.regenerate_acknowledged,
                            egui::Button::new("Regenerate"),
                        )
                        .clicked()
                    {
                        output.command(AppCommand::RegenerateOutput(RegenerateOutputRequest {
                            output_id: output_id.clone(),
                            acknowledged: true,
                        }));
                        close = true;
                    }
                });
            },
        );
        if close {
            self.confirming_regenerate = None;
            self.regenerate_acknowledged = false;
        }
    }
}

fn note_of(text: &str) -> Option<String> {
    let trimmed = text.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

fn entailment_label(entailment: &str) -> &str {
    match entailment {
        "supported" => "Supported by the cited passage",
        "not_supported" => "Not supported by the cited passage",
        _ => "Not checked",
    }
}

/// The grounding state of a claim, as a glyph and a label (§5.3).
pub(crate) fn claim_state_badge(ui: &mut Ui, claim: &ClaimDto) {
    let (glyph, label, tone, description) = match claim.state.as_str() {
        "source_linked" => (
            "⛓",
            "Source linked",
            Tone::Success,
            "Every anchor of this claim resolved to text in the transcript.",
        ),
        "human_confirmed" => (
            "✓",
            "Confirmed by a person",
            Tone::Success,
            "A reviewer accepted this claim against the transcript.",
        ),
        _ => (
            "!",
            "Unverified",
            Tone::Warning,
            "This claim is not backed by a resolved source anchor.",
        ),
    };
    Badge::new(glyph, label, tone)
        .description(description)
        .ui(ui);

    if claim.review_required {
        Badge::new("!", "Review required", Tone::Warning).ui(ui);
        for reason in &claim.review_reasons {
            ui.label(format!("• {reason}"));
        }
    }
}

/// The stale marker of an output, when it has one.
pub(crate) fn stale_badge(ui: &mut Ui, output: &OutputDto) {
    let Some(stale) = output.stale.as_ref() else {
        return;
    };
    Badge::new("⧗", "Stale", Tone::Warning)
        .description(&format!(
            "Marked stale at {} because {}. It was generated from an earlier transcript \
             revision; regenerate it to review the current one.",
            stale.stale_at, stale.reason
        ))
        .ui(ui);
}
