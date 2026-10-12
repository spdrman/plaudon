//! Engine selection policy (spec §8.3, §8.4 — task F1.4).
//!
//! §8.4 states the policy in prose:
//!
//! * **macOS** — Apple SpeechTranscriber "when OS/hardware/locale/assets
//!   allow", tested *at runtime*, "not deduced solely from macOS version";
//!   whisper.cpp as the cross-platform alternative.
//! * **Windows** — whisper.cpp as the validated default local engine.
//! * **Cloud** — OpenRouter's dedicated transcription endpoint only, never "a
//!   text-only chat model used as if it can decode audio".
//!
//! This module turns that into a deterministic function. Three properties are
//! load-bearing:
//!
//! 1. **Every candidate gets a verdict.** The result lists each candidate with
//!    either `Selected` or a specific rejection, so the UI can say *why*
//!    rather than silently downgrading (§5.4).
//! 2. **Unknown never satisfies.** Capability gating delegates to
//!    [`plaudon_ai_contracts::policy::evaluate_stt_candidate`], where every
//!    check is `Tri::satisfies` or `Known::get`.
//! 3. **Nothing here performs I/O.** Selection is a pure function of declared
//!    capabilities, a runtime probe result the caller supplies, and policy.
//!    That is what makes "zero outgoing bytes when consent is absent" (E2E #7)
//!    structurally true rather than a hope: a rejected cloud engine is never
//!    even constructed.

use plaudon_ai_contracts::capabilities::{PrivacyClass, RequestFamily, SttCapabilities};
use plaudon_ai_contracts::policy::{evaluate_stt_candidate, EngineRejection, SttRequirements};
use plaudon_domain::model::EngineId;

/// Engine identifier of the Apple native speech engine.
pub const APPLE_ENGINE_ID: &str = "apple-speech";

/// Engine identifier of the bundled whisper.cpp engine.
pub const WHISPER_ENGINE_ID: &str = "whisper.cpp";

/// Host platform, as it affects engine availability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Platform {
    /// macOS, where the Apple engine may exist.
    MacOs,
    /// Windows, where whisper.cpp is the default local engine.
    Windows,
    /// Anything else. Local STT is whisper.cpp only.
    Other,
}

impl Platform {
    /// The platform this build is running on.
    #[must_use]
    pub const fn current() -> Self {
        #[cfg(target_os = "macos")]
        {
            Self::MacOs
        }
        #[cfg(target_os = "windows")]
        {
            Self::Windows
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            Self::Other
        }
    }

    /// Whether a platform-exclusive engine may be offered here at all.
    #[must_use]
    pub fn offers_engine(self, engine_id: &EngineId) -> bool {
        if engine_id.as_str() == APPLE_ENGINE_ID {
            return self == Self::MacOs;
        }
        true
    }

    /// Stable label for diagnostics.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MacOs => "macos",
            Self::Windows => "windows",
            Self::Other => "other",
        }
    }
}

/// Result of a runtime availability probe (§8.4).
///
/// A probe result is required for every platform-exclusive engine; the policy
/// refuses to infer availability from the operating-system version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeOutcome {
    /// The probe ran and the engine works.
    Succeeded,
    /// The probe ran and the engine does not work here.
    Failed(String),
    /// No probe has run. Treated as unavailable.
    NotRun,
}

/// Whether the user has permitted cloud processing, and in what mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloudPolicy {
    /// Local-only mode: every app-controlled network operation is refused
    /// (§1.2(15)).
    LocalOnly,
    /// Network is permitted but the user has not consented to cloud AI.
    NetworkWithoutConsent,
    /// The user granted cloud AI consent.
    ConsentGranted,
}

impl CloudPolicy {
    /// Whether a cloud engine may be considered at all.
    #[must_use]
    pub const fn permits_cloud(self) -> bool {
        matches!(self, Self::ConsentGranted)
    }
}

