//! Graceful degradation and alternative-engine policy (task H2.5).
//!
//! [`crate::stt::selection`] answers "which transcription engine may run this
//! job". This module answers the three questions that surround it, all of
//! which spec 11.4, 11.6 and DoD 22(4) make unconditional:
//!
//! 1. **Why is Apple native AI unavailable here?** An explanation built from
//!    the runtime probe's typed reason, with the corrective actions spec 5.4
//!    requires. Never a generic error.
//! 2. **What runs instead?** whisper.cpp, automatically, because it is local
//!    and the privacy posture does not change. OpenRouter never, because it
//!    does (spec 11.6: "never an automatic cloud fallback").
//! 3. **What happens when availability is lost mid-job?** The job stops and
//!    asks, unless the only change is between two engines with the same
//!    privacy class.
//!
//! The rule that generates all three answers is one sentence: *an automatic
//! engine change is permitted only when it cannot change where the data
//! goes.* Everything else is a user decision.

use plaudon_ai_contracts::capabilities::{
    AvailabilityReason, PrivacyClass, SttCapabilities, TextModelCapabilities,
};
use plaudon_domain::capability::CapabilityStatus;
use plaudon_domain::model::EngineId;

use crate::stt::selection::{
    select_engine, CloudPolicy, EngineCandidate, EngineDecision, ProbeOutcome, SelectionRequest,
    APPLE_ENGINE_ID, WHISPER_ENGINE_ID,
};

/// Engine identifier of the Apple on-device language model.
pub const APPLE_TEXT_ENGINE_ID: &str = "apple-foundation";

/// Engine identifier of the cross-platform local text model (P1, EPIC M).
pub const LOCAL_LLM_ENGINE_ID: &str = "local-llm";

/// Engine identifier of the OpenRouter gateway.
pub const OPENROUTER_ENGINE_ID: &str = "openrouter";

/// A corrective action the interface offers alongside an explanation (§5.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CorrectiveAction {
    /// Nothing the user can do; the hardware or OS decides.
    None,
    /// Turn Apple Intelligence on in System Settings.
    EnableAppleIntelligence,
    /// Install the speech assets for the chosen language.
    InstallSpeechAssets,
    /// Wait for an installation or preparation already under way.
    WaitForAssets,
    /// Choose a different language for this recording.
    ChooseAnotherLanguage,
    /// Grant the permission the operating system withheld.
    GrantPermission,
    /// Update macOS to a build that provides the feature.
    UpdateMacOs,
    /// Use the local whisper.cpp engine, which needs nothing from Apple.
    UseLocalWhisper,
    /// Retry the probe; the failure was transient.
    RetryProbe,
}

impl CorrectiveAction {
    /// Short label for a button or list item.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::None => "No action available",
            Self::EnableAppleIntelligence => "Turn on Apple Intelligence in System Settings",
            Self::InstallSpeechAssets => "Install the speech model for this language",
            Self::WaitForAssets => "Wait for the speech model to finish preparing",
            Self::ChooseAnotherLanguage => "Choose a different language",
            Self::GrantPermission => "Grant the permission in System Settings",
            Self::UpdateMacOs => "Update macOS",
            Self::UseLocalWhisper => "Transcribe with the local whisper.cpp engine",
            Self::RetryProbe => "Check again",
        }
    }
}

/// Why a native engine is unavailable, in terms the interface can show.
///
/// Built from the probe's typed [`AvailabilityReason`], so the sentence names
/// the runtime evidence rather than restating that something went wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnavailabilityExplanation {
    /// Engine the explanation is about.
    pub engine_id: EngineId,
    /// One sentence naming the observed cause.
    pub headline: String,
    /// Machine-readable discriminator, for logs and tests.
    pub code: &'static str,
    /// Actions the user can take, strongest first.
    pub actions: Vec<CorrectiveAction>,
    /// Whether the cause can change without the user doing anything.
    pub transient: bool,
}

