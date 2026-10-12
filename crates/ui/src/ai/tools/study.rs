//! Run a study tool and read what it produced (§9.3, §9.5).
//!
//! Study artifacts are deliberately not part of the default meeting product,
//! so this panel offers exactly the study tools `list_transcript_tools`
//! reports: a tool the resolved text model cannot satisfy is listed with the
//! reason it is not offered rather than quietly disappearing, because a tool
//! that vanishes looks like a tool that does not exist.
//!
//! Flashcards, quiz items with their answer key, and definitions are rendered
//! with their citations under the same §8.6A rule as everywhere else: a seek
//! control exists only for an anchor the backend reported a position for.
//! Items the validator refused are listed with their JSON Pointer path and
//! the reason (§9.5) — a refused item is a fact about the run, not something
//! to drop.
//!
//! **Why the export is a clipboard export.** No command in this build writes a
//! study artifact to disk: `generate_study_artifacts` stores the artifacts in the
//! library and `export_recording` renders transcripts, not study sets. A
//! `Save as…` control here would therefore have nowhere to write, and a panel
//! must not offer an affordance that ends in a file that was never created.
//! Copying to the clipboard uses no port and claims nothing: the three
//! renderings come from [`plaudon_app::tool_presentation`], so the words are
//! the application layer's and not this panel's.

use egui::Ui;
use plaudon_app::command::AppCommand;
use plaudon_app::dto::{
    DefinitionDto, FlashcardDto, GenerateStudyArtifactsRequest, GenerateStudyArtifactsResponse,
    ListModelsResponse, ListStudyArtifactsResponse, ListTranscriptToolsResponse, QuizItemDto,
    RejectedStudyItemDto, StudyArtifactsDto, TranscriptToolDto,
};
use plaudon_app::tone::Tone;
use plaudon_app::tool_presentation::{
    study_artifacts_json, study_artifacts_markdown, study_artifacts_text,
};

use super::{anchors_block, dispatch_badge, resolve_route, route_facts, ToolDispatch};
use crate::ai::{fact, AiOutput, Badge, RecordingContext};

/// The tool kinds that produce study artifacts (§9.3).
///
/// The wire strings of `TranscriptToolDto::kind`; the names shown to the user
/// are the backend's `display_name`, never a label composed here.
const STUDY_KINDS: &[&str] = &["flashcards", "quiz", "definitions"];

/// Running a study tool and reading its artifacts.
#[derive(Debug, Default)]
pub struct StudyPanel {
    selected_tool: Option<String>,
    dispatch: ToolDispatch,
    set_index: usize,
}

impl StudyPanel {
    /// Draw the panel.
    ///
    /// `tools` is the last `list_transcript_tools` response, `latest` the
    /// result of the run just dispatched, `stored` the sets already in the
    /// library and `models` the catalogue the disclosure is built from. All
    /// are borrowed; the panel keeps no copy.
    pub fn ui(
        &mut self,
        ui: &mut Ui,
        context: RecordingContext<'_>,
        tools: Option<&ListTranscriptToolsResponse>,
        latest: Option<&GenerateStudyArtifactsResponse>,
        stored: Option<&ListStudyArtifactsResponse>,
        models: Option<&ListModelsResponse>,
    ) -> AiOutput {
        let mut output = AiOutput::default();
        ui.label(egui::RichText::new("Study tools").heading());
        ui.label(
            "Flashcards, a quiz and definitions, generated from this recording's transcript and \
             cited back to it.",
        );

        self.choose_and_run(ui, context, tools, models, &mut output);

        if let Some(result) = latest {
            ui.separator();
            ui.label(egui::RichText::new("This run").heading());
            fact(
                ui,
                "Tool",
                &format!("{} version {}", result.tool_id, result.tool_version),
            );
            fact(ui, "Pinned to revision", &result.transcript_revision_id);
            dispatch_badge(ui, result.dispatched);
            if let Some(run_id) = result.tool_run_id.as_deref() {
                fact(ui, "Tool run", run_id);
            }
            ui.push_id("study-run", |ui| {
                artifacts_block(ui, context, &result.artifacts, &mut output);
                export_block(ui, &result.artifacts);
            });
            rejected_block(ui, &result.rejected);
        }

        ui.separator();
        self.stored(ui, context, stored, &mut output);
        output
    }