/// One engine offered to the policy.
#[derive(Debug, Clone, PartialEq)]
pub struct EngineCandidate {
    /// What the engine declares (§8.3).
    pub capabilities: SttCapabilities,
    /// Result of the runtime availability probe.
    ///
    /// Local engines that need no probe pass [`ProbeOutcome::Succeeded`];
    /// a platform engine that was never probed passes
    /// [`ProbeOutcome::NotRun`], which is refused.
    pub probe: ProbeOutcome,
}

impl EngineCandidate {
    /// A candidate whose runtime probe already succeeded.
    #[must_use]
    pub fn probed(capabilities: SttCapabilities) -> Self {
        Self {
            capabilities,
            probe: ProbeOutcome::Succeeded,
        }
    }

    /// This candidate's engine identifier.
    #[must_use]
    pub fn engine_id(&self) -> &EngineId {
        &self.capabilities.engine_id
    }

    fn is_cloud(&self) -> bool {
        !self.capabilities.offline.satisfies()
    }
}

/// Everything selection needs besides the candidates.
#[derive(Debug, Clone, PartialEq)]
pub struct SelectionRequest {
    /// Host platform.
    pub platform: Platform,
    /// What the job needs from the engine.
    pub requirements: SttRequirements,
    /// Cloud permission state.
    pub cloud_policy: CloudPolicy,
}

impl SelectionRequest {
    /// A local-only request on the current platform.
    #[must_use]
    pub fn local_only(requirements: SttRequirements) -> Self {
        Self {
            platform: Platform::current(),
            requirements,
            cloud_policy: CloudPolicy::LocalOnly,
        }
    }
}

/// Why a candidate was not chosen.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SelectionRejection {
    /// A declared capability does not satisfy the job.
    #[error("capability check failed: {0:?}")]
    Capability(EngineRejection),

    /// This engine does not exist on this platform (§8.4: Apple is macOS-only).
    #[error("engine is not offered on {platform}")]
    PlatformEngineNotOffered {
        /// Platform label.
        platform: &'static str,
    },

    /// The runtime probe ran and failed.
    #[error("runtime availability probe failed: {0}")]
    RuntimeProbeFailed(String),

    /// No runtime probe has been performed.
    #[error("runtime availability has not been probed; availability is never inferred from the OS version")]
    RuntimeProbeNotRun,

    /// Local-only mode forbids every networked engine (§1.2(15)).
    #[error("local-only mode refuses networked engines")]
    LocalOnlyModeBlocksCloud,

    /// Cloud consent has not been granted (E2E #7).
    #[error("cloud processing requires explicit consent, which has not been granted")]
    CloudConsentMissing,

    /// The model's wire contract is unverified, so it is shown as
    /// **Not verified** rather than dispatched (§8.6A, E2E #22).
    #[error("request family `{family}` is not verified for audio; model is shown as Not verified")]
    RequestFamilyNotVerified {
        /// The declared family.
        family: &'static str,
    },

    /// A cloud engine did not resolve a privacy route (§10.8).
    #[error("privacy route for this cloud engine could not be resolved")]
    PrivacyRouteUnresolved,

    /// A better-ranked candidate was already chosen.
    #[error("another candidate was preferred on this platform")]
    NotPreferred,
}

/// A candidate and its verdict.
#[derive(Debug, Clone, PartialEq)]
pub struct CandidateOutcome {
    /// Engine the verdict applies to.
    pub engine_id: EngineId,
    /// `None` when selected, otherwise the reason it was not.
    pub rejection: Option<SelectionRejection>,
}

impl CandidateOutcome {
    /// Whether this candidate was the chosen engine.
    #[must_use]
    pub const fn is_selected(&self) -> bool {
        self.rejection.is_none()
    }
}

/// The full, explained selection result.
#[derive(Debug, Clone, PartialEq)]
pub struct EngineDecision {
    /// Chosen engine, if any candidate qualified.
    pub chosen: Option<EngineId>,
    /// Every candidate with its verdict, in evaluation order.
    pub outcomes: Vec<CandidateOutcome>,
}

