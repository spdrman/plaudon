//! Per-task default models, per-recording overrides and stage re-runs
//! (spec 10.2, 8.6, 14.2).
//!
//! Three independent levels, strictly ordered and never collapsed:
//!
//! 1. a **one-shot override** passed with a single `start_processing` call,
//!    which changes nothing stored;
//! 2. a **per-recording override**, which is stored but never touches the
//!    default;
//! 3. the **per-task default**.
//!
//! Choosing a different model for one recording therefore cannot become a
//! global configuration change, which is the failure mode spec 10.2 calls out.

use serde::{Deserialize, Serialize};

/// A pipeline stage a model can be selected for (spec 14.2 `ProcessingStage`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    /// Speech to text.
    Transcribe,
    /// Speaker segmentation.
    Diarize,
    /// Text normalisation.
    Normalize,
    /// Per-section summary.
    SectionSummary,
    /// Consolidated summary.
    ConsolidatedSummary,
    /// Structured extraction (decisions, actions, risks).
    StructuredExtraction,
    /// Citation verification.
    CitationVerification,
    /// A transcript tool run.
    ToolRun,
}

impl Stage {
    /// Every stage, for exhaustive tests and for the command validator.
    pub const ALL: [Stage; 8] = [
        Stage::Transcribe,
        Stage::Diarize,
        Stage::Normalize,
        Stage::SectionSummary,
        Stage::ConsolidatedSummary,
        Stage::StructuredExtraction,
        Stage::CitationVerification,
        Stage::ToolRun,
    ];

    /// Stable wire label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Stage::Transcribe => "transcribe",
            Stage::Diarize => "diarize",
            Stage::Normalize => "normalize",
            Stage::SectionSummary => "section_summary",
            Stage::ConsolidatedSummary => "consolidated_summary",
            Stage::StructuredExtraction => "structured_extraction",
            Stage::CitationVerification => "citation_verification",
            Stage::ToolRun => "tool_run",
        }
    }

    /// Parse a wire label, refusing anything unknown.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|stage| stage.as_str() == raw)
    }

    /// Stages that consume a transcript revision and therefore go stale when
    /// it changes (spec 9.6A).
    #[must_use]
    pub const fn depends_on_transcript(self) -> bool {
        !matches!(self, Stage::Transcribe)
    }

    /// Stages re-run when `self` produces a new result.
    ///
    /// Re-transcribing invalidates everything derived from the transcript;
    /// re-summarizing invalidates nothing upstream. The list is deliberately
    /// narrow: a re-run executes the chosen stage and its dependents, not the
    /// whole pipeline (spec 10.2).
    #[must_use]
    pub fn dependents(self) -> Vec<Stage> {
        match self {
            Stage::Transcribe => vec![
                Stage::Diarize,
                Stage::Normalize,
                Stage::SectionSummary,
                Stage::ConsolidatedSummary,
                Stage::StructuredExtraction,
                Stage::CitationVerification,
                Stage::ToolRun,
            ],
            Stage::Normalize | Stage::Diarize => vec![
                Stage::SectionSummary,
                Stage::ConsolidatedSummary,
                Stage::StructuredExtraction,
                Stage::CitationVerification,
            ],
            Stage::SectionSummary => vec![
                Stage::ConsolidatedSummary,
                Stage::StructuredExtraction,
                Stage::CitationVerification,
            ],
            Stage::ConsolidatedSummary => {
                vec![Stage::StructuredExtraction, Stage::CitationVerification]
            }
            Stage::StructuredExtraction => vec![Stage::CitationVerification],
            Stage::CitationVerification | Stage::ToolRun => Vec::new(),
        }
    }
}

/// A chosen engine and model for one stage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelChoice {
    /// Engine identifier, e.g. `openrouter` or `whisper.cpp`.
    pub engine: String,
    /// Model identifier within that engine. `None` means the engine's own
    /// default, which is how a local engine with one installed model is
    /// expressed.
    pub model_id: Option<String>,
}

impl ModelChoice {
    /// Build a choice.
    #[must_use]
    pub fn new(engine: impl Into<String>, model_id: Option<String>) -> Self {
        Self {
            engine: engine.into(),
            model_id,
        }
    }

    /// Whether this choice routes through the cloud relay.
    #[must_use]
    pub fn is_cloud(&self) -> bool {
        self.engine == plaudon_ai_openrouter::registry::OPENROUTER_ENGINE_ID
    }
}

/// Where an effective choice came from. Shown in the picker so the user can
/// see whether they are looking at a default or an override.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChoiceSource {
    /// Passed with this one request.
    OneShot,
    /// Stored for this recording.
    RecordingOverride,
    /// The stage default.
    TaskDefault,
}

/// The resolved choice and where it came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectiveChoice {
    /// The choice.
    pub choice: ModelChoice,
    /// Its origin.
    pub source: ChoiceSource,
}

