//! Interaction tests for the source-grounded review panel and the outputs
//! viewer — the user interface that replaces the unfinished React task I2.3
//! (#136).
//!
//! The acceptance criteria of #136 are asserted here directly: field
//! rendering including the missing-owner and missing-date placeholders, the
//! anchor seek, the absence of a seek affordance when no position exists, the
//! exclusion of ungrounded claims from the verified view, and full keyboard
//! operation.

mod ai_fixtures;

use ai_fixtures as fx;
use egui_kittest::kittest::Queryable as _;
use egui_kittest::Harness;
use plaudon_app::command::AppCommand;
use plaudon_domain::transcript::AlignmentOrigin;
use plaudon_ui::ai::{
    AiIntent, AiOutput, OutputsViewer, RecordingContext, ReviewPanel, SeekRequest,
};

/// A window tall enough that no control of a panel is clipped.
///
/// The shell hosts these panels inside a scroll area; a test that scrolled
/// before every click would assert on scrolling, not on the panel. The
/// deterministic width from `plaudon-ui-testkit` is kept, and these tests take
/// no pixel snapshot, so the snapshot policy is unaffected.
const TALL: egui::Vec2 = egui::vec2(1280.0, 4000.0);

#[derive(Default)]
struct Collected {
    names: Vec<&'static str>,
    reviews: Vec<plaudon_app::dto::ReviewClaimRequest>,
    regenerations: Vec<plaudon_app::dto::RegenerateOutputRequest>,
    seeks: Vec<SeekRequest>,
}

impl Collected {
    fn absorb(&mut self, mut output: AiOutput) {
        output.drain_seeks_into(&mut self.seeks);
        for intent in output.intents {
            if let AiIntent::Command(command) = intent {
                self.names.push(command.name());
                match command {
                    AppCommand::ReviewClaim(request) => self.reviews.push(request),
                    AppCommand::RegenerateOutput(request) => self.regenerations.push(request),
                    _ => {}
                }
            }
        }
    }
}

struct ReviewApp {
    panel: ReviewPanel,
    capabilities: plaudon_app::dto::Capabilities,
    outputs: plaudon_app::dto::ListOutputsResponse,
    collected: Collected,
}

fn review_harness(outputs: plaudon_app::dto::ListOutputsResponse) -> Harness<'static, ReviewApp> {
    plaudon_ui_testkit::builder()
        .with_size(TALL)
        .build_ui_state(
            |ui, app: &mut ReviewApp| {
                egui::Frame::NONE.show(ui, |ui| {
                    let context = RecordingContext {
                        recording_id: fx::RECORDING_ID,
                        duration_ms: Some(600_000),
                        revision_id: Some(fx::REVISION_ID),
                        alignment_origin: AlignmentOrigin::Native,
                        title: "Board meeting",
                        capabilities: Some(&app.capabilities),
                    };
                    let output = app.panel.ui(ui, context, Some(&app.outputs));
                    app.collected.absorb(output);
                });
            },
            ReviewApp {
                panel: ReviewPanel::default(),
                capabilities: fx::capabilities(false, Vec::new()),
                outputs,
                collected: Collected::default(),
            },
        )
}