/// Explain an [`AvailabilityReason`] for `engine_id`.
///
/// Spec 5.4 requires a specific corrective action per state, and DoD 22(4)
/// requires the explanation to cite concrete runtime evidence; both are why
/// this takes the probe's typed reason rather than a message string.
#[must_use]
pub fn explain(engine_id: &EngineId, reason: &AvailabilityReason) -> UnavailabilityExplanation {
    let (headline, code, actions, transient): (String, &'static str, Vec<CorrectiveAction>, bool) =
        match reason {
            AvailabilityReason::Available => (
                "This engine is available.".to_owned(),
                "available",
                vec![],
                false,
            ),
            AvailabilityReason::NotProbed => (
                "This engine has not been checked on this machine yet.".to_owned(),
                "not_probed",
                vec![CorrectiveAction::RetryProbe],
                true,
            ),
            AvailabilityReason::PlatformUnsupported { platform } => {
                explain_platform_unsupported(platform)
            }
            AvailabilityReason::HardwareUnsupported { detail } => (
                format!("This Mac is not eligible for Apple on-device AI ({detail})."),
                "hardware_unsupported",
                vec![CorrectiveAction::UseLocalWhisper],
                false,
            ),
            AvailabilityReason::DisabledByUser => (
                "Apple Intelligence is switched off on this Mac.".to_owned(),
                "disabled_by_user",
                vec![
                    CorrectiveAction::EnableAppleIntelligence,
                    CorrectiveAction::UseLocalWhisper,
                ],
                true,
            ),
            AvailabilityReason::ModelAssetMissing { asset } => (
                format!("The Apple speech model for {asset} is not installed on this Mac."),
                "model_asset_missing",
                vec![
                    CorrectiveAction::InstallSpeechAssets,
                    CorrectiveAction::UseLocalWhisper,
                ],
                true,
            ),
            AvailabilityReason::ModelAssetNotReady { asset } => (
                format!("The Apple speech model for {asset} is still preparing."),
                "model_asset_not_ready",
                vec![
                    CorrectiveAction::WaitForAssets,
                    CorrectiveAction::UseLocalWhisper,
                ],
                true,
            ),
            AvailabilityReason::LocaleUnsupported { locale } => (
                format!("Apple on-device speech does not support {locale}."),
                "locale_unsupported",
                vec![
                    CorrectiveAction::ChooseAnotherLanguage,
                    CorrectiveAction::UseLocalWhisper,
                ],
                false,
            ),
            AvailabilityReason::CredentialMissing => (
                "This engine needs a credential that is not stored.".to_owned(),
                "credential_missing",
                vec![CorrectiveAction::None],
                true,
            ),
            AvailabilityReason::LocalOnlyMode => (
                "Local-only mode is on, and this engine would need the network.".to_owned(),
                "local_only_mode",
                vec![CorrectiveAction::UseLocalWhisper],
                true,
            ),
            AvailabilityReason::ProbeFailed { detail } => {
                if detail.starts_with("permission_denied") {
                    (
                        format!("macOS withheld a permission this engine needs ({detail})."),
                        "permission_denied",
                        vec![
                            CorrectiveAction::GrantPermission,
                            CorrectiveAction::UseLocalWhisper,
                        ],
                        true,
                    )
                } else {
                    (
                        format!(
                            "The availability check for this engine did not succeed ({detail})."
                        ),
                        "probe_failed",
                        vec![
                            CorrectiveAction::RetryProbe,
                            CorrectiveAction::UseLocalWhisper,
                        ],
                        true,
                    )
                }
            }
        };
    UnavailabilityExplanation {
        engine_id: engine_id.clone(),
        headline,
        code,
        actions,
        transient,
    }
}

/// Split `PlatformUnsupported` by its detail, because the three ways to reach
/// it want three different sentences (#306).
///
/// A build without the bridge (or one whose SDK compiled the bridge without
/// the frameworks) is a fact about this copy of the app, and updating macOS
/// cannot change it. A Mac older than the frameworks is a fact about the
/// system, and updating can. Off macOS there is nothing Apple to offer at
/// all. An unrecognised detail is reported as observed, without guessing
/// which of those it is.
fn explain_platform_unsupported(
    platform: &str,
) -> (String, &'static str, Vec<CorrectiveAction>, bool) {
    match platform {
        "bridge_not_linked" => (
            "This copy of Plaudon was built without the Apple bridge, so it cannot use Apple \
             on-device speech or language models on any Mac (bridge_not_linked)."
                .to_owned(),
            "build_without_apple_bridge",
            vec![CorrectiveAction::UseLocalWhisper],
            false,
        ),
        "speech_framework_absent" | "foundation_models_framework_absent" => (
            format!(
                "This copy of Plaudon was built without the Apple framework this engine needs, \
                 so it cannot use it on any Mac ({platform})."
            ),
            "build_without_apple_bridge",
            vec![CorrectiveAction::UseLocalWhisper],
            false,
        ),
        detail if detail.ends_with("_requires_macos_26") => (
            format!(
                "Apple on-device speech and language models need macOS 26 or later ({detail})."
            ),
            "macos_too_old",
            vec![
                CorrectiveAction::UpdateMacOs,
                CorrectiveAction::UseLocalWhisper,
            ],
            false,
        ),
        "not_macos" => (
            "Apple on-device speech and language models are available only on macOS (not_macos)."
                .to_owned(),
            "not_macos",
            vec![CorrectiveAction::UseLocalWhisper],
            false,
        ),
        other => (
            format!("Apple on-device speech and language models are not available here ({other})."),
            "platform_unsupported",
            vec![CorrectiveAction::UseLocalWhisper],
            false,
        ),
    }
}

/// A text engine offered to the policy.
#[derive(Debug, Clone, PartialEq)]
pub struct TextEngineCandidate {
    /// What the engine declares.
    pub capabilities: TextModelCapabilities,
    /// Whether the user explicitly chose this engine for this job.
    ///
    /// The only way a cloud engine is ever selected (spec 11.4).
    pub user_selected: bool,
}

impl TextEngineCandidate {
    /// A candidate the user did not specifically ask for.
    #[must_use]
    pub const fn offered(capabilities: TextModelCapabilities) -> Self {
        Self {
            capabilities,
            user_selected: false,
        }
    }

    /// A candidate the user chose for this job.
    #[must_use]
    pub const fn chosen(capabilities: TextModelCapabilities) -> Self {
        Self {
            capabilities,
            user_selected: true,
        }
    }

    fn is_cloud(&self) -> bool {
        !self.capabilities.offline.satisfies()
    }

    fn engine_id(&self) -> &EngineId {
        &self.capabilities.engine_id
    }
}

/// Why a text engine was not chosen.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TextRejection {
    /// The capability is not validated for this configuration.
    #[error("the model is not validated for this configuration: {status:?}")]
    NotSupportedStatus {
        /// Status the capability declared.
        status: CapabilityStatus,
    },
    /// The runtime probe says the model is unavailable.
    #[error("{0}")]
    NotAvailable(String),
    /// A cloud model was offered but the user did not select it (spec 11.4).
    #[error("a cloud model is only ever used when the user selects it for this job")]
    CloudNotUserSelected,
    /// Local-only mode forbids every networked engine.
    #[error("local-only mode refuses networked engines")]
    LocalOnlyModeBlocksCloud,
    /// Cloud consent has not been granted.
    #[error("cloud processing requires explicit consent, which has not been granted")]
    CloudConsentMissing,
    /// The privacy route could not be resolved (spec 10.8).
    #[error("the privacy route for this model could not be resolved")]
    PrivacyRouteUnresolved,
    /// Structured output was required and the model does not declare it.
    #[error("the model does not declare structured output support")]
    StructuredOutputNotDeclared,
    /// Another eligible engine outranked this one.
    #[error("another candidate was preferred")]
    NotPreferred,
}