impl EngineDecision {
    /// Human-readable explanation of why each candidate was or was not used.
    ///
    /// Used verbatim by the UI when it has to tell the user that the Apple
    /// engine was unavailable and whisper.cpp ran instead (E2E #10).
    #[must_use]
    pub fn explain(&self) -> Vec<String> {
        self.outcomes
            .iter()
            .map(|outcome| match &outcome.rejection {
                None => format!("{}: selected", outcome.engine_id.as_str()),
                Some(rejection) => format!("{}: {rejection}", outcome.engine_id.as_str()),
            })
            .collect()
    }

    /// The rejection recorded for one engine, if it was evaluated.
    #[must_use]
    pub fn rejection_for(&self, engine_id: &str) -> Option<&SelectionRejection> {
        self.outcomes
            .iter()
            .find(|outcome| outcome.engine_id.as_str() == engine_id)
            .and_then(|outcome| outcome.rejection.as_ref())
    }
}

/// Preference rank: lower is tried first.
///
/// Local engines always outrank cloud ones, and on macOS the Apple engine
/// outranks whisper.cpp because §8.4 prefers the native path when it is
/// genuinely available. Ranking is total and deterministic: ties break on the
/// engine identifier, so the result never depends on input ordering.
fn preference_rank(platform: Platform, candidate: &EngineCandidate) -> (u8, String) {
    let engine = candidate.engine_id().as_str().to_string();
    let tier = match (platform, engine.as_str(), candidate.is_cloud()) {
        (_, _, true) => 30,
        (Platform::MacOs, APPLE_ENGINE_ID, false) => 10,
        (_, WHISPER_ENGINE_ID, false) => 11,
        (_, _, false) => 20,
    };
    (tier, engine)
}

/// Choose an engine, explaining every candidate.
///
/// Every candidate is evaluated on its own merits, so a rejected engine
/// always reports the *real* reason it was refused rather than being masked
/// by a higher-ranked winner. `NotPreferred` therefore means exactly one
/// thing: this engine was eligible, and another eligible engine outranked it.
///
/// Deterministic: the same inputs always produce the same decision and the
/// same ordering of outcomes.
#[must_use]
pub fn select_engine(request: &SelectionRequest, candidates: &[EngineCandidate]) -> EngineDecision {
    let mut ordered: Vec<&EngineCandidate> = candidates.iter().collect();
    ordered.sort_by_key(|candidate| preference_rank(request.platform, candidate));

    let mut outcomes: Vec<CandidateOutcome> = ordered
        .iter()
        .map(|candidate| CandidateOutcome {
            engine_id: candidate.engine_id().clone(),
            rejection: evaluate(request, candidate),
        })
        .collect();

    let winner = outcomes
        .iter()
        .position(|outcome| outcome.rejection.is_none());
    let chosen = winner.map(|index| outcomes[index].engine_id.clone());

    if let Some(winner) = winner {
        for (index, outcome) in outcomes.iter_mut().enumerate() {
            if index != winner && outcome.rejection.is_none() {
                outcome.rejection = Some(SelectionRejection::NotPreferred);
            }
        }
    }

    EngineDecision { chosen, outcomes }
}

