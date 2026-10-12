//! Interaction tests for the summarization panel, the model picker and the
//! templates placeholder.

mod ai_fixtures;

use ai_fixtures as fx;
use egui_kittest::kittest::Queryable as _;
use egui_kittest::Harness;
use plaudon_app::cloud_presentation::ProcessingStage;
use plaudon_app::command::AppCommand;
use plaudon_app::dto::ListModelsResponse;
use plaudon_domain::transcript::AlignmentOrigin;
use plaudon_ui::ai::{
    AiIntent, AiOutput, ModelPicker, RecordingContext, SummarizationPanel, TemplatesPlaceholder,
};

/// A window tall enough that no control of a panel is clipped; these tests
/// take no pixel snapshot, so the snapshot policy is unaffected.
const TALL: egui::Vec2 = egui::vec2(1280.0, 4000.0);

#[derive(Default)]
struct Collected {
    names: Vec<&'static str>,
    start_processing: Vec<plaudon_app::dto::StartProcessingRequest>,
    verifications: Vec<plaudon_app::dto::VerifyModelRequest>,
}

impl Collected {
    fn absorb(&mut self, output: AiOutput) {
        for intent in output.intents {
            if let AiIntent::Command(command) = intent {
                self.names.push(command.name());
                match command {
                    AppCommand::StartProcessing(request) => self.start_processing.push(request),
                    AppCommand::VerifyModel(request) => self.verifications.push(request),
                    _ => {}
                }
            }
        }
    }
}

fn announced<State>(harness: &Harness<'_, State>) -> String {
    plaudon_ui_testkit::a11y::nodes(harness)
        .into_iter()
        .filter_map(|node| node.announced().map(str::to_owned))
        .collect::<Vec<_>>()
        .join("\n")
}

struct SummarizeApp {
    panel: SummarizationPanel,
    capabilities: plaudon_app::dto::Capabilities,
    models: ListModelsResponse,
    outputs: plaudon_app::dto::ListOutputsResponse,
    collected: Collected,
}

fn summarize_harness(local_only: bool) -> Harness<'static, SummarizeApp> {
    plaudon_ui_testkit::builder()
        .with_size(TALL)
        .build_ui_state(
            |ui, app: &mut SummarizeApp| {
                let context = RecordingContext {
                    recording_id: fx::RECORDING_ID,
                    duration_ms: Some(600_000),
                    revision_id: Some(fx::REVISION_ID),
                    alignment_origin: AlignmentOrigin::Native,
                    title: "Board meeting",
                    capabilities: Some(&app.capabilities),
                };
                let output = app.panel.ui(
                    ui,
                    context,
                    Some(&app.models),
                    Some(&app.outputs),
                    None,
                    fx::NOW_MS,
                );
                app.collected.absorb(output);
            },
            SummarizeApp {
                panel: SummarizationPanel::default(),
                capabilities: fx::capabilities(local_only, Vec::new()),
                models: fx::models(vec![fx::local_model(), fx::cloud_model()]),
                outputs: fx::outputs(vec![fx::output(
                    vec![fx::decision_claim("source_linked")],
                    None,
                )]),
                collected: Collected::default(),
            },
        )
}

#[test]
fn the_panel_defaults_to_the_consolidated_summary_and_offers_the_text_stages() {
    let mut harness = summarize_harness(false);
    harness.run();
    let text = announced(&harness);
    for stage in [
        "Transcript normalization",
        "Section summaries",
        "Consolidated summary",
        "Structured extraction",
        "Citation verification",
    ] {
        assert!(text.contains(stage), "missing {stage}: {text}");
    }
    assert_eq!(
        harness.state().panel.stage(),
        ProcessingStage::ConsolidatedSummary
    );
}

#[test]
fn the_template_of_the_existing_output_is_shown_and_no_picker_is_invented() {
    let mut harness = summarize_harness(false);
    harness.run();
    let text = announced(&harness);
    assert!(text.contains("Template: meeting.v1"), "{text}");
    assert!(text.contains("Template version: 1.2.0"), "{text}");
    assert!(text.contains("Pipeline version: 3"), "{text}");
    assert!(
        text.contains("No command publishes the template catalogue")
            || text.contains("no command publishes the template catalogue")
            || text.contains("Templates are fixed in this build"),
        "{text}"
    );
}

#[test]
fn a_summary_stage_dispatches_start_processing_for_that_stage() {
    let mut harness = summarize_harness(false);
    harness.run();
    harness.get_by_label("Section summaries").click();
    harness.run();
    harness.get_by_label("Whisper small").click();
    harness.run();
    harness.get_by_label("Run section summaries").click();
    harness.run();

    let requests = &harness.state().collected.start_processing;
    assert_eq!(requests.len(), 1, "{:?}", harness.state().collected.names);
    assert_eq!(requests[0].stage, "section_summary");
    assert!(requests[0].rerun);
}