/// A text candidate and its verdict.
#[derive(Debug, Clone, PartialEq)]
pub struct TextOutcome {
    /// Engine the verdict applies to.
    pub engine_id: EngineId,
    /// `None` when selected.
    pub rejection: Option<TextRejection>,
    /// Explanation, when the runtime probe is the reason.
    pub explanation: Option<UnavailabilityExplanation>,
}

/// The explained text-engine decision.
#[derive(Debug, Clone, PartialEq)]
pub struct TextEngineDecision {
    /// Chosen engine, if any candidate qualified.
    pub chosen: Option<EngineId>,
    /// Every candidate with its verdict, in evaluation order.
    pub outcomes: Vec<TextOutcome>,
}

impl TextEngineDecision {
    /// The verdict recorded for one engine.
    #[must_use]
    pub fn rejection_for(&self, engine_id: &str) -> Option<&TextRejection> {
        self.outcomes
            .iter()
            .find(|outcome| outcome.engine_id.as_str() == engine_id)
            .and_then(|outcome| outcome.rejection.as_ref())
    }

    /// Lines the interface shows when it has to explain the decision.
    #[must_use]
    pub fn explain(&self) -> Vec<String> {
        self.outcomes
            .iter()
            .map(|outcome| match (&outcome.rejection, &outcome.explanation) {
                (None, _) => format!("{}: selected", outcome.engine_id.as_str()),
                (Some(rejection), Some(explanation)) => format!(
                    "{}: {rejection} ({})",
                    outcome.engine_id.as_str(),
                    explanation.headline
                ),
                (Some(rejection), None) => {
                    format!("{}: {rejection}", outcome.engine_id.as_str())
                }
            })
            .collect()
    }
}

/// What a text generation job needs.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TextRequirements {
    /// Whether the job needs schema-constrained output.
    pub require_structured_output: bool,
    /// Whether the job must run without leaving the machine.
    pub require_offline: bool,
}

/// Choose a text model, explaining every candidate.
///
/// On-device models are preferred, in the order Apple, cross-platform local,
/// everything else; a cloud model is only ever chosen when the user selected
/// it *for this job* and the cloud policy already permits dispatch. There is
/// no path through this function in which an unavailable Apple model causes a
/// cloud request (spec 11.4, DoD 22(4), E2E 18.7(10)).
#[must_use]
pub fn select_text_model(
    candidates: &[TextEngineCandidate],
    requirements: &TextRequirements,
    cloud_policy: CloudPolicy,
) -> TextEngineDecision {
    let mut ordered: Vec<&TextEngineCandidate> = candidates.iter().collect();
    ordered.sort_by_key(|candidate| text_preference_rank(candidate));

    let mut outcomes: Vec<TextOutcome> = ordered
        .iter()
        .map(|candidate| {
            let rejection = evaluate_text(candidate, requirements, cloud_policy);
            let explanation = if candidate.capabilities.availability_reason.is_available() {
                None
            } else {
                Some(explain(
                    candidate.engine_id(),
                    &candidate.capabilities.availability_reason,
                ))
            };
            TextOutcome {
                engine_id: candidate.engine_id().clone(),
                rejection,
                explanation,
            }
        })
        .collect();

    let winner = outcomes
        .iter()
        .position(|outcome| outcome.rejection.is_none());
    let chosen = winner.map(|index| outcomes[index].engine_id.clone());
    if let Some(winner) = winner {
        for (index, outcome) in outcomes.iter_mut().enumerate() {
            if index != winner && outcome.rejection.is_none() {
                outcome.rejection = Some(TextRejection::NotPreferred);
            }
        }
    }
    TextEngineDecision { chosen, outcomes }
}

