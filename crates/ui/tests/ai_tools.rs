//! Interaction tests for the transcript-tool panels — the ask-about-recording
//! chat, the study viewer and the tool-run provenance list
//! (`docs/EGUI_MIGRATION_PLAN.md` row A11, EPIC O task O2.6).
//!
//! Every assertion is made the way a user or a screen reader would make it:
//! find a control by the words on it, act on it, and check which
//! [`plaudon_app::command::AppCommand`] or [`plaudon_ui::ai::SeekRequest`] the
//! panel produced. No test reaches into a panel's private state, and no test
//! takes a pixel snapshot.

mod ai_fixtures;

use ai_fixtures as fx;
use egui_kittest::kittest::Queryable as _;
use egui_kittest::Harness;
use plaudon_app::command::AppCommand;
use plaudon_app::dto::{CostConfidence, CostValue};
use plaudon_domain::transcript::AlignmentOrigin;
use plaudon_ui::ai::{
    AiIntent, AiOutput, AskPanel, RecordingContext, SeekRequest, StudyPanel, ToolRunsPanel,
};

/// A window tall enough that no control of a panel is clipped.
///
/// The shell hosts these panels inside a scroll area; a test that scrolled
/// before every click would assert on scrolling, not on the panel.
const TALL: egui::Vec2 = egui::vec2(1280.0, 6000.0);

#[derive(Default)]
struct Collected {
    names: Vec<&'static str>,
    asks: Vec<plaudon_app::dto::AskRecordingRequest>,
    runs: Vec<plaudon_app::dto::GenerateStudyArtifactsRequest>,
    grants: Vec<plaudon_app::dto::GrantCloudConsentRequest>,
    seeks: Vec<SeekRequest>,
    copied: Vec<String>,
}

impl Collected {
    fn absorb(&mut self, mut output: AiOutput) {
        output.drain_seeks_into(&mut self.seeks);
        for intent in output.intents {
            if let AiIntent::Command(command) = intent {
                self.names.push(command.name());
                match command {
                    AppCommand::AskRecording(request) => self.asks.push(request),
                    AppCommand::GenerateStudyArtifacts(request) => self.runs.push(request),
                    AppCommand::GrantCloudConsent(request) => self.grants.push(request),
                    _ => {}
                }
            }
        }
    }

    /// Record what this frame put on the clipboard.
    fn absorb_clipboard(&mut self, ui: &egui::Ui) {
        ui.ctx().output(|out| {
            for command in &out.commands {
                if let egui::OutputCommand::CopyText(text) = command {
                    self.copied.push(text.clone());
                }
            }
        });
    }
}

fn announced<State>(harness: &Harness<'_, State>) -> String {
    plaudon_ui_testkit::a11y::nodes(harness)
        .into_iter()
        .filter_map(|node| node.announced().map(str::to_owned))
        .collect::<Vec<_>>()
        .join("\n")
}

/* ------------------------------------------------------------- ask panel */

struct AskApp {
    panel: AskPanel,
    capabilities: plaudon_app::dto::Capabilities,
    tools: plaudon_app::dto::ListTranscriptToolsResponse,
    conversations: Option<plaudon_app::dto::ListConversationsResponse>,
    latest: Option<plaudon_app::dto::AskRecordingResponse>,
    models: plaudon_app::dto::ListModelsResponse,
    collected: Collected,
}

impl AskApp {
    fn new(route: plaudon_app::dto::TextRouteDto) -> Self {
        Self {
            panel: AskPanel::default(),
            capabilities: fx::capabilities(false, Vec::new()),
            tools: fx::transcript_tools(Vec::new(), Some(route)),
            conversations: None,
            latest: None,
            models: fx::models(vec![fx::local_model(), fx::cloud_model()]),
            collected: Collected::default(),
        }
    }

    fn local_only(mut self) -> Self {
        self.capabilities = fx::capabilities(true, Vec::new());
        self
    }

    fn with_answer(mut self, answer: plaudon_app::dto::AskRecordingResponse) -> Self {
        self.latest = Some(answer);
        self
    }

    fn with_conversations(
        mut self,
        conversations: plaudon_app::dto::ListConversationsResponse,
    ) -> Self {
        self.conversations = Some(conversations);
        self
    }

