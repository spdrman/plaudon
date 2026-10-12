//! The transcript-tool surfaces (§9.2, §9.3, §9.6, §9.7, §9.8).
//!
//! `docs/EGUI_MIGRATION_PLAN.md` row A11 recorded the gap these panels close:
//! the transcript tools of spec §9 had neither a command nor a user interface,
//! so a grounded answer, a study artifact and the provenance of the run that
//! produced them were all invisible. O2.6 added the six commands
//! (`list_transcript_tools`, `ask_recording`, `list_conversations`,
//! `generate_study_artifacts`, `list_study_artifacts`, `list_tool_runs`) and this module
//! renders them:
//!
//! * [`AskPanel`] — ask a question about one recording and read the answer
//!   with its citations;
//! * [`StudyPanel`] — run a study tool and read, and export, the artifacts;
//! * [`ToolRunsPanel`] — what actually ran, under which consent, at what cost.
//!
//! Like every panel in [`crate::ai`], these are host-driven: they borrow the
//! last response, return [`AiIntent`](super::AiIntent)s, and reach no port, no
//! database and no network. The rules they obey — which model a route
//! resolves to, whether a grant covers it, what an estimate may be called —
//! are [`plaudon_app::cloud_presentation`]'s, not theirs.

mod ask;
mod runs;
mod study;

pub use ask::AskPanel;
pub use runs::ToolRunsPanel;
pub use study::StudyPanel;

use egui::Ui;
use plaudon_app::cloud_presentation::{
    consent_covers, cost_presentation, custom_model_card, estimate_dispatch_cost, ProcessingStage,
    LOCAL_ONLY_REFUSAL,
};
use plaudon_app::dto::{
    AnchorDto, CostConfidence, ListModelsResponse, ModelCard, ModelOverride, RejectedAnchorDto,
    TextRouteDto,
};
use plaudon_app::tone::Tone;

use super::{fact, AiIntent, AiOutput, Badge, ConsentSheet, RecordingContext, SeekRequest};

/// The text route a tool run would take, resolved against the model catalogue.
///
/// The card is what the §4.4 disclosure and the estimate are built from, so it
/// is resolved once per frame and never stored in a panel: the host owns the
/// catalogue and the panel renders whatever the last `list_models` returned.
#[derive(Debug)]
pub(crate) struct ToolRoute {
    /// Catalogue entry for the model the route names, or the all-unknown card
    /// of [`custom_model_card`] when the catalogue does not list it.
    pub card: ModelCard,
    /// Whether running this route sends content off the device.
    pub is_cloud: bool,
}

impl ToolRoute {
    /// The per-run override that pins the run to the route that was disclosed.
    ///
    /// The user consented to *this* model; sending the resolved route back as
    /// the override is what keeps the run and the disclosure the same thing.
    pub(crate) fn model_override(&self) -> ModelOverride {
        ModelOverride {
            engine: self.card.engine.clone(),
            model_id: self.card.model_id.clone(),
        }
    }
}

/// Resolve the route `list_transcript_tools` reported into a model card.
///
/// `None` when the route names no model: a disclosure cannot name a provider
/// route it does not know, so the panel refuses to offer the run rather than
/// describing something it has not been told.
///
/// A route is treated as cloud when **either** the backend says so or the
/// capability inventory does not report the engine as network-free. Two
/// sources of the same fact disagreeing is not permission to send.
pub(crate) fn resolve_route(
    route: &TextRouteDto,
    models: Option<&ListModelsResponse>,
    context: RecordingContext<'_>,
) -> Option<ToolRoute> {
    let model_id = route.model_id.as_deref()?;
    let card = models
        .and_then(|response| {
            response
                .models
                .iter()
                .find(|card| card.model_id == model_id)
        })
        .cloned()
        .unwrap_or_else(|| custom_model_card(model_id, &route.engine));
    Some(ToolRoute {
        card,
        is_cloud: route.is_cloud || !context.engine_is_local(&route.engine),
    })
}