/// Preference rank: lower is tried first. Ties break on the identifier, so
/// the decision never depends on input ordering.
fn text_preference_rank(candidate: &TextEngineCandidate) -> (u8, String) {
    let engine = candidate.engine_id().as_str().to_owned();
    let tier = match (engine.as_str(), candidate.is_cloud()) {
        (_, true) => 30,
        (APPLE_TEXT_ENGINE_ID, false) => 10,
        (LOCAL_LLM_ENGINE_ID, false) => 11,
        (_, false) => 20,
    };
    (tier, engine)
}

fn evaluate_text(
    candidate: &TextEngineCandidate,
    requirements: &TextRequirements,
    cloud_policy: CloudPolicy,
) -> Option<TextRejection> {
    let caps = &candidate.capabilities;
    if !caps.effective_status().is_selectable() {
        return Some(TextRejection::NotSupportedStatus {
            status: caps.effective_status(),
        });
    }
    if !caps.availability_reason.is_available() {
        return Some(TextRejection::NotAvailable(
            explain(candidate.engine_id(), &caps.availability_reason).headline,
        ));
    }
    if candidate.is_cloud() {
        if requirements.require_offline {
            return Some(TextRejection::LocalOnlyModeBlocksCloud);
        }
        match cloud_policy {
            CloudPolicy::LocalOnly => return Some(TextRejection::LocalOnlyModeBlocksCloud),
            CloudPolicy::NetworkWithoutConsent => return Some(TextRejection::CloudConsentMissing),
            CloudPolicy::ConsentGranted => {}
        }
        // Spec 11.4: the cloud alternative is user-selected, never a fallback
        // the application reaches for on its own.
        if !candidate.user_selected {
            return Some(TextRejection::CloudNotUserSelected);
        }
        match caps.privacy_class.get() {
            Some(class) if class.is_dispatchable() => {}
            _ => return Some(TextRejection::PrivacyRouteUnresolved),
        }
    }
    if requirements.require_structured_output && !caps.supports_structured_output.satisfies() {
        return Some(TextRejection::StructuredOutputNotDeclared);
    }
    None
}

/// What to do when an engine becomes unavailable while a job is queued or
/// running.
#[derive(Debug, Clone, PartialEq)]
pub enum Degradation {
    /// Nothing changed; keep going.
    Continue,
    /// Switch automatically. Only ever produced when the replacement has the
    /// same privacy class, so the data does not go anywhere new.
    Reroute {
        /// Engine to use instead.
        engine_id: EngineId,
        /// Why the original engine stopped being usable.
        explanation: UnavailabilityExplanation,
    },
    /// Stop and ask. The job holds a persisted waiting state rather than
    /// being silently re-routed or silently failed (spec 3, 5.4).
    AwaitUserChoice {
        /// Why the original engine stopped being usable.
        explanation: UnavailabilityExplanation,
        /// Engines the user may pick, in preference order.
        alternatives: Vec<EngineId>,
        /// Why no automatic choice was made.
        reason: DegradationBlock,
    },
}

/// Why a mid-job degradation could not be resolved automatically.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum DegradationBlock {
    /// Every remaining engine would change where the data goes.
    #[error("the only remaining engines would send this recording off the machine")]
    WouldChangePrivacyClass,
    /// Nothing is left to run the job on.
    #[error("no other engine can run this job")]
    NoAlternative,
}

/// Decide what a running job should do after availability changed.
///
/// `current` is the engine the job was configured with; `candidates` are the
/// engines still available, with their capabilities. An automatic reroute
/// requires three things to hold at once: the replacement is available, it
/// runs offline, and the engine the job was using was also offline. Losing a
/// local engine never silently promotes a cloud one.
#[must_use]
pub fn degrade(
    current: &EngineId,
    current_reason: &AvailabilityReason,
    current_was_offline: bool,
    candidates: &[SttCapabilities],
) -> Degradation {
    if current_reason.is_available() {
        return Degradation::Continue;
    }
    let explanation = explain(current, current_reason);

    let usable: Vec<&SttCapabilities> = candidates
        .iter()
        .filter(|caps| &caps.engine_id != current && caps.is_selectable())
        .collect();
    if usable.is_empty() {
        return Degradation::AwaitUserChoice {
            explanation,
            alternatives: Vec::new(),
            reason: DegradationBlock::NoAlternative,
        };
    }

    // Prefer the whisper.cpp fallback by name, then any other local engine.
    let mut local: Vec<&&SttCapabilities> = usable
        .iter()
        .filter(|caps| {
            caps.offline.satisfies() && caps.privacy_class.get() == Some(&PrivacyClass::OnDevice)
        })
        .collect();
    local.sort_by_key(|caps| {
        (
            u8::from(caps.engine_id.as_str() != WHISPER_ENGINE_ID),
            caps.engine_id.as_str().to_owned(),
        )
    });

    if current_was_offline {
        if let Some(replacement) = local.first() {
            return Degradation::Reroute {
                engine_id: replacement.engine_id.clone(),
                explanation,
            };
        }
        return Degradation::AwaitUserChoice {
            explanation,
            alternatives: usable.iter().map(|caps| caps.engine_id.clone()).collect(),
            reason: DegradationBlock::WouldChangePrivacyClass,
        };
    }

    // The job was already using a cloud engine. Moving it to a local engine
    // changes cost and quality, and moving it to another cloud engine changes
    // the data route; spec 10.6 forbids the second outright, and the first is
    // a decision worth asking about.
    Degradation::AwaitUserChoice {
        explanation,
        alternatives: usable.iter().map(|caps| caps.engine_id.clone()).collect(),
        reason: DegradationBlock::WouldChangePrivacyClass,
    }
}