#[test]
fn local_only_mode_blocks_the_cloud_model_in_the_summary_panel_too() {
    let mut harness = summarize_harness(true);
    harness.run();
    let text = announced(&harness);
    assert!(text.contains("Blocked by Local-only Mode"), "{text}");

    harness.get_by_label("Vendor Whisper Large").click();
    harness.run();
    harness.run();
    assert!(harness.state().collected.start_processing.is_empty());
}

/* ------------------------------------------------------------ model picker */

struct PickerApp {
    picker: ModelPicker,
    capabilities: plaudon_app::dto::Capabilities,
    models: ListModelsResponse,
    collected: Collected,
}

fn picker_harness(
    models: ListModelsResponse,
    consents: Vec<plaudon_app::dto::ConsentSummary>,
) -> Harness<'static, PickerApp> {
    plaudon_ui_testkit::builder()
        .with_size(TALL)
        .build_ui_state(
            |ui, app: &mut PickerApp| {
                let context = RecordingContext {
                    recording_id: fx::RECORDING_ID,
                    duration_ms: Some(600_000),
                    revision_id: Some(fx::REVISION_ID),
                    alignment_origin: AlignmentOrigin::Native,
                    title: "Board meeting",
                    capabilities: Some(&app.capabilities),
                };
                let output = app.picker.ui(
                    ui,
                    "Model",
                    ProcessingStage::Transcribe,
                    Some(&app.models),
                    context,
                );
                app.collected.absorb(output);
            },
            PickerApp {
                picker: ModelPicker::default(),
                capabilities: fx::capabilities(false, consents),
                models,
                collected: Collected::default(),
            },
        )
}

#[test]
fn a_user_typed_model_id_is_listed_as_not_verified_and_cannot_be_chosen() {
    let mut harness = picker_harness(fx::models(vec![fx::local_model()]), Vec::new());
    harness.run();
    harness.get_by_label("Advanced: custom model ID").focus();
    harness.run();
    harness
        .get_by_label("Advanced: custom model ID")
        .type_text("vendor/untested");
    harness.run();
    harness.get_by_label("Add custom model ID").click();
    harness.run();

    let text = announced(&harness);
    assert!(text.contains("vendor/untested"), "{text}");
    assert!(text.contains("! Not verified"), "{text}");
    assert!(
        text.contains("You typed this model id, so nothing has been tested for it."),
        "{text}"
    );
    assert!(text.contains("Not selectable"), "{text}");

    harness.get_by_label("vendor/untested").click();
    harness.run();
    assert!(
        harness.state().picker.selected().is_none(),
        "an untested model id must not become the selection"
    );
}

#[test]
fn an_unverified_model_offers_a_capability_check_behind_consent_and_a_cost_confirmation() {
    let mut card = fx::cloud_model();
    card.verification = "not_verified".to_owned();
    card.verification_reason = Some("no_observed_exchange".to_owned());
    card.selectable = false;
    let consents = vec![fx::grant("model_policy", "vendor/whisper-large", None)];
    let mut harness = picker_harness(fx::models(vec![card]), consents);
    harness.run();
    let text = announced(&harness);
    assert!(
        text.contains("No request and response has been observed for it."),
        "{text}"
    );

    harness
        .get_by_label("Run capability check on vendor/whisper-large")
        .click();
    harness.run();
    let text = announced(&harness);
    assert!(
        text.contains("Running the check sends about 5 seconds of bundled test audio"),
        "{text}"
    );
    assert!(harness.state().collected.verifications.is_empty());

    harness
        .get_by_label("Send the test clip and accept the cost")
        .click();
    harness.run();
    let checks = &harness.state().collected.verifications;
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].task, "transcription");
    assert!(checks[0].cost_acknowledged);
}

#[test]
fn a_degraded_catalogue_says_it_is_the_pinned_allowlist() {
    let mut models = fx::models(vec![fx::local_model()]);
    models.degraded_reason = Some("no_key".to_owned());
    models.catalog_source = "pinned_allowlist".to_owned();
    let mut harness = picker_harness(models, Vec::new());
    harness.run();
    let text = announced(&harness);
    assert!(text.contains("Pinned allowlist"), "{text}");
    assert!(
        text.contains("no OpenRouter key is saved, so the catalogue was not fetched"),
        "{text}"
    );
}

#[test]
fn an_unknown_price_is_stated_as_unknown_and_never_as_zero() {
    let mut harness = picker_harness(fx::models(vec![fx::local_model()]), Vec::new());
    harness.run();
    let text = announced(&harness);
    assert!(
        text.contains("Price: Price unknown (runs on this computer)"),
        "{text}"
    );
    assert!(!text.contains("USD 0.000000"), "{text}");
}

/* -------------------------------------------------------------- templates */

#[test]
fn the_templates_placeholder_says_exactly_what_is_missing() {
    let mut harness = plaudon_ui_testkit::builder().build_ui_state(
        |ui, app: &mut TemplatesPlaceholder| app.ui(ui),
        TemplatesPlaceholder,
    );
    harness.run();
    let text = announced(&harness);
    assert!(text.contains("Templates are fixed in this build"), "{text}");
    assert!(
        text.contains("a command that lists templates"),
        "the placeholder must name the missing prerequisite: {text}"
    );
}