/// Resolve the three levels (spec 10.2).
#[must_use]
pub fn effective(
    one_shot: Option<&ModelChoice>,
    recording_override: Option<&ModelChoice>,
    task_default: Option<&ModelChoice>,
) -> Option<EffectiveChoice> {
    if let Some(choice) = one_shot {
        return Some(EffectiveChoice {
            choice: choice.clone(),
            source: ChoiceSource::OneShot,
        });
    }
    if let Some(choice) = recording_override {
        return Some(EffectiveChoice {
            choice: choice.clone(),
            source: ChoiceSource::RecordingOverride,
        });
    }
    task_default.map(|choice| EffectiveChoice {
        choice: choice.clone(),
        source: ChoiceSource::TaskDefault,
    })
}

/// An output that depends on a transcript revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DependentOutput {
    /// Output identifier.
    pub id: String,
    /// Stage that produced it.
    pub stage: Stage,
    /// Transcript revision it was generated from.
    pub input_revision: String,
}

/// Outputs that must be marked stale after a stage produced a new revision.
///
/// Nothing is deleted and nothing is regenerated automatically: spec 9.6A
/// requires historical outputs to be preserved and explicitly marked, and a
/// user to ask for a regeneration.
#[must_use]
pub fn stale_after_rerun(
    rerun: Stage,
    new_revision: &str,
    outputs: &[DependentOutput],
) -> Vec<String> {
    let dependents = rerun.dependents();
    outputs
        .iter()
        .filter(|output| {
            dependents.contains(&output.stage) && output.input_revision != new_revision
        })
        .map(|output| output.id.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cloud(model: &str) -> ModelChoice {
        ModelChoice::new("openrouter", Some(model.to_owned()))
    }

    fn local() -> ModelChoice {
        ModelChoice::new("whisper.cpp", Some("base.en".to_owned()))
    }

    #[test]
    fn an_override_wins_over_the_default_without_changing_it() {
        let default = local();
        let recording = cloud("openai/whisper-1");
        let resolved = effective(None, Some(&recording), Some(&default)).expect("resolved");
        assert_eq!(resolved.choice, recording);
        assert_eq!(resolved.source, ChoiceSource::RecordingOverride);
        // The default value is untouched: resolution is a pure read.
        assert_eq!(default, local());
    }

    #[test]
    fn a_one_shot_override_wins_over_a_stored_one() {
        let resolved = effective(
            Some(&cloud("openai/gpt-audio")),
            Some(&cloud("openai/whisper-1")),
            Some(&local()),
        )
        .expect("resolved");
        assert_eq!(resolved.source, ChoiceSource::OneShot);
        assert_eq!(resolved.choice.model_id.as_deref(), Some("openai/gpt-audio"));
    }

    #[test]
    fn with_nothing_configured_there_is_no_choice_rather_than_a_guess() {
        assert_eq!(effective(None, None, None), None);
    }

    #[test]
    fn only_the_cloud_engine_is_treated_as_cloud() {
        assert!(cloud("m").is_cloud());
        assert!(!local().is_cloud());
    }

    #[test]
    fn a_rerun_invalidates_only_dependents_of_that_stage() {
        let outputs = vec![
            DependentOutput {
                id: "summary".to_owned(),
                stage: Stage::ConsolidatedSummary,
                input_revision: "rev-1".to_owned(),
            },
            DependentOutput {
                id: "actions".to_owned(),
                stage: Stage::StructuredExtraction,
                input_revision: "rev-1".to_owned(),
            },
        ];
        let stale = stale_after_rerun(Stage::Transcribe, "rev-2", &outputs);
        assert_eq!(stale, vec!["summary".to_owned(), "actions".to_owned()]);

        // Re-running the last stage invalidates nothing.
        assert!(stale_after_rerun(Stage::CitationVerification, "rev-2", &outputs).is_empty());
    }

    #[test]
    fn an_output_already_on_the_new_revision_is_not_marked_stale() {
        let outputs = vec![DependentOutput {
            id: "summary".to_owned(),
            stage: Stage::ConsolidatedSummary,
            input_revision: "rev-2".to_owned(),
        }];
        assert!(stale_after_rerun(Stage::Transcribe, "rev-2", &outputs).is_empty());
    }

    #[test]
    fn a_rerun_never_includes_the_stage_itself() {
        for stage in Stage::ALL {
            assert!(
                !stage.dependents().contains(&stage),
                "{stage:?} depends on itself"
            );
        }
    }

    #[test]
    fn stage_labels_round_trip() {
        for stage in Stage::ALL {
            assert_eq!(Stage::parse(stage.as_str()), Some(stage));
        }
        assert_eq!(Stage::parse("not_a_stage"), None);
    }
}