/// Turn a probed capability into a selection candidate.
///
/// The selection policy refuses to infer availability from an operating
/// system version, so every platform-exclusive engine must arrive with a
/// [`ProbeOutcome`]. This is the one correct way to produce it: the outcome
/// mirrors the probe's typed [`AvailabilityReason`], and the failure string
/// is the explanation the interface will show, not a second vocabulary.
#[must_use]
pub fn candidate_from_probe(capabilities: SttCapabilities) -> EngineCandidate {
    let probe = match &capabilities.availability_reason {
        AvailabilityReason::Available => ProbeOutcome::Succeeded,
        AvailabilityReason::NotProbed => ProbeOutcome::NotRun,
        reason => ProbeOutcome::Failed(explain(&capabilities.engine_id, reason).headline),
    };
    EngineCandidate {
        capabilities,
        probe,
    }
}

/// The transcription engine an Apple-unavailable host should use.
///
/// A thin, named wrapper over [`crate::stt::selection::select_engine`] that
/// exists so the degradation path in the application reads as one call and
/// is covered by one test: Apple first when it is genuinely available,
/// whisper.cpp otherwise, never a cloud engine without consent.
#[must_use]
pub fn select_with_apple_fallback(
    request: &SelectionRequest,
    candidates: &[EngineCandidate],
) -> EngineDecision {
    select_engine(request, candidates)
}

/// Whether `decision` degraded away from Apple to the local fallback.
#[must_use]
pub fn degraded_to_local_fallback(decision: &EngineDecision) -> bool {
    decision
        .chosen
        .as_ref()
        .is_some_and(|engine| engine.as_str() == WHISPER_ENGINE_ID)
        && decision.rejection_for(APPLE_ENGINE_ID).is_some()
}

#[cfg(test)]
mod tests {
    use plaudon_ai_contracts::capabilities::{PrivacyClass, RequestFamily};
    use plaudon_ai_contracts::policy::SttRequirements;
    use plaudon_domain::capability::{Known, Tri};
    use plaudon_domain::locale::LanguageTag;
    use plaudon_domain::model::ModelIdentity;

    use crate::stt::selection::Platform;

    use super::*;

    fn stt(engine: &str, available: bool, offline: bool) -> SttCapabilities {
        let id = EngineId::new(engine);
        let mut caps = SttCapabilities::unknown(id.clone(), ModelIdentity::new(id, "m"));
        caps.status = CapabilityStatus::Supported;
        caps.availability_reason = if available {
            AvailabilityReason::Available
        } else {
            AvailabilityReason::DisabledByUser
        };
        caps.offline = Tri::from_observation(Some(offline));
        caps.privacy_class = Known::Known(if offline {
            PrivacyClass::OnDevice
        } else {
            PrivacyClass::CloudRetaining
        });
        caps.request_family = if offline {
            RequestFamily::Unsupported
        } else {
            RequestFamily::MultipartStt
        };
        caps.can_cancel = Tri::Yes;
        caps.supports_segments = Tri::Yes;
        caps.locales = Known::Known(vec![LanguageTag::parse("en-US").unwrap()]);
        caps
    }

    fn text(engine: &str, available: bool, offline: bool) -> TextModelCapabilities {
        let id = EngineId::new(engine);
        let mut caps = TextModelCapabilities::unknown(id.clone(), ModelIdentity::new(id, "m"));
        caps.status = CapabilityStatus::Supported;
        caps.availability_reason = if available {
            AvailabilityReason::Available
        } else {
            AvailabilityReason::HardwareUnsupported {
                detail: "device_not_eligible".to_owned(),
            }
        };
        caps.offline = Tri::from_observation(Some(offline));
        caps.privacy_class = Known::Known(if offline {
            PrivacyClass::OnDevice
        } else {
            PrivacyClass::CloudZeroRetention
        });
        caps.supports_structured_output = Tri::Yes;
        caps.can_cancel = Tri::Yes;
        caps
    }