fn announced<State>(harness: &Harness<'_, State>) -> String {
    plaudon_ui_testkit::a11y::nodes(harness)
        .into_iter()
        .filter_map(|node| node.announced().map(str::to_owned))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn a_grounded_claim_shows_its_state_its_fields_and_its_excerpt() {
    let claim = fx::action_item_claim("source_linked", false, vec![fx::anchor(true, Some(90_000))]);
    let mut harness = review_harness(fx::outputs(vec![fx::output(vec![claim], None)]));
    harness.run();
    let text = announced(&harness);
    assert!(text.contains("⛓ Source linked"), "{text}");
    assert!(text.contains("Owner: Unassigned"), "{text}");
    assert!(text.contains("Due date: Not specified"), "{text}");
    assert!(text.contains("we will ship on Friday"), "{text}");
    assert!(
        text.contains("✓ Anchor resolved in the transcript"),
        "{text}"
    );
}

#[test]
fn an_anchor_with_a_position_seeks_playback_to_it() {
    let claim = fx::action_item_claim("source_linked", false, vec![fx::anchor(true, Some(90_000))]);
    let mut harness = review_harness(fx::outputs(vec![fx::output(vec![claim], None)]));
    harness.run();
    harness.get_by_label("Seek to this passage").click();
    harness.run();

    let seeks = &harness.state().collected.seeks;
    assert_eq!(seeks.len(), 1);
    assert_eq!(seeks[0].position_ms, 90_000);
    assert_eq!(seeks[0].seq_index, 7);
    assert_eq!(seeks[0].claim_id.as_deref(), Some(fx::CLAIM_ID));
    assert_eq!(seeks[0].recording_id, fx::RECORDING_ID);
}

#[test]
fn an_anchor_without_a_position_offers_no_seek_and_invents_no_time() {
    let claim = fx::action_item_claim("unverified", true, vec![fx::anchor(false, None)]);
    let mut harness = review_harness(fx::outputs(vec![fx::output(vec![claim], None)]));
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
    assert!(text.contains("! Anchor did not resolve"), "{text}");
    assert!(text.contains("no_match_in_revision"), "{text}");
}

#[test]
fn a_claim_marked_for_review_states_the_reason() {
    let claim = fx::action_item_claim("unverified", true, vec![fx::anchor(false, None)]);
    let mut harness = review_harness(fx::outputs(vec![fx::output(vec![claim], None)]));
    harness.run();
    let text = announced(&harness);
    assert!(text.contains("! Review required"), "{text}");
    assert!(text.contains("anchor_unresolved"), "{text}");
    assert!(text.contains("! Unverified"), "{text}");
}

#[test]
fn the_verified_view_excludes_claims_whose_grounding_failed() {
    let failed = fx::action_item_claim("unverified", true, vec![fx::anchor(false, None)]);
    let grounded = fx::decision_claim("source_linked");
    let mut harness = review_harness(fx::outputs(vec![fx::output(vec![failed, grounded], None)]));
    harness.run();
    assert!(announced(&harness).contains("Ship the release"));

    harness.get_by_label("Verified only").click();
    harness.run();
    let text = announced(&harness);
    assert!(
        !text.contains("Ship the release"),
        "an ungrounded claim must not appear in the verified view: {text}"
    );
    assert!(text.contains("The board approved the budget"), "{text}");
}

#[test]
fn confirming_a_claim_dispatches_review_claim_with_the_note() {
    let claim = fx::action_item_claim("unverified", true, vec![fx::anchor(false, None)]);
    let mut harness = review_harness(fx::outputs(vec![fx::output(vec![claim], None)]));
    harness.run();
    harness.get_by_label("Review note (optional)").focus();
    harness.run();
    harness
        .get_by_label("Review note (optional)")
        .type_text("checked against 00:12");
    harness.run();
    harness.get_by_label("Confirm this claim").click();
    harness.run();

    let reviews = &harness.state().collected.reviews;
    assert_eq!(reviews.len(), 1, "{:?}", harness.state().collected.names);
    assert!(reviews[0].accepted);
    assert_eq!(reviews[0].claim_id, fx::CLAIM_ID);
    assert_eq!(reviews[0].output_id, fx::OUTPUT_ID);
    assert_eq!(reviews[0].note.as_deref(), Some("checked against 00:12"));
}

#[test]
fn rejecting_a_claim_dispatches_review_claim_as_rejected() {
    let claim = fx::action_item_claim("source_linked", false, vec![fx::anchor(true, Some(1_000))]);
    let mut harness = review_harness(fx::outputs(vec![fx::output(vec![claim], None)]));
    harness.run();
    harness.get_by_label("Reject this claim").click();
    harness.run();
    let reviews = &harness.state().collected.reviews;
    assert_eq!(reviews.len(), 1);
    assert!(!reviews[0].accepted);
    assert_eq!(reviews[0].note, None);
}

#[test]
fn a_stale_output_says_when_and_why_it_went_stale() {
    let claim = fx::decision_claim("source_linked");
    let mut harness = review_harness(fx::outputs(vec![fx::output(
        vec![claim],
        Some(("2026-02-02T09:00:00Z", "the transcript was edited")),
    )]));
    harness.run();
    let text = announced(&harness);
    assert!(text.contains("⧗ Stale"), "{text}");
    assert!(text.contains("2026-02-02T09:00:00Z"), "{text}");
    assert!(text.contains("the transcript was edited"), "{text}");
}

#[test]
fn regeneration_requires_an_explicit_acknowledgement() {
    let claim = fx::decision_claim("source_linked");
    let mut harness = review_harness(fx::outputs(vec![fx::output(
        vec![claim],
        Some(("2026-02-02T09:00:00Z", "the transcript was edited")),
    )]));
    harness.run();
    harness.get_by_label("Regenerate this output").click();
    harness.run();
    harness.get_by_label("Regenerate").click();
    harness.run();
    assert!(
        harness.state().collected.regenerations.is_empty(),
        "the control is disabled until the acknowledgement is given"
    );

    harness
        .get_by_label("I understand this starts a new run that may be billed")
        .click();
    harness.run();
    harness.get_by_label("Regenerate").click();
    harness.run();

    let regenerations = &harness.state().collected.regenerations;
    assert_eq!(regenerations.len(), 1);
    assert!(regenerations[0].acknowledged);
    assert_eq!(regenerations[0].output_id, fx::OUTPUT_ID);
}

#[test]
fn the_review_panel_is_fully_operable_from_the_keyboard() {
    let claim = fx::action_item_claim("source_linked", false, vec![fx::anchor(true, Some(90_000))]);
    let mut harness = review_harness(fx::outputs(vec![fx::output(vec![claim], None)]));
    harness.run();
    let order = plaudon_ui_testkit::a11y::focus_order(&mut harness, 40);
    for expected in [
        "All claims",
        "Verified only",
        "Review required",
        "Seek to this passage",
        "Confirm this claim",
        "Reject this claim",
        "Regenerate this output",
    ] {
        assert!(
            order.iter().any(|label| label.contains(expected)),
            "{expected} is unreachable by Tab: {order:?}"
        );
    }
}

#[test]
fn an_empty_output_list_says_there_is_nothing_to_review() {
    let mut harness = review_harness(fx::outputs(Vec::new()));
    harness.run();
    assert!(announced(&harness).contains("No output has been generated for this recording yet"));
}

/* -------------------------------------------------------- outputs viewer */

struct OutputsApp {
    viewer: OutputsViewer,
    capabilities: plaudon_app::dto::Capabilities,
    outputs: plaudon_app::dto::ListOutputsResponse,
    speakers: plaudon_app::dto::ListSpeakersResponse,
    collected: Collected,
}

#[test]
fn the_outputs_viewer_groups_the_section_nine_two_field_sets() {
    let claims = vec![
        fx::action_item_claim("source_linked", false, vec![fx::anchor(true, Some(1_000))]),
        fx::decision_claim("human_confirmed"),
    ];
    let mut harness = plaudon_ui_testkit::builder()
        .with_size(TALL)
        .build_ui_state(
            |ui, app: &mut OutputsApp| {
                egui::Frame::NONE.show(ui, |ui| {
                    let context = RecordingContext {
                        recording_id: fx::RECORDING_ID,
                        duration_ms: Some(600_000),
                        revision_id: Some(fx::REVISION_ID),
                        alignment_origin: AlignmentOrigin::Native,
                        title: "Board meeting",
                        capabilities: Some(&app.capabilities),
                    };
                    let output = app.viewer.ui(
                        ui,
                        context,
                        Some(&app.outputs),
                        Some(&app.speakers),
                        fx::NOW_MS,
                    );
                    app.collected.absorb(output);
                });
            },
            OutputsApp {
                viewer: OutputsViewer::default(),
                capabilities: fx::capabilities(false, Vec::new()),
                outputs: fx::outputs(vec![fx::output(claims, None)]),
                // Renamed but not confirmed, which is the only state in
                // which "Confirm speaker" is offered.
                speakers: plaudon_app::dto::ListSpeakersResponse {
                    speakers: vec![fx::speaker(Some("Dana"), false)],
                },
                collected: Collected::default(),
            },
        );
    harness.run();
    let text = announced(&harness);
    assert!(text.contains("Key decisions"), "{text}");
    assert!(text.contains("Action items"), "{text}");
    assert!(text.contains("Owner: Unassigned"), "{text}");
    assert!(text.contains("Due date: Not specified"), "{text}");
    assert!(text.contains("Participants: Not evidenced"), "{text}");
    assert!(text.contains("Speaker identification"), "{text}");
    assert!(text.contains("Machine label: SPEAKER_00"), "{text}");
    assert!(
        text.contains("Speaker: Dana (inferred cluster)"),
        "a typed name is not an identity until it is confirmed: {text}"
    );
    assert!(text.contains("Template: meeting.v1"), "{text}");

    harness.get_by_label("Confirm speaker").click();
    harness.run();
    assert_eq!(harness.state().collected.names, vec!["confirm_speaker"]);
}

#[test]
fn a_seek_request_becomes_an_absolute_player_seek() {
    let request = SeekRequest {
        recording_id: fx::RECORDING_ID.to_owned(),
        position_ms: 90_000,
        claim_id: Some(fx::CLAIM_ID.to_owned()),
        seq_index: 7,
    };
    assert_eq!(
        request.player_command(),
        plaudon_audio_playback::sync::PlayerCommand::Seek(
            plaudon_audio_playback::time::Micros::from_millis(90_000)
        )
    );
}
