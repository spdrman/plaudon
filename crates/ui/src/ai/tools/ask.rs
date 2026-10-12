//! Ask a question about one recording (§9.8).
//!
//! The answer is produced from the recording's transcript and from nothing
//! else, so this panel is built around the three outcomes `ask_recording`
//! returns, none of which is an error:
//!
//! * **answered** — the retrieved passages support an answer, and every
//!   citation that resolved is listed with its excerpt, its segment and, when
//!   the backend reported a position, a seek control;
//! * **insufficient evidence** — the degraded case: passages were retrieved
//!   but they do not support an answer, so none was composed;
//! * **not supported by this recording** — the recording does not contain
//!   what was asked, which is a legitimate answer and is shown as one.
//!
//! Each outcome is a glyph *and* a label, never a colour (§5.3). The anchors
//! the model produced that did not verify are listed with their rejection
//! codes: a grounding failure that the reader cannot see is a grounding
//! failure that looks like success (§9.6).
//!
//! A conversation is pinned to the transcript revision it was answered from.
//! When that revision is no longer the head revision the conversation says so,
//! because an answer about an earlier revision is not an answer about the
//! transcript now on screen.
//!
//! Dispatch goes through the shared tool gate: the estimate first, Local-only
//! Mode refuses a cloud route outright, and the §4.4 disclosure opens before
//! anything leaves the device.

use egui::Ui;
use plaudon_app::command::AppCommand;
use plaudon_app::dto::{
    AskRecordingRequest, AskRecordingResponse, ConversationDto, ConversationTurnDto,
    ListConversationsResponse, ListModelsResponse, ListTranscriptToolsResponse,
};
use plaudon_app::tone::Tone;

use super::{
    anchors_block, dispatch_badge, rejected_anchors_block, resolve_route, route_facts, ToolDispatch,
};
use crate::ai::{fact, AiOutput, Badge, RecordingContext};

/// Asking about a recording, and reading what was answered.
#[derive(Debug, Default)]
pub struct AskPanel {
    question: String,
    dispatch: ToolDispatch,
    conversation_index: usize,
}

impl AskPanel {
    /// Draw the panel.
    ///
    /// Every argument is the host's last response, borrowed: `tools` resolves
    /// the text route the question would run on, `conversations` is the
    /// recorded history, `latest` the answer to the question just asked, and
    /// `models` the catalogue the disclosure describes the route from. The
    /// panel keeps none of them.
    pub fn ui(
        &mut self,
        ui: &mut Ui,
        context: RecordingContext<'_>,
        tools: Option<&ListTranscriptToolsResponse>,
        conversations: Option<&ListConversationsResponse>,
        latest: Option<&AskRecordingResponse>,
        models: Option<&ListModelsResponse>,
    ) -> AiOutput {
        let mut output = AiOutput::default();
        ui.label(egui::RichText::new("Ask about this recording").heading());
        ui.label(
            "Answers are produced from this recording's transcript only, and every statement is \
             cited back to it.",
        );

        self.compose(ui, context, tools, models, &mut output);

        if let Some(answer) = latest {
            ui.separator();
            ui.label(egui::RichText::new("Latest answer").heading());
            Self::answer(ui, context, answer, &mut output);
        }

        ui.separator();
        self.conversation(ui, context, conversations, &mut output);
        output
    }

    /// The question field, the route and the gated send control.
    fn compose(
        &mut self,
        ui: &mut Ui,
        context: RecordingContext<'_>,
        tools: Option<&ListTranscriptToolsResponse>,
        models: Option<&ListModelsResponse>,
        output: &mut AiOutput,
    ) {
        let label = ui.label("Your question");
        ui.add(egui::TextEdit::singleline(&mut self.question).desired_width(480.0))
            .labelled_by(label.id);
        let question = self.question.trim().to_owned();
        if question.is_empty() {
            ui.label("Type a question to ask. An empty question is not sent.");
        }

        let Some(tools) = tools else {
            ui.label("The transcript tools have not been read yet.");
            return;
        };
        let Some(route) = tools.text_model.as_ref() else {
            ui.label(
                "No text model is resolved for this recording, so there is nothing to ask a \
                 question of.",
            );
            return;
        };
        route_facts(ui, route);
        if !route.available {
            return;
        }
        let Some(resolved) = resolve_route(route, models, context) else {
            ui.label(
                "This route names no model, so the disclosure cannot say what would receive the \
                 question. Nothing is sent.",
            );
            return;
        };

        if let Some(approval) = self.dispatch.ui(
            ui,
            context,
            &resolved,
            "Ask this question",
            !question.is_empty(),
            output,
        ) {
            output.command(AppCommand::AskRecording(AskRecordingRequest {
                recording_id: context.recording_id.to_owned(),
                question,
                revision_id: context.revision_id.map(str::to_owned),
                model_override: Some(resolved.model_override()),
                cost_acknowledged: approval.charge_acknowledged,
            }));
        }
    }