/// The engine, the model and where the run would happen.
pub(crate) fn route_facts(ui: &mut Ui, route: &TextRouteDto) {
    fact(ui, "Engine", &route.engine);
    fact(
        ui,
        "Model",
        route
            .model_id
            .as_deref()
            .unwrap_or("This route names no model."),
    );
    if route.is_cloud {
        Badge::new("↗", "Runs in the cloud", Tone::Warning)
            .description(
                "The retrieved transcript passages would be sent to the provider for this run.",
            )
            .ui(ui);
    } else {
        Badge::new("⌂", "Runs on this device", Tone::Success)
            .description("Nothing leaves this computer for this run.")
            .ui(ui);
    }
    if !route.available {
        Badge::new("⊘", "This route cannot run", Tone::Danger)
            .description(
                route
                    .unavailable_reason
                    .as_deref()
                    .unwrap_or("No reason was reported."),
            )
            .ui(ui);
    }
}

/// What the gate returns on the frame a run is authorised.
#[derive(Debug)]
pub(crate) struct DispatchApproval {
    /// Whether the user explicitly accepted an unestimable charge (§10.4).
    /// `false` whenever the charge could be estimated, so the flag never
    /// claims an acknowledgement that was not asked for.
    pub charge_acknowledged: bool,
}

/// The estimate, the consent gate and the run control shared by the tool
/// panels.
///
/// The same four rules `StageDispatch` applies to a
/// pipeline stage, applied to a tool run: show the estimate before dispatch
/// (§4.4), refuse a cloud route while Local-only Mode is on (§4.3), open the
/// §4.4 sheet before the first cloud run and proceed only under a scope that
/// authorises it, and require an explicit acknowledgement when the charge
/// cannot be estimated (§10.4). It is a separate type because a tool run is
/// not `start_processing`: the command is the panel's to build, so the gate
/// returns an approval instead of emitting one.
#[derive(Debug, Default)]
pub(crate) struct ToolDispatch {
    consent: ConsentSheet,
    cost_acknowledged: bool,
    pending: bool,
}

impl ToolDispatch {
    /// Draw the estimate, the gates and the run control.
    ///
    /// `ready` is the panel's own precondition — a typed question, a selected
    /// tool — and only disables the control; it never produces a sentence
    /// about what the backend would have said.
    pub(crate) fn ui(
        &mut self,
        ui: &mut Ui,
        context: RecordingContext<'_>,
        route: &ToolRoute,
        start_label: &str,
        ready: bool,
        output: &mut AiOutput,
    ) -> Option<DispatchApproval> {
        let estimate = estimate_dispatch_cost(&route.card, context.duration_ms);
        let cost = cost_presentation(&estimate);
        fact(ui, "Estimated charge before dispatch", &cost.label);
        ui.label(cost.detail);

        let blocked = context.local_only() && route.is_cloud;
        let needs_consent = route.is_cloud
            && !consent_covers(
                context.consents(),
                &route.card.model_id,
                Some(context.recording_id),
            );
        if needs_consent && !blocked {
            Badge::new("!", "Consent is required before this runs", Tone::Warning)
                .description(
                    "Nothing has been sent. The disclosure opens before anything leaves this \
                     device.",
                )
                .ui(ui);
        }

        let unknown_cost = estimate.confidence == CostConfidence::Unknown;
        if unknown_cost && route.is_cloud {
            ui.checkbox(
                &mut self.cost_acknowledged,
                "I accept that the charge for this run cannot be estimated in advance",
            );
        }
        let acknowledged = !unknown_cost || !route.is_cloud || self.cost_acknowledged;
        let charge_acknowledged = unknown_cost && self.cost_acknowledged;

        let mut approved = None;
        if ui
            .add_enabled(
                !blocked && ready && acknowledged,
                egui::Button::new(start_label),
            )
            .clicked()
        {
            if needs_consent {
                self.consent.open();
                self.pending = true;
            } else {
                approved = Some(DispatchApproval {
                    charge_acknowledged,
                });
            }
        }
        if blocked {
            Badge::new("⛔", "Refused", Tone::Danger)
                .description(LOCAL_ONLY_REFUSAL)
                .ui(ui);
        }

        if let Some(decision) = self.consent.ui(
            ui,
            &route.card,
            // A transcript tool run always dispatches, so it always has a
            // purpose and a content class; the `Option` exists for the
            // local-only stages, which never reach this panel.
            ProcessingStage::ToolRun
                .consent_purpose()
                .expect("a tool run dispatches, so it has a consent purpose"),
            ProcessingStage::ToolRun
                .content_type()
                .expect("a tool run dispatches, so it has a content class"),
            Some(context.recording_id),
            &estimate,
            output,
        ) {
            if decision.authorized && self.pending {
                approved = Some(DispatchApproval {
                    charge_acknowledged,
                });
            } else {
                // `Ask every time` stores a preference and authorises nothing,
                // so the run the user asked for does not happen and the panel
                // says so rather than silently dropping it.
                ui.label(
                    "Preference saved. \"Ask every time\" does not authorise this run, so \
                     nothing was sent.",
                );
            }
            self.pending = false;
        }
        approved
    }
}