    /// The offered tools, the route, and the gated run control.
    fn choose_and_run(
        &mut self,
        ui: &mut Ui,
        context: RecordingContext<'_>,
        tools: Option<&ListTranscriptToolsResponse>,
        models: Option<&ListModelsResponse>,
        output: &mut AiOutput,
    ) {
        let Some(tools) = tools else {
            ui.label("The transcript tools have not been read yet.");
            return;
        };
        let study: Vec<&TranscriptToolDto> = tools
            .tools
            .iter()
            .filter(|tool| STUDY_KINDS.contains(&tool.kind.as_str()))
            .collect();
        if study.is_empty() {
            ui.label("No study tool is registered in this build.");
            return;
        }

        let mut offerable = 0_usize;
        for tool in &study {
            ui.push_id(("study-tool", tool.tool_id.as_str()), |ui| {
                if tool.offerable {
                    offerable += 1;
                    ui.radio_value(
                        &mut self.selected_tool,
                        Some(tool.tool_id.clone()),
                        tool.display_name.as_str(),
                    );
                } else {
                    ui.label(tool.display_name.as_str());
                    Badge::new("⊘", "Not offered", Tone::Warning)
                        .description(
                            tool.unavailable_reason
                                .as_deref()
                                .unwrap_or("No reason was reported."),
                        )
                        .ui(ui);
                }
            });
        }
        let selected_is_offerable = self.selected_tool.as_deref().is_some_and(|id| {
            study
                .iter()
                .any(|tool| tool.tool_id == id && tool.offerable)
        });
        if offerable == 0 {
            ui.label("No study tool can run on the resolved text model right now.");
            return;
        }
        if !selected_is_offerable {
            ui.label("Choose a study tool to run.");
        }

        let Some(route) = tools.text_model.as_ref() else {
            ui.label("No text model is resolved for this recording, so no tool can run.");
            return;
        };
        route_facts(ui, route);
        if !route.available {
            return;
        }
        let Some(resolved) = resolve_route(route, models, context) else {
            ui.label(
                "This route names no model, so the disclosure cannot say what would receive the \
                 transcript passages. Nothing is sent.",
            );
            return;
        };

        if let Some(approval) = self.dispatch.ui(
            ui,
            context,
            &resolved,
            "Run this study tool",
            selected_is_offerable,
            output,
        ) {
            if let Some(tool_id) = self.selected_tool.clone() {
                output.command(AppCommand::GenerateStudyArtifacts(
                    GenerateStudyArtifactsRequest {
                        recording_id: context.recording_id.to_owned(),
                        tool_id,
                        revision_id: context.revision_id.map(str::to_owned),
                        model_override: Some(resolved.model_override()),
                        cost_acknowledged: approval.charge_acknowledged,
                    },
                ));
            }
        }
    }

    /// The study sets already stored for this recording.
    fn stored(
        &mut self,
        ui: &mut Ui,
        context: RecordingContext<'_>,
        stored: Option<&ListStudyArtifactsResponse>,
        output: &mut AiOutput,
    ) {
        ui.label(egui::RichText::new("Saved study sets").heading());
        let Some(response) = stored else {
            ui.label("Saved study sets have not been read yet.");
            return;
        };
        if response.sets.is_empty() {
            ui.label("No study set has been saved for this recording yet.");
            return;
        }
        self.set_index = self.set_index.min(response.sets.len() - 1);
        if response.sets.len() > 1 {
            ui.horizontal_wrapped(|ui| {
                for (index, set) in response.sets.iter().enumerate() {
                    ui.radio_value(
                        &mut self.set_index,
                        index,
                        format!("{} · {}", set.tool_id, set.created_at),
                    );
                }
            });
        }
        let set = &response.sets[self.set_index];
        fact(
            ui,
            "Tool",
            &format!("{} version {}", set.tool_id, set.tool_version),
        );
        fact(ui, "Pinned to revision", &set.transcript_revision_id);
        fact(ui, "Saved at", &set.created_at);
        if !set.revision_is_current {
            Badge::new("⧗", "Produced from a superseded revision", Tone::Warning)
                .description(
                    "The transcript has changed since this set was produced, so its citations \
                     point at an earlier revision.",
                )
                .ui(ui);
        }
        ui.push_id(("study-set", set.output_id.as_str()), |ui| {
            artifacts_block(ui, context, &set.artifacts, output);
            export_block(ui, &set.artifacts);
        });
    }
}