    fn view(&mut self, ui: &mut egui::Ui) {
        let context = RecordingContext {
            recording_id: fx::RECORDING_ID,
            duration_ms: Some(600_000),
            revision_id: Some(fx::REVISION_ID),
            alignment_origin: AlignmentOrigin::Native,
            title: "Board meeting",
            capabilities: Some(&self.capabilities),
        };
        let output = self.panel.ui(
            ui,
            context,
            Some(&self.tools),
            self.conversations.as_ref(),
            self.latest.as_ref(),
            Some(&self.models),
        );
        self.collected.absorb(output);
    }
}

fn ask_harness(app: AskApp) -> Harness<'static, AskApp> {
    plaudon_ui_testkit::builder()
        .with_size(TALL)
        .build_ui_state(|ui, app: &mut AskApp| app.view(ui), app)
}

fn ask_question(harness: &mut Harness<'_, AskApp>, question: &str) {
    harness.get_by_label("Your question").focus();
    harness.run();
    harness.get_by_label("Your question").type_text(question);
    harness.run();
    harness.get_by_label("Ask this question").click();
    harness.run();
}

#[test]
fn asking_a_question_emits_ask_recording_with_the_typed_question() {
    let mut harness = ask_harness(AskApp::new(fx::local_text_route()));
    harness.run();
    ask_question(&mut harness, "When does the release ship?");

    let collected = &harness.state().collected;
    assert_eq!(collected.asks.len(), 1, "{:?}", collected.names);
    let request = &collected.asks[0];
    assert_eq!(request.question, "When does the release ship?");
    assert_eq!(request.recording_id, fx::RECORDING_ID);
    assert_eq!(request.revision_id.as_deref(), Some(fx::REVISION_ID));
    assert_eq!(
        request.model_override.as_ref().expect("override").engine,
        "whisper.cpp"
    );
    assert!(!request.cost_acknowledged);
    assert_eq!(collected.names, ["ask_recording"]);
}

#[test]
fn an_empty_question_is_not_sent() {
    let mut harness = ask_harness(AskApp::new(fx::local_text_route()));
    harness.run();
    assert!(
        announced(&harness).contains("An empty question is not sent."),
        "{}",
        announced(&harness)
    );
    harness.get_by_label("Ask this question").click();
    harness.run();
    assert!(
        harness.state().collected.names.is_empty(),
        "the control is disabled until a question is typed: {:?}",
        harness.state().collected.names
    );
}

#[test]
fn an_answered_response_renders_its_anchors_and_seeks_to_the_cited_position() {
    let answer = fx::ask_answer(
        "answered",
        "The release ships on Friday.",
        vec![fx::anchor(true, Some(90_000))],
        Vec::new(),
        true,
    );
    let mut harness = ask_harness(AskApp::new(fx::local_text_route()).with_answer(answer));
    harness.run();

    let text = announced(&harness);
    assert!(text.contains("✓ Answered from the recording"), "{text}");
    assert!(text.contains("The release ships on Friday."), "{text}");
    assert!(text.contains("we will ship on Friday"), "{text}");
    assert!(text.contains("Segment: 7"), "{text}");
    assert!(
        text.contains("✓ Anchor resolved in the transcript"),
        "{text}"
    );

    harness.get_by_label("Seek to this passage").click();
    harness.run();
    let seeks = &harness.state().collected.seeks;
    assert_eq!(seeks.len(), 1, "exactly one seek request");
    assert_eq!(seeks[0].position_ms, 90_000);
    assert_eq!(seeks[0].seq_index, 7);
    assert_eq!(seeks[0].recording_id, fx::RECORDING_ID);
    assert_eq!(seeks[0].claim_id, None);
}

#[test]
fn an_anchor_without_a_position_offers_no_seek_control_at_all() {
    let answer = fx::ask_answer(
        "answered",
        "The release ships on Friday.",
        vec![fx::anchor(true, None)],
        Vec::new(),
        true,
    );
    let mut harness = ask_harness(AskApp::new(fx::local_text_route()).with_answer(answer));
    harness.run();
    let text = announced(&harness);
    assert!(
        !text.contains("Seek to this passage"),
        "no seek affordance may exist without a reported position: {text}"
    );
    assert!(
        text.contains("This passage has no playback position"),
        "{text}"
    );
}