    #[test]
    fn every_unavailability_reason_explains_itself_with_an_action() {
        let engine = EngineId::new(APPLE_ENGINE_ID);
        let reasons = [
            AvailabilityReason::NotProbed,
            AvailabilityReason::PlatformUnsupported {
                platform: "macos_25".into(),
            },
            AvailabilityReason::HardwareUnsupported {
                detail: "device_not_eligible".into(),
            },
            AvailabilityReason::DisabledByUser,
            AvailabilityReason::ModelAssetMissing {
                asset: "en_US".into(),
            },
            AvailabilityReason::ModelAssetNotReady {
                asset: "en_US".into(),
            },
            AvailabilityReason::LocaleUnsupported {
                locale: LanguageTag::parse("cy-GB").unwrap(),
            },
            AvailabilityReason::CredentialMissing,
            AvailabilityReason::LocalOnlyMode,
            AvailabilityReason::ProbeFailed {
                detail: "engine_timeout".into(),
            },
        ];
        let mut codes = std::collections::BTreeSet::new();
        for reason in &reasons {
            let explanation = explain(&engine, reason);
            assert!(
                !explanation.headline.is_empty(),
                "{reason:?} produced no sentence"
            );
            assert!(
                !explanation.actions.is_empty(),
                "{reason:?} offered no corrective action"
            );
            codes.insert(explanation.code);
        }
        assert_eq!(codes.len(), reasons.len(), "codes must be distinct");
    }

    #[test]
    fn a_withheld_permission_is_not_reported_as_a_generic_probe_failure() {
        let explanation = explain(
            &EngineId::new(APPLE_ENGINE_ID),
            &AvailabilityReason::ProbeFailed {
                detail: "permission_denied:speech_recognition_denied".into(),
            },
        );
        assert_eq!(explanation.code, "permission_denied");
        assert!(explanation
            .actions
            .contains(&CorrectiveAction::GrantPermission));
    }

    #[test]
    fn every_apple_failure_offers_the_local_engine() {
        let engine = EngineId::new(APPLE_ENGINE_ID);
        for reason in [
            AvailabilityReason::PlatformUnsupported {
                platform: "x".into(),
            },
            AvailabilityReason::HardwareUnsupported { detail: "x".into() },
            AvailabilityReason::DisabledByUser,
            AvailabilityReason::ModelAssetMissing { asset: "x".into() },
            AvailabilityReason::ProbeFailed { detail: "x".into() },
        ] {
            let explanation = explain(&engine, &reason);
            assert!(
                explanation
                    .actions
                    .contains(&CorrectiveAction::UseLocalWhisper),
                "{reason:?} did not offer the local engine"
            );
        }
    }

    #[test]
    fn apple_unavailable_selects_whisper_and_never_the_cloud() {
        let request = SelectionRequest {
            platform: Platform::MacOs,
            requirements: SttRequirements {
                locale: Some(LanguageTag::parse("en-US").unwrap()),
                ..SttRequirements::default()
            },
            cloud_policy: CloudPolicy::ConsentGranted,
        };
        let candidates = vec![
            EngineCandidate {
                capabilities: stt(APPLE_ENGINE_ID, false, true),
                probe: ProbeOutcome::Failed("apple_intelligence_not_enabled".into()),
            },
            EngineCandidate::probed(stt(WHISPER_ENGINE_ID, true, true)),
            EngineCandidate::probed(stt(OPENROUTER_ENGINE_ID, true, false)),
        ];
        let decision = select_with_apple_fallback(&request, &candidates);
        assert_eq!(
            decision.chosen.as_ref().map(EngineId::as_str),
            Some(WHISPER_ENGINE_ID)
        );
        assert!(degraded_to_local_fallback(&decision));
    }

    #[test]
    fn an_unavailable_apple_text_model_never_promotes_openrouter() {
        let candidates = vec![
            TextEngineCandidate::offered(text(APPLE_TEXT_ENGINE_ID, false, true)),
            TextEngineCandidate::offered(text(OPENROUTER_ENGINE_ID, true, false)),
        ];
        let decision = select_text_model(
            &candidates,
            &TextRequirements::default(),
            CloudPolicy::ConsentGranted,
        );
        assert_eq!(decision.chosen, None, "{:?}", decision.explain());
        assert_eq!(
            decision.rejection_for(OPENROUTER_ENGINE_ID),
            Some(&TextRejection::CloudNotUserSelected)
        );
        let apple = decision
            .outcomes
            .iter()
            .find(|o| o.engine_id.as_str() == APPLE_TEXT_ENGINE_ID)
            .unwrap();
        assert_eq!(
            apple.explanation.as_ref().unwrap().code,
            "hardware_unsupported"
        );
    }

    #[test]
    fn a_user_selected_cloud_model_is_used_once_consent_exists() {
        let candidates = vec![
            TextEngineCandidate::offered(text(APPLE_TEXT_ENGINE_ID, false, true)),
            TextEngineCandidate::chosen(text(OPENROUTER_ENGINE_ID, true, false)),
        ];
        let decision = select_text_model(
            &candidates,
            &TextRequirements::default(),
            CloudPolicy::ConsentGranted,
        );
        assert_eq!(
            decision.chosen.as_ref().map(EngineId::as_str),
            Some(OPENROUTER_ENGINE_ID)
        );
    }

    #[test]
    fn a_user_selected_cloud_model_still_needs_consent() {
        let candidates = vec![TextEngineCandidate::chosen(text(
            OPENROUTER_ENGINE_ID,
            true,
            false,
        ))];
        for (policy, expected) in [
            (
                CloudPolicy::LocalOnly,
                TextRejection::LocalOnlyModeBlocksCloud,
            ),
            (
                CloudPolicy::NetworkWithoutConsent,
                TextRejection::CloudConsentMissing,
            ),
        ] {
            let decision = select_text_model(&candidates, &TextRequirements::default(), policy);
            assert_eq!(decision.chosen, None);
            assert_eq!(
                decision.rejection_for(OPENROUTER_ENGINE_ID),
                Some(&expected)
            );
        }
    }