fn evaluate(request: &SelectionRequest, candidate: &EngineCandidate) -> Option<SelectionRejection> {
    // 1. Platform exclusivity. Apple is never offered off macOS, whatever the
    //    capability payload claims.
    if !request.platform.offers_engine(candidate.engine_id()) {
        return Some(SelectionRejection::PlatformEngineNotOffered {
            platform: request.platform.as_str(),
        });
    }

    // 2. Runtime probe. Availability is tested, never deduced (§8.4).
    match &candidate.probe {
        ProbeOutcome::Succeeded => {}
        ProbeOutcome::Failed(reason) => {
            return Some(SelectionRejection::RuntimeProbeFailed(reason.clone()))
        }
        ProbeOutcome::NotRun => return Some(SelectionRejection::RuntimeProbeNotRun),
    }

    // 3. Network policy and consent, before any capability detail. A cloud
    //    engine that fails here is never constructed, so no byte can leave.
    if candidate.is_cloud() {
        match request.cloud_policy {
            CloudPolicy::LocalOnly => return Some(SelectionRejection::LocalOnlyModeBlocksCloud),
            CloudPolicy::NetworkWithoutConsent => {
                return Some(SelectionRejection::CloudConsentMissing)
            }
            CloudPolicy::ConsentGranted => {}
        }
        // 4. Wire contract. `unknown` and `unsupported` both fail closed and
        //    surface as Not verified (§8.6A, E2E #22).
        if !candidate.capabilities.request_family.has_builder() {
            return Some(SelectionRejection::RequestFamilyNotVerified {
                family: family_label(candidate.capabilities.request_family),
            });
        }
        // 5. Privacy route must resolve (§10.8).
        match candidate.capabilities.privacy_class.get() {
            Some(class) if class.is_dispatchable() => {}
            _ => return Some(SelectionRejection::PrivacyRouteUnresolved),
        }
        if matches!(
            candidate.capabilities.privacy_class.get(),
            Some(PrivacyClass::OnDevice)
        ) {
            // An engine that claims it is not offline but routes on-device is
            // self-contradictory; refuse rather than guess.
            return Some(SelectionRejection::PrivacyRouteUnresolved);
        }
    }

    // 6. Declared capabilities versus the job's requirements.
    evaluate_stt_candidate(&candidate.capabilities, &request.requirements)
        .map(SelectionRejection::Capability)
}