#[test]
fn not_supported_by_recording_is_an_answer_and_reports_that_nothing_was_sent() {
    let answer = fx::ask_answer(
        "not_supported_by_recording",
        "This recording does not discuss the hiring plan.",
        Vec::new(),
        Vec::new(),
        false,
    );
    let mut harness = ask_harness(AskApp::new(fx::local_text_route()).with_answer(answer));
    harness.run();
    let text = announced(&harness);
    assert!(text.contains("∅ Not supported by this recording"), "{text}");
    assert!(
        text.contains("This recording does not discuss the hiring plan."),
        "{text}"
    );
    assert!(text.contains("⌂ Nothing left this device"), "{text}");
    assert!(
        !text.contains("Something went wrong"),
        "an unanswerable question is not an error: {text}"
    );
}

#[test]
fn insufficient_evidence_renders_the_degraded_outcome_and_its_rejected_anchors() {
    let answer = fx::ask_answer(
        "insufficient_evidence",
        "No answer was produced.",
        Vec::new(),
        vec![
            fx::rejected_anchor("excerpt_mismatch"),
            fx::rejected_anchor("segment_out_of_range"),
        ],
        true,
    );
    let mut harness = ask_harness(AskApp::new(fx::local_text_route()).with_answer(answer));
    harness.run();
    let text = announced(&harness);
    assert!(
        text.contains("! Not enough evidence for an answer"),
        "{text}"
    );
    assert!(
        text.contains("the retrieved passages do not support an answer"),
        "{text}"
    );
    assert!(text.contains("Rejected citations (2)"), "{text}");
    assert!(text.contains("excerpt_mismatch"), "{text}");
    assert!(text.contains("segment_out_of_range"), "{text}");
    assert!(text.contains("the budget tripled"), "{text}");
    assert!(text.contains("∅ No source anchor"), "{text}");
}

#[test]
fn a_conversation_pinned_to_a_superseded_revision_says_so() {
    let turns = vec![
        fx::turn(0, "user", "When does the release ship?", None, Vec::new()),
        fx::turn(
            1,
            "assistant",
            "On Friday.",
            Some("answered"),
            vec![fx::anchor(true, Some(90_000))],
        ),
    ];
    let mut harness = ask_harness(
        AskApp::new(fx::local_text_route()).with_conversations(fx::conversations(false, turns)),
    );
    harness.run();
    let text = announced(&harness);
    assert!(
        text.contains("⧗ Answered from a superseded revision"),
        "{text}"
    );
    assert!(text.contains("You asked"), "{text}");
    assert!(text.contains("When does the release ship?"), "{text}");
    assert!(text.contains("On Friday."), "{text}");
    assert!(text.contains("✓ Answered from the recording"), "{text}");
}

#[test]
fn a_cloud_route_without_a_grant_opens_the_consent_sheet_and_sends_nothing_first() {
    let mut harness = ask_harness(AskApp::new(fx::cloud_text_route()));
    harness.run();
    assert!(
        announced(&harness).contains("Consent is required before this runs"),
        "{}",
        announced(&harness)
    );
    ask_question(&mut harness, "What was decided?");

    assert!(
        harness.state().collected.asks.is_empty(),
        "nothing may be dispatched before the disclosure is answered"
    );
    let text = announced(&harness);
    assert!(
        text.contains("Send this content to a cloud provider?"),
        "{text}"
    );
    assert!(text.contains("openrouter → vendor"), "{text}");

    // "Ask every time" authorises nothing, so the question still does not go.
    harness
        .get_by_label("Save preference without sending")
        .click();
    harness.run();
    assert!(
        harness.state().collected.asks.is_empty(),
        "'Ask every time' is not permission to dispatch"
    );
    assert_eq!(harness.state().collected.grants.len(), 1);

    harness.get_by_label("Ask this question").click();
    harness.run();
    harness.get_by_label("Allow for this recording").click();
    harness.run();
    harness.get_by_label("Approve and continue").click();
    harness.run();

    let collected = &harness.state().collected;
    assert_eq!(collected.asks.len(), 1, "{:?}", collected.names);
    assert_eq!(collected.asks[0].question, "What was decided?");
    assert_eq!(collected.grants.len(), 2);
    assert_eq!(collected.grants[1].scope.kind, "recording");
}