/// One verified citation: the excerpt, where it is, and the seek control when
/// — and only when — the backend reported a position for it.
///
/// The §8.6A rule of the review panel, applied to a tool answer: an anchor
/// without a reported time gets no seek affordance at all, because a control
/// that guessed a position would be inventing evidence. The caller pushes an
/// identity for the row.
pub(crate) fn anchor_block(
    ui: &mut Ui,
    context: RecordingContext<'_>,
    anchor: &AnchorDto,
    output: &mut AiOutput,
) {
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
                        claim_id: None,
                        seq_index: anchor.seq_index,
                    }));
                }
            }
            None => {
                ui.label(
                    "This passage has no playback position, so it cannot be played from here.",
                );
            }
        }
    });
}

/// The citations of one item, or the statement that it has none.
pub(crate) fn anchors_block(
    ui: &mut Ui,
    context: RecordingContext<'_>,
    anchors: &[AnchorDto],
    output: &mut AiOutput,
) {
    if anchors.is_empty() {
        Badge::new("∅", "No source anchor", Tone::Warning)
            .description(
                "Nothing in the transcript was cited here, so it cannot be checked against the \
                 recording.",
            )
            .ui(ui);
        return;
    }
    for (index, anchor) in anchors.iter().enumerate() {
        ui.push_id(index, |ui| anchor_block(ui, context, anchor, output));
    }
}

/// The anchors the model produced that did not verify (§9.6).
///
/// Listed rather than dropped: a grounding failure the reader cannot see is a
/// grounding failure that looks like success.
pub(crate) fn rejected_anchors_block(ui: &mut Ui, rejected: &[RejectedAnchorDto]) {
    if rejected.is_empty() {
        return;
    }
    ui.label(egui::RichText::new(format!("Rejected citations ({})", rejected.len())).strong());
    ui.label(
        "These passages were quoted by the model but did not match the transcript revision this \
         answer is pinned to, so they were not accepted as evidence.",
    );
    for (index, anchor) in rejected.iter().enumerate() {
        ui.push_id(("rejected-anchor", index), |ui| {
            ui.group(|ui| {
                ui.label(egui::RichText::new(format!("“{}”", anchor.excerpt)).italics());
                fact(ui, "Claimed segment", &anchor.seq_index.to_string());
                fact(
                    ui,
                    "Claimed characters",
                    &format!("{}–{}", anchor.char_start, anchor.char_end),
                );
                Badge::new("!", "Citation rejected", Tone::Danger)
                    .description(&anchor.rejection_code)
                    .ui(ui);
            });
        });
    }
}

/// Whether anything left this device for a run, as a fact the user can read.
///
/// `false` is the good news and is stated as such: the run happened without
/// sending anything anywhere.
pub(crate) fn dispatch_badge(ui: &mut Ui, dispatched: bool) {
    if dispatched {
        Badge::new("↗", "Sent off this device", Tone::Warning)
            .description("The retrieved transcript passages were sent to the model's provider.")
            .ui(ui);
    } else {
        Badge::new("⌂", "Nothing left this device", Tone::Success)
            .description("This ran entirely on this computer; no content was sent anywhere.")
            .ui(ui);
    }
}