const fn family_label(family: RequestFamily) -> &'static str {
    match family {
        RequestFamily::MultipartStt => "multipart_stt",
        RequestFamily::AudioChat => "audio_chat",
        RequestFamily::Unsupported => "unsupported",
        RequestFamily::Unknown => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plaudon_ai_contracts::capabilities::{AvailabilityReason, MediaFormat};
    use plaudon_domain::capability::{CapabilityStatus, Known, Tri};
    use plaudon_domain::locale::LanguageTag;
    use plaudon_domain::model::ModelIdentity;

    fn wav() -> MediaFormat {
        MediaFormat::new("wav", "pcm_s16le")
    }

    fn english() -> LanguageTag {
        LanguageTag::parse("en").expect("tag")
    }

    fn base(engine: &str, model: &str) -> SttCapabilities {
        let engine_id = EngineId::new(engine);
        let mut caps =
            SttCapabilities::unknown(engine_id.clone(), ModelIdentity::new(engine_id, model));
        caps.status = CapabilityStatus::Supported;
        caps.availability_reason = AvailabilityReason::Available;
        caps.supports_segments = Tri::Yes;
        caps.can_cancel = Tri::Yes;
        caps.supported_formats = Known::Known(vec![wav()]);
        caps.locales = Known::Known(vec![english()]);
        caps
    }

    fn whisper() -> SttCapabilities {
        let mut caps = base(WHISPER_ENGINE_ID, "tiny.en");
        caps.offline = Tri::Yes;
        caps.privacy_class = Known::Known(PrivacyClass::OnDevice);
        caps.request_family = RequestFamily::Unsupported;
        caps
    }

    fn apple() -> SttCapabilities {
        let mut caps = base(APPLE_ENGINE_ID, "speech-transcriber");
        caps.offline = Tri::Yes;
        caps.privacy_class = Known::Known(PrivacyClass::OnDevice);
        caps.request_family = RequestFamily::Unsupported;
        caps
    }

    fn cloud(family: RequestFamily) -> SttCapabilities {
        let mut caps = base("openrouter", "some/stt-model");
        caps.offline = Tri::No;
        caps.privacy_class = Known::Known(PrivacyClass::CloudZeroRetention);
        caps.request_family = family;
        caps
    }

    fn requirements() -> SttRequirements {
        SttRequirements {
            format: Some(wav()),
            locale: Some(english()),
            require_segments: true,
            require_cancellation: true,
            ..SttRequirements::default()
        }
    }

    fn request(platform: Platform, cloud_policy: CloudPolicy) -> SelectionRequest {
        SelectionRequest {
            platform,
            requirements: requirements(),
            cloud_policy,
        }
    }

    #[test]
    fn whisper_is_the_windows_default_and_apple_is_never_offered() {
        let decision = select_engine(
            &request(Platform::Windows, CloudPolicy::LocalOnly),
            &[
                EngineCandidate::probed(apple()),
                EngineCandidate::probed(whisper()),
            ],
        );
        assert_eq!(
            decision.chosen.as_ref().map(EngineId::as_str),
            Some(WHISPER_ENGINE_ID)
        );
        assert_eq!(
            decision.rejection_for(APPLE_ENGINE_ID),
            Some(&SelectionRejection::PlatformEngineNotOffered {
                platform: "windows"
            })
        );
    }

    #[test]
    fn macos_prefers_apple_when_the_runtime_probe_succeeds() {
        let decision = select_engine(
            &request(Platform::MacOs, CloudPolicy::LocalOnly),
            &[
                EngineCandidate::probed(whisper()),
                EngineCandidate::probed(apple()),
            ],
        );
        assert_eq!(
            decision.chosen.as_ref().map(EngineId::as_str),
            Some(APPLE_ENGINE_ID)
        );
        assert_eq!(
            decision.rejection_for(WHISPER_ENGINE_ID),
            Some(&SelectionRejection::NotPreferred)
        );
    }

    #[test]
    fn a_failed_apple_probe_falls_back_to_whisper_with_a_stated_reason() {
        let decision = select_engine(
            &request(Platform::MacOs, CloudPolicy::LocalOnly),
            &[
                EngineCandidate {
                    capabilities: apple(),
                    probe: ProbeOutcome::Failed("speech assets not installed".into()),
                },
                EngineCandidate::probed(whisper()),
            ],
        );
        assert_eq!(
            decision.chosen.as_ref().map(EngineId::as_str),
            Some(WHISPER_ENGINE_ID)
        );
        let reason = decision.rejection_for(APPLE_ENGINE_ID).expect("reason");
        assert_eq!(
            reason,
            &SelectionRejection::RuntimeProbeFailed("speech assets not installed".into())
        );
        assert!(decision
            .explain()
            .iter()
            .any(|line| line.contains("speech assets not installed")));
    }

    #[test]
    fn an_unprobed_platform_engine_is_never_inferred_available() {
        let decision = select_engine(
            &request(Platform::MacOs, CloudPolicy::LocalOnly),
            &[
                EngineCandidate {
                    capabilities: apple(),
                    probe: ProbeOutcome::NotRun,
                },
                EngineCandidate::probed(whisper()),
            ],
        );
        assert_eq!(
            decision.rejection_for(APPLE_ENGINE_ID),
            Some(&SelectionRejection::RuntimeProbeNotRun)
        );
        assert_eq!(
            decision.chosen.as_ref().map(EngineId::as_str),
            Some(WHISPER_ENGINE_ID)
        );
    }

    #[test]
    fn a_failed_apple_probe_does_not_change_cloud_policy() {
        // E2E #10: falling back to whisper must not quietly enable cloud.
        let decision = select_engine(
            &request(Platform::MacOs, CloudPolicy::NetworkWithoutConsent),
            &[
                EngineCandidate {
                    capabilities: apple(),
                    probe: ProbeOutcome::Failed("unavailable".into()),
                },
                EngineCandidate::probed(cloud(RequestFamily::MultipartStt)),
                EngineCandidate::probed(whisper()),
            ],
        );
        assert_eq!(
            decision.chosen.as_ref().map(EngineId::as_str),
            Some(WHISPER_ENGINE_ID)
        );
        assert_eq!(
            decision.rejection_for("openrouter"),
            Some(&SelectionRejection::CloudConsentMissing),
            "an Apple probe failure must never grant cloud consent"
        );
    }

    #[test]
    fn local_only_mode_excludes_every_cloud_engine() {
        let decision = select_engine(
            &request(Platform::MacOs, CloudPolicy::LocalOnly),
            &[EngineCandidate::probed(cloud(RequestFamily::MultipartStt))],
        );
        assert_eq!(decision.chosen, None);
        assert_eq!(
            decision.rejection_for("openrouter"),
            Some(&SelectionRejection::LocalOnlyModeBlocksCloud)
        );
    }

    #[test]
    fn absent_consent_excludes_every_cloud_engine() {
        let decision = select_engine(
            &request(Platform::MacOs, CloudPolicy::NetworkWithoutConsent),
            &[EngineCandidate::probed(cloud(RequestFamily::MultipartStt))],
        );
        assert_eq!(decision.chosen, None);
        assert_eq!(
            decision.rejection_for("openrouter"),
            Some(&SelectionRejection::CloudConsentMissing)
        );
    }

    #[test]
    fn an_unverified_request_family_is_never_selected() {
        for family in [RequestFamily::Unknown, RequestFamily::Unsupported] {
            let decision = select_engine(
                &request(Platform::MacOs, CloudPolicy::ConsentGranted),
                &[EngineCandidate::probed(cloud(family))],
            );
            assert_eq!(decision.chosen, None, "{family:?} was selected");
            assert!(matches!(
                decision.rejection_for("openrouter"),
                Some(SelectionRejection::RequestFamilyNotVerified { .. })
            ));
        }
    }

    #[test]
    fn a_verified_cloud_engine_is_selectable_once_consent_exists() {
        let decision = select_engine(
            &request(Platform::MacOs, CloudPolicy::ConsentGranted),
            &[EngineCandidate::probed(cloud(RequestFamily::MultipartStt))],
        );
        assert_eq!(
            decision.chosen.as_ref().map(EngineId::as_str),
            Some("openrouter")
        );
    }

    #[test]
    fn an_unresolved_privacy_route_blocks_dispatch() {
        let mut caps = cloud(RequestFamily::MultipartStt);
        caps.privacy_class = Known::Unknown;
        let decision = select_engine(
            &request(Platform::MacOs, CloudPolicy::ConsentGranted),
            &[EngineCandidate::probed(caps)],
        );
        assert_eq!(
            decision.rejection_for("openrouter"),
            Some(&SelectionRejection::PrivacyRouteUnresolved)
        );
    }

    #[test]
    fn an_engine_that_does_not_declare_offline_is_treated_as_networked() {
        // `offline = Unknown` must not be read as "runs locally"; the engine
        // falls under the cloud gate and local-only mode refuses it.
        let engine = EngineId::new(WHISPER_ENGINE_ID);
        let unknown =
            SttCapabilities::unknown(engine.clone(), ModelIdentity::new(engine, "tiny.en"));
        assert_eq!(unknown.offline, Tri::Unknown);
        let decision = select_engine(
            &request(Platform::Windows, CloudPolicy::LocalOnly),
            &[EngineCandidate::probed(unknown)],
        );
        assert_eq!(decision.chosen, None);
        assert_eq!(
            decision.rejection_for(WHISPER_ENGINE_ID),
            Some(&SelectionRejection::LocalOnlyModeBlocksCloud)
        );
    }

    #[test]
    fn unknown_capabilities_never_satisfy_a_requirement() {
        // Offline is declared, so the cloud gate does not apply; every other
        // field is `unknown`, and none of them may satisfy a requirement.
        let engine = EngineId::new(WHISPER_ENGINE_ID);
        let mut unknown =
            SttCapabilities::unknown(engine.clone(), ModelIdentity::new(engine, "tiny.en"));
        unknown.offline = Tri::Yes;
        unknown.status = CapabilityStatus::Supported;
        unknown.availability_reason = AvailabilityReason::Available;
        let decision = select_engine(
            &request(Platform::Windows, CloudPolicy::LocalOnly),
            &[EngineCandidate::probed(unknown)],
        );
        assert_eq!(decision.chosen, None);
        assert!(
            matches!(
                decision.rejection_for(WHISPER_ENGINE_ID),
                Some(SelectionRejection::Capability(_))
            ),
            "got {:?}",
            decision.rejection_for(WHISPER_ENGINE_ID)
        );
    }

    #[test]
    fn an_unprobed_capability_status_is_not_selectable() {
        // `CapabilityStatus::Unknown` is the default; only `Supported` is
        // selectable (spec 6.6).
        let mut caps = whisper();
        caps.status = CapabilityStatus::Unknown;
        let decision = select_engine(
            &request(Platform::Windows, CloudPolicy::LocalOnly),
            &[EngineCandidate::probed(caps)],
        );
        assert_eq!(
            decision.rejection_for(WHISPER_ENGINE_ID),
            Some(&SelectionRejection::Capability(
                EngineRejection::NotSupportedStatus
            ))
        );
    }

    #[test]
    fn an_engine_exceeding_a_declared_limit_is_never_chosen() {
        let mut caps = whisper();
        caps.max_duration_ms = Known::Known(60_000);
        let mut requirements = requirements();
        requirements.duration_ms = Some(3_600_000);
        let decision = select_engine(
            &SelectionRequest {
                platform: Platform::Windows,
                requirements,
                cloud_policy: CloudPolicy::LocalOnly,
            },
            &[EngineCandidate::probed(caps)],
        );
        assert_eq!(
            decision.rejection_for(WHISPER_ENGINE_ID),
            Some(&SelectionRejection::Capability(
                EngineRejection::DurationTooLong
            ))
        );
    }

    #[test]
    fn a_locale_the_engine_does_not_declare_is_rejected() {
        let mut requirements = requirements();
        requirements.locale = Some(LanguageTag::parse("de").expect("tag"));
        let decision = select_engine(
            &SelectionRequest {
                platform: Platform::Windows,
                requirements,
                cloud_policy: CloudPolicy::LocalOnly,
            },
            &[EngineCandidate::probed(whisper())],
        );
        assert_eq!(
            decision.rejection_for(WHISPER_ENGINE_ID),
            Some(&SelectionRejection::Capability(
                EngineRejection::LocaleNotDeclared
            ))
        );
    }

    #[test]
    fn selection_is_independent_of_candidate_ordering() {
        let request = request(Platform::MacOs, CloudPolicy::ConsentGranted);
        let forward = select_engine(
            &request,
            &[
                EngineCandidate::probed(whisper()),
                EngineCandidate::probed(apple()),
                EngineCandidate::probed(cloud(RequestFamily::MultipartStt)),
            ],
        );
        let reversed = select_engine(
            &request,
            &[
                EngineCandidate::probed(cloud(RequestFamily::MultipartStt)),
                EngineCandidate::probed(apple()),
                EngineCandidate::probed(whisper()),
            ],
        );
        assert_eq!(forward, reversed);
    }

    #[test]
    fn every_candidate_receives_an_explanation() {
        let decision = select_engine(
            &request(Platform::Windows, CloudPolicy::LocalOnly),
            &[
                EngineCandidate::probed(apple()),
                EngineCandidate::probed(whisper()),
                EngineCandidate::probed(cloud(RequestFamily::Unknown)),
            ],
        );
        assert_eq!(decision.outcomes.len(), 3);
        assert_eq!(decision.explain().len(), 3);
        assert_eq!(
            decision.outcomes.iter().filter(|o| o.is_selected()).count(),
            1
        );
    }

    #[test]
    fn no_eligible_engine_is_explained_rather_than_defaulted() {
        let decision = select_engine(
            &request(Platform::Windows, CloudPolicy::LocalOnly),
            &[EngineCandidate {
                capabilities: whisper(),
                probe: ProbeOutcome::Failed("no model installed".into()),
            }],
        );
        assert_eq!(decision.chosen, None);
        assert_eq!(
            decision.explain(),
            vec!["whisper.cpp: runtime availability probe failed: no model installed"]
        );
    }

    #[test]
    fn an_empty_candidate_list_selects_nothing() {
        let decision = select_engine(&request(Platform::MacOs, CloudPolicy::LocalOnly), &[]);
        assert_eq!(decision.chosen, None);
        assert_eq!(decision.outcomes.len(), 0);
    }
}