#[test]
fn an_unestimable_charge_requires_an_explicit_acknowledgement() {
    // A model the catalogue does not list has no price at all, so the charge
    // cannot be estimated and §10.4 requires the acknowledgement.
    let route = fx::text_route("openrouter", Some("vendor/unlisted-text"), true);
    let mut harness = ask_harness(AskApp::new(route));
    harness.run();
    assert!(
        announced(&harness).contains("Cost unknown"),
        "{}",
        announced(&harness)
    );

    ask_question(&mut harness, "What was decided?");
    assert!(
        harness.state().collected.names.is_empty(),
        "the control stays disabled until the charge is acknowledged: {:?}",
        harness.state().collected.names
    );

    harness
        .get_by_label("I accept that the charge for this run cannot be estimated in advance")
        .click();
    harness.run();
    harness.get_by_label("Ask this question").click();
    harness.run();
    harness.get_by_label("Allow for this recording").click();
    harness.run();
    harness.get_by_label("Approve and continue").click();
    harness.run();

    let collected = &harness.state().collected;
    assert_eq!(collected.asks.len(), 1, "{:?}", collected.names);
    assert!(
        collected.asks[0].cost_acknowledged,
        "the acknowledgement must travel with the request"
    );
}

#[test]
fn local_only_mode_refuses_the_cloud_route_with_the_published_copy() {
    let mut harness = ask_harness(AskApp::new(fx::cloud_text_route()).local_only());
    harness.run();
    let text = announced(&harness);
    assert!(text.contains("⛔ Refused"), "{text}");
    assert!(
        text.contains(plaudon_app::cloud_presentation::LOCAL_ONLY_REFUSAL),
        "the refusal must be the published copy: {text}"
    );

    ask_question(&mut harness, "What was decided?");
    assert!(
        harness.state().collected.names.is_empty(),
        "a refused route emits nothing: {:?}",
        harness.state().collected.names
    );
    assert!(
        !announced(&harness).contains("Send this content to a cloud provider?"),
        "the disclosure must not open for a refused route"
    );
}

#[test]
fn the_ask_panel_is_fully_operable_from_the_keyboard() {
    let answer = fx::ask_answer(
        "answered",
        "The release ships on Friday.",
        vec![fx::anchor(true, Some(90_000))],
        Vec::new(),
        true,
    );
    let mut harness = ask_harness(AskApp::new(fx::local_text_route()).with_answer(answer));
    harness.run();
    // A disabled control is deliberately not focusable, so the question is
    // typed first: the sweep then has to reach every control the user can
    // actually operate in this state.
    harness.get_by_label("Your question").focus();
    harness.run();
    harness
        .get_by_label("Your question")
        .type_text("When does the release ship?");
    harness.run();
    let order = plaudon_ui_testkit::a11y::focus_order(&mut harness, 40);
    for expected in ["Your question", "Ask this question", "Seek to this passage"] {
        assert!(
            order.iter().any(|label| label.contains(expected)),
            "{expected} is unreachable by Tab: {order:?}"
        );
    }
}

/* ----------------------------------------------------------- study panel */

struct StudyApp {
    panel: StudyPanel,
    capabilities: plaudon_app::dto::Capabilities,
    tools: plaudon_app::dto::ListTranscriptToolsResponse,
    latest: Option<plaudon_app::dto::GenerateStudyArtifactsResponse>,
    stored: Option<plaudon_app::dto::ListStudyArtifactsResponse>,
    models: plaudon_app::dto::ListModelsResponse,
    collected: Collected,
}

impl StudyApp {
    fn new() -> Self {
        Self {
            panel: StudyPanel::default(),
            capabilities: fx::capabilities(false, Vec::new()),
            tools: fx::study_tools(fx::local_text_route()),
            latest: None,
            stored: None,
            models: fx::models(vec![fx::local_model(), fx::cloud_model()]),
            collected: Collected::default(),
        }
    }