    #[test]
    fn apple_text_outranks_the_cross_platform_local_model() {
        let candidates = vec![
            TextEngineCandidate::offered(text(LOCAL_LLM_ENGINE_ID, true, true)),
            TextEngineCandidate::offered(text(APPLE_TEXT_ENGINE_ID, true, true)),
        ];
        let decision = select_text_model(
            &candidates,
            &TextRequirements::default(),
            CloudPolicy::LocalOnly,
        );
        assert_eq!(
            decision.chosen.as_ref().map(EngineId::as_str),
            Some(APPLE_TEXT_ENGINE_ID)
        );
        assert_eq!(
            decision.rejection_for(LOCAL_LLM_ENGINE_ID),
            Some(&TextRejection::NotPreferred)
        );
    }

    #[test]
    fn structured_output_is_required_fail_closed() {
        let mut caps = text(APPLE_TEXT_ENGINE_ID, true, true);
        caps.supports_structured_output = Tri::Unknown;
        let decision = select_text_model(
            &[TextEngineCandidate::offered(caps)],
            &TextRequirements {
                require_structured_output: true,
                require_offline: true,
            },
            CloudPolicy::LocalOnly,
        );
        assert_eq!(
            decision.rejection_for(APPLE_TEXT_ENGINE_ID),
            Some(&TextRejection::StructuredOutputNotDeclared)
        );
    }

    #[test]
    fn selection_is_deterministic_regardless_of_candidate_order() {
        let a = TextEngineCandidate::offered(text(APPLE_TEXT_ENGINE_ID, true, true));
        let b = TextEngineCandidate::offered(text(LOCAL_LLM_ENGINE_ID, true, true));
        let forwards = select_text_model(
            &[a.clone(), b.clone()],
            &TextRequirements::default(),
            CloudPolicy::LocalOnly,
        );
        let backwards = select_text_model(
            &[b, a],
            &TextRequirements::default(),
            CloudPolicy::LocalOnly,
        );
        assert_eq!(forwards, backwards);
    }

    #[test]
    fn losing_a_local_engine_reroutes_to_the_local_fallback() {
        let decision = degrade(
            &EngineId::new(APPLE_ENGINE_ID),
            &AvailabilityReason::DisabledByUser,
            true,
            &[stt(WHISPER_ENGINE_ID, true, true)],
        );
        match decision {
            Degradation::Reroute {
                engine_id,
                explanation,
            } => {
                assert_eq!(engine_id.as_str(), WHISPER_ENGINE_ID);
                assert_eq!(explanation.code, "disabled_by_user");
            }
            other => panic!("expected a local reroute, got {other:?}"),
        }
    }

    #[test]
    fn losing_a_local_engine_with_only_cloud_left_asks_the_user() {
        let decision = degrade(
            &EngineId::new(APPLE_ENGINE_ID),
            &AvailabilityReason::DisabledByUser,
            true,
            &[stt(OPENROUTER_ENGINE_ID, true, false)],
        );
        match decision {
            Degradation::AwaitUserChoice {
                reason,
                alternatives,
                ..
            } => {
                assert_eq!(reason, DegradationBlock::WouldChangePrivacyClass);
                assert_eq!(alternatives, [EngineId::new(OPENROUTER_ENGINE_ID)]);
            }
            other => panic!("expected a user choice, got {other:?}"),
        }
    }

    #[test]
    fn losing_a_cloud_engine_never_silently_runs_locally() {
        let decision = degrade(
            &EngineId::new(OPENROUTER_ENGINE_ID),
            &AvailabilityReason::CredentialMissing,
            false,
            &[stt(WHISPER_ENGINE_ID, true, true)],
        );
        assert!(matches!(decision, Degradation::AwaitUserChoice { .. }));
    }

    #[test]
    fn nothing_left_is_reported_as_nothing_left() {
        let decision = degrade(
            &EngineId::new(APPLE_ENGINE_ID),
            &AvailabilityReason::HardwareUnsupported { detail: "x".into() },
            true,
            &[],
        );
        match decision {
            Degradation::AwaitUserChoice { reason, .. } => {
                assert_eq!(reason, DegradationBlock::NoAlternative);
            }
            other => panic!("expected NoAlternative, got {other:?}"),
        }
    }

    #[test]
    fn a_probe_result_becomes_the_candidate_outcome() {
        let available = candidate_from_probe(stt(APPLE_ENGINE_ID, true, true));
        assert_eq!(available.probe, ProbeOutcome::Succeeded);

        let mut unprobed = stt(APPLE_ENGINE_ID, true, true);
        unprobed.availability_reason = AvailabilityReason::NotProbed;
        assert_eq!(
            candidate_from_probe(unprobed).probe,
            ProbeOutcome::NotRun,
            "an unprobed engine is never treated as available"
        );

        let failed = candidate_from_probe(stt(APPLE_ENGINE_ID, false, true));
        match failed.probe {
            ProbeOutcome::Failed(reason) => {
                assert!(
                    reason.contains("Apple Intelligence is switched off"),
                    "the candidate must carry the explanation, got {reason}"
                );
            }
            other => panic!("expected a failed probe, got {other:?}"),
        }
    }