/// Flashcards, quiz items and definitions, each with its citations.
fn artifacts_block(
    ui: &mut Ui,
    context: RecordingContext<'_>,
    artifacts: &StudyArtifactsDto,
    output: &mut AiOutput,
) {
    if artifacts.flashcards.is_empty()
        && artifacts.quiz.is_empty()
        && artifacts.definitions.is_empty()
    {
        ui.label("This set contains no accepted study items.");
        return;
    }
    if !artifacts.flashcards.is_empty() {
        ui.label(
            egui::RichText::new(format!("Flashcards ({})", artifacts.flashcards.len())).strong(),
        );
        for (index, card) in artifacts.flashcards.iter().enumerate() {
            ui.push_id(("flashcard", index), |ui| {
                flashcard(ui, context, card, output);
            });
        }
    }
    if !artifacts.quiz.is_empty() {
        ui.label(egui::RichText::new(format!("Quiz ({})", artifacts.quiz.len())).strong());
        for (index, item) in artifacts.quiz.iter().enumerate() {
            ui.push_id(("quiz", index), |ui| {
                quiz_item(ui, context, item, output);
            });
        }
    }
    if !artifacts.definitions.is_empty() {
        ui.label(
            egui::RichText::new(format!("Definitions ({})", artifacts.definitions.len())).strong(),
        );
        for (index, definition) in artifacts.definitions.iter().enumerate() {
            ui.push_id(("definition", index), |ui| {
                definition_item(ui, context, definition, output);
            });
        }
    }
}

/// One flashcard.
fn flashcard(
    ui: &mut Ui,
    context: RecordingContext<'_>,
    card: &FlashcardDto,
    output: &mut AiOutput,
) {
    ui.group(|ui| {
        fact(ui, "Front", &card.front);
        fact(ui, "Back", &card.back);
        anchors_block(ui, context, &card.anchors, output);
    });
}

/// One quiz item with its answer key.
fn quiz_item(
    ui: &mut Ui,
    context: RecordingContext<'_>,
    item: &QuizItemDto,
    output: &mut AiOutput,
) {
    ui.group(|ui| {
        ui.label(egui::RichText::new(&item.question).strong());
        for (index, option) in item.options.iter().enumerate() {
            ui.label(format!("{}. {option}", index + 1));
        }
        match item.options.get(item.answer_index as usize) {
            Some(answer) => fact(
                ui,
                "Answer key",
                &format!("{}. {answer}", item.answer_index + 1),
            ),
            // The key points outside the options that were returned. Showing
            // a number with no option behind it would present a position in a
            // list as an answer, so the gap is stated instead.
            None => {
                Badge::new("!", "Answer key does not match the options", Tone::Danger)
                    .description(
                        "The answer key points at an option this item does not contain, so no \
                         correct answer can be shown.",
                    )
                    .ui(ui);
            }
        }
        anchors_block(ui, context, &item.anchors, output);
    });
}

/// One definition.
fn definition_item(
    ui: &mut Ui,
    context: RecordingContext<'_>,
    definition: &DefinitionDto,
    output: &mut AiOutput,
) {
    ui.group(|ui| {
        fact(ui, "Term", &definition.term);
        fact(ui, "Definition", &definition.definition);
        anchors_block(ui, context, &definition.anchors, output);
    });
}

/// The items the validator refused, with their JSON Pointer paths (§9.5).
fn rejected_block(ui: &mut Ui, rejected: &[RejectedStudyItemDto]) {
    if rejected.is_empty() {
        return;
    }
    ui.label(egui::RichText::new(format!("Refused items ({})", rejected.len())).strong());
    ui.label(
        "These items were produced by the model and refused before they were stored, so they are \
         not part of the set above.",
    );
    for (index, item) in rejected.iter().enumerate() {
        ui.push_id(("rejected-study-item", index), |ui| {
            ui.group(|ui| {
                fact(ui, "Path", &item.path);
                Badge::new("!", "Refused", Tone::Danger)
                    .description(&item.detail)
                    .ui(ui);
                fact(ui, "Reason", &item.reason);
            });
        });
    }
}

/// Copy the set in one of the three renderings the application layer owns.
fn export_block(ui: &mut Ui, artifacts: &StudyArtifactsDto) {
    ui.horizontal_wrapped(|ui| {
        if ui.button("Copy as Markdown").clicked() {
            ui.ctx().copy_text(study_artifacts_markdown(artifacts));
        }
        if ui.button("Copy as plain text").clicked() {
            ui.ctx().copy_text(study_artifacts_text(artifacts));
        }
        if ui.button("Copy as JSON").clicked() {
            ui.ctx().copy_text(study_artifacts_json(artifacts));
        }
    });
    ui.label(
        "These controls put the set on the clipboard. This build writes no study artifact file.",
    );
}