    fn with_result(mut self, result: plaudon_app::dto::GenerateStudyArtifactsResponse) -> Self {
        self.latest = Some(result);
        self
    }

    fn view(&mut self, ui: &mut egui::Ui) {
        let context = RecordingContext {
            recording_id: fx::RECORDING_ID,
            duration_ms: Some(600_000),
            revision_id: Some(fx::REVISION_ID),
            alignment_origin: AlignmentOrigin::Native,
            title: "Board meeting",
            capabilities: Some(&self.capabilities),
        };
        let output = self.panel.ui(
            ui,
            context,
            Some(&self.tools),
            self.latest.as_ref(),
            self.stored.as_ref(),
            Some(&self.models),
        );
        self.collected.absorb(output);
        self.collected.absorb_clipboard(ui);
    }
}

fn study_harness(app: StudyApp) -> Harness<'static, StudyApp> {
    plaudon_ui_testkit::builder()
        .with_size(TALL)
        .build_ui_state(|ui, app: &mut StudyApp| app.view(ui), app)
}

#[test]
fn a_study_run_emits_generate_study_artifacts_for_the_chosen_tool() {
    let mut harness = study_harness(study_app_with_artifacts());
    harness.run();
    harness.get_by_label("Flashcards").click();
    harness.run();
    harness.get_by_label("Run this study tool").click();
    harness.run();

    let collected = &harness.state().collected;
    assert_eq!(collected.runs.len(), 1, "{:?}", collected.names);
    assert_eq!(collected.runs[0].tool_id, "flashcards");
    assert_eq!(collected.runs[0].recording_id, fx::RECORDING_ID);
    assert_eq!(
        collected.runs[0].revision_id.as_deref(),
        Some(fx::REVISION_ID)
    );
    assert!(!collected.runs[0].cost_acknowledged);
}

#[test]
fn a_tool_that_cannot_run_says_why_it_is_not_offered() {
    let mut harness = study_harness(StudyApp::new());
    harness.run();
    let text = announced(&harness);
    assert!(text.contains("⊘ Not offered"), "{text}");
    assert!(
        text.contains("This model does not guarantee schema-constrained output."),
        "{text}"
    );
    assert!(
        !text.contains("Meeting summary"),
        "a default meeting output is not a study tool: {text}"
    );
}

#[test]
fn a_study_result_renders_the_artifacts_with_their_citations_and_refusals() {
    let mut harness = study_harness(study_app_with_artifacts());
    harness.run();
    let text = announced(&harness);

    assert!(text.contains("Flashcards (1)"), "{text}");
    assert!(
        text.contains("Front: When does the release ship?"),
        "{text}"
    );
    assert!(text.contains("Back: On Friday."), "{text}");
    assert!(text.contains("Quiz (1)"), "{text}");
    assert!(
        text.contains("Which day was agreed for the release?"),
        "{text}"
    );
    assert!(text.contains("2. Friday"), "{text}");
    assert!(text.contains("Answer key: 2. Friday"), "{text}");
    assert!(text.contains("Definitions (1)"), "{text}");
    assert!(text.contains("Term: Release window"), "{text}");
    assert!(
        text.contains("we will ship on Friday"),
        "the citations: {text}"
    );
    assert!(text.contains("⌂ Nothing left this device"), "{text}");

    assert!(text.contains("Refused items (1)"), "{text}");
    assert!(text.contains("Path: /flashcards/3"), "{text}");
    assert!(text.contains("This card cited no passage."), "{text}");
    assert!(text.contains("Reason: missing_anchor"), "{text}");
}

#[test]
fn a_study_set_is_exported_to_the_clipboard_in_three_renderings() {
    let mut harness = study_harness(study_app_with_artifacts());
    harness.run();
    for label in ["Copy as Markdown", "Copy as plain text", "Copy as JSON"] {
        harness.get_by_label(label).click();
        harness.run();
    }
    let copied = &harness.state().collected.copied;
    assert_eq!(copied.len(), 3, "one clipboard write per rendering");
    assert!(
        copied.iter().all(|text| !text.trim().is_empty()),
        "no rendering may be empty: {copied:?}"
    );
    assert_ne!(copied[0], copied[1]);
    assert_ne!(copied[1], copied[2]);
    assert_ne!(copied[0], copied[2]);
}