    /// One `ask_recording` response in full.
    fn answer(
        ui: &mut Ui,
        context: RecordingContext<'_>,
        answer: &AskRecordingResponse,
        output: &mut AiOutput,
    ) {
        ui.push_id(("ask-answer", answer.conversation_id.as_str()), |ui| {
            outcome_badge(ui, &answer.outcome);
            ui.label(&answer.text);
            dispatch_badge(ui, answer.dispatched);
            fact(ui, "Answered from revision", &answer.transcript_revision_id);
            fact(
                ui,
                "Tool",
                &format!("{} version {}", answer.tool_id, answer.tool_version),
            );
            if let Some(run_id) = answer.tool_run_id.as_deref() {
                fact(ui, "Tool run", run_id);
            }
            fact(
                ui,
                "Transcript segments retrieved",
                &retrieved_label(&answer.retrieved_segments),
            );

            ui.label(egui::RichText::new("Citations").strong());
            anchors_block(ui, context, &answer.anchors, output);
            rejected_anchors_block(ui, &answer.rejected_anchors);
        });
    }

    /// The recorded conversations, pinned to the revision they were answered
    /// from.
    fn conversation(
        &mut self,
        ui: &mut Ui,
        context: RecordingContext<'_>,
        conversations: Option<&ListConversationsResponse>,
        output: &mut AiOutput,
    ) {
        ui.label(egui::RichText::new("Conversation").heading());
        let Some(response) = conversations else {
            ui.label("Earlier questions have not been read yet.");
            return;
        };
        if response.conversations.is_empty() {
            ui.label("No question has been asked about this recording yet.");
            return;
        }
        self.conversation_index = self
            .conversation_index
            .min(response.conversations.len() - 1);
        if response.conversations.len() > 1 {
            ui.horizontal_wrapped(|ui| {
                for (index, candidate) in response.conversations.iter().enumerate() {
                    ui.radio_value(
                        &mut self.conversation_index,
                        index,
                        format!("Conversation of {}", candidate.created_at),
                    );
                }
            });
        }
        let current: &ConversationDto = &response.conversations[self.conversation_index];
        fact(ui, "Pinned to revision", &current.transcript_revision_id);
        if current.revision_is_current {
            Badge::new("✓", "Answered from the current revision", Tone::Success).ui(ui);
        } else {
            Badge::new("⧗", "Answered from a superseded revision", Tone::Warning)
                .description(
                    "The transcript has changed since these answers were produced, so their \
                     citations point at an earlier revision. Ask again to answer from the \
                     current one.",
                )
                .ui(ui);
        }
        if current.turns.is_empty() {
            ui.label("This conversation has no recorded turns.");
        }
        for turn in &current.turns {
            Self::turn(ui, context, turn, output);
        }
    }

    /// One recorded turn.
    fn turn(
        ui: &mut Ui,
        context: RecordingContext<'_>,
        turn: &ConversationTurnDto,
        output: &mut AiOutput,
    ) {
        ui.push_id(("turn", turn.message_id.as_str()), |ui| {
            ui.group(|ui| {
                ui.label(egui::RichText::new(role_label(&turn.role)).strong());
                ui.label(&turn.text);
                fact(ui, "Recorded at", &turn.created_at);
                if let Some(outcome) = turn.outcome.as_deref() {
                    outcome_badge(ui, outcome);
                }
                if let Some(run_id) = turn.tool_run_id.as_deref() {
                    fact(ui, "Tool run", run_id);
                }
                if !turn.anchors.is_empty() {
                    anchors_block(ui, context, &turn.anchors, output);
                }
            });
        });
    }
}

/// Who spoke in a recorded turn.
fn role_label(role: &str) -> &str {
    match role {
        "user" => "You asked",
        "assistant" => "Answer",
        other => other,
    }
}

/// Which segments were retrieved, or that none were.
fn retrieved_label(segments: &[u32]) -> String {
    if segments.is_empty() {
        return "No transcript segment was retrieved for this question.".to_owned();
    }
    segments
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

/// The §9.8 outcome, as a glyph and a label (§5.3).
///
/// Every outcome is rendered as an answer the user can read; none of them is
/// a failure of the application, and none is signalled by colour alone.
pub(crate) fn outcome_badge(ui: &mut Ui, outcome: &str) {
    match outcome {
        "answered" => Badge::new("✓", "Answered from the recording", Tone::Success)
            .description("The cited passages below support this answer.")
            .ui(ui),
        "insufficient_evidence" => Badge::new(
            "!",
            "Not enough evidence for an answer",
            Tone::Warning,
        )
        .description(
            "No answer was composed, because the retrieved passages do not support an answer.",
        )
        .ui(ui),
        "not_supported_by_recording" => Badge::new(
            "∅",
            "Not supported by this recording",
            Tone::Neutral,
        )
        .description(
            "This recording does not cover what you asked, and nothing outside it was consulted.",
        )
        .ui(ui),
        other => Badge::new("?", "Outcome not recognised", Tone::Neutral)
            .description(other)
            .ui(ui),
    };
}