    #[test]
    fn an_available_engine_is_never_degraded() {
        assert_eq!(
            degrade(
                &EngineId::new(APPLE_ENGINE_ID),
                &AvailabilityReason::Available,
                true,
                &[stt(WHISPER_ENGINE_ID, true, true)],
            ),
            Degradation::Continue
        );
    }

    #[test]
    fn whisper_is_preferred_over_another_local_engine_when_both_remain() {
        let decision = degrade(
            &EngineId::new(APPLE_ENGINE_ID),
            &AvailabilityReason::DisabledByUser,
            true,
            &[
                stt("some-other-local", true, true),
                stt(WHISPER_ENGINE_ID, true, true),
            ],
        );
        match decision {
            Degradation::Reroute { engine_id, .. } => {
                assert_eq!(engine_id.as_str(), WHISPER_ENGINE_ID);
            }
            other => panic!("expected whisper, got {other:?}"),
        }
    }

    // ---------------------------------------------------------- #306 ----
    //
    // The released v0.1.0 macOS bundle was built without the Swift bridge,
    // and the Engines screen blamed the user's Mac for it: "not provided by
    // this system ... Update macOS". A build decision has to be reported as
    // a build decision, and a genuinely old macOS keeps its own sentence.

    fn apple_engines() -> [EngineId; 2] {
        [
            EngineId::new(APPLE_ENGINE_ID),
            EngineId::new(APPLE_TEXT_ENGINE_ID),
        ]
    }

    fn platform(detail: &str) -> AvailabilityReason {
        AvailabilityReason::PlatformUnsupported {
            platform: detail.into(),
        }
    }

    #[test]
    fn issue_306_a_build_without_the_bridge_says_so() {
        for engine in apple_engines() {
            let explanation = explain(&engine, &platform("bridge_not_linked"));
            assert!(
                explanation
                    .headline
                    .contains("built without the Apple bridge"),
                "{engine:?}: {}",
                explanation.headline
            );
            assert_eq!(explanation.code, "build_without_apple_bridge");
        }
    }

    #[test]
    fn issue_306_a_build_without_the_bridge_does_not_blame_the_system() {
        for engine in apple_engines() {
            let explanation = explain(&engine, &platform("bridge_not_linked"));
            assert!(
                !explanation.headline.contains("not provided by this system"),
                "{engine:?}: {}",
                explanation.headline
            );
            assert!(
                !explanation.actions.contains(&CorrectiveAction::UpdateMacOs),
                "updating macOS cannot add a bridge the build left out"
            );
            assert!(explanation
                .actions
                .contains(&CorrectiveAction::UseLocalWhisper));
        }
    }

    #[test]
    fn issue_306_a_build_compiled_without_the_frameworks_is_a_build_fact_too() {
        // `#if canImport(Speech)` failing is decided by the SDK at compile
        // time, not by the Mac the bundle runs on.
        for detail in [
            "speech_framework_absent",
            "foundation_models_framework_absent",
        ] {
            let explanation = explain(&EngineId::new(APPLE_ENGINE_ID), &platform(detail));
            assert_eq!(explanation.code, "build_without_apple_bridge", "{detail}");
            assert!(
                !explanation.actions.contains(&CorrectiveAction::UpdateMacOs),
                "{detail}"
            );
        }
    }

    #[test]
    fn issue_306_an_older_macos_keeps_an_honest_update_message() {
        for detail in [
            "speech_requires_macos_26",
            "foundation_models_requires_macos_26",
        ] {
            let explanation = explain(&EngineId::new(APPLE_ENGINE_ID), &platform(detail));
            assert_eq!(explanation.code, "macos_too_old", "{detail}");
            assert!(
                explanation.headline.contains("macOS 26 or later"),
                "{detail}: {}",
                explanation.headline
            );
            assert_eq!(
                explanation.actions,
                vec![
                    CorrectiveAction::UpdateMacOs,
                    CorrectiveAction::UseLocalWhisper
                ]
            );
        }
    }

    #[test]
    fn issue_306_off_macos_the_sentence_names_the_os_not_the_build() {
        let explanation = explain(&EngineId::new(APPLE_ENGINE_ID), &platform("not_macos"));
        assert_eq!(explanation.code, "not_macos");
        assert!(
            explanation.headline.contains("only on macOS"),
            "{}",
            explanation.headline
        );
        assert!(!explanation.actions.contains(&CorrectiveAction::UpdateMacOs));
    }

    #[test]
    fn issue_306_the_causes_have_distinct_codes() {
        let engine = EngineId::new(APPLE_ENGINE_ID);
        let codes: std::collections::BTreeSet<_> = [
            "bridge_not_linked",
            "speech_requires_macos_26",
            "not_macos",
            "something_unforeseen",
        ]
        .iter()
        .map(|detail| explain(&engine, &platform(detail)).code)
        .collect();
        assert_eq!(codes.len(), 4, "{codes:?}");
    }
}