#[test]
fn the_study_panel_is_fully_operable_from_the_keyboard() {
    let mut harness = study_harness(study_app_with_artifacts());
    harness.run();
    // The run control is disabled, and so unfocusable, until a tool is
    // chosen; the sweep covers the state in which the panel can be used.
    harness.get_by_label("Flashcards").click();
    harness.run();
    let order = plaudon_ui_testkit::a11y::focus_order(&mut harness, 60);
    for expected in [
        "Flashcards",
        "Run this study tool",
        "Copy as Markdown",
        "Seek to this passage",
    ] {
        assert!(
            order.iter().any(|label| label.contains(expected)),
            "{expected} is unreachable by Tab: {order:?}"
        );
    }
}

fn study_app_with_artifacts() -> StudyApp {
    StudyApp::new().with_result(fx::study_run(
        fx::study_artifacts(),
        vec![fx::rejected_study_item(
            "/flashcards/3",
            "missing_anchor",
            "This card cited no passage.",
        )],
        false,
    ))
}

/* ------------------------------------------------------- tool run list */

struct RunsApp {
    panel: ToolRunsPanel,
    runs: plaudon_app::dto::ListToolRunsResponse,
}

fn runs_harness(runs: plaudon_app::dto::ListToolRunsResponse) -> Harness<'static, RunsApp> {
    plaudon_ui_testkit::builder()
        .with_size(TALL)
        .build_ui_state(
            |ui, app: &mut RunsApp| app.panel.ui(ui, Some(&app.runs)),
            RunsApp {
                panel: ToolRunsPanel,
                runs,
            },
        )
}

#[test]
fn the_tool_run_list_renders_an_unknown_cost_as_unknown() {
    let mut harness = runs_harness(fx::tool_runs(vec![fx::tool_run(
        "succeeded",
        CostValue::unknown(),
        true,
    )]));
    harness.run();
    let text = announced(&harness);
    assert!(text.contains("? Cost unknown"), "{text}");
    assert!(
        !text.contains("0.00"),
        "an unknown cost is never rendered as zero: {text}"
    );
}

#[test]
fn the_tool_run_list_shows_the_provenance_of_each_run() {
    let mut harness = runs_harness(fx::tool_runs(vec![fx::tool_run(
        "rejected",
        CostValue {
            confidence: CostConfidence::Finalized,
            amount: Some(fx::usd("0.004000")),
        },
        true,
    )]));
    harness.run();
    let text = announced(&harness);
    assert!(text.contains("flashcards version 2"), "{text}");
    assert!(text.contains("! Output rejected"), "{text}");
    assert!(text.contains("Engine: openrouter"), "{text}");
    assert!(
        text.contains("Model: vendor/whisper-large (2026-01)"),
        "{text}"
    );
    assert!(text.contains("Prompt version: flashcards/2"), "{text}");
    assert!(
        text.contains("Output schema version: flashcards.v2"),
        "{text}"
    );
    assert!(
        text.contains("Input hash (SHA-256): 1111111111111111"),
        "{text}"
    );
    assert!(
        text.contains("Output hash (SHA-256): 2222222222222222"),
        "{text}"
    );
    assert!(text.contains("↗ Allowed to leave this device"), "{text}");
    assert!(
        text.contains("Privacy class: Left this machine; the provider may retain the content"),
        "{text}"
    );
    assert!(text.contains("Consent grant: consent-1"), "{text}");
    assert!(text.contains("= Final USD 0.004"), "{text}");
}

#[test]
fn a_run_that_stayed_on_this_device_says_so_and_names_no_grant() {
    let mut harness = runs_harness(fx::tool_runs(vec![fx::tool_run(
        "succeeded",
        CostValue::unknown(),
        false,
    )]));
    harness.run();
    let text = announced(&harness);
    assert!(text.contains("⌂ Kept on this device"), "{text}");
    assert!(
        text.contains("Privacy class: Ran entirely on this machine"),
        "{text}"
    );
    assert!(
        text.contains("Consent grant: No consent grant applied to this run."),
        "{text}"
    );
}
